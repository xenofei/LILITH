<#
.SYNOPSIS
  S0.6 (APP-135), run by the owner: starts ComfyUI on 127.0.0.1 with the spike's own
  GPU-saturating test workflow in a loop, runs the deployed spike under that load, measures, and
  stops ComfyUI.

.DESCRIPTION
  The owner runs this script (an agent never does). Steps:
    1. refuses to start if something already answers on 127.0.0.1:<Port> (it never touches a
       ComfyUI it did not start);
    2. starts ComfyUI (`--listen 127.0.0.1 --port <Port>`, hidden window) and waits for
       /system_stats;
    3. starts the GPU sampler (nvidia-smi, every 500 ms, its own clock) and runs
       spikes/canvas/load/comfy_load.py with comfy/load-workflow.json (Z-Image Turbo int8,
       1024x1024, batch 2, 8 steps; PreviewImage, so nothing lands in ComfyUI's output folder);
    4. waits until GPU utilisation has been >= 95 % for 10 s in a row;
    5. records the machine's CPU load, then starts the deployed spike: --no-activate --on-top
       --load --load-rate <RateHz> [--charts-batched] --show-web 2000 --measure 60. Under --load
       the Web dock has its own place below Jobs and Grid, so the scrolled list stays visible;
       the spike's PDH "GPU Process Memory(pid_<pid>_*)" Local and Dedicated Usage are sampled
       once a second;
    6. stops the samplers, the load and the ComfyUI process tree it started, and writes
       results/s06[-fallback].json and the run's files under build/runs/<stamp>-s06/.

  Verdict (lib/SpikeMeasure.psm1, Get-S06Verdict):
    - FAIL if the app hung, crashed or logged no measured report (with any stack file named);
    - INVALID if the GPU samples don't cover the measured 60 s (>= 48 samples, no gap over 2 s),
      the GPU dipped under 95 %, the refresh rate is unknown, or the list was hidden;
    - else PASS if heartbeat lateness p99 < 16 ms and frame interval p99 < the frame threshold:
      16 ms, or 1.5 vsync periods on a screen whose period is >= 16 ms (60 Hz and below).

  Before running: the 240 Hz monitor must be on and primary (main-a opens at (40, 40) on the
  primary screen). The spike window stays on top during the 65 s run (an occluded window's
  timers are throttled, spike 04); don't use the mouse or keyboard while it runs.

  -SamplersOnly -ProcessId <pid> runs just the two samplers against another process for
  -SamplerSeconds and reports their coverage (no ComfyUI, no app); it checks the sampling.

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1
  pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1 -RateHz 5 -ChartsBatched   # S0.6's fallback
#>
param(
    [int]$Port = 8188,
    [int]$RateHz = 10,
    [switch]$ChartsBatched,
    [int]$MeasureSeconds = 60,
    [string]$ComfyPython = (Join-Path $env:USERPROFILE 'ComfyUI\.venv\Scripts\python.exe'),
    [string]$ComfyMain   = (Join-Path $env:USERPROFILE 'ComfyUI\ComfyUI\main.py'),
    [switch]$SamplersOnly,
    [int]$ProcessId = 0,
    [int]$SamplerSeconds = 60
)
$ErrorActionPreference = 'Stop'
$Root    = $PSScriptRoot
$Repo    = (Resolve-Path (Join-Path $Root '..\..')).Path
$Results = Join-Path $Root 'results'
$Deploy  = Join-Path $Root 'build\deploy'
$Exe     = Join-Path $Deploy 'app_shell_spike.exe'
$Url     = "http://127.0.0.1:$Port"
$stamp   = Get-Date -Format 'yyyyMMdd-HHmmss'
$Runs    = Join-Path $Root "build\runs\$stamp-s06"
New-Item -ItemType Directory -Force $Runs, $Results | Out-Null
Import-Module (Join-Path $Root 'lib\SpikeMeasure.psm1') -Force

Add-Type -Namespace AppShellComfy -Name Win -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'
[void][AppShellComfy.Win]::SetErrorMode(0x0001 -bor 0x8000)

function Test-Comfy { try { (Invoke-WebRequest "$Url/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 } catch { $false } }

$gpuFile = Join-Path $Runs 'gpu.csv'
$vramFile = Join-Path $Runs 'vram.jsonl'

# ---- -SamplersOnly: check the sampling against another process ------------------------------
if ($SamplersOnly) {
    if (-not $ProcessId) { throw '-SamplersOnly needs -ProcessId' }
    $g = Start-GpuSampler $gpuFile
    $v = Start-VramSampler $ProcessId $vramFile
    $t0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    Start-Sleep -Seconds ($SamplerSeconds + 2)
    $t1 = $t0 + $SamplerSeconds * 1000 + 1000
    Stop-Process -Id $g.Id -Force -ErrorAction SilentlyContinue
    Stop-Job $v; Remove-Job $v -Force
    $gs = Read-GpuSamples $gpuFile
    $vs = Read-JsonLines $vramFile
    $win = [ordered]@{ start = $t0 + 1000; end = $t1 }
    $cov = Test-GpuCoverage -Samples $gs -StartMs $win.start -EndMs $win.end -MinUtil 0
    $vt = @($vs | ForEach-Object { [long]$_.t } | Where-Object { $_ -ge $win.start -and $_ -le $win.end } | Sort-Object)
    $vGap = 0; for ($i = 1; $i -lt $vt.Count; $i++) { $vGap = [math]::Max($vGap, $vt[$i] - $vt[$i - 1]) }
    $out = [ordered]@{ seconds = $SamplerSeconds; gpu = $cov; vram_samples = $vt.Count; vram_max_gap_ms = $vGap; files = $Runs }
    $out | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $Results 's06-samplers-check.json')
    $out | ConvertTo-Json -Depth 4
    return
}

# The deployed folder must be complete before anything starts (no launch blind).
$need = 'app_shell_spike.exe', 'Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll', 'Qt6Quick.dll', 'kddockwidgets-qt62.dll', 'spike_shell_shim.dll', 'vcruntime140.dll', 'platforms\qwindows.dll', 'SpikeWeb\spike_web.dll'
$missing = @($need | Where-Object { -not (Test-Path (Join-Path $Deploy $_)) })
if ($missing.Count) { throw "the deploy folder is incomplete (run run-spike.ps1 first): missing $($missing -join ', ')" }
if (Test-Comfy) { throw "something already answers on $Url; this script only measures with a ComfyUI it starts itself" }
if (-not (Test-Path $ComfyPython) -or -not (Test-Path $ComfyMain)) { throw "ComfyUI not found at $ComfyMain" }

$res = [ordered]@{ check = 'S0.6'; started = (Get-Date).ToString('o'); rate_hz = $RateHz; charts_batched = [bool]$ChartsBatched; runs = $Runs }
$comfy = $null; $load = $null; $app = $null; $gpuProc = $null; $vramJob = $null
try {
    Write-Host "starting ComfyUI on $Url ..."
    $comfy = Start-Process -FilePath $ComfyPython -ArgumentList @($ComfyMain, '--listen', '127.0.0.1', '--port', "$Port") `
        -WorkingDirectory (Split-Path $ComfyMain) -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Runs 'comfy.out.log') -RedirectStandardError (Join-Path $Runs 'comfy.err.log')
    $deadline = (Get-Date).AddSeconds(300)
    while (-not (Test-Comfy) -and (Get-Date) -lt $deadline -and -not $comfy.HasExited) { Start-Sleep -Seconds 2 }
    if (-not (Test-Comfy)) { throw 'ComfyUI did not answer /system_stats within 300 s' }
    $res.comfy_version = (Invoke-RestMethod "$Url/system_stats" -TimeoutSec 5).system.comfyui_version

    $gpuProc = Start-GpuSampler $gpuFile
    $load = Start-Process -FilePath $ComfyPython -WindowStyle Hidden -PassThru -ArgumentList @('-I',
        (Join-Path $Repo 'spikes\canvas\load\comfy_load.py'), '--port', "$Port", '--seconds', "$($MeasureSeconds + 180)",
        '--workflow', (Join-Path $Root 'comfy\load-workflow.json'), '--summary', (Join-Path $Runs 'comfy-load.json')) `
        -RedirectStandardOutput (Join-Path $Runs 'comfy-load.out.log') -RedirectStandardError (Join-Path $Runs 'comfy-load.err.log')

    # Wait for the GPU to be saturated (>= 95 % for 10 s in a row), at most 240 s (model load).
    $deadline = (Get-Date).AddSeconds(240); $saturated = $false
    while (-not $saturated -and (Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 1
        $last = @(Read-GpuSamples $gpuFile | Select-Object -Last 20)
        $saturated = ($last.Count -ge 20) -and (@($last | Where-Object { $_.util -lt 95 }).Count -eq 0)
    }
    $res.gpu_saturated_before_start = $saturated

    $res.cpu_percent_before_app = Get-CpuPercent
    $log = Join-Path $Runs 's06.jsonl'
    $appArgs = @('--log', $log, '--no-activate', '--on-top', '--load', '--load-rate', "$RateHz", '--show-web', '2000', '--measure', "$MeasureSeconds")
    if ($ChartsBatched) { $appArgs += '--charts-batched' }
    $psi = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $Deploy
    $app = [System.Diagnostics.Process]::Start($psi)
    $appStart = ([DateTimeOffset]$app.StartTime).ToUnixTimeMilliseconds()
    $vramJob = Start-VramSampler $app.Id $vramFile
    $res.app_timed_out = -not $app.WaitForExit(($MeasureSeconds + 90) * 1000)
    if ($res.app_timed_out) { try { $app.Kill($true) } catch {}; $app.WaitForExit() }
    $res.app_exit_code = if ($res.app_timed_out) { $null } else { $app.ExitCode }
    $res.cpu_percent_after_app = Get-CpuPercent

    Stop-Process -Id $gpuProc.Id -Force -ErrorAction SilentlyContinue
    Stop-Job $vramJob -ErrorAction SilentlyContinue

    $ev = Read-JsonLines $log
    $measured = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'measured' } | Select-Object -Last 1
    $warm = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'warmup' } | Select-Object -Last 1
    $counts = $ev | Where-Object { $_.event -eq 'load_counts' } | Select-Object -Last 1
    $webShown = $ev | Where-Object { $_.event -eq 'web_dock_shown' } | Select-Object -First 1
    $hb = if ($measured) { Get-NearestRankStats @($measured.heartbeat_lateness_ms.raw) } else { $null }
    $fr = if ($measured) { Get-NearestRankStats @($measured.frame_interval_ms.raw) } else { $null }
    $screenA = if ($measured) { @($measured.screens) | Where-Object { $_.window -eq 'main-a' } | Select-Object -First 1 } else { $null }
    $refresh = if ($screenA) { [double]$screenA.refresh_hz } else { 0 }
    $endEpoch = if ($measured) { $appStart + [long]$measured.t_ms } else { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
    $cov = Test-GpuCoverage -Samples (Read-GpuSamples $gpuFile) -StartMs ($endEpoch - $MeasureSeconds * 1000) -EndMs $endEpoch
    $vram = @(Read-JsonLines $vramFile)
    $vWeb = @($vram | Where-Object { $webShown -and $_.t -ge ($appStart + [long]$webShown.t_ms + 3000) })

    $res.screen = $screenA
    $res.frame_threshold_ms = Get-FrameThresholdMs $refresh
    $res.heartbeat_lateness_ms = $hb
    $res.frame_interval_ms = $fr
    $res.warmup = [ordered]@{ heartbeat = $warm.heartbeat_lateness_ms | Select-Object n, p50, p99, max; frames = $warm.frame_interval_ms | Select-Object n, p50, p99, max }
    $res.gpu_coverage = $cov
    $res.spike_vram_with_web_dock_mib = [ordered]@{
        local_usage_max = ($vWeb | Measure-Object local_usage_mib -Maximum).Maximum
        local_usage_p50 = (Get-NearestRankStats @($vWeb | ForEach-Object { $_.local_usage_mib })).p50
        dedicated_usage_max = ($vWeb | Measure-Object dedicated_usage_mib -Maximum).Maximum; samples = $vWeb.Count }
    $res.load_counts = $counts
    $res.stack_files = @(Get-ChildItem $Runs -Filter 's06.jsonl.*stacks.txt' -ErrorAction SilentlyContinue | ForEach-Object Name)
    $verdict = Get-S06Verdict -HasMeasuredReport ([bool]$measured) -ExitCode $res.app_exit_code -TimedOut $res.app_timed_out `
        -Heartbeat $hb -Frames $fr -RefreshHz $refresh -Coverage $cov
    # The list must have been on screen for the whole measured window.
    if ($verdict -notmatch '^FAIL' -and $counts -and $counts.grid_hidden_seconds -gt 0) {
        $verdict = "INVALID (the scrolled list was hidden for $($counts.grid_hidden_seconds) s)"
    }
    if ($verdict -match '^FAIL' -and $res.stack_files.Count) { $verdict += "; stacks: $($res.stack_files -join ', ')" }
    $res.verdict = $verdict
    $res.log = $log
} catch {
    $res.verdict = 'ERROR'
    $res.error = "$($_.Exception.Message)"
} finally {
    if ($app -and -not $app.HasExited) { try { $app.Kill($true) } catch {} }
    if ($gpuProc -and -not $gpuProc.HasExited) { Stop-Process -Id $gpuProc.Id -Force -ErrorAction SilentlyContinue }
    if ($vramJob) { Stop-Job $vramJob -ErrorAction SilentlyContinue; Remove-Job $vramJob -Force -ErrorAction SilentlyContinue }
    if ($load -and -not $load.HasExited) { & taskkill /PID $load.Id /T /F *>&1 | Out-Null }
    if ($comfy -and -not $comfy.HasExited) { & taskkill /PID $comfy.Id /T /F *>&1 | Out-Null; Write-Host 'stopped the ComfyUI process this script started' }
    $res.comfy_stopped = -not (Test-Comfy)
    $suffix = if ($RateHz -ne 10 -or $ChartsBatched) { '-fallback' } else { '' }
    $res | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $Results "s06$suffix.json")
    Write-Host "S0.6: $($res.verdict)"
}
