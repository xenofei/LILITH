<#
.SYNOPSIS
  Re-scores a finished S0.6 result offline under the current GPU rule (Test-GpuCoverage's level),
  from the run's own raw files; it starts nothing. It adds verdict_rescored, rescore_rule and
  gpu_coverage_rescored to the result file and keeps the original verdict.

.DESCRIPTION
  The measured window is [app start + measured t_ms - 60 s, app start + measured t_ms], as
  comfy-load.ps1 scored it. Runs from before app_start_epoch_ms was recorded (owner run 3) get it
  back from the original score: the one run of consecutive gpu.csv samples that has the recorded
  sample count and exactly the recorded dips. The heartbeat, frame, refresh-rate, exit, workload
  and web results are taken from the result file as recorded.

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/rescore-s06.ps1 -Result spikes/app-shell/results/s06.json
#>
param([Parameter(Mandatory)][string]$Result, [int]$MeasureSeconds = 60)
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'lib\SpikeMeasure.psm1') -Force
$res = Get-Content $Result -Raw | ConvertFrom-Json
$samples = @(Read-GpuSamples (Join-Path $res.runs 'gpu.csv') | Sort-Object t)
$old = $res.gpu_coverage
$winMs = $MeasureSeconds * 1000

if ($res.app_start_epoch_ms) {
    $ev = Read-JsonLines (Join-Path $res.runs 's06.jsonl')
    $m = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'measured' } | Select-Object -Last 1
    $end = [long]$res.app_start_epoch_ms + [long]$m.t_ms; $start = $end - $winMs; $how = 'app_start_epoch_ms'
} else {
    # The window holds a run of exactly n consecutive samples: a run i..i+n-1 fits a window of
    # that length if it spans no more than it, and the samples just outside it span more.
    $oldDips = @($old.dips | ForEach-Object { [long]$_.t }) -join ','
    $n = [int]$old.n
    $found = @()
    for ($i = 0; $i + $n -le $samples.Count; $i++) {
        $j = $i + $n - 1
        if ($samples[$j].t - $samples[$i].t -gt $winMs) { continue }
        $before = if ($i -gt 0) { $samples[$i - 1].t } else { [long]::MinValue / 2 }
        $after = if ($j + 1 -lt $samples.Count) { $samples[$j + 1].t } else { [long]::MaxValue / 2 }
        if ($after - $before -le $winMs) { continue }
        $in = $samples[$i..$j]
        $dips = @($in | Where-Object { $_.util -lt 95 } | ForEach-Object { [long]$_.t }) -join ','
        if ($dips -eq $oldDips) { $found += , @($in[0].t, $in[-1].t) }
    }
    if (-not $found.Count) { throw 'no run of gpu.csv samples matches the recorded coverage; not re-scoring' }
    # Runs that differ only by a sample above 95 % at an edge score the same under any lower
    # level (their samples below it are the recorded dips' subset); that is checked, not assumed.
    $windows = @($found | ForEach-Object {
        $first = [long]$_[0]; $last = [long]$_[1]; $slack = $winMs - ($last - $first)
        $s0 = $first - [math]::Floor($slack / 2); , @($s0, ($s0 + $winMs)) })
    $scores = @($windows | ForEach-Object { $c = Test-GpuCoverage -Samples $samples -StartMs $_[0] -EndMs $_[1]; "$($c.n)/$($c.below_min)/$($c.saturated)" } | Select-Object -Unique)
    if ($scores.Count -ne 1) { throw "the $($windows.Count) windows that match the recorded coverage score differently ($($scores -join ' vs ')); not re-scoring" }
    $start = $windows[0][0]; $end = $windows[0][1]
    $how = "matched to the recorded coverage (sample count and dips; $($windows.Count) candidate windows, all scoring the same)"
}
$cov = Test-GpuCoverage -Samples $samples -StartMs $start -EndMs $end
$hb = $res.heartbeat_lateness_ms; $fr = $res.frame_interval_ms
$verdict = Get-S06Verdict -HasMeasuredReport ([bool]$hb) -ExitCode $res.app_exit_code -TimedOut ([bool]$res.app_timed_out) `
    -Heartbeat $hb -Frames $fr -RefreshHz ([double]$res.primary_refresh_hz) -Coverage $cov
$verdict = Complete-S06Verdict $verdict @($res.workload_problems) ([bool]$res.refresh_mismatch)

$res | Add-Member -Force -NotePropertyName verdict_rescored -NotePropertyValue $verdict
$res | Add-Member -Force -NotePropertyName rescore_rule -NotePropertyValue ([ordered]@{
    rule = "GPU utilisation >= $($cov.level) % in at least 90 % of the samples (was >= 95 %), dense as before"
    why = 'owner run 3: under the real ComfyUI loop the GPU reads 99 % with isolated 92-93 % samples, so 95 % measured the workload''s ceiling (a deviation from the plan''s S0.6 rule)'
    window = $how; rescored = (Get-Date).ToString('o') })
$res | Add-Member -Force -NotePropertyName gpu_coverage_rescored -NotePropertyValue $cov
$res | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 $Result
"original: $($res.verdict)"
"rescored: $verdict  (n $($cov.n), >= $($cov.level) % in $([math]::Round(100 * $cov.saturated_fraction, 1)) %, largest gap $($cov.max_gap_ms) ms; window $how)"
