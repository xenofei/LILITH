<#
.SYNOPSIS
  S0.6 (APP-135), run by the owner: starts ComfyUI on 127.0.0.1 with the spike's own
  GPU-saturating test workflow in a loop, runs the deployed spike under that load, measures, and
  stops ComfyUI.

.DESCRIPTION
  The owner runs this script (an agent never does). Steps:
    1. refuses to start if the deploy folder is older than the build, or if something already
       answers on 127.0.0.1:<Port> (it never touches a ComfyUI it did not start);
    2. starts ComfyUI (`--listen 127.0.0.1 --port <Port>`, hidden window) and waits for
       /system_stats;
    3. starts the GPU sampler (nvidia-smi, every 500 ms, its own clock) and comfy/keep_busy.py,
       which keeps 2 prompts of comfy/load-workflow.json outstanding (Z-Image Turbo int8,
       1024x1024, batch 2, 20 steps; PreviewImage, so nothing lands in ComfyUI's output folder)
       until this script stops it;
    4. waits (at most 300 s) until the last 20 s of samples meet the measured window's GPU
       condition; if they never do, the app is not run and the result is INVALID;
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
      fewer than 90 % of them are >= 95 % (brief dips between prompts are allowed and recorded),
      the refresh rate is unknown, or the list was hidden;
    - else PASS if heartbeat lateness p99 < 16 ms and frame interval p99 < the frame threshold:
      max(16 ms, 1.5 vsync periods of the primary display), on main-a's frames alone (75 Hz gives
      20 ms, 60 Hz 25 ms, 144 Hz and up 16 ms). Frames over 1.5 vsync periods are counted too.

  Run it on whichever display is primary: the result records its refresh rate, and the frame
  threshold follows from it. The spike window stays on top during the 65 s run (an occluded window's
  timers are throttled, spike 04); don't use the mouse or keyboard while it runs.

  Everything it starts is in a kill-on-close Windows job object held by this script's own pwsh,
  so closing its console window also ends ComfyUI, nvidia-smi, the load, the VRAM sampler and the
  app; Ctrl+C and errors still clean up explicitly, and closing the job's handle at the end ends
  whatever is left of them. Each of them is created suspended, put in the job, and only then
  resumed, so its own children are in the job from their start too. The script's own pwsh is not
  in the job.

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

# Everything this script starts (ComfyUI and its children, nvidia-smi, keep_busy, the VRAM
# sampler, the app) must die with it, even if its console window is closed: an orphaned ComfyUI
# holds the GPU and port 8188, and the next run then refuses. So this pwsh creates a kill-on-close
# job before it starts anything and holds its only handle; when this pwsh ends, however it ends,
# Windows closes the handle and everything in the job dies. `finally` closes it too, after the
# explicit stops. This pwsh itself is not in the job.
# Children don't join a job by themselves here: the Store pwsh is an MSIX-packaged app, and
# Windows creates its out-of-package children (python, nvidia-smi, the app) outside every job it
# is in (fix round 4 measured this: cmd escaped, an in-package pwsh didn't). So each process is
# started with Start-ProcessInJob: created suspended, assigned, then resumed. Its own children,
# which aren't packaged, then inherit the job from their first instruction. Nothing started
# before or elsewhere is ever put in the job.
$job = New-KillOnCloseJob

Add-Type -Namespace AppShellComfy -Name Win -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'
[void][AppShellComfy.Win]::SetErrorMode(0x0001 -bor 0x8000)

function Test-Comfy { try { (Invoke-WebRequest "$Url/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 } catch { $false } }

$gpuFile = Join-Path $Runs 'gpu.csv'
$vramFile = Join-Path $Runs 'vram.jsonl'

# ---- -SamplersOnly: check the sampling against another process ------------------------------
if ($SamplersOnly) {
    if (-not $ProcessId) { throw '-SamplersOnly needs -ProcessId' }
    $g = $null; $v = $null
    try {   # Ctrl+C or a throw still stops both samplers, also in an in-process run
        $g = Start-GpuSampler $gpuFile -Job $job
        $v = Start-VramSampler $ProcessId $vramFile -Job $job
        $t0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        Start-Sleep -Seconds ($SamplerSeconds + 2)
        $t1 = $t0 + $SamplerSeconds * 1000 + 1000
    } finally {
        if ($g -and -not $g.HasExited) { try { $g.Kill() } catch {} }
        if ($v -and -not $v.HasExited) { try { $v.Kill() } catch {} }
        $job.Dispose()
    }
    $gs = Read-GpuSamples $gpuFile
    $vs = Read-VramSamples $vramFile
    $win = [ordered]@{ start = $t0 + 1000; end = $t1 }
    $cov = Test-GpuCoverage -Samples $gs -StartMs $win.start -EndMs $win.end -MinUtil 0
    $vt = @($vs | ForEach-Object { [long]$_.t } | Where-Object { $_ -ge $win.start -and $_ -le $win.end } | Sort-Object)
    $vGap = 0; for ($i = 1; $i -lt $vt.Count; $i++) { $vGap = [math]::Max($vGap, $vt[$i] - $vt[$i - 1]) }
    $out = [ordered]@{ seconds = $SamplerSeconds; gpu = $cov; vram_samples = $vt.Count; vram_max_gap_ms = $vGap
                       vram_missing_retries = @(Read-JsonLines $vramFile | Where-Object { $_.missing }).Count
                       os_primary_refresh_hz = (Get-OsPrimaryRefreshHz); files = $Runs }
    $out | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 (Join-Path $Results 's06-samplers-check.json')
    $out | ConvertTo-Json -Depth 4
    return
}

# The deployed folder must be complete before anything starts (no launch blind).
$need = 'app_shell_spike.exe', 'Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll', 'Qt6Quick.dll', 'kddockwidgets-qt62.dll', 'spike_shell_shim.dll', 'vcruntime140.dll', 'platforms\qwindows.dll', 'SpikeWeb\spike_web.dll'
$missing = @($need | Where-Object { -not (Test-Path (Join-Path $Deploy $_)) })
if ($missing.Count) { throw "the deploy folder is incomplete (run run-spike.ps1 first): missing $($missing -join ', ')" }
# ...and built from the current build (the owner's first run measured a stale deploy).
$stale = @(Test-DeployCurrent $Deploy (Join-Path $Root 'build\cmake\bin'))
if ($stale.Count) { throw "the deploy folder is older than the build ($($stale -join ', ')); redeploy first: pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Only S0.4" }
# No other spike instance during the measurement: it would share the GPU, the CPU and the
# persistent WebEngine profile (for example a spike window left open from an earlier check).
$others = @(Get-SpikeAppProcesses)
if ($others.Count) { throw "another spike app is running (pid $(($others | ForEach-Object ProcessId) -join ', ')); close it first" }
if (Test-Comfy) { throw "something already answers on $Url; this script only measures with a ComfyUI it starts itself" }
if (-not (Test-Path $ComfyPython) -or -not (Test-Path $ComfyMain)) { throw "ComfyUI not found at $ComfyMain" }

$res = [ordered]@{ check = 'S0.6'; started = (Get-Date).ToString('o'); rate_hz = $RateHz; charts_batched = [bool]$ChartsBatched; runs = $Runs }
$comfy = $null; $load = $null; $app = $null; $gpuProc = $null; $vramProc = $null
try {
    Write-Host "starting ComfyUI on $Url ..."
    $comfy = Start-ProcessInJob $job $ComfyPython -ArgumentList @($ComfyMain, '--listen', '127.0.0.1', '--port', "$Port") `
        -WorkingDirectory (Split-Path $ComfyMain) `
        -StdOut (Join-Path $Runs 'comfy.out.log') -StdErr (Join-Path $Runs 'comfy.err.log')
    $deadline = (Get-Date).AddSeconds(300)
    while (-not (Test-Comfy) -and (Get-Date) -lt $deadline -and -not $comfy.HasExited) { Start-Sleep -Seconds 2 }
    if (-not (Test-Comfy)) { throw 'ComfyUI did not answer /system_stats within 300 s' }
    $res.comfy_version = (Invoke-RestMethod "$Url/system_stats" -TimeoutSec 5).system.comfyui_version

    $gpuProc = Start-GpuSampler $gpuFile -Job $job
    # The load keeps a prompt queued behind the running one (comfy/keep_busy.py, depth 2) and runs
    # until this script stops it, so it can't end before the measured minute does (the owner's
    # first run: one prompt at a time, ~75 % of samples >= 95 %, and the load ended 240 s after it
    # started, before the app's measured minute).
    $stopFile = Join-Path $Runs 'stop-load'
    $load = Start-ProcessInJob $job $ComfyPython -ArgumentList @('-I',
        (Join-Path $Root 'comfy\keep_busy.py'), '--port', "$Port", '--depth', '2', '--seconds', '1800', '--stop-file', $stopFile,
        '--parent-pid', "$PID",   # it stops by itself if this script's window is closed
        '--workflow', (Join-Path $Root 'comfy\load-workflow.json'), '--summary', (Join-Path $Runs 'keep-busy.json')) `
        -StdOut (Join-Path $Runs 'keep-busy.out.log') -StdErr (Join-Path $Runs 'keep-busy.err.log')

    # Gate: the measured minute starts only once the last 20 s of samples meet the measured
    # window's own GPU condition (dense, >= 95 % in at least 90 % of the samples). At most 300 s
    # (model load); if it never gets there, the app is not run at all and the result says why. It
    # stops waiting at once if the load or ComfyUI exits (keep_busy would otherwise poll a dead
    # ComfyUI until the deadline).
    $deadline = (Get-Date).AddSeconds(300); $saturated = $false
    while (-not $saturated -and (Get-Date) -lt $deadline -and -not $load.HasExited -and -not $comfy.HasExited) {
        Start-Sleep -Seconds 1
        $saturated = Test-GpuSaturatedRecently (Read-GpuSamples $gpuFile) ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
    }
    $res.gpu_saturated_before_start = $saturated
    # How close the load came, either way: the GPU condition over the last 20 s and 60 s, and how
    # many prompts keep_busy has finished.
    $nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $gs = Read-GpuSamples $gpuFile
    $res.gate = [ordered]@{
        last_20_s = (Test-GpuCoverage -Samples $gs -StartMs ($nowMs - 20000) -EndMs $nowMs) | Select-Object n, saturated_fraction, min_util, max_gap_ms, dense, saturated
        last_60_s = (Test-GpuCoverage -Samples $gs -StartMs ($nowMs - 60000) -EndMs $nowMs) | Select-Object n, saturated_fraction, min_util, max_gap_ms, dense, saturated
        prompts_finished = @(Get-Content (Join-Path $Runs 'keep-busy.out.log') -ErrorAction SilentlyContinue | Where-Object { $_ -match '^prompt ' }).Count }
    if (-not $saturated) {
        $res.load_exit_code = if ($load.HasExited) { $load.ExitCode } else { $null }
        throw "the GPU never reached the measurement condition before the measured minute (load exited: $($load.HasExited), ComfyUI exited: $($comfy.HasExited)); see keep-busy.*.log and comfy.err.log in $Runs"
    }
    $res.os_primary_refresh_hz = Get-OsPrimaryRefreshHz

    $res.cpu_percent_before_app = Get-CpuPercent
    $log = Join-Path $Runs 's06.jsonl'
    $appArgs = @('--log', $log, '--no-activate', '--on-top', '--load', '--load-rate', "$RateHz", '--show-web', '2000', '--measure', "$MeasureSeconds")
    if ($ChartsBatched) { $appArgs += '--charts-batched' }
    $app = Start-ProcessInJob $job $Exe -ArgumentList $appArgs -WorkingDirectory $Deploy
    $appStart = ([DateTimeOffset]$app.StartTime).ToUnixTimeMilliseconds()
    $vramProc = Start-VramSampler $app.Id $vramFile -Job $job
    $res.in_kill_on_close_job = [ordered]@{ comfy = (Test-ProcessInJob $job $comfy); load = (Test-ProcessInJob $job $load)
                                           gpu_sampler = (Test-ProcessInJob $job $gpuProc); vram_sampler = (Test-ProcessInJob $job $vramProc)
                                           app = (Test-ProcessInJob $job $app) }
    # These are the processes the script started itself: for ComfyUI and keep_busy that is the
    # venv's python.exe launcher, not the interpreter it starts (which inherits the job from the
    # launcher, but isn't checked here). Start-ProcessInJob ends a process it couldn't assign
    # before it runs, so a false here would mean the check itself is wrong; say so rather than
    # record it silently.
    $notIn = @($res.in_kill_on_close_job.Keys | Where-Object { -not $res.in_kill_on_close_job[$_] })
    if ($notIn.Count) { Write-Warning "not in the kill-on-close job: $($notIn -join ', '); closing this window would leave them running" }
    # Short waits in a loop, so Ctrl+C reaches `finally` (and stops ComfyUI) at once.
    $waitUntil = (Get-Date).AddSeconds($MeasureSeconds + 90)
    while (-not $app.WaitForExit(250) -and (Get-Date) -lt $waitUntil) { }
    $res.app_timed_out = -not $app.HasExited
    if ($res.app_timed_out) { try { $app.Kill($true) } catch {}; $app.WaitForExit() }
    $res.app_exit_code = if ($res.app_timed_out) { $null } else { $app.ExitCode }
    $res.cpu_percent_after_app = Get-CpuPercent

    Set-Content -Path $stopFile -Value 'stop'
    Stop-Process -Id $gpuProc.Id -Force -ErrorAction SilentlyContinue
    if (-not $vramProc.HasExited) { try { $vramProc.Kill() } catch {} }

    $ev = Read-JsonLines $log
    $measured = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'measured' } | Select-Object -Last 1
    $warm = $ev | Where-Object { $_.event -eq 'stall_report' -and $_.label -eq 'warmup' } | Select-Object -Last 1
    $counts = $ev | Where-Object { $_.event -eq 'load_counts' } | Select-Object -Last 1
    $webShown = $ev | Where-Object { $_.event -eq 'web_dock_shown' } | Select-Object -First 1
    $hb = if ($measured) { Get-NearestRankStats @($measured.heartbeat_lateness_ms.raw) } else { $null }
    $fr = if ($measured) { Get-NearestRankStats @($measured.frame_interval_ms.raw) } else { $null }
    $rr = Resolve-RefreshHz $measured $res.os_primary_refresh_hz
    $refresh = $rr.hz
    $endEpoch = if ($measured) { $appStart + [long]$measured.t_ms } else { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
    $cov = Test-GpuCoverage -Samples (Read-GpuSamples $gpuFile) -StartMs ($endEpoch - $MeasureSeconds * 1000) -EndMs $endEpoch
    $vram = @(Read-VramSamples $vramFile)
    $vWeb = @($vram | Where-Object { $webShown -and $_.t -ge ($appStart + [long]$webShown.t_ms + 3000) })
    $res.vram_samples_total = $vram.Count
    $res.vram_missing_retries = @(Read-JsonLines $vramFile | Where-Object { $_.missing }).Count

    $res.primary_refresh_hz = $refresh
    $res.refresh_source = $rr.source
    $res.windows = if ($measured) { $measured.windows } else { $null }   # refresh rate and on_primary per window, no names
    $res.frame_threshold_ms = Get-FrameThresholdMs $refresh
    $res.dropped_frames = if ($measured) { Get-DroppedFrames @($measured.frame_interval_ms.raw) $refresh } else { $null }
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
    $res.refresh_mismatch = $rr.mismatch
    $res.web = [ordered]@{ problems = @(Test-S06Web $ev)
                           loads = @($ev | Where-Object { $_.event -eq 'web_load' } | ForEach-Object { "$($_.status) $($_.url)" }) }
    # A web view that didn't load counts with the workload: the run is then INVALID, with the reason.
    $res.workload_problems = @(Test-S06Workload $counts $MeasureSeconds $RateHz) + @($res.web.problems)
    $verdict = Get-S06Verdict -HasMeasuredReport ([bool]$measured) -ExitCode $res.app_exit_code -TimedOut $res.app_timed_out `
        -Heartbeat $hb -Frames $fr -RefreshHz $refresh -Coverage $cov
    # The bar's workload must have run, with the list on screen throughout, and the refresh rate
    # must be trustworthy; otherwise a completed run is INVALID (a FAIL stays a FAIL).
    $verdict = Complete-S06Verdict $verdict $res.workload_problems $rr.mismatch
    if ($verdict -match '^FAIL' -and $res.stack_files.Count) { $verdict += "; stacks: $($res.stack_files -join ', ')" }
    $res.verdict = $verdict
    $res.log = $log
} catch {
    # A load that never saturated the GPU is a measurement condition not met (no app was run).
    $res.verdict = if ($res.Contains('gpu_saturated_before_start') -and -not $res.gpu_saturated_before_start) {
        "INVALID ($($_.Exception.Message))" } else { 'ERROR' }
    $res.error = "$($_.Exception.Message)"
} finally {
    if ($stopFile -and -not (Test-Path $stopFile)) { Set-Content -Path $stopFile -Value 'stop' -ErrorAction SilentlyContinue }
    if ($app -and -not $app.HasExited) { try { $app.Kill($true) } catch {} }
    if ($gpuProc -and -not $gpuProc.HasExited) { Stop-Process -Id $gpuProc.Id -Force -ErrorAction SilentlyContinue }
    if ($vramProc -and -not $vramProc.HasExited) { try { $vramProc.Kill() } catch {} }
    # .NET's tree kill takes a child only if it started after its parent, so a reused parent pid
    # can't pull in an unrelated process (taskkill /T doesn't check that).
    if ($load -and -not $load.HasExited) { try { $load.Kill($true) } catch {} }
    if ($comfy -and -not $comfy.HasExited) {
        try { $comfy.Kill($true) } catch {}; [void]$comfy.WaitForExit(10000)
        Write-Host 'stopped the ComfyUI process this script started'
    }
    # Then whatever is left in the job (exactly what this script put there) ends with its handle.
    $job.Dispose()
    $res.comfy_stopped = -not (Test-Comfy)
    $suffix = if ($RateHz -ne 10 -or $ChartsBatched) { '-fallback' } else { '' }
    $res | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $Results "s06$suffix.json")
    Write-Host "S0.6: $($res.verdict)"
}
