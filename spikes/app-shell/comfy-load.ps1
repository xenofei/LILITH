<#
.SYNOPSIS
  S0.6 (APP-135), owner present: starts ComfyUI on 127.0.0.1 with the spike's own GPU-saturating
  test workflow in a loop, runs the deployed spike under that load, measures, and stops ComfyUI.

.DESCRIPTION
  Run only after the owner says go. Steps:
    1. refuses to start if something already answers on 127.0.0.1:<Port> (it never touches a
       ComfyUI it did not start);
    2. starts ComfyUI (`--listen 127.0.0.1 --port <Port>`, hidden window) and waits for
       /system_stats;
    3. runs spikes/canvas/load/comfy_load.py with comfy/load-workflow.json (Z-Image Turbo int8,
       1024x1024, batch 2, 8 steps; PreviewImage, so nothing lands in ComfyUI's output folder)
       for the whole measurement;
    4. samples NVML GPU utilisation (nvidia-smi) every second, and waits until it has been
       >= 95 % for 10 s in a row before starting the app;
    5. starts the deployed spike: --no-activate --on-top --load --load-rate <RateHz>
       [--charts-batched] --show-web 2000 --measure 60, and samples the spike's PDH
       "GPU Process Memory(pid_<pid>_*)\Local Usage" and "Dedicated Usage" once a second;
    6. stops the load and the ComfyUI process tree it started, and writes results/s06.json:
       heartbeat lateness and frame interval (n, p50, p95, p99, max), GPU utilisation over the
       measured window, the spike's VRAM with the web dock shown, and the verdict.

  The spike window stays on top during the 65 s run (an occluded window's timers are throttled,
  spike 04); please don't use the mouse or keyboard while it runs.

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
    [string]$ComfyMain   = (Join-Path $env:USERPROFILE 'ComfyUI\ComfyUI\main.py')
)
$ErrorActionPreference = 'Stop'
$Root    = $PSScriptRoot
$Repo    = (Resolve-Path (Join-Path $Root '..\..')).Path
$Runs    = Join-Path $Root 'build\runs'
$Results = Join-Path $Root 'results'
$Deploy  = Join-Path $Root 'build\deploy'
$Exe     = Join-Path $Deploy 'app_shell_spike.exe'
$Url     = "http://127.0.0.1:$Port"
New-Item -ItemType Directory -Force $Runs, $Results | Out-Null

Add-Type -Namespace AppShellComfy -Name Win -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'
[void][AppShellComfy.Win]::SetErrorMode(0x0001 -bor 0x8000)

function Test-Comfy { try { (Invoke-WebRequest "$Url/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 } catch { $false } }
function Get-GpuUtil {
    $q = & nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits 2>$null | Select-Object -First 1
    if ($q -match '^\s*(\d+)\s*,\s*(\d+)') { [ordered]@{ t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds(); util = [int]$Matches[1]; mem_mib = [int]$Matches[2] } }
}
function Get-Stats([double[]]$v) {
    if (-not $v -or $v.Count -eq 0) { return $null }
    $s = @($v | Sort-Object)
    $pick = { param($p) $s[[math]::Max(0, [math]::Ceiling($p / 100 * $s.Count) - 1)] }
    [ordered]@{ n = $s.Count; p50 = (& $pick 50); p95 = (& $pick 95); p99 = (& $pick 99); max = $s[-1] }
}

# The deployed folder must be complete before anything starts (no launch blind).
$need = 'app_shell_spike.exe', 'Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll', 'Qt6Quick.dll', 'kddockwidgets-qt62.dll', 'spike_shell_shim.dll', 'vcruntime140.dll', 'platforms\qwindows.dll', 'SpikeWeb\spike_web.dll'
$missing = @($need | Where-Object { -not (Test-Path (Join-Path $Deploy $_)) })
if ($missing.Count) { throw "the deploy folder is incomplete (run run-spike.ps1 first): missing $($missing -join ', ')" }
if (Test-Comfy) { throw "something already answers on $Url; this script only measures with a ComfyUI it starts itself" }
if (-not (Test-Path $ComfyPython) -or -not (Test-Path $ComfyMain)) { throw "ComfyUI not found at $ComfyMain" }

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$res = [ordered]@{ check = 'S0.6'; started = (Get-Date).ToString('o'); rate_hz = $RateHz; charts_batched = [bool]$ChartsBatched }
$comfy = $null; $load = $null; $app = $null
$gpu = [System.Collections.Generic.List[object]]::new()
$vram = [System.Collections.Generic.List[object]]::new()
try {
    Write-Host "starting ComfyUI on $Url ..."
    $comfy = Start-Process -FilePath $ComfyPython -ArgumentList @($ComfyMain, '--listen', '127.0.0.1', '--port', "$Port") `
        -WorkingDirectory (Split-Path $ComfyMain) -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Runs "comfy-$stamp.out.log") -RedirectStandardError (Join-Path $Runs "comfy-$stamp.err.log")
    $deadline = (Get-Date).AddSeconds(300)
    while (-not (Test-Comfy) -and (Get-Date) -lt $deadline -and -not $comfy.HasExited) { Start-Sleep -Seconds 2 }
    if (-not (Test-Comfy)) { throw 'ComfyUI did not answer /system_stats within 300 s' }
    $res.comfy_version = (Invoke-RestMethod "$Url/system_stats" -TimeoutSec 5).system.comfyui_version

    $loadSeconds = $MeasureSeconds + 180
    $load = Start-Process -FilePath $ComfyPython -WindowStyle Hidden -PassThru -ArgumentList @('-I',
        (Join-Path $Repo 'spikes\canvas\load\comfy_load.py'), '--port', "$Port", '--seconds', "$loadSeconds",
        '--workflow', (Join-Path $Root 'comfy\load-workflow.json'), '--summary', (Join-Path $Runs "comfy-load-$stamp.json")) `
        -RedirectStandardOutput (Join-Path $Runs "comfy-load-$stamp.out.log") -RedirectStandardError (Join-Path $Runs "comfy-load-$stamp.err.log")

    # Wait for the GPU to be saturated (>= 95 % for 10 s in a row), at most 240 s (model load).
    $run = 0; $deadline = (Get-Date).AddSeconds(240)
    while ($run -lt 10 -and (Get-Date) -lt $deadline) {
        $g = Get-GpuUtil; if ($g) { $gpu.Add($g); if ($g.util -ge 95) { $run++ } else { $run = 0 } }
        Start-Sleep -Seconds 1
    }
    $res.gpu_saturated_before_start = $run -ge 10

    $log = Join-Path $Runs "s06-$stamp.jsonl"
    $appArgs = @('--log', $log, '--no-activate', '--on-top', '--load', '--load-rate', "$RateHz", '--show-web', '2000', '--measure', "$MeasureSeconds")
    if ($ChartsBatched) { $appArgs += '--charts-batched' }
    $psi = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $Deploy
    $appStart = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $app = [System.Diagnostics.Process]::Start($psi)
    $counters = foreach ($n in 'Local Usage', 'Dedicated Usage') { "\GPU Process Memory(pid_$($app.Id)_*)\$n" }
    $deadline = (Get-Date).AddSeconds($MeasureSeconds + 90)
    while (-not $app.HasExited -and (Get-Date) -lt $deadline) {
        $g = Get-GpuUtil; if ($g) { $gpu.Add($g) }
        try {
            $c = Get-Counter -Counter $counters -ErrorAction Stop
            $row = [ordered]@{ t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
            foreach ($key in 'local usage', 'dedicated usage') {
                $row[($key -replace ' ', '_') + '_mib'] = [math]::Round((($c.CounterSamples | Where-Object { $_.Path -like "*\$key" } | Measure-Object CookedValue -Sum).Sum) / 1MB, 1)
            }
            $vram.Add($row)
        } catch {}
        Start-Sleep -Milliseconds 700
    }
    if (-not $app.HasExited) { $app.Kill($true); $res.app_timed_out = $true }
    $app.WaitForExit()
    $res.app_exit_code = $app.ExitCode

    $ev = @(Get-Content $log | ForEach-Object { try { $_ | ConvertFrom-Json } catch {} } | Where-Object { $_ })
    $measured = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'measured' } | Select-Object -Last 1
    $warm = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'warmup' } | Select-Object -Last 1
    $counts = $ev | Where-Object { $_.event -eq 'load_counts' } | Select-Object -Last 1
    $webShown = $ev | Where-Object { $_.event -eq 'web_dock_shown' } | Select-Object -First 1
    $hb = Get-Stats @($measured.heartbeat_lateness_ms.raw)
    $fr = Get-Stats @($measured.frame_interval_ms.raw)
    # The measured window: from the measured report's time back MeasureSeconds.
    $endEpoch = $appStart + [int64]$measured.t_ms
    $startEpoch = $endEpoch - $MeasureSeconds * 1000
    $gpuIn = @($gpu | Where-Object { $_.t -ge $startEpoch -and $_.t -le $endEpoch } | ForEach-Object { $_.util })
    $res.heartbeat_lateness_ms = $hb
    $res.frame_interval_ms = $fr
    $res.warmup = [ordered]@{ heartbeat = $warm.heartbeat_lateness_ms | Select-Object n, p50, p99, max; frames = $warm.frame_interval_ms | Select-Object n, p50, p99, max }
    $res.gpu_util_measured = [ordered]@{ n = $gpuIn.Count; min = ($gpuIn | Measure-Object -Minimum).Minimum; p50 = (Get-Stats $gpuIn).p50; below_95 = @($gpuIn | Where-Object { $_ -lt 95 }).Count }
    $vWeb = @($vram | Where-Object { $webShown -and $_.t -ge ($appStart + [int64]$webShown.t_ms + 3000) })
    $res.spike_vram_with_web_dock_mib = [ordered]@{ local_usage_max = ($vWeb | Measure-Object local_usage_mib -Maximum).Maximum
                                                     local_usage_p50 = (Get-Stats @($vWeb | ForEach-Object { $_.local_usage_mib })).p50
                                                     dedicated_usage_max = ($vWeb | Measure-Object dedicated_usage_mib -Maximum).Maximum; samples = $vWeb.Count }
    $res.load_counts = $counts
    $gpuOk = ($gpuIn.Count -ge [math]::Floor($MeasureSeconds * 0.8)) -and (@($gpuIn | Where-Object { $_ -lt 95 }).Count -eq 0)
    $res.gpu_condition_met = $gpuOk
    $pass = $hb -and $fr -and ($hb.p99 -lt 16) -and ($fr.p99 -lt 16) -and ($res.app_exit_code -eq 0)
    $res.verdict = if (-not $gpuOk) { 'INVALID (GPU not >= 95 % throughout; rerun)' } elseif ($pass) { 'PASS' } else { 'FAIL' }
    $res.log = $log
} catch {
    $res.verdict = 'ERROR'
    $res.error = "$($_.Exception.Message)"
} finally {
    if ($app -and -not $app.HasExited) { try { $app.Kill($true) } catch {} }
    if ($load -and -not $load.HasExited) { & taskkill /PID $load.Id /T /F *>&1 | Out-Null }
    if ($comfy -and -not $comfy.HasExited) { & taskkill /PID $comfy.Id /T /F *>&1 | Out-Null; Write-Host 'stopped the ComfyUI process this script started' }
    $res.comfy_stopped = -not (Test-Comfy)
    $suffix = if ($RateHz -ne 10 -or $ChartsBatched) { '-fallback' } else { '' }
    $res | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $Results "s06$suffix.json")
    Write-Host "S0.6: $($res.verdict)"
}
