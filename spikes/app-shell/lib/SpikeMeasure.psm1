<#
  Measurement helpers for the app-shell spike (S0.6, and the CPU load beside every timing).
  Pure functions (stats, coverage, verdict) are tested by tests/SpikeMeasure.Tests.ps1 (Pester).
#>

# Nearest-rank percentiles of a series; $null for an empty one (never a fake n = 1).
function Get-NearestRankStats([object[]]$Values) {
    # Untyped on purpose: a [double[]] parameter would turn $null into 0.
    $v = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
    if ($v.Count -eq 0) { return $null }
    $s = @($v | Sort-Object)
    $pick = { param($p) $s[[math]::Max(0, [math]::Ceiling($p / 100 * $s.Count) - 1)] }
    [ordered]@{ n = $s.Count; p50 = (& $pick 50); p95 = (& $pick 95); p99 = (& $pick 99); max = $s[-1] }
}

# Was the GPU loaded through the window [StartMs, EndMs]? Two conditions (controller ruling):
# - dense: at least 0.8 samples a second (48 in 60 s) and no gap over 2 s, the window's edges
#   counting as gaps;
# - saturated: utilisation >= 95 % in at least 90 % of the samples, so brief dips between prompts
#   are allowed. Every dip is recorded.
function Test-GpuCoverage {
    param(
        [object[]]$Samples,            # objects with t (epoch ms) and util (%)
        [long]$StartMs,
        [long]$EndMs,
        [int]$MinUtil = 95,
        [int]$MaxGapMs = 2000,
        [double]$MinPerSecond = 0.8,
        [double]$MinSaturatedFraction = 0.9
    )
    $in = @($Samples | Where-Object { $_.t -ge $StartMs -and $_.t -le $EndMs } | Sort-Object t)
    $needed = [math]::Floor(($EndMs - $StartMs) / 1000 * $MinPerSecond)
    $edges = @($StartMs) + @($in | ForEach-Object { [long]$_.t }) + @($EndMs)
    $maxGap = 0
    for ($i = 1; $i -lt $edges.Count; $i++) { $maxGap = [math]::Max($maxGap, $edges[$i] - $edges[$i - 1]) }
    $dips = @($in | Where-Object { $_.util -lt $MinUtil })
    $below = $dips.Count
    $fraction = if ($in.Count) { [math]::Round(($in.Count - $below) / $in.Count, 4) } else { 0 }
    [ordered]@{
        n = $in.Count; needed = [int]$needed; max_gap_ms = [long]$maxGap
        min_util = $(if ($in.Count) { ($in | Measure-Object util -Minimum).Minimum } else { $null })
        below_min = $below; saturated_fraction = $fraction
        dips = @($dips | ForEach-Object { [ordered]@{ t = [long]$_.t; util = $_.util } })
        dense = ($in.Count -ge $needed) -and ($maxGap -le $MaxGapMs)
        saturated = ($in.Count -gt 0) -and ($fraction -ge $MinSaturatedFraction)
    }
}

# The frame-interval threshold for the primary display (controller ruling):
# max(16 ms, 1.5 vsync periods). It is continuous, never stricter than the bar's 16 ms, and always
# fails a dropped vsync (two periods): 240 and 144 Hz give 16, 75 Hz 20, 60 Hz 25.
function Get-FrameThresholdMs([double]$RefreshHz) {
    if (-not $RefreshHz -or $RefreshHz -le 0) { return $null }
    [math]::Round([math]::Max(16.0, 1.5 * 1000.0 / $RefreshHz), 3)
}

# Frames slower than 1.5 vsync periods (dropped vsyncs), counted directly for the reader.
function Get-DroppedFrames([object[]]$IntervalsMs, [double]$RefreshHz) {
    $v = @($IntervalsMs | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
    if (-not $RefreshHz -or $RefreshHz -le 0 -or $v.Count -eq 0) { return $null }
    $limit = 1.5 * 1000.0 / $RefreshHz
    $n = @($v | Where-Object { $_ -gt $limit }).Count
    [ordered]@{ over_1_5_vsync = $n; fraction = [math]::Round($n / $v.Count, 4); limit_ms = [math]::Round($limit, 3) }
}
# S0.6's verdict. A crash, a hang or a missing measured report is FAIL (that is the APP-135
# failure), decided before the measurement's validity; the GPU condition only makes a completed
# run INVALID.
function Get-S06Verdict {
    param(
        [bool]$HasMeasuredReport,
        $ExitCode,
        [bool]$TimedOut,
        $Heartbeat,            # Get-NearestRankStats output or $null
        $Frames,               # Get-NearestRankStats output or $null
        [double]$RefreshHz,
        $Coverage              # Test-GpuCoverage output
    )
    if ($TimedOut) { return 'FAIL (the app hung and was killed)' }
    if ($null -eq $ExitCode -or $ExitCode -ne 0) { return "FAIL (the app exited with $ExitCode)" }
    if (-not $HasMeasuredReport -or -not $Heartbeat -or -not $Frames) { return 'FAIL (no measured stall report)' }
    $threshold = Get-FrameThresholdMs $RefreshHz
    if ($null -eq $threshold) { return 'INVALID (the screen refresh rate was not recorded)' }
    if (-not $Coverage.dense) { return "INVALID (GPU samples too sparse: $($Coverage.n) of $($Coverage.needed), largest gap $($Coverage.max_gap_ms) ms)" }
    if (-not $Coverage.saturated) { return "INVALID (GPU >= 95 % in only $([math]::Round(100 * $Coverage.saturated_fraction, 1)) % of the samples; 90 % needed)" }
    $hbOk = $Heartbeat.p99 -lt 16
    $frOk = $Frames.p99 -lt $threshold
    if ($hbOk -and $frOk) { 'PASS' } else { "FAIL (heartbeat p99 $($Heartbeat.p99) ms, frame p99 $($Frames.p99) ms against $threshold ms)" }
}

# nvidia-smi samples its own clock every $IntervalMs into $Out (CSV: timestamp, util, memory).
function Start-GpuSampler([string]$Out, [int]$IntervalMs = 500) {
    $smi = (Get-Command nvidia-smi.exe -ErrorAction Stop).Source
    Start-Process -FilePath $smi -WindowStyle Hidden -PassThru -RedirectStandardOutput $Out -ArgumentList @(
        '--query-gpu=timestamp,utilization.gpu,memory.used', '--format=csv,noheader,nounits', "-lms", "$IntervalMs")
}

function Read-GpuSamples([string]$File) {
    if (-not (Test-Path $File)) { return @() }
    @(Get-Content $File | ForEach-Object {
        $p = $_ -split '\s*,\s*'
        if ($p.Count -ge 3) {
            $ts = [datetime]::ParseExact($p[0].Trim(), 'yyyy/MM/dd HH:mm:ss.fff', [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeLocal)
            [pscustomobject]@{ t = ([DateTimeOffset]$ts).ToUnixTimeMilliseconds(); util = [int]$p[1]; mem_mib = [int]$p[2] }
        }
    } | Where-Object { $_ })
}

# PDH "GPU Process Memory(pid_<pid>_*)" Local Usage and Dedicated Usage, once a second, as JSON
# lines in $Out, until the job is stopped.
function Start-VramSampler([int]$ProcessId, [string]$Out) {
    Start-Job -ArgumentList $ProcessId, $Out -ScriptBlock {
        param($procId, $out)
        $paths = foreach ($n in 'Local Usage', 'Dedicated Usage') { "\GPU Process Memory(pid_${procId}_*)\$n" }
        Get-Counter -Counter $paths -Continuous -SampleInterval 1 -ErrorAction SilentlyContinue | ForEach-Object {
            $row = [ordered]@{ t = ([DateTimeOffset]$_.Timestamp).ToUnixTimeMilliseconds() }
            foreach ($key in 'local usage', 'dedicated usage') {
                $sum = ($_.CounterSamples | Where-Object { $_.Path -like "*\$key" } | Measure-Object CookedValue -Sum).Sum
                $row[($key -replace ' ', '_') + '_mib'] = [math]::Round($sum / 1MB, 1)
            }
            Add-Content -Path $out -Value ($row | ConvertTo-Json -Compress)
        }
    }
}

function Read-JsonLines([string]$File) {
    if (-not (Test-Path $File)) { return @() }
    @(Get-Content $File | ForEach-Object { try { $_ | ConvertFrom-Json } catch {} } | Where-Object { $_ })
}

# S0.1's warning lines, as whole blocks, split by where each warning is raised:
#   own        - raised in one of our files: a relative path (CMake prints our files relative to
#                the project, e.g. `CMakeLists.txt:29`), or a path under spikes/app-shell;
#   kddw       - raised in KDDockWidgets' sources (_deps/kddockwidgets...);
#   toolchain  - raised in Qt, CMake or the cxx-qt/Corrosion helpers.
# A CMake message keeps its body, its "Call Stack (most recent call first):" lines and the
# "This warning is for project developers" note; it ends at the next other unindented line.
# via_own_file is set when the call stack passes through one of our files (recorded, not judged).
function Split-BuildWarnings([string[]]$Lines) {
    $out = [ordered]@{ own = @(); kddw = @(); toolchain = @() }
    $foreign = '(?i)^[A-Za-z]:[\\/]|_deps[\\/]'
    for ($i = 0; $i -lt $Lines.Count; $i++) {
        $l = $Lines[$i]
        if ($l -notmatch '(?i)\bwarning\b') { continue }
        if ($l -match '(?i)warning(s)? (generated|emitted)|0 warning') { continue }
        $isCMake = $l -match '^CMake Warning'
        $endIdx = [math]::Min($i + 3, $Lines.Count - 1)
        if ($isCMake) {
            $j = $i + 1
            while ($j -lt $Lines.Count -and ($Lines[$j] -match '^\s' -or -not $Lines[$j].Trim() -or
                                             $Lines[$j] -match '^Call Stack \(most recent call first\):' -or
                                             $Lines[$j] -match '^This warning is for project developers')) { $j++ }
            $endIdx = [math]::Max($i, $j - 1)
        }
        $block = ($Lines[$i..$endIdx] -join "`n").TrimEnd()
        # Where it is raised: "CMake Warning [(dev)] at <file>:<line>" or "<file>(<line>): warning".
        $origin = if ($isCMake -and $l -match '^CMake Warning(?: \(dev\))? at (.+?):\d+') { $Matches[1] }
                  elseif ($l -match '^\s*(.+?)\(\d+(?:,\d+)?\)\s*:\s*warning') { $Matches[1] }
                  else { $null }
        $stack = @($block -split "`n" | Where-Object { $_ -match '^\s+\S.*:\d+ \(' } | ForEach-Object { $_.Trim() })
        $viaOwn = @($stack | Where-Object { $_ -notmatch $foreign -and $_ -match '(^|[\\/])CMakeLists\.txt:\d+' }).Count -gt 0
        $entry = [ordered]@{ origin = $origin; via_own_file = $viaOwn; text = $block }
        if ("$origin$l" -match '(?i)_deps[\\/]kddockwidgets') { $out.kddw += $entry }
        elseif ($origin -and ($origin -match '(?i)spikes[\\/]app-shell[\\/]' -or $origin -notmatch $foreign)) { $out.own += $entry }
        elseif ($origin -or "$l" -match '(?i)C:[\\/]Qt[\\/]|Program Files|_deps[\\/](cxxqt|corrosion)') { $out.toolchain += $entry }
        else { $out.own += $entry }   # no origin we can place: counted as ours, the strict way
        if ($isCMake) { $i = $endIdx }   # the block's own lines (its note included) are not new warnings
    }
    $out
}
# The machine's CPU load (%), averaged over one second; recorded beside every timing.
function Get-CpuPercent {
    try { [math]::Round((Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 1).CounterSamples[0].CookedValue, 1) }
    catch { -1 }
}

Export-ModuleMember -Function Get-NearestRankStats, Test-GpuCoverage, Get-FrameThresholdMs, Get-DroppedFrames, Get-S06Verdict, Split-BuildWarnings,
    Start-GpuSampler, Read-GpuSamples, Start-VramSampler, Read-JsonLines, Get-CpuPercent
