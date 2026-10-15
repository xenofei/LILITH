<#
.SYNOPSIS
  Task 4 (IMG-153, APP-135, IMG-040): canvas frame times and GUI-thread stalls, idle and while
  ComfyUI saturates the GPU. Writes results/frametimes-<backend>-<idle|load>.json and merges the
  criteria into results/frametimes-summary.json.

.DESCRIPTION
  Scene: canvas.exe --layers 20 --doc 8192x8192 --auto-pan (1280x800 window, zoom 1), no debug layer,
  no GPU timestamps. Each run lasts WarmupSec + Seconds; the first WarmupSec are reported separately
  (start-up) and the criteria use the following Seconds.
    idle  ComfyUI not generating.
    load  comfy_load.py --seconds <Seconds+1> --trim-flag %TEMP%\ww-spike-trim --unload-first starts
          3 s after the canvas: /free (unload models), then one Z-Image Turbo prompt at a time, fresh
          seed each, the trim flag touched before each submit.
  During both runs load\sample-gpu.ps1 samples the canvas process's PDH "GPU Process Memory" counters
  and the GPU utilization once a second.

  Criteria (docs/spikes/04-frametimes.md):
    1  idle: frame-time p99 <= 16.7 ms
    2  load: GUI stall p99 < 16 ms (stall = 4 ms heartbeat more than 4 ms late); frame-time p99 is
       reported against the 33 ms target (not a hard fail)
    3  load: after each trim (first PDH sample >= 1 s after the flag was touched) the canvas process's
       Dedicated Usage is < 1,000,000,000 bytes
    4  negative check (run with -NegativeCheck while ComfyUI is stopped): comfy_load.py prints
       "ComfyUI unreachable at 127.0.0.1:8188" and exits 2 within 5 s

  The window is kept above other windows (--on-top): Windows 11 gives a fully occluded or minimized
  window's process the default 15.6 ms timer resolution, and the 4 ms heartbeat then reads ~11.6 ms
  late on every beat even with an idle GUI thread.

  -GuiPan (diagnostic) adds --gui-pan: the pan is animated on the GUI thread, so every frame needs a
  scene-graph sync, as a pan driven by input events would. Results go to results/diag/ and to the
  summary key "<backend>-guipan"; the criteria are evaluated the same way but are informational.
  -Tag <name> marks a repeat run the same way (results/diag/, summary key "<backend>-<name>").

  -Diagnostics runs the stall-analysis controls instead (results/diag/): the window without the canvas
  (--no-canvas), and the canvas without --on-top.

.EXAMPLE
  pwsh -File spikes/canvas/run-frametimes.ps1 -Backend d3d11
  pwsh -File spikes/canvas/run-frametimes.ps1 -NegativeCheck
#>
param(
    [ValidateSet('d3d11', 'd3d12')] [string]$Backend = 'd3d11',
    [ValidateSet('both', 'idle', 'load')] [string]$Mode = 'both',
    [int]$Seconds = 120,
    [int]$WarmupSec = 5,
    [switch]$NoBuild,
    [switch]$NegativeCheck,
    [switch]$Diagnostics,
    [switch]$GuiPan,
    [string]$Tag = '',
    [int]$Attempts = 3,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'

$Root     = $PSScriptRoot
$Repo     = (Resolve-Path (Join-Path $Root '..\..')).Path
$Build    = Join-Path $Root 'build'
$BinDir   = Join-Path $Build 'cmake\bin'
$Runs     = Join-Path $Build 'runs'
$Results  = Join-Path $Root 'results'
$Exe      = Join-Path $BinDir 'canvas.exe'
$Load     = Join-Path $Root 'load\comfy_load.py'
$Sampler  = Join-Path $Root 'load\sample-gpu.ps1'
$Comfy    = 'http://127.0.0.1:8188'
$TrimFlag = Join-Path $env:TEMP 'ww-spike-trim'
$Summary  = Join-Path $Results 'frametimes-summary.json'

. (Join-Path $Repo 'spikes\toolchain\env.ps1')
New-Item -ItemType Directory -Force $Build, $Runs, $Results | Out-Null

function Test-Comfy {
    try { $null = Invoke-RestMethod -Uri "$Comfy/system_stats" -TimeoutSec 3; $true } catch { $false }
}

function Merge-Summary([string]$key, $value) {
    $doc = [ordered]@{}
    if (Test-Path $Summary) {
        try { $old = Get-Content $Summary -Raw | ConvertFrom-Json -AsHashtable; foreach ($k in $old.Keys) { $doc[$k] = $old[$k] } } catch {}
    }
    $doc['updated'] = (Get-Date).ToString('o')
    $doc[$key] = $value
    $doc | ConvertTo-Json -Depth 12 | Set-Content -Encoding utf8 $Summary
}

# ------------------------------------------------------------------------------------------
# Criterion 4: the negative check (ComfyUI must be stopped).
# ------------------------------------------------------------------------------------------
if ($NegativeCheck) {
    if (Test-Comfy) { Write-Host 'ComfyUI is reachable at 127.0.0.1:8188; stop it before the negative check.'; exit 1 }
    $psi = [System.Diagnostics.ProcessStartInfo]::new($Python)
    foreach ($a in @($Load, '--seconds', '10')) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    $stdout = $p.StandardOutput.ReadToEnd(); $stderr = $p.StandardError.ReadToEnd()
    $p.WaitForExit(); $sw.Stop()
    $expected = 'ComfyUI unreachable at 127.0.0.1:8188'
    $pass = ($stdout.Trim() -eq $expected) -and ($p.ExitCode -eq 2) -and ($sw.ElapsedMilliseconds -lt 5000)
    $r = [ordered]@{ criterion = 4; pass = $pass; generated = (Get-Date).ToString('o')
                     command = "$Python $Load --seconds 10"; stdout = $stdout.Trim(); stderr = $stderr.Trim()
                     exit_code = $p.ExitCode; wall_ms = $sw.ElapsedMilliseconds; expected_stdout = $expected }
    $r | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Results 'negative-check.json')
    Write-Host ("criterion 4 : {0} (stdout '{1}', exit {2}, {3} ms)" -f $(if ($pass) { 'PASS' } else { 'FAIL' }), $stdout.Trim(), $p.ExitCode, $sw.ElapsedMilliseconds)
    exit $(if ($pass) { 0 } else { 1 })
}

# ------------------------------------------------------------------------------------------
# Build.
# ------------------------------------------------------------------------------------------
if (-not $NoBuild) {
    & cmake -S $Root -B (Join-Path $Build 'cmake') -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo *>&1 | Out-File (Join-Path $Build 'configure.log')
    if ($LASTEXITCODE) { throw 'cmake configure failed (build/configure.log)' }
    & cmake --build (Join-Path $Build 'cmake') *>&1 | Out-File (Join-Path $Build 'build.log')
    if ($LASTEXITCODE) { throw 'build failed (build/build.log)' }
}
if (-not (Test-Path $Exe)) { throw "canvas.exe missing at $Exe" }

function Start-Canvas([string[]]$extra, [hashtable]$envSet = @{}) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($a in $extra) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $BinDir
    $psi.Environment['QSG_RHI_BACKEND'] = $Backend
    $psi.Environment.Remove('QSG_RHI_DEBUG_LAYER') | Out-Null
    foreach ($k in $envSet.Keys) { $psi.Environment[$k] = $envSet[$k] }
    [System.Diagnostics.Process]::Start($psi)
}
function Start-Sampler([int]$id, [string]$out) {
    Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList @('-NoProfile', '-File', $Sampler, '-ProcessId', $id, '-Out', $out) -WindowStyle Hidden -PassThru
}
function Read-Jsonl([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content $path | Where-Object { $_.Trim() } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } | Where-Object { $_ })
}
function Strip($s) { if ($null -eq $s) { return $null }; [ordered]@{ n = $s.n; p50 = $s.p50; p95 = $s.p95; p99 = $s.p99; max = $s.max } }

$durationMs = ($WarmupSec + $Seconds) * 1000
$canvasArgs = @('--layers', '20', '--doc', '8192x8192', '--backend', $Backend, '--auto-pan', '--on-top',
                '--measure-after', "$($WarmupSec * 1000)", '--quit-after', "$durationMs", '--trim-flag', $TrimFlag)
if ($GuiPan) { $canvasArgs += '--gui-pan' }
$suffix = if ($GuiPan) { '-guipan' } else { '' }
if ($Tag) { $suffix += "-$Tag" }   # -Tag: a repeat or diagnostic run that must not overwrite the main results
$ftDir = if ($suffix) { Join-Path $Results 'diag' } else { $Results }
New-Item -ItemType Directory -Force $ftDir | Out-Null

# ------------------------------------------------------------------------------------------
# Runs.
# ------------------------------------------------------------------------------------------
$crit = [ordered]@{}
function Set-Check([string]$n, [bool]$pass, $detail) {
    $o = [ordered]@{ criterion = $n; pass = $pass }
    foreach ($k in $detail.Keys) { $o[$k] = $detail[$k] }
    $script:crit[$n] = $o
    Write-Host ("criterion {0} [{1}] : {2}" -f $n, $Backend, $(if ($pass) { 'PASS' } else { 'FAIL' }))
}

function Invoke-Run([string]$kind) {
    $ft = Join-Path $ftDir "frametimes-$Backend-$kind$suffix.json"
    $log = Join-Path $Runs "ft-$Backend-$kind$suffix.jsonl"
    $pdh = Join-Path $Runs "pdh-$Backend-$kind$suffix.jsonl"
    $comfySummary = Join-Path $Runs "comfy-load-$Backend$suffix.json"
    Remove-Item $ft, $log, $pdh -ErrorAction SilentlyContinue
    if ($kind -eq 'load') {
        if (-not (Test-Comfy)) { throw 'ComfyUI is not reachable at 127.0.0.1:8188; start it before the load run.' }
        $q = Invoke-RestMethod -Uri "$Comfy/queue" -TimeoutSec 5
        if (@($q.queue_running).Count -or @($q.queue_pending).Count) { throw 'ComfyUI has jobs queued or running; this run only measures its own prompts.' }
        Remove-Item $comfySummary -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path $TrimFlag)) { Set-Content -Path $TrimFlag -Value '' }
    Write-Host "[$Backend] $kind run: $($WarmupSec + $Seconds) s ..."
    $canvas = Start-Canvas ($canvasArgs + @('--frametimes', $ft, '--log', $log))
    $samplerProc = Start-Sampler $canvas.Id $pdh
    $loadProc = $null
    if ($kind -eq 'load') {
        Start-Sleep -Seconds 3
        $loadProc = Start-Process -FilePath $Python -ArgumentList @("`"$Load`"", '--seconds', "$($Seconds + 1)", '--trim-flag', "`"$TrimFlag`"",
                                                                     '--unload-first', '--summary', "`"$comfySummary`"") -NoNewWindow -PassThru
    }
    $canvas.WaitForExit()
    $canvasExit = $canvas.ExitCode
    if ($loadProc) { $loadProc.WaitForExit() }
    $null = $samplerProc.WaitForExit(15000)
    $j = if (Test-Path $ft) { Get-Content $ft -Raw | ConvertFrom-Json } else { $null }
    $pd = Read-Jsonl $pdh
    $cs = if ($kind -eq 'load' -and (Test-Path $comfySummary)) { Get-Content $comfySummary -Raw | ConvertFrom-Json } else { $null }
    [ordered]@{ kind = $kind; frametimes = $ft; log = $log; canvas_exit = $canvasExit; j = $j; pdh = $pd; comfy = $cs
                load_exit = $(if ($loadProc) { $loadProc.ExitCode } else { $null }) }
}

function Get-RunSummary($r) {
    $j = $r.j
    $ded = @($r.pdh | Where-Object { $null -ne $_.dedicated_usage } | ForEach-Object { [double]$_.dedicated_usage })
    $util = @($r.pdh | Where-Object { $null -ne $_.gpu_util_percent } | ForEach-Object { [double]$_.gpu_util_percent })
    # Measurement conditions in the measured window: user input (GetLastInputInfo) and window state.
    $w0 = [int64]$j.start_epoch_ms + [int64]([double]$j.measure_after_s * 1000)
    $w1 = [int64]$j.start_epoch_ms + [int64]([double]$j.end_s * 1000)
    $inWin = @($r.pdh | Where-Object { [int64]$_.epoch_ms -ge $w0 -and [int64]$_.epoch_ms -le $w1 })
    $inputs = @($inWin | Where-Object { $null -ne $_.user_idle_ms -and [int64]$_.user_idle_ms -ge 0 -and [int64]$_.user_idle_ms -lt 1500 })
    $minimized = @($inWin | Where-Object { $_.window_minimized -eq $true })
    $conditions = [ordered]@{
        rule = 'valid = no keyboard/mouse input (GetLastInputInfo idle < 1.5 s at a 1 s sample) and the window never minimized during the measured window'
        samples = $inWin.Count
        samples_with_user_input = $inputs.Count
        user_input_at_s = @($inputs | ForEach-Object { [math]::Round(([int64]$_.epoch_ms - [int64]$j.start_epoch_ms) / 1000.0, 1) })
        samples_minimized = $minimized.Count
        samples_foreground = @($inWin | Where-Object { $_.window_foreground -eq $true }).Count
        valid = ($inWin.Count -gt 0) -and ($inputs.Count -eq 0) -and ($minimized.Count -eq 0)
    }
    [ordered]@{
        frametimes_file = (Split-Path $r.frametimes -Leaf); canvas_exit = $r.canvas_exit
        conditions = $conditions
        start_epoch_ms = $j.start_epoch_ms; measure_after_s = $j.measure_after_s; end_s = $j.end_s
        frame_ms = Strip $j.frame_ms; stall_ms = Strip $j.stall_ms; heartbeat_late_ms = Strip $j.heartbeat_late_ms
        sync_wait_ms = Strip $j.sync_wait_ms
        stall_attribution = [ordered]@{ stall_ms_total = $j.stall_attribution.stall_ms_total
                                        stall_ms_during_sync_wait = $j.stall_attribution.stall_ms_during_sync_wait
                                        fraction_during_sync_wait = $j.stall_attribution.fraction_during_sync_wait
                                        stalls_mostly_sync_wait = $j.stall_attribution.stalls_mostly_sync_wait }
        cache_mib = Strip $j.cache_mib
        warmup = [ordered]@{ frame_ms = Strip $j.warmup.frame_ms; stall_ms = Strip $j.warmup.stall_ms }
        trims = $j.trims
        gpu_process_memory_dedicated_mib = [ordered]@{ n = $ded.Count
                                                       max = $(if ($ded.Count) { ($ded | Measure-Object -Maximum).Maximum / 1MB } else { $null })
                                                       min = $(if ($ded.Count) { ($ded | Measure-Object -Minimum).Minimum / 1MB } else { $null }) }
        gpu_util_percent = [ordered]@{ n = $util.Count; mean = $(if ($util.Count) { ($util | Measure-Object -Average).Average } else { $null })
                                       min = $(if ($util.Count) { ($util | Measure-Object -Minimum).Minimum } else { $null }) }
        pdh_samples = @($r.pdh)
    }
}

# ------------------------------------------------------------------------------------------
# Diagnostics (stall analysis controls).
# ------------------------------------------------------------------------------------------
if ($Diagnostics) {
    $diag = Join-Path $Results 'diag'
    New-Item -ItemType Directory -Force $diag | Out-Null
    $out = [ordered]@{}
    $controls = [ordered]@{
        'nocanvas'  = @('--layers', '20', '--doc', '8192x8192', '--backend', $Backend, '--no-canvas', '--on-top', '--measure-after', "$($WarmupSec * 1000)", '--quit-after', "$durationMs")
        'notontop'  = @($canvasArgs | Where-Object { $_ -ne '--on-top' })
    }
    foreach ($name in $controls.Keys) {
        $ft = Join-Path $diag "frametimes-$Backend-idle-$name.json"
        $pdh = Join-Path $Runs "pdh-$Backend-idle-$name.jsonl"
        Remove-Item $ft, $pdh -ErrorAction SilentlyContinue
        Write-Host "[$Backend] diagnostics: $name ($($WarmupSec + $Seconds) s) ..."
        $p = Start-Canvas ($controls[$name] + @('--frametimes', $ft))
        $samplerProc = Start-Sampler $p.Id $pdh
        $p.WaitForExit()
        $null = $samplerProc.WaitForExit(15000)
        $r = [ordered]@{ kind = 'idle'; frametimes = $ft; canvas_exit = $p.ExitCode; j = (Get-Content $ft -Raw | ConvertFrom-Json); pdh = (Read-Jsonl $pdh) }
        $sum = Get-RunSummary $r
        $sum.Remove('pdh_samples')
        $out[$name] = $sum
        Write-Host ("  frame p99 {0} ms; stalls n {1}, p99 {2} ms; heartbeat lateness p50 {3} ms; {4} of stall time during sync waits; valid {5}" -f
            $r.j.frame_ms.p99, $r.j.stall_ms.n, $r.j.stall_ms.p99, $r.j.heartbeat_late_ms.p50, $r.j.stall_attribution.fraction_during_sync_wait, $sum.conditions.valid)
    }
    Merge-Summary "diagnostics_$Backend" $out
    exit 0
}

# A run is repeated (up to -Attempts times) when the session saw keyboard/mouse input or the window
# was minimized during the measured window; the attempts are recorded.
function Invoke-ValidRun([string]$kind) {
    $tried = @()
    for ($a = 1; $a -le $Attempts; $a++) {
        $r = Invoke-Run $kind
        $sum = Get-RunSummary $r
        $tried += [ordered]@{ attempt = $a; valid = $sum.conditions.valid; user_input_at_s = $sum.conditions.user_input_at_s
                              samples_minimized = $sum.conditions.samples_minimized
                              frame_ms = $sum.frame_ms; stall_ms = $sum.stall_ms }
        Write-Host ("[{0}] {1} attempt {2}: conditions valid {3}; input at [{4}] s; minimized samples {5}" -f $Backend, $kind, $a,
            $sum.conditions.valid, ($sum.conditions.user_input_at_s -join ','), $sum.conditions.samples_minimized)
        if ($sum.conditions.valid) { break }
    }
    $sum.attempts = $tried
    @{ run = $r; summary = $sum }
}

$doc = [ordered]@{ generated = (Get-Date).ToString('o'); backend = $Backend; seconds = $Seconds; warmup_s = $WarmupSec
                   scene = "canvas.exe $($canvasArgs -join ' ')"; trim_flag = $TrimFlag
                   gui_pan_diagnostic = [bool]$GuiPan }

if ($Mode -in 'both', 'idle') {
    $v = Invoke-ValidRun 'idle'
    $idleRun = $v.run
    $doc.idle = $v.summary
    $fp99 = $idleRun.j.frame_ms.p99
    Set-Check '1' ([bool]($null -ne $fp99 -and $fp99 -le 16.7 -and $idleRun.canvas_exit -eq 0)) ([ordered]@{
        rule = 'idle frame-time p99 <= 16.7 ms'; frame_ms = Strip $idleRun.j.frame_ms; canvas_exit = $idleRun.canvas_exit
        measurement_valid = $doc.idle.conditions.valid })
}
if ($Mode -in 'both', 'load') {
    $v = Invoke-ValidRun 'load'
    $loadRun = $v.run
    $doc.load = $v.summary
    $cs = $loadRun.comfy
    $prompts = @($cs.prompts)
    $doc.load.comfy_load = [ordered]@{
        exit_code = $loadRun.load_exit; comfyui_version = $cs.comfyui_version; pytorch_version = $cs.pytorch_version
        prompts_ok = $cs.prompts_ok; prompts_total = $cs.prompts_total; duration_ms_p50 = $cs.duration_ms_p50; duration_ms_max = $cs.duration_ms_max
        free = $cs.free
        prompts = @($prompts | ForEach-Object { [ordered]@{ i = $_.i; status = $_.status; trim_touch_epoch_ms = $_.trim_touch_epoch_ms
                                                            submitted_epoch_ms = $_.submitted_epoch_ms; finished_epoch_ms = $_.finished_epoch_ms
                                                            duration_ms = $_.duration_ms; output_sizes = $_.output_sizes } }) }
    $sp99 = $loadRun.j.stall_ms.p99; if ($null -eq $sp99) { $sp99 = 0 }
    $fp99 = $loadRun.j.frame_ms.p99
    $loadOk = ($loadRun.load_exit -eq 0) -and ($cs.prompts_ok -ge 1) -and ($cs.prompts_ok -eq $cs.prompts_total)
    Set-Check '2' ([bool]($sp99 -lt 16 -and $loadRun.canvas_exit -eq 0 -and $loadOk)) ([ordered]@{
        rule = 'load stall p99 < 16 ms; frame-time p99 target <= 33 ms (reported)'
        stall_ms = Strip $loadRun.j.stall_ms; frame_ms = Strip $loadRun.j.frame_ms
        frame_p99_target_33ms_met = ($null -ne $fp99 -and $fp99 -le 33)
        comfy_load_ok = $loadOk; prompts_ok = $cs.prompts_ok; canvas_exit = $loadRun.canvas_exit
        measurement_valid = $doc.load.conditions.valid })

    # Criterion 3: the first PDH sample >= 1 s after each trim touch.
    $samples = @($loadRun.pdh | Where-Object { $null -ne $_.dedicated_usage } | Sort-Object { [int64]$_.epoch_ms })
    $endEpoch = [int64]$loadRun.j.start_epoch_ms + [int64]([double]$loadRun.j.end_s * 1000)
    $after = @()
    foreach ($pr in $prompts) {
        if ($null -eq $pr.trim_touch_epoch_ms -or [int64]$pr.trim_touch_epoch_ms -gt $endEpoch - 1000) { continue }
        $s = $samples | Where-Object { [int64]$_.epoch_ms -ge [int64]$pr.trim_touch_epoch_ms + 1000 } | Select-Object -First 1
        $trim = @($loadRun.j.trims | Where-Object { [int64]$_.epoch_ms -ge [int64]$pr.trim_touch_epoch_ms } | Select-Object -First 1)
        $after += [ordered]@{ prompt = $pr.i; touch_epoch_ms = $pr.trim_touch_epoch_ms
                              canvas_trim_after_ms = $(if ($trim) { [int64]$trim[0].epoch_ms - [int64]$pr.trim_touch_epoch_ms } else { $null })
                              cache_mib_after_trim = $(if ($trim) { $trim[0].cache_mib_after } else { $null })
                              sample_epoch_ms = $(if ($s) { $s.epoch_ms } else { $null })
                              dedicated_bytes = $(if ($s) { $s.dedicated_usage } else { $null })
                              dedicated_mib = $(if ($s) { [math]::Round($s.dedicated_usage / 1MB, 1) } else { $null })
                              shared_mib = $(if ($s) { [math]::Round($s.shared_usage / 1MB, 1) } else { $null })
                              total_committed_mib = $(if ($s) { [math]::Round($s.total_committed / 1MB, 1) } else { $null }) }
    }
    $measured = @($after | Where-Object { $null -ne $_.dedicated_bytes })
    $maxAfter = if ($measured.Count) { ($measured | ForEach-Object { [double]$_.dedicated_bytes } | Measure-Object -Maximum).Maximum } else { $null }
    $maxRun = if ($samples.Count) { ($samples | ForEach-Object { [double]$_.dedicated_usage } | Measure-Object -Maximum).Maximum } else { $null }
    $trimsSeen = @($loadRun.j.trims).Count
    Set-Check '3' ([bool]($measured.Count -ge 1 -and $maxAfter -lt 1e9 -and $trimsSeen -ge $measured.Count)) ([ordered]@{
        rule = 'first PDH Dedicated Usage sample >= 1 s after each trim touch < 1,000,000,000 bytes'
        trims_by_canvas = $trimsSeen; touches_checked = $measured.Count
        max_after_trim_bytes = $maxAfter; max_after_trim_mib = $(if ($maxAfter) { [math]::Round($maxAfter / 1MB, 1) } else { $null })
        max_whole_run_bytes = $maxRun; max_whole_run_mib = $(if ($maxRun) { [math]::Round($maxRun / 1MB, 1) } else { $null })
        after_trim = $after })
}
$neg = Join-Path $Results 'negative-check.json'
if (Test-Path $neg) {
    $n = Get-Content $neg -Raw | ConvertFrom-Json
    $crit['4'] = [ordered]@{ criterion = '4'; pass = [bool]$n.pass; from = 'results/negative-check.json'; stdout = $n.stdout; exit_code = $n.exit_code; wall_ms = $n.wall_ms }
    Write-Host ("criterion 4 [{0}] : {1} (from negative-check.json)" -f $Backend, $(if ($n.pass) { 'PASS' } else { 'FAIL' }))
} else {
    Write-Host 'criterion 4 : not run (pwsh -File spikes/canvas/run-frametimes.ps1 -NegativeCheck with ComfyUI stopped)'
}
$doc.criteria = @($crit.Values)
Merge-Summary "$Backend$suffix" $doc
$failed = @($crit.Values | Where-Object { -not $_.pass } | ForEach-Object { $_.criterion })
if ($failed.Count) { Write-Host "[$Backend] FAILED criteria: $($failed -join ', ')"; exit 1 }
Write-Host "[$Backend] criteria evaluated: $(@($crit.Keys) -join ', ') all PASS"; exit 0
