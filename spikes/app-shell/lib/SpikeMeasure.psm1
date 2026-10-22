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

# Did the GPU samples cover the window [StartMs, EndMs] densely enough to say "utilisation >= 95 %
# throughout"? Coverage is judged by the largest gap between consecutive samples, counting the
# window's edges, and by a minimum count (48 in 60 s at the default 0.8 per second).
function Test-GpuCoverage {
    param(
        [object[]]$Samples,            # objects with t (epoch ms) and util (%)
        [long]$StartMs,
        [long]$EndMs,
        [int]$MinUtil = 95,
        [int]$MaxGapMs = 2000,
        [double]$MinPerSecond = 0.8
    )
    $in = @($Samples | Where-Object { $_.t -ge $StartMs -and $_.t -le $EndMs } | Sort-Object t)
    $needed = [math]::Floor(($EndMs - $StartMs) / 1000 * $MinPerSecond)
    $edges = @($StartMs) + @($in | ForEach-Object { [long]$_.t }) + @($EndMs)
    $maxGap = 0
    for ($i = 1; $i -lt $edges.Count; $i++) { $maxGap = [math]::Max($maxGap, $edges[$i] - $edges[$i - 1]) }
    $below = @($in | Where-Object { $_.util -lt $MinUtil }).Count
    [ordered]@{
        n = $in.Count; needed = [int]$needed; max_gap_ms = [long]$maxGap
        min_util = $(if ($in.Count) { ($in | Measure-Object util -Minimum).Minimum } else { $null })
        below_min = $below
        dense = ($in.Count -ge $needed) -and ($maxGap -le $MaxGapMs)
        saturated = ($in.Count -gt 0) -and ($below -eq 0)
    }
}

# The frame-interval threshold for a screen. The bar is p99 < 16 ms; a frame can't come faster
# than the vsync period, so on a screen whose period is >= 16 ms (60 Hz and below) a run that
# drops no vsync would fail on vsync alone. There the threshold becomes 1.5 vsync periods: one
# period passes, a dropped frame (two periods) fails. At 62.5 Hz and above it stays 16 ms.
function Get-FrameThresholdMs([double]$RefreshHz) {
    if (-not $RefreshHz -or $RefreshHz -le 0) { return $null }
    $period = 1000.0 / $RefreshHz
    if ($period -lt 16) { return 16.0 }
    [math]::Round(1.5 * $period, 3)
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
    if (-not $Coverage.saturated) { return "INVALID (GPU below 95 % in $($Coverage.below_min) samples)" }
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

# The machine's CPU load (%), averaged over one second; recorded beside every timing.
function Get-CpuPercent {
    try { [math]::Round((Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 1).CounterSamples[0].CookedValue, 1) }
    catch { -1 }
}

Export-ModuleMember -Function Get-NearestRankStats, Test-GpuCoverage, Get-FrameThresholdMs, Get-S06Verdict,
    Start-GpuSampler, Read-GpuSamples, Start-VramSampler, Read-JsonLines, Get-CpuPercent
