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

# PDH "GPU Process Memory(pid_<pid>_*)" Local Usage and Dedicated Usage, about once a second, as
# JSON lines in $Out, until the job is stopped. The instance exists only once the process has a
# D3D device, so each sample is its own Get-Counter call and a missing instance is retried, not
# fatal (a -Continuous Get-Counter on a not-yet-existing instance ends at once: the owner's first
# S0.6 run got 0 samples). Retries are written as {"t":..,"missing":true} lines.
function Start-VramSampler([int]$ProcessId, [string]$Out) {
    Start-Job -ArgumentList $ProcessId, $Out -ScriptBlock {
        param($procId, $out)
        $paths = foreach ($n in 'Local Usage', 'Dedicated Usage') { "\GPU Process Memory(pid_${procId}_*)\$n" }
        while ($true) {
            $t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            try {
                $c = Get-Counter -Counter $paths -SampleInterval 1 -MaxSamples 1 -ErrorAction Stop
                $row = [ordered]@{ t = ([DateTimeOffset]$c.Timestamp).ToUnixTimeMilliseconds() }
                foreach ($key in 'local usage', 'dedicated usage') {
                    $sum = ($c.CounterSamples | Where-Object { $_.Path -like "*\$key" } | Measure-Object CookedValue -Sum).Sum
                    $row[($key -replace ' ', '_') + '_mib'] = [math]::Round($sum / 1MB, 1)
                }
            } catch {
                $row = [ordered]@{ t = $t; missing = $true }
                Start-Sleep -Milliseconds 500
            }
            Add-Content -Path $out -Value ($row | ConvertTo-Json -Compress)
        }
    }
}

# VRAM samples that hold values (the "missing" retries left out).
function Read-VramSamples([string]$File) {
    @(Read-JsonLines $File | Where-Object { -not $_.missing -and $null -ne $_.local_usage_mib })
}

$script:DisplaySource = @'
[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct DEVMODE {
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
    public short dmSpecVersion; public short dmDriverVersion; public short dmSize; public short dmDriverExtra;
    public int dmFields; public int dmPositionX; public int dmPositionY; public int dmDisplayOrientation;
    public int dmDisplayFixedOutput; public short dmColor; public short dmDuplex; public short dmYResolution;
    public short dmTTOption; public short dmCollate;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName;
    public short dmLogPixels; public int dmBitsPerPel; public int dmPelsWidth; public int dmPelsHeight;
    public int dmDisplayFlags; public int dmDisplayFrequency;
    public int dmICMMethod; public int dmICMIntent; public int dmMediaType; public int dmDitherType;
    public int dmReserved1; public int dmReserved2; public int dmPanningWidth; public int dmPanningHeight;
}
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern bool EnumDisplaySettingsW(string deviceName, int modeNum, ref DEVMODE devMode);
public static int PrimaryHz() {
    var dm = new DEVMODE(); dm.dmSize = (short)Marshal.SizeOf(typeof(DEVMODE));
    return EnumDisplaySettingsW(null, -1, ref dm) ? dm.dmDisplayFrequency : 0;
}
'@

# The primary display's refresh rate from Windows (EnumDisplaySettingsW with
# ENUM_CURRENT_SETTINGS for the primary device), as a cross-check of what the app reports.
# 0 if it can't be read.
function Get-OsPrimaryRefreshHz {
    if (-not ('SpikeMeasure.Display' -as [type])) {
        Add-Type -Namespace SpikeMeasure -Name Display -MemberDefinition $script:DisplaySource
    }
    [double][SpikeMeasure.Display]::PrimaryHz()
}

# Which refresh rate S0.6 judges with, and where it came from: the app's report
# (`primary_refresh_hz`; builds before fix round 2 logged `screens[].refresh_hz`), else Windows'.
function Resolve-RefreshHz($StallReport, [double]$OsHz) {
    if ($StallReport -and $StallReport.primary_refresh_hz -gt 0) {
        return [ordered]@{ hz = [double]$StallReport.primary_refresh_hz; source = 'app' }
    }
    if ($StallReport -and $StallReport.screens) {
        $p = @($StallReport.screens) | Where-Object { $_.primary -and $_.refresh_hz -gt 0 } | Select-Object -First 1
        if ($p) { return [ordered]@{ hz = [double]$p.refresh_hz; source = 'app (older report format)' } }
    }
    if ($OsHz -gt 0) { return [ordered]@{ hz = $OsHz; source = 'windows' } }
    [ordered]@{ hz = 0.0; source = 'none' }
}

# Is the deploy folder built from the current build? Our binaries in it must match the build
# tree's byte for byte; the owner's first S0.6 run used a deploy from before fix round 2. Returns
# the stale or missing files (empty when current).
function Test-DeployCurrent([string]$Deploy, [string]$BinDir) {
    $ours = 'app_shell_spike.exe', 'spike_shell_shim.dll', 'kddockwidgets-qt62.dll', 'SpikeWeb\spike_web.dll'
    @($ours | Where-Object {
        $a = Join-Path $Deploy $_; $b = Join-Path $BinDir $_
        -not (Test-Path $a) -or -not (Test-Path $b) -or (Get-FileHash $a).Hash -ne (Get-FileHash $b).Hash
    })
}

# The saturation gate before the measured minute: the same conditions as the measured window
# (dense, and >= 95 % in at least 90 % of the samples) over the last $WindowMs of samples.
function Test-GpuSaturatedRecently([object[]]$Samples, [long]$NowMs, [long]$WindowMs = 20000) {
    $c = Test-GpuCoverage -Samples $Samples -StartMs ($NowMs - $WindowMs) -EndMs $NowMs
    [bool]($c.dense -and $c.saturated)
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
    Get-OsPrimaryRefreshHz, Resolve-RefreshHz, Test-DeployCurrent, Test-GpuSaturatedRecently, Read-VramSamples,
    Start-GpuSampler, Read-GpuSamples, Start-VramSampler, Read-JsonLines, Get-CpuPercent
