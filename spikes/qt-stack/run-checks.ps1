<#
.SYNOPSIS
  Builds, deploys and runs the Qt stack spike's automated checks (Task 2, criteria 1-7) and
  writes spikes/qt-stack/results/checks.json.

.DESCRIPTION
  1  configure + build (Ninja, MSVC, default warning settings)
  2  start: three docks open, backend D3D11 in the log
  3  --auto-float-canvas: initialize >= 2, quit logged, exit code 0
  4  webengine_loaded never before the Web dock is shown; dumpbin shows no Qt6WebEngine* DLL
  5  ComfyUI down: error page, load failure logged, app responsive.  ComfyUI up: ComfyUI UI shown.
  6  windeployqt --qmldir qml output passes 2-3 with Qt and MSVC removed from PATH
  7  SpikeCounter.increment() from QML updates the bound text

  Build output, deploy output and per-run logs go under spikes/qt-stack/build/ (git-ignored).

.PARAMETER Clean
  Delete build/cmake before configuring (from-scratch build, so the warning count is complete).

.PARAMETER Comfy
  both (default): run the ComfyUI-down half, then start ComfyUI on 127.0.0.1:8188 (only if it is
  not already up), run the ComfyUI-up half and stop the process this script started.
  down / up: run one half only (up never starts ComfyUI).  skip: neither half.

.EXAMPLE
  pwsh -File spikes/qt-stack/run-checks.ps1
#>
param(
    [ValidateSet('both', 'down', 'up', 'skip')] [string]$Comfy = 'both',
    [string]$ComfyPython = (Join-Path $env:USERPROFILE 'ComfyUI\.venv\Scripts\python.exe'),
    [string]$ComfyMain   = (Join-Path $env:USERPROFILE 'ComfyUI\ComfyUI\main.py'),
    [switch]$NoBuild,
    [switch]$Clean   # delete build/cmake first, so criterion 1 is a from-scratch build
)
$ErrorActionPreference = 'Stop'

$Root    = $PSScriptRoot
$Repo    = (Resolve-Path (Join-Path $Root '..\..')).Path
$Build   = Join-Path $Root 'build'
$BinDir  = Join-Path $Build 'cmake\bin'
$Deploy  = Join-Path $Build 'deploy'
$Runs    = Join-Path $Build 'runs'
$Results = Join-Path $Root 'results'
$ExeName = 'qt_stack_spike.exe'
$ShimDll = 'spike_shim.dll'
$ComfyUrl = 'http://127.0.0.1:8188'

. (Join-Path $Repo 'spikes\toolchain\env.ps1')
New-Item -ItemType Directory -Force $Build, $Runs, $Results | Out-Null

$checks = [ordered]@{}
function Set-Check([int]$n, [bool]$pass, [hashtable]$detail) {
    $o = [ordered]@{ criterion = $n; pass = $pass }
    foreach ($k in $detail.Keys) { $o[$k] = $detail[$k] }
    $script:checks["$n"] = $o
    $tag = if ($pass) { 'PASS' } else { 'FAIL' }
    Write-Host "criterion $n : $tag"
}

function Write-Results {
    $all = foreach ($n in 1..7) {
        if ($checks.Contains("$n")) { $checks["$n"] }
        else { [ordered]@{ criterion = $n; pass = $false; not_run = $true } }
    }
    $doc = [ordered]@{
        generated   = (Get-Date).ToString('o')
        qt_dir      = $env:QTDIR
        qt_version  = (& qmake -query QT_VERSION 2>$null)
        checks      = @($all)
    }
    $doc | ConvertTo-Json -Depth 12 | Set-Content -Encoding utf8 (Join-Path $Results 'checks.json')
    $failed = @($all | Where-Object { -not $_.pass } | ForEach-Object { $_.criterion })
    if ($failed.Count) { Write-Host "FAILED criteria: $($failed -join ', ')"; exit 1 }
    Write-Host 'ALL CRITERIA PASSED'; exit 0
}

function Read-Log([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content $path | Where-Object { $_.Trim() } | ForEach-Object {
        try { $_ | ConvertFrom-Json } catch { $null }
    } | Where-Object { $_ })
}

function Test-Comfy {
    try { (Invoke-WebRequest "$ComfyUrl/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 }
    catch { $false }
}

# Starts the app, polls Process.Responding once a second, kills it after $TimeoutSec.
function Invoke-App([string]$exe, [string[]]$appArgs, [hashtable]$envSet = @{}, [string[]]$envUnset = @(),
                    [int]$TimeoutSec = 60) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = Split-Path $exe
    foreach ($k in $envSet.Keys) { $psi.Environment[$k] = $envSet[$k] }
    foreach ($k in $envUnset) { [void]$psi.Environment.Remove($k) }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    $polls = [System.Collections.Generic.List[object]]::new()
    while (-not $p.WaitForExit(1000)) {
        try { $p.Refresh(); $polls.Add([ordered]@{ t_ms = $sw.ElapsedMilliseconds; responding = $p.Responding }) } catch {}
        if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) {
            try { $p.Kill($true) } catch {}
            $p.WaitForExit()
            return [ordered]@{ exit_code = $null; timed_out = $true; wall_ms = $sw.ElapsedMilliseconds; polls = @($polls) }
        }
    }
    $p.WaitForExit()
    [ordered]@{ exit_code = $p.ExitCode; timed_out = $false; wall_ms = $sw.ElapsedMilliseconds; polls = @($polls) }
}

function Get-Events($log, [string]$name) { @($log | Where-Object { $_.event -eq $name }) }

# Criterion 2 (+7): plain start, counter clicked three times, quit after 6 s.
function Test-Start([string]$exe, [string]$tag, [hashtable]$envSet = @{}, [string[]]$envUnset = @()) {
    $log = Join-Path $Runs "$tag.jsonl"; Remove-Item $log -ErrorAction SilentlyContinue
    $shot = Join-Path $Runs "$tag.png"
    $run = Invoke-App $exe @('--log', $log, '--auto-counter', '1500', '--quit-after', '6000', '--screenshot', $shot) $envSet $envUnset
    $ev = Read-Log $log
    $docks = Get-Events $ev 'docks' | Select-Object -Last 1
    $open = @($docks.docks | Where-Object { $_.open } | ForEach-Object { $_.name })
    $backend = @(Get-Events $ev 'backend' | ForEach-Object { $_.backend })
    $ok = ($run.exit_code -eq 0) -and ($open.Count -eq 3) -and ($backend -contains 'D3D11') -and
          ((Get-Events $ev 'quit').Count -ge 1)
    [ordered]@{ pass = $ok; log = $log; screenshot = $shot; exit_code = $run.exit_code; timed_out = $run.timed_out
                docks_open = $open; backends = $backend; events = $ev; run = $run }
}

# Criterion 3: --auto-float-canvas.
function Test-Float([string]$exe, [string]$tag, [hashtable]$envSet = @{}, [string[]]$envUnset = @()) {
    $log = Join-Path $Runs "$tag.jsonl"; Remove-Item $log -ErrorAction SilentlyContinue
    $run = Invoke-App $exe @('--log', $log, '--auto-float-canvas') $envSet $envUnset
    $ev = Read-Log $log
    $init = Get-Events $ev 'initialize'
    # Also require the parenthetical: a new renderer on float and another on re-dock (>= 3 in all).
    $ok = ($run.exit_code -eq 0) -and ($init.Count -ge 2) -and ((Get-Events $ev 'quit').Count -ge 1) -and
          ((Get-Events $ev 'renderer_created').Count -ge 3)
    [ordered]@{ pass = $ok; log = $log; exit_code = $run.exit_code; timed_out = $run.timed_out; wall_ms = $run.wall_ms
                initialize_count = $init.Count
                renderers_created = (Get-Events $ev 'renderer_created').Count
                float_events = @(Get-Events $ev 'float' | ForEach-Object { [ordered]@{ t_ms = $_.t_ms; floating = $_.floating; canvas_in_main_window = $_.canvas_in_main_window } })
                backends = @(Get-Events $ev 'backend' | ForEach-Object { $_.backend }) }
}

# One Web-dock run: Web tab shown at 2 s, counter clicked at 8 s, quit at $quitMs.
function Invoke-WebRun([string]$tag, [int]$quitMs) {
    $exe = Join-Path $BinDir $ExeName
    $log = Join-Path $Runs "$tag.jsonl"; Remove-Item $log -ErrorAction SilentlyContinue
    $shot = Join-Path $Runs "$tag.png"
    $run = Invoke-App $exe @('--log', $log, '--show-web', '2000', '--auto-counter', '8000', '--quit-after', "$quitMs", '--screenshot', $shot, '--heartbeat', '250')
    [ordered]@{ log = $log; screenshot = $shot; run = $run; events = (Read-Log $log) }
}

# ------------------------------------------------------------------------------------------
# Criterion 1: configure + build.
# ------------------------------------------------------------------------------------------
$cmakeLists = Join-Path $Root 'CMakeLists.txt'
if (-not (Test-Path $cmakeLists)) {
    Set-Check 1 $false @{ reason = "no CMake project: $cmakeLists does not exist" }
    Write-Results
}
$cmakeBuild = Join-Path $Build 'cmake'
$confLog = Join-Path $Build 'configure.log'
$buildLog = Join-Path $Build 'build.log'
if ($Clean -and (Test-Path $cmakeBuild)) { Remove-Item -Recurse -Force $cmakeBuild }
if (-not $NoBuild) {
    & cmake -S $Root -B $cmakeBuild -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo *>&1 | Tee-Object -FilePath $confLog | Out-Null
    $confExit = $LASTEXITCODE
    $buildExit = $null
    if ($confExit -eq 0) {
        & cmake --build $cmakeBuild *>&1 | Tee-Object -FilePath $buildLog | Out-Null
        $buildExit = $LASTEXITCODE
    }
} else { $confExit = 0; $buildExit = 0 }
$buildText = if (Test-Path $buildLog) { Get-Content $buildLog } else { @() }
$warnLines = @($buildText | Where-Object { $_ -match '\bwarning\b' } | Select-Object -Unique)
# R3: nothing in our own CMake/Cargo files may switch warnings or warnings-as-errors off.
$ownFiles = @($cmakeLists) + @(Get-ChildItem (Join-Path $Root 'rust') -Recurse -Include Cargo.toml, build.rs -ErrorAction SilentlyContinue | ForEach-Object FullName)
$suppress = @(Select-String -Path $ownFiles -Pattern '/W0|/WX-|-w\b|-Wno-error|/wd\d|KDDockWidgets_WERROR|allow\(warnings\)|RUSTFLAGS' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" })
$exePath = Join-Path $BinDir $ExeName
$c1 = ($confExit -eq 0) -and ($buildExit -eq 0) -and (Test-Path $exePath) -and ($suppress.Count -eq 0)
Set-Check 1 $c1 @{ configure_exit = $confExit; build_exit = $buildExit; exe = $exePath
                   warning_lines = $warnLines.Count; warnings = @($warnLines | Select-Object -First 40)
                   warning_suppression_in_own_files = $suppress }
if (-not $c1) { Write-Results }

# ------------------------------------------------------------------------------------------
# Criteria 2, 7: start + counter.  Criterion 3: auto float.
# ------------------------------------------------------------------------------------------
$start = Test-Start $exePath 'c2-start'
Set-Check 2 $start.pass @{ exit_code = $start.exit_code; timed_out = $start.timed_out; docks_open = $start.docks_open
                           backends = $start.backends; log = $start.log; screenshot = $start.screenshot }

$counter = @(Get-Events $start.events 'counter')
$last = $counter | Select-Object -Last 1
$c7 = ($counter.Count -ge 3) -and ($last.count -eq 3) -and ($last.text -eq 'Count: 3') -and
      (@($counter | Where-Object { $_.text -ne "Count: $($_.count)" }).Count -eq 0)
Set-Check 7 $c7 @{ counter_events = @($counter | ForEach-Object { [ordered]@{ t_ms = $_.t_ms; count = $_.count; text = $_.text } }) }

$float = Test-Float $exePath 'c3-float'
$fd = @{}; foreach ($k in $float.Keys) { if ($k -ne 'pass') { $fd[$k] = $float[$k] } }
Set-Check 3 $float.pass $fd

# ------------------------------------------------------------------------------------------
# Criterion 5 (and the runtime half of 4): Web dock with ComfyUI down and up.
# ------------------------------------------------------------------------------------------
$web = [ordered]@{}
$c5 = [ordered]@{}
if ($Comfy -in 'both', 'down') {
    if (Test-Comfy) {
        $c5.down = [ordered]@{ pass = $false; not_verified = 'ComfyUI already answers on 127.0.0.1:8188; it was not started by this script, so it was left alone' }
    } else {
        $r = Invoke-WebRun 'c5-down' 14000
        $web.down = $r
        $ev = $r.events
        $fail = @(Get-Events $ev 'web_load' | Where-Object { $_.status -eq 'failed' })
        $failT = if ($fail.Count) { $fail[0].t_ms } else { [int]::MaxValue }
        $after = @(Get-Events $ev 'counter' | Where-Object { $_.t_ms -gt $failT })
        $pollsAfter = @($r.run.polls | Where-Object { $_.t_ms -gt $failT })
        # Largest gap between GUI-thread heartbeats (250 ms timer) after the load failure.
        $beats = @(Get-Events $ev 'heartbeat' | Where-Object { $_.t_ms -ge $failT } | ForEach-Object { [long]$_.t_ms })
        $maxGap = 0; for ($i = 1; $i -lt $beats.Count; $i++) { $maxGap = [Math]::Max($maxGap, $beats[$i] - $beats[$i - 1]) }
        # Chromium's error page: the failed load reports isErrorPage and the page is chrome-error://.
        $probe = Get-Events $ev 'web_probe' | Select-Object -Last 1
        $errorPage = ($fail.Count -ge 1) -and ($fail[0].is_error_page -eq $true) -and ("$($probe.js_url)" -like 'chrome-error://*')
        $ok = ($fail.Count -ge 1) -and $errorPage -and ($after.Count -ge 1) -and ($after[-1].text -eq "Count: $($after[-1].count)") -and
              (@($pollsAfter | Where-Object { -not $_.responding }).Count -eq 0) -and ($r.run.exit_code -eq 0) -and
              ($beats.Count -ge 10) -and ($maxGap -lt 1000)
        $c5.down = [ordered]@{ pass = $ok; exit_code = $r.run.exit_code; load_failures = @($fail | ForEach-Object { [ordered]@{ t_ms = $_.t_ms; url = $_.url; error_code = $_.error_code; error_domain = $_.error_domain; error_string = $_.error_string } })
                               heartbeats_after_failure = $beats.Count; max_heartbeat_gap_ms = $maxGap
                               counter_after_failure = @($after | ForEach-Object { $_.text }); not_responding_polls_after_failure = @($pollsAfter | Where-Object { -not $_.responding }).Count
                               error_page_shown = $errorPage; web_probe = $probe; log = $r.log; screenshot = $r.screenshot }
    }
}
if ($Comfy -in 'both', 'up') {
    $started = $null
    if (-not (Test-Comfy) -and $Comfy -eq 'both') {
        Write-Host 'starting ComfyUI on 127.0.0.1:8188 ...'
        $started = Start-Process -FilePath $ComfyPython -ArgumentList @($ComfyMain, '--listen', '127.0.0.1', '--port', '8188') `
            -WorkingDirectory (Split-Path $ComfyMain) -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput (Join-Path $Runs 'comfy.out.log') -RedirectStandardError (Join-Path $Runs 'comfy.err.log')
        $deadline = (Get-Date).AddSeconds(240)
        while (-not (Test-Comfy) -and (Get-Date) -lt $deadline -and -not $started.HasExited) { Start-Sleep -Seconds 2 }
    }
    try {
        if (-not (Test-Comfy)) {
            $c5.up = [ordered]@{ pass = $false; not_verified = 'ComfyUI did not answer /system_stats on 127.0.0.1:8188' }
        } else {
            $r = Invoke-WebRun 'c5-up' 25000
            $web.up = $r
            $ev = $r.events
            $okLoad = @(Get-Events $ev 'web_load' | Where-Object { $_.status -eq 'succeeded' -and $_.url -like "$ComfyUrl*" })
            $probe = Get-Events $ev 'web_probe' | Select-Object -Last 1
            $ok = ($okLoad.Count -ge 1) -and ($probe.title -match 'ComfyUI') -and $probe.has_graph_canvas -and ($r.run.exit_code -eq 0)
            $c5.up = [ordered]@{ pass = [bool]$ok; exit_code = $r.run.exit_code; load_succeeded = $okLoad.Count; web_probe = $probe
                                 comfy_started_by_script = [bool]$started; log = $r.log; screenshot = $r.screenshot }
        }
    } finally {
        if ($started -and -not $started.HasExited) {
            & taskkill /PID $started.Id /T /F *>&1 | Out-Null
            Write-Host 'stopped the ComfyUI process this script started'
        }
    }
}
if ($Comfy -ne 'skip') {
    $halves = @($c5.Values)
    $c5pass = ($halves.Count -eq 2) -and (@($halves | Where-Object { -not $_.pass }).Count -eq 0)
    Set-Check 5 $c5pass @{ comfy_mode = $Comfy; down = $c5.down; up = $c5.up }
}

# ------------------------------------------------------------------------------------------
# Criterion 4: lazy WebEngine + dumpbin.
# ------------------------------------------------------------------------------------------
$deps = [ordered]@{}
foreach ($bin in @((Join-Path $BinDir $ExeName), (Join-Path $BinDir $ShimDll))) {
    $out = & dumpbin /dependents $bin 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ -match '\.dll$' -and $_ -notmatch '^Dump of file' }
    $deps[(Split-Path $bin -Leaf)] = @($out)
}
$webDeps = @($deps.Values | ForEach-Object { $_ } | Where-Object { $_ -match '^Qt6WebEngine' })
$noWeb = @(Get-Events $start.events 'webengine_loaded').Count -eq 0
$startDll = (Get-Events $start.events 'window_shown' | Select-Object -First 1).webengine_dll_in_process
$ordering = @()
foreach ($k in $web.Keys) {
    $ev = $web[$k].events
    $shown = Get-Events $ev 'web_dock_shown' | Select-Object -First 1
    $loaded = Get-Events $ev 'webengine_loaded' | Select-Object -First 1
    $ordering += [ordered]@{ run = $k; web_dock_shown_t_ms = $shown.t_ms; webengine_loaded_t_ms = $loaded.t_ms
                             dll_in_process_before_show = $shown.webengine_dll_in_process
                             ok = ($null -ne $shown) -and ($null -ne $loaded) -and ($loaded.t_ms -ge $shown.t_ms) -and (-not $shown.webengine_dll_in_process) }
}
$c4 = ($webDeps.Count -eq 0) -and ($deps.Count -eq 2) -and (@($deps.Values | Where-Object { $_.Count -eq 0 }).Count -eq 0) -and
      $noWeb -and ($startDll -eq $false) -and ($ordering.Count -ge 1) -and (@($ordering | Where-Object { -not $_.ok }).Count -eq 0)
Set-Check 4 $c4 @{ dependents = $deps; webengine_dependents = $webDeps; start_run_has_webengine_loaded = (-not $noWeb)
                   start_run_webengine_dll_in_process = $startDll; web_runs = $ordering }

# ------------------------------------------------------------------------------------------
# Criterion 6: windeployqt output, run with Qt and MSVC stripped from PATH.
# ------------------------------------------------------------------------------------------
if (Test-Path $Deploy) { Remove-Item -Recurse -Force $Deploy }
New-Item -ItemType Directory -Force $Deploy | Out-Null
Copy-Item (Join-Path $BinDir '*.exe'), (Join-Path $BinDir '*.dll') $Deploy
$wdqLog = Join-Path $Build 'windeployqt.log'
& windeployqt --release --qmldir (Join-Path $Root 'qml') --dir $Deploy (Join-Path $Deploy $ExeName) *>&1 | Tee-Object -FilePath $wdqLog | Out-Null
$wdqExit = $LASTEXITCODE
$stripRe = '\\Qt\\|Microsoft Visual Studio|Windows Kits|\\MSVC\\|\\Qt6?$'
$cleanPath = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch $stripRe }) -join ';'
$leaks = @(($cleanPath -split ';') | Where-Object { $_ -and ((Test-Path (Join-Path $_ 'Qt6Core.dll')) -or (Test-Path (Join-Path $_ 'cl.exe'))) })
$envSet = @{ PATH = $cleanPath }
$envUnset = @('QTDIR', 'QT_PLUGIN_PATH', 'QML_IMPORT_PATH', 'QML2_IMPORT_PATH', 'QT_QPA_PLATFORM_PLUGIN_PATH', 'CMAKE_PREFIX_PATH',
              'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsRedistDir', 'INCLUDE', 'LIB', 'LIBPATH')
$dExe = Join-Path $Deploy $ExeName
$dStart = Test-Start $dExe 'c6-start' $envSet $envUnset
$dFloat = Test-Float $dExe 'c6-float' $envSet $envUnset
# Informational (not part of criterion 6's text): the deployed WebEngine also has to work, since a
# missing WebEngine resource is the most likely deploy failure. The Web dock is shown and the load
# result recorded (ComfyUI is normally stopped by now, so a failed load with the error page is fine).
$dWebLog = Join-Path $Runs 'c6-web.jsonl'
if (Test-Path $dWebLog) { Remove-Item $dWebLog }
$dWebRun = Invoke-App $dExe @('--log', $dWebLog, '--show-web', '1000', '--quit-after', '10000') $envSet $envUnset
$dWebEv = Read-Log $dWebLog
$dWeb = [ordered]@{ exit_code = $dWebRun.exit_code
                    webengine_loaded = (Get-Events $dWebEv 'webengine_loaded').Count -ge 1
                    web_load = @(Get-Events $dWebEv 'web_load' | ForEach-Object { "$($_.status) $($_.error_string)".Trim() })
                    render_process_terminated = (Get-Events $dWebEv 'web_render_process_terminated').Count
                    probe_js_url = (Get-Events $dWebEv 'web_probe' | Select-Object -Last 1).js_url
                    qt_warnings = @(Get-Events $dWebEv 'qt_msg' | Where-Object { $_.type -ne 'debug' } | ForEach-Object { $_.msg } | Select-Object -First 10)
                    log = $dWebLog }

$c6 = ($wdqExit -eq 0) -and ($leaks.Count -eq 0) -and $dStart.pass -and $dFloat.pass
Set-Check 6 $c6 @{ windeployqt_exit = $wdqExit; deploy_dir = $Deploy; path_dirs_with_qt_or_cl = $leaks
                   path_dirs_removed = @(($env:PATH -split ';') | Where-Object { $_ -and $_ -match $stripRe } | Select-Object -Unique)
                   web_info = $dWeb
                   start = [ordered]@{ pass = $dStart.pass; exit_code = $dStart.exit_code; docks_open = $dStart.docks_open; backends = $dStart.backends; log = $dStart.log }
                   float = [ordered]@{ pass = $dFloat.pass; exit_code = $dFloat.exit_code; initialize_count = $dFloat.initialize_count; renderers_created = $dFloat.renderers_created; log = $dFloat.log } }

# Keep the window grabs next to checks.json (start, ComfyUI down, ComfyUI up).
foreach ($n in 'c2-start', 'c5-down', 'c5-up') {
    $png = Join-Path $Runs "$n.png"
    if (Test-Path $png) { Copy-Item $png (Join-Path $Results "$n.png") -Force }
}

Write-Results
