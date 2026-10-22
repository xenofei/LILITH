<#
.SYNOPSIS
  App-shell spike (plan 4a, Task 0): builds from scratch, deploys, runs every check that needs no
  owner and writes spikes/app-shell/results/checks.json. docs/spikes/10-app-shell.md §3 describes
  each check; OWNER-STEPS.md lists the checks the owner runs.

.DESCRIPTION
  Safety, for every app launch:
    - this process sets SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX), which
      every child inherits, so a missing DLL is an exit code (0xC0000135), never a dialog;
    - the Qt, KDDockWidgets, shim and (in the deploy folder) VC++ DLLs are checked before each
      launch; a missing one stops that check with a logged reason;
    - every app run uses --no-activate, and the foreground window is sampled during the run;
    - every run has a timeout, after which the process tree is killed.

.PARAMETER Clean
  Delete build/cmake first (from-scratch build, so S0.1's warning count is complete).
.PARAMETER KddwTag
  KDDockWidgets tag (S0.1: v2.5.0; the fallback is v2.4.1).
.PARAMETER Only
  Run only these checks (S0.1, S0.2, S0.3, S0.4, S0.5a, S0.8, S0.9). S0.1's build always runs
  unless -NoBuild.
.PARAMETER WarmStarts
  S0.5 (a): number of timed starts (default 10).

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Clean
#>
param(
    [switch]$Clean,
    [switch]$NoBuild,
    [string]$KddwTag = 'v2.5.0',
    [string[]]$Only = @(),
    [int]$WarmStarts = 10,
    [int]$BusyCpuPercent = 35
)
$ErrorActionPreference = 'Stop'
# -Only S0.1,S0.3 arrives as one string through pwsh -File.
$Only = @($Only | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

$Root    = $PSScriptRoot
$Repo    = (Resolve-Path (Join-Path $Root '..\..')).Path
$Build   = Join-Path $Root 'build'
$CmakeDir = Join-Path $Build 'cmake'
$BinDir  = Join-Path $CmakeDir 'bin'
$Deploy  = Join-Path $Build 'deploy'
$Runs    = Join-Path $Build 'runs'
$Results = Join-Path $Root 'results'
$ExeName = 'app_shell_spike.exe'
$LocalSpike = Join-Path $env:LOCALAPPDATA 'LILITH-spike'

. (Join-Path $Repo 'spikes\toolchain\env.ps1')
New-Item -ItemType Directory -Force $Build, $Runs, $Results | Out-Null

Add-Type -Namespace AppShellSpike -Name Win -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
'@
[void][AppShellSpike.Win]::SetErrorMode(0x0001 -bor 0x8000)

$started = Get-Date
$checks = [ordered]@{}
function Want([string]$id) { $Only.Count -eq 0 -or $Only -contains $id }
function Set-Check([string]$id, [string]$verdict, $detail) {
    $o = [ordered]@{ check = $id; verdict = $verdict }
    foreach ($k in $detail.Keys) { $o[$k] = $detail[$k] }
    $script:checks[$id] = $o
    Write-Host ("{0,-6} {1}" -f $id, $verdict)
}
function Save-Results {
    $doc = [ordered]@{
        generated = (Get-Date).ToString('o')
        minutes = [math]::Round(((Get-Date) - $started).TotalMinutes, 1)
        qt_version = (& qmake -query QT_VERSION 2>$null)
        kddw_tag = $KddwTag
        owner_checks = @('S0.5b', 'S0.5c', 'S0.6', 'S0.7', 'S0.9 (CI)', 'S0.10')
        checks = @($checks.Values)
    }
    $doc | ConvertTo-Json -Depth 14 | Set-Content -Encoding utf8 (Join-Path $Results 'checks.json')
}

function Read-Log([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content $path | Where-Object { $_.Trim() } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } | Where-Object { $_ })
}
function Get-Events($log, [string]$name) { @($log | Where-Object { $_.event -eq $name }) }

function Get-CpuPercent {
    try { [math]::Round((Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 1).CounterSamples[0].CookedValue, 1) }
    catch { -1 }
}

# The DLLs a launch needs. Build tree: Qt from QTDIR\bin (first on PATH), ours from bin\.
# Deploy folder: everything in the folder itself, including the VC++ runtime.
$QtDlls = 'Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll', 'Qt6Quick.dll', 'Qt6QuickControls2.dll', 'Qt6Network.dll'
$OurDlls = 'kddockwidgets-qt62.dll', 'spike_shell_shim.dll'
$CrtDlls = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll'
function Test-Launchable([string]$exe, [bool]$deployed) {
    $dir = Split-Path $exe
    $missing = @()
    if (-not (Test-Path $exe)) { $missing += (Split-Path $exe -Leaf) }
    foreach ($d in $OurDlls) { if (-not (Test-Path (Join-Path $dir $d))) { $missing += $d } }
    if ($deployed) {
        foreach ($d in $QtDlls + $CrtDlls) { if (-not (Test-Path (Join-Path $dir $d))) { $missing += $d } }
        if (-not (Test-Path (Join-Path $dir 'platforms\qwindows.dll'))) { $missing += 'platforms\qwindows.dll' }
    } else {
        $qtBin = Join-Path $env:QTDIR 'bin'
        if (($env:PATH -split ';')[0] -ne $qtBin) { $missing += 'Qt bin first on PATH' }
        foreach ($d in $QtDlls) { if (-not (Test-Path (Join-Path $qtBin $d))) { $missing += $d } }
    }
    $missing
}

# Starts a process (inheriting this process's error mode), samples the foreground window's
# process every 250 ms, kills the tree after $TimeoutSec.
function Invoke-App([string]$exe, [string[]]$appArgs, [hashtable]$envSet = @{}, [string[]]$envUnset = @(),
                    [int]$TimeoutSec = 60, [bool]$Deployed = $false) {
    $missing = @(Test-Launchable $exe $Deployed)
    if ($missing.Count) {
        Write-Host "NOT LAUNCHED ${exe}: missing $($missing -join ', ')"
        return [ordered]@{ launched = $false; missing = $missing; exit_code = $null; timed_out = $false }
    }
    $psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = Split-Path $exe
    foreach ($k in $envSet.Keys) { $psi.Environment[$k] = $envSet[$k] }
    foreach ($k in $envUnset) { [void]$psi.Environment.Remove($k) }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    $fgHits = 0; $samples = 0
    while (-not $p.WaitForExit(250)) {
        $samples++
        if ([AppShellSpike.Win]::ForegroundPid() -eq $p.Id) { $fgHits++ }
        if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) {
            try { $p.Kill($true) } catch {}
            $p.WaitForExit()
            return [ordered]@{ launched = $true; exit_code = $null; timed_out = $true; wall_ms = $sw.ElapsedMilliseconds
                               foreground_samples = $samples; foreground_was_ours = $fgHits }
        }
    }
    $p.WaitForExit()
    [ordered]@{ launched = $true; exit_code = $p.ExitCode; timed_out = $false; wall_ms = $sw.ElapsedMilliseconds
                foreground_samples = $samples; foreground_was_ours = $fgHits }
}

function Invoke-Spike([string]$exe, [string]$tag, [string[]]$extra, [hashtable]$envSet = @{}, [string[]]$envUnset = @(),
                      [int]$TimeoutSec = 60, [bool]$Deployed = $false) {
    $log = Join-Path $Runs "$tag.jsonl"
    Remove-Item $log -ErrorAction SilentlyContinue
    $run = Invoke-App $exe (@('--log', $log, '--no-activate') + $extra) $envSet $envUnset $TimeoutSec $Deployed
    [ordered]@{ tag = $tag; log = $log; run = $run; events = (Read-Log $log) }
}

# ---- geometry comparison (S0.1) ------------------------------------------------------------
function Get-Snapshot($events, [string]$label) { Get-Events $events 'layout_snapshot' | Where-Object { $_.label -eq $label } | Select-Object -Last 1 }
function Test-Near($a, $b, [int]$tol = 2) {
    if ($null -eq $a -or $null -eq $b -or $a.Count -ne $b.Count) { return $false }
    for ($i = 0; $i -lt $a.Count; $i++) { if ([math]::Abs([double]$a[$i] - [double]$b[$i]) -gt $tol) { return $false } }
    $true
}
function Compare-Snapshots($saved, $restored) {
    $problems = @()
    if (-not $saved -or -not $restored) { return @('snapshot missing') }
    $sw = @($saved.windows.PSObject.Properties.Name | Sort-Object)
    $rw = @($restored.windows.PSObject.Properties.Name | Sort-Object)
    if (($sw -join ',') -ne ($rw -join ',')) { $problems += "windows differ: saved [$($sw -join ', ')] restored [$($rw -join ', ')]" }
    foreach ($n in $sw) {
        if (-not (Test-Near $saved.windows.$n $restored.windows.$n)) { $problems += "window $n saved $($saved.windows.$n -join ',') restored $($restored.windows.$n -join ',')" }
    }
    foreach ($d in $saved.docks) {
        $r = $restored.docks | Where-Object { $_.name -eq $d.name }
        if (-not $r) { $problems += "dock $($d.name) missing"; continue }
        if ($r.window -ne $d.window -or $r.open -ne $d.open -or $r.floating -ne $d.floating) { $problems += "dock $($d.name) in $($r.window) (saved $($d.window))" }
        elseif (-not (Test-Near $d.rect $r.rect)) { $problems += "dock $($d.name) rect $($r.rect -join ',') (saved $($d.rect -join ','))" }
    }
    $problems
}

# Runs one save + restore cycle; returns its result.
function Invoke-LayoutCycle([string]$exe, [string]$tag, [hashtable]$envSet = @{}, [string[]]$envUnset = @(), [bool]$Deployed = $false) {
    $file = Join-Path $Runs "$tag-layout.json"
    Remove-Item $file -ErrorAction SilentlyContinue
    $save = Invoke-Spike $exe "$tag-save" @('--layout-save', $file) $envSet $envUnset 60 $Deployed
    $restore = Invoke-Spike $exe "$tag-restore" @('--layout-restore', $file) $envSet $envUnset 60 $Deployed
    $saved = Get-Snapshot $save.events 'saved'
    $restored = Get-Snapshot $restore.events 'restored'
    $before = Get-Snapshot $restore.events 'before_restore'
    $problems = @(Compare-Snapshots $saved $restored)
    # The restore must have had something to do: the default layout differs from the saved one.
    $differed = $before -and (@(Compare-Snapshots $saved $before).Count -gt 0)
    $ok = ($save.run.exit_code -eq 0) -and ($restore.run.exit_code -eq 0) -and (Test-Path $file) -and
          ($problems.Count -eq 0) -and $differed -and
          (@(Get-Events $restore.events 'layout_restore' | Where-Object { $_.ok }).Count -eq 1)
    [ordered]@{ pass = $ok; save_exit = $save.run.exit_code; restore_exit = $restore.run.exit_code
                problems = $problems; default_differed_from_saved = [bool]$differed
                saved_windows = $saved.windows; restored_windows = $restored.windows
                focus_taken = ($save.run.foreground_was_ours + $restore.run.foreground_was_ours) -gt 0
                logs = @($save.log, $restore.log) }
}

$exePath = Join-Path $BinDir $ExeName
$s08Before = @(Get-ChildItem $env:LOCALAPPDATA -Directory -ErrorAction SilentlyContinue | ForEach-Object Name)
$appDataBefore = @(Get-ChildItem $env:APPDATA -Directory -ErrorAction SilentlyContinue | ForEach-Object Name)
if (Want 'S0.8') {
    # Start S0.8 from nothing: the spike's own local folder only.
    if (Test-Path $LocalSpike) { Remove-Item -Recurse -Force $LocalSpike }
    $s08Before = @(Get-ChildItem $env:LOCALAPPDATA -Directory | ForEach-Object Name)
}

# ==========================================================================================
# S0.1 (build part): configure and build from scratch, warnings split by origin.
# ==========================================================================================
$confLog = Join-Path $Build 'configure.log'
$buildLog = Join-Path $Build 'build.log'
$buildInfo = [ordered]@{}
if (-not $NoBuild) {
    if ($Clean -and (Test-Path $CmakeDir)) { Remove-Item -Recurse -Force $CmakeDir }
    $t = [Diagnostics.Stopwatch]::StartNew()
    & cmake -S $Root -B $CmakeDir -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo "-DSPIKE_KDDW_TAG=$KddwTag" *>&1 | Out-File -Encoding utf8 $confLog
    $buildInfo.configure_exit = $LASTEXITCODE
    $buildInfo.configure_seconds = [math]::Round($t.Elapsed.TotalSeconds)
    if ($buildInfo.configure_exit -eq 0) {
        $t.Restart()
        & cmake --build $CmakeDir *>&1 | Out-File -Encoding utf8 $buildLog
        $buildInfo.build_exit = $LASTEXITCODE
        $buildInfo.build_seconds = [math]::Round($t.Elapsed.TotalSeconds)
    }
    $buildInfo.clean = [bool]$Clean
}
function Split-Warnings([string[]]$lines) {
    $own = @(); $kddw = @(); $toolchain = @()
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $l = $lines[$i]
        if ($l -notmatch '(?i)\bwarning\b') { continue }
        if ($l -match '(?i)warning(s)? (generated|emitted)|0 warning') { continue }
        $ctx = ($lines[$i..([math]::Min($i + 3, $lines.Count - 1))] -join ' ')
        if ($l -match '(?i)_deps[\\/]kddockwidgets') { $kddw += $l }
        elseif ($l -match '(?i)C:[\\/]Qt[\\/]|Program Files|cmake-4|QmlAssetDownloader|_deps[\\/](cxxqt|corrosion)') { $toolchain += $l }
        elseif ($l -match '^\s*CMake Warning' -and $ctx -match '(?i)C:[\\/]Qt[\\/]|Program Files') { $toolchain += $l }
        else { $own += $l }
    }
    [ordered]@{ own = @($own | Select-Object -Unique); kddw = @($kddw | Select-Object -Unique); toolchain = @($toolchain | Select-Object -Unique) }
}
$confW = Split-Warnings @(if (Test-Path $confLog) { Get-Content $confLog })
$buildW = Split-Warnings @(if (Test-Path $buildLog) { Get-Content $buildLog })
$ownFiles = @(Join-Path $Root 'CMakeLists.txt') + @(Get-ChildItem (Join-Path $Root 'rust') -Recurse -Include Cargo.toml, build.rs | ForEach-Object FullName)
$suppress = @(Select-String -Path $ownFiles -Pattern '/W[0-4]\b|/WX|-w\b|-Wno-|/wd\d|KDDockWidgets_WERROR|COMPILE_WARNING_AS_ERROR|allow\(warnings\)|RUSTFLAGS|cap-lints' | ForEach-Object { "$($_.Filename):$($_.LineNumber): $($_.Line.Trim())" })
$buildOk = ($NoBuild -or (($buildInfo.configure_exit -eq 0) -and ($buildInfo.build_exit -eq 0))) -and (Test-Path $exePath)
$buildInfo.warning_lines_own = @($confW.own + $buildW.own)
$buildInfo.warning_lines_kddw = @($confW.kddw + $buildW.kddw)
$buildInfo.warning_lines_toolchain = @($confW.toolchain + $buildW.toolchain)
$buildInfo.warning_flags_in_own_files = $suppress
$buildPass = $buildOk -and ($buildInfo.warning_lines_own.Count -eq 0) -and ($suppress.Count -eq 0)
$kddwCommit = try { (& git -C (Join-Path $CmakeDir '_deps\kddockwidgets-src') rev-parse --short HEAD 2>$null) } catch { $null }
$buildInfo.kddw_commit = $kddwCommit
if (-not $buildOk) {
    Set-Check 'S0.1' 'FAIL' ([ordered]@{ build = $buildInfo; reason = 'configure or build failed' })
    Save-Results; exit 1
}

# ==========================================================================================
# S0.1 (layout part): 3 save + restore cycles, and the per-window round trip.
# ==========================================================================================
if (Want 'S0.1') {
    $cycles = @(foreach ($i in 1..3) { Invoke-LayoutCycle $exePath "s01-cycle$i" })
    $win = Invoke-Spike $exePath 's01-window' @('--window-roundtrip', (Join-Path $Runs 's01-window-layout.json'))
    $ev = $win.events
    $wSaved = Get-Snapshot $ev 'window_saved'
    $wDist = Get-Snapshot $ev 'window_disturbed'
    $wRest = Get-Snapshot $ev 'window_restored'
    $wFile = Get-Events $ev 'window_file' | Select-Object -Last 1
    $chartsSaved = $wSaved.docks | Where-Object { $_.name -eq 'charts' }
    $chartsDist = $wDist.docks | Where-Object { $_.name -eq 'charts' }
    $chartsRest = $wRest.docks | Where-Object { $_.name -eq 'charts' }
    $winChecks = [ordered]@{
        file_has_only_main_b = ($wFile -and (@($wFile.main_windows) -join ',') -eq 'main-b' -and $wFile.floating_windows -eq 0)
        disturbed = ($wDist -and $chartsDist.floating -and -not (Test-Near $wSaved.windows.'main-b' $wDist.windows.'main-b'))
        main_b_back = ($wRest -and (Test-Near $wSaved.windows.'main-b' $wRest.windows.'main-b'))
        charts_back_in_main_b = ($chartsRest -and $chartsRest.window -eq 'main-b' -and -not $chartsRest.floating -and (Test-Near $chartsSaved.rect $chartsRest.rect))
        main_a_left_alone = ($wRest -and (Test-Near $wDist.windows.'main-a' $wRest.windows.'main-a') -and -not (Test-Near $wSaved.windows.'main-a' $wRest.windows.'main-a'))
        floating_counter_left_alone = ($wRest -and (Test-Near $wDist.windows.'floating:counter' $wRest.windows.'floating:counter'))
        exit_zero = ($win.run.exit_code -eq 0)
    }
    $winPass = @($winChecks.Values | Where-Object { -not $_ }).Count -eq 0
    $layoutPass = (@($cycles | Where-Object { -not $_.pass }).Count -eq 0) -and $winPass
    $focus = (@($cycles | Where-Object { $_.focus_taken }).Count -gt 0) -or ($win.run.foreground_was_ours -gt 0)
    Set-Check 'S0.1' ($(if ($buildPass -and $layoutPass) { 'PASS' } else { 'FAIL' })) ([ordered]@{
        kddw_tag = $KddwTag; build = $buildInfo; build_pass = $buildPass
        cycles = $cycles; window_roundtrip = [ordered]@{ pass = $winPass; checks = $winChecks; file = $wFile
            saved = $wSaved.windows; disturbed = $wDist.windows; restored = $wRest.windows; log = $win.log }
        focus_taken = $focus; minutes_since_start = [math]::Round(((Get-Date) - $started).TotalMinutes, 1) })
    Save-Results
}

# ==========================================================================================
# S0.3: lazy WebEngine library.
# ==========================================================================================
function Get-Dependents([string]$bin) {
    @(& dumpbin /nologo /dependents $bin 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ -match '\.dll$' -and $_ -notmatch 'Dump of file' })
}
if (Want 'S0.3') {
    $deps = [ordered]@{}
    foreach ($b in @($exePath, (Join-Path $BinDir 'spike_shell_shim.dll'))) { $deps[(Split-Path $b -Leaf)] = Get-Dependents $b }
    $bridgeLib = Get-ChildItem $CmakeDir -Recurse -Filter 'spike_shell_bridge.lib' -ErrorAction SilentlyContinue | Select-Object -First 1
    $bridge = [ordered]@{ path = $null; dependents = @(); webengine_in_directives = $null; webengine_symbols = $null }
    if ($bridgeLib) {
        $bridge.path = $bridgeLib.FullName.Replace($Root, 'spikes\app-shell')
        $bridge.dependents = Get-Dependents $bridgeLib.FullName
        $dir = & dumpbin /nologo /directives $bridgeLib.FullName 2>&1 | Out-String
        $bridge.webengine_in_directives = $dir -match '(?i)webengine'
        $sym = & dumpbin /nologo /linkermember:1 $bridgeLib.FullName 2>&1 | Out-String
        $bridge.webengine_symbols = $sym -match '(?i)webengine'
    }
    $webPluginDeps = Get-Dependents (Join-Path $BinDir 'SpikeWeb\spike_web.dll')
    $webDeps = @($deps.Values | ForEach-Object { $_ } | Where-Object { $_ -match '^Qt6WebEngine' })

    $r = Invoke-Spike $exePath 's03-web' @('--show-web', '3000', '--quit-after', '40000') -TimeoutSec 120
    $ev = $r.events
    $startEv = Get-Events $ev 'start' | Select-Object -First 1
    $probe = Get-Events $ev 'module_probe' | Select-Object -First 1
    $shown = Get-Events $ev 'web_dock_shown' | Select-Object -First 1
    $loaded = Get-Events $ev 'webengine_loaded' | Select-Object -First 1
    $profile = Get-Events $ev 'web_profile' | Select-Object -First 1
    $loads = @(Get-Events $ev 'web_load')
    $deprec = @(Get-Events $ev 'qt_msg' | Where-Object { $_.msg -match '(?i)deprecat' } | ForEach-Object { $_.msg })
    $qtWarn = @(Get-Events $ev 'qt_msg' | Where-Object { $_.type -ne 'debug' } | ForEach-Object { "[$($_.type)] $($_.msg)" } | Select-Object -First 20)
    $want = ($LocalSpike + '\image\').Replace('\', '/').ToLowerInvariant()
    $under = { param($p) $p -and ("$p".Replace('\', '/').ToLowerInvariant() + '/').StartsWith($want) }
    $storageDir = if ($profile) { "$($profile.persistent_storage_path)".Replace('/', '\') } else { $null }
    $storageFiles = if ($storageDir -and (Test-Path $storageDir)) { @(Get-ChildItem $storageDir -Recurse -File).Count } else { 0 }
    $c = [ordered]@{
        no_webengine_in_dependents = ($webDeps.Count -eq 0) -and ($deps.Count -eq 2) -and (@($deps.Values | Where-Object { $_.Count -eq 0 }).Count -eq 0)
        bridge_has_no_webengine = $bridgeLib -and -not $bridge.webengine_in_directives -and -not $bridge.webengine_symbols
        not_loaded_at_start = ($startEv -and $startEv.webengine_dll_in_process -eq $false)
        not_loaded_until_shown = ($probe -and $probe.probes -gt 0 -and $probe.first_non_null_t_ms -lt 0 -and $shown -and $shown.webengine_dll_in_process -eq $false)
        loaded_after_shown = ($loaded -and $loaded.webengine_dll_in_process -eq $true -and $loaded.t_ms -ge $shown.t_ms)
        persistent_named_profile = ($profile -and $profile.off_the_record -eq $false -and $profile.storage_name -eq 'image')
        storage_under_local_app_dir = ((& $under $profile.persistent_storage_path) -and (& $under $profile.cache_path) -and $storageFiles -gt 0)
        no_deprecation_warning = ($deprec.Count -eq 0)
        about_blank_loads = (@($loads | Where-Object { $_.status -eq 'succeeded' -and $_.url -eq 'about:blank' }).Count -ge 1)
        exit_zero = ($r.run.exit_code -eq 0)
    }
    $pass = @($c.Values | Where-Object { -not $_ }).Count -eq 0
    Set-Check 'S0.3' ($(if ($pass) { 'PASS' } else { 'FAIL' })) ([ordered]@{
        checks = $c; dependents = $deps; bridge = $bridge; spike_web_plugin_dependents = $webPluginDeps
        module_probe = $probe; web_dock_shown_t_ms = $shown.t_ms; webengine_loaded_t_ms = $loaded.t_ms
        profile = $profile; profile_storage_files = $storageFiles; web_loads = $loads; deprecation_messages = $deprec
        qt_messages = $qtWarn; appdata_roaming_spike_folder = (Test-Path (Join-Path $env:APPDATA 'LILITH-spike'))
        focus_taken = ($r.run.foreground_was_ours -gt 0); exit_code = $r.run.exit_code; log = $r.log })
    Save-Results
}

# ==========================================================================================
# S0.4: deploy, sizes, and the stripped-environment runs.
# ==========================================================================================
$SkipPluginTypes = 'designer,generic,help,networkinformation,position,qmllint,qmlls,qmltooling,sqldrivers,webview'
$KddwQml = Join-Path $CmakeDir '_deps\kddockwidgets-src\src\qtquick\views\qml'
$QmlDirs = @((Join-Path $Root 'qml'), (Join-Path $Root 'web'), (Join-Path $Root 'shim'), $KddwQml)
$TrimFlags = @('--no-translations', '--no-opengl-sw', '--no-system-dxc-compiler', '--no-ffmpeg', '--skip-plugin-types', $SkipPluginTypes)
$stripRe = '\\Qt\\|Microsoft Visual Studio|Windows Kits|\\MSVC\\|\\Qt6?$'
$cleanPath = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch $stripRe }) -join ';'
$StripSet = @{ PATH = $cleanPath }
$StripUnset = @('QTDIR', 'QT_PLUGIN_PATH', 'QML_IMPORT_PATH', 'QML2_IMPORT_PATH', 'QT_QPA_PLATFORM_PLUGIN_PATH', 'CMAKE_PREFIX_PATH',
                'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsRedistDir', 'INCLUDE', 'LIB', 'LIBPATH', 'QMAKE')
$dExe = Join-Path $Deploy $ExeName

function Copy-OurFiles([string]$dest) {
    New-Item -ItemType Directory -Force $dest, (Join-Path $dest 'SpikeWeb') | Out-Null
    Copy-Item (Join-Path $BinDir $ExeName), (Join-Path $BinDir 'spike_shell_shim.dll'), (Join-Path $BinDir 'kddockwidgets-qt62.dll') $dest
    Copy-Item (Join-Path $BinDir 'SpikeWeb\qmldir'), (Join-Path $BinDir 'SpikeWeb\spike_web.dll'), (Join-Path $BinDir 'SpikeWeb\spike_web.qmltypes') (Join-Path $dest 'SpikeWeb')
}
# Sizes per configuration. `windeployqt --dry-run --list source` exits 1 once WebEngine is
# involved (it reads QtWebEngineProcess.exe's dependents from the target folder, which a dry run
# never fills), so the dry run's exit and list are recorded, and the size comes from a real
# deploy of the same configuration into a scratch folder (with --list source too).
function Get-DryRunList([string[]]$qmlDirs, [string[]]$flags) {
    $tmp = Join-Path $Build 'sizecheck'
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
    Copy-OurFiles $tmp
    $base = @('--release') + @($qmlDirs | ForEach-Object { '--qmldir', $_ }) + $flags
    $dry = @(& windeployqt @($base + @('--dry-run', '--list', 'source', '--dir', $tmp, (Join-Path $tmp $ExeName))) 2>$null)
    $dryExit = $LASTEXITCODE
    $dryFiles = @($dry | ForEach-Object { "$_".Trim() } | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -Unique)
    $real = @(& windeployqt @($base + @('--list', 'source', '--dir', $tmp, (Join-Path $tmp $ExeName))) 2>$null)
    $realExit = $LASTEXITCODE
    $realFiles = @($real | ForEach-Object { "$_".Trim() } | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Select-Object -Unique)
    Copy-Item (Join-Path $env:VCToolsRedistDir 'x64\Microsoft.VC143.CRT\*.dll') $tmp
    $all = @(Get-ChildItem $tmp -Recurse -File)
    $redist = @($all | Where-Object { $_.Name -like 'vc_redist*' })
    $bytes = ($all | Measure-Object Length -Sum).Sum
    $r = [ordered]@{ dry_run_exit = $dryExit; dry_run_listed = $dryFiles.Count; deploy_exit = $realExit; deploy_listed = $realFiles.Count
        files = $all.Count; mib = [math]::Round($bytes / 1MB, 1)
        mib_without_vc_redist_exe = [math]::Round(($bytes - ($redist | Measure-Object Length -Sum).Sum) / 1MB, 1)
        webengine_files = @($all | Where-Object { $_.FullName -match '(?i)webengine' }).Count
        plugin_dirs = @(Get-ChildItem $tmp -Directory | Where-Object { $_.Name -notin 'qml', 'SpikeWeb', 'resources', 'translations' } | ForEach-Object Name) }
    Remove-Item -Recurse -Force $tmp
    $r
}

if ((Want 'S0.4') -or (Want 'S0.5a') -or (Want 'S0.8')) {
    $sizes = [ordered]@{
        untrimmed = Get-DryRunList $QmlDirs @()
        trimmed = Get-DryRunList $QmlDirs $TrimFlags
        trimmed_without_webengine = Get-DryRunList @($QmlDirs | Where-Object { $_ -notmatch '\\web$' }) $TrimFlags
    }
    if (Test-Path $Deploy) { Remove-Item -Recurse -Force $Deploy }
    Copy-OurFiles $Deploy
    $wdqLog = Join-Path $Build 'windeployqt.log'
    $a = @('--release') + @($QmlDirs | ForEach-Object { '--qmldir', $_ }) + $TrimFlags + @('--dir', $Deploy, $dExe)
    & windeployqt @a *>&1 | Out-File -Encoding utf8 $wdqLog
    $wdqExit = $LASTEXITCODE
    $crtDir = Join-Path $env:VCToolsRedistDir 'x64\Microsoft.VC143.CRT'
    Copy-Item (Join-Path $crtDir '*.dll') $Deploy
    $vcRedistCopied = @(Get-ChildItem $Deploy -Filter 'vc_redist*').Count
    $all = @(Get-ChildItem $Deploy -Recurse -File)
    $deployed = [ordered]@{ files = $all.Count; mib = [math]::Round(($all | Measure-Object Length -Sum).Sum / 1MB, 1)
        top_level_dirs = @(Get-ChildItem $Deploy -Directory | ForEach-Object Name)
        plugin_dirs = @(Get-ChildItem $Deploy -Directory | Where-Object { $_.Name -notin 'qml', 'SpikeWeb', 'resources', 'translations' } | ForEach-Object Name)
        biggest = @($all | Sort-Object Length -Descending | Select-Object -First 8 | ForEach-Object { "{0} {1:N1} MiB" -f $_.FullName.Replace($Deploy + '\', ''), ($_.Length / 1MB) })
        vc_redist_exe_copied = $vcRedistCopied }
    $leaks = @(($cleanPath -split ';') | Where-Object { $_ -and ((Test-Path (Join-Path $_ 'Qt6Core.dll')) -or (Test-Path (Join-Path $_ 'cl.exe'))) })
    $dCycle = Invoke-LayoutCycle $dExe 's04-deploy' $StripSet $StripUnset $true
    $dWeb = Invoke-Spike $dExe 's04-web' @('--show-web', '3000', '--quit-after', '40000') $StripSet $StripUnset 120 $true
    $wev = $dWeb.events
    $wShown = Get-Events $wev 'web_dock_shown' | Select-Object -First 1
    $wLoaded = Get-Events $wev 'webengine_loaded' | Select-Object -First 1
    $wProbe = Get-Events $wev 'module_probe' | Select-Object -First 1
    $webOk = ($dWeb.run.exit_code -eq 0) -and $wProbe -and ($wProbe.first_non_null_t_ms -lt 0) -and $wShown -and (-not $wShown.webengine_dll_in_process) -and
             $wLoaded -and $wLoaded.webengine_dll_in_process -and (@(Get-Events $wev 'web_load' | Where-Object { $_.status -eq 'succeeded' }).Count -ge 1)
    $hard = ($wdqExit -eq 0) -and ($leaks.Count -eq 0) -and $dCycle.pass -and $webOk
    Set-Check 'S0.4' ($(if ($hard) { 'PASS' } else { 'FAIL' })) ([ordered]@{
        windeployqt_exit = $wdqExit
        windeployqt_args = @('--release') + @($QmlDirs | ForEach-Object { '--qmldir', $_.Replace($Root, 'spikes\app-shell').Replace($CmakeDir, '<build>') }) + $TrimFlags
        skip_plugin_types = $SkipPluginTypes
        dry_run_sizes = $sizes; deployed_folder = $deployed
        soft_target_300_mib = ($deployed.mib -le 300)
        stripped_path_dirs_with_qt_or_cl = $leaks
        restore_from_deploy = $dCycle
        lazy_load_from_deploy = [ordered]@{ pass = $webOk; exit_code = $dWeb.run.exit_code; probe = $wProbe
            shown_t_ms = $wShown.t_ms; loaded_t_ms = $wLoaded.t_ms
            web_loads = @(Get-Events $wev 'web_load' | ForEach-Object { "$($_.status) $($_.url)" })
            qt_warnings = @(Get-Events $wev 'qt_msg' | Where-Object { $_.type -ne 'debug' } | ForEach-Object { $_.msg } | Select-Object -First 10)
            log = $dWeb.log } })
    Save-Results
}

# ==========================================================================================
# S0.5 (a): warm starts of the deployed app.
# ==========================================================================================
if (Want 'S0.5a') {
    $starts = @()
    $cpu = @()
    for ($i = 0; $i -le $WarmStarts; $i++) {
        # Measurement condition: wait (up to 3 minutes) for the machine to be idle, so other
        # people's work doesn't decide the start time; the CPU load is recorded either way.
        $waitStart = Get-Date
        $cpuNow = Get-CpuPercent
        while ($cpuNow -ge $BusyCpuPercent -and ((Get-Date) - $waitStart).TotalSeconds -lt 180) { Start-Sleep -Seconds 5; $cpuNow = Get-CpuPercent }
        $r = Invoke-Spike $dExe "s05a-start$i" @('--quit-after', '2000') $StripSet $StripUnset 120 $true
        $fi = Get-Events $r.events 'first_frame' | Select-Object -First 1
        $it = Get-Events $r.events 'interactive' | Select-Object -First 1
        $ql = Get-Events $r.events 'qml_loaded' | Select-Object -First 1
        $st = Get-Events $r.events 'start' | Select-Object -First 1
        $row = [ordered]@{ run = $i; untimed = ($i -eq 0); exit_code = $r.run.exit_code; cpu_percent_before = $cpuNow
                           main_ms = $st.t_ms; qml_loaded_ms = $ql.ms; first_frame_ms = $fi.ms; interactive_ms = $it.ms
                           focus_taken = ($r.run.foreground_was_ours -gt 0) }
        $starts += $row
        if ($i -gt 0) { $cpu += $cpuNow }
    }
    $timed = @($starts | Where-Object { -not $_.untimed })
    $iv = @($timed | ForEach-Object { [double]$_.interactive_ms } | Sort-Object)
    $fv = @($timed | ForEach-Object { [double]$_.first_frame_ms } | Sort-Object)
    $p50 = { param($v) if ($v.Count) { $v[[math]::Max(0, [math]::Ceiling(0.5 * $v.Count) - 1)] } else { $null } }
    $avgCpu = [math]::Round(($cpu | Measure-Object -Average).Average, 1)
    $allLogged = @($timed | Where-Object { $null -eq $_.interactive_ms -or $_.exit_code -ne 0 }).Count -eq 0
    $under = $allLogged -and (@($timed | Where-Object { [double]$_.interactive_ms -ge 2000 }).Count -eq 0)
    $verdict = if ($under) { 'PASS' } elseif ($avgCpu -ge $BusyCpuPercent) { 'INCONCLUSIVE' } else { 'FAIL' }
    Set-Check 'S0.5a' $verdict ([ordered]@{
        interactive_ms = [ordered]@{ n = $iv.Count; p50 = (& $p50 $iv); max = ($iv | Select-Object -Last 1) }
        first_frame_ms = [ordered]@{ n = $fv.Count; p50 = (& $p50 $fv); max = ($fv | Select-Object -Last 1) }
        machine_cpu_percent_avg = $avgCpu; busy_threshold_percent = $BusyCpuPercent
        note = if ($verdict -eq 'INCONCLUSIVE') { 'over 2,000 ms while the machine was busy with other work; rerun when idle (OWNER-STEPS.md)' } else { $null }
        starts = $starts })
    Save-Results
}

# ==========================================================================================
# S0.8: caches.
# ==========================================================================================
if (Want 'S0.8') {
    $after = @(Get-ChildItem $env:LOCALAPPDATA -Directory | ForEach-Object Name)
    $new = @($after | Where-Object { $_ -notin $s08Before })
    $exeNamed = @($new | Where-Object { $_ -in 'app_shell_spike', 'tst_app_shell' })
    $cacheRoot = Join-Path $LocalSpike 'image'
    $qmlcache = @(Get-ChildItem $cacheRoot -Recurse -Directory -Filter 'qmlcache' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName.Replace($env:LOCALAPPDATA, '%LOCALAPPDATA%') })
    $pipeline = @(Get-ChildItem $cacheRoot -Recurse -Directory -Filter 'qtpipelinecache-*' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName.Replace($env:LOCALAPPDATA, '%LOCALAPPDATA%') })
    $qmlFiles = @(Get-ChildItem $cacheRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.Extension -in '.qmlc', '.jsc' }).Count
    $pipeFiles = @(Get-ChildItem $cacheRoot -Recurse -File -ErrorAction SilentlyContinue | Where-Object { $_.DirectoryName -match 'qtpipelinecache' }).Count
    $exeCacheElsewhere = @('app_shell_spike' | Where-Object { Test-Path (Join-Path $env:LOCALAPPDATA $_) })
    $appDataNew = @(Get-ChildItem $env:APPDATA -Directory | ForEach-Object Name | Where-Object { $_ -notin $appDataBefore })
    # The bar is about where the caches go. Qt Quick creates its pipeline-cache folder there; on
    # D3D11 with precompiled shaders it may hold no data, which is recorded, not failed.
    $pass = ($qmlcache.Count -ge 1) -and ($qmlFiles -gt 0) -and ($pipeline.Count -ge 1) -and ($exeNamed.Count -eq 0) -and ($exeCacheElsewhere.Count -eq 0)
    Set-Check 'S0.8' ($(if ($pass) { 'PASS' } else { 'FAIL' })) ([ordered]@{
        qmlcache_dirs = $qmlcache; qmlcache_files = $qmlFiles; pipeline_cache_dirs = $pipeline; pipeline_cache_files = $pipeFiles
        new_localappdata_dirs = $new; exe_named_dirs = @($exeNamed + $exeCacheElsewhere | Select-Object -Unique)
        new_roaming_appdata_dirs = $appDataNew })
    Save-Results
}

# ==========================================================================================
# S0.9 (local): the offscreen Qt Quick Test, item by item, per environment.
# ==========================================================================================
if (Want 'S0.9') {
    $tst = Join-Path $BinDir 'tst_app_shell.exe'
    $items = 'test_kddw_main_and_floating', 'test_shape_polyline', 'test_web_loader', 'test_web_about_blank', 'test_grab_not_flat'
    $envs = [ordered]@{
        'offscreen' = @{ QT_QPA_PLATFORM = 'offscreen' }
        'offscreen+software' = @{ QT_QPA_PLATFORM = 'offscreen'; QT_QUICK_BACKEND = 'software' }
    }
    $table = [ordered]@{}
    foreach ($envName in $envs.Keys) {
        $rows = [ordered]@{}
        foreach ($it in $items) {
            $out = Join-Path $Runs "s09-$($envName -replace '\W', '_')-$it.txt"
            $r = Invoke-App $tst @('-input', (Join-Path $Root 'tests'), "AppShell::$it", '-o', "$out,txt") $envs[$envName] @() 180 $false
            $txt = if (Test-Path $out) { Get-Content $out } else { @() }
            $passed = [bool]($txt -match "^PASS\s+:\s+(\S+::)?AppShell::$it\(\)")
            $rows[$it] = [ordered]@{ pass = $passed; exit_code = $r.exit_code; timed_out = $r.timed_out
                                     detail = @($txt | Where-Object { $_ -match 'FAIL|Actual|Expected|QWARN|QDEBUG|Loc:|warning' } | Select-Object -First 8) }
        }
        $table[$envName] = $rows
    }
    $required = 'test_kddw_main_and_floating', 'test_shape_polyline', 'test_web_loader', 'test_grab_not_flat'
    $envVerdict = [ordered]@{}
    foreach ($e in $table.Keys) { $envVerdict[$e] = @($required | Where-Object { -not $table[$e][$_].pass }).Count -eq 0 }
    $okEnv = @($envVerdict.Keys | Where-Object { $envVerdict[$_] }) | Select-Object -First 1
    Set-Check 'S0.9' ($(if ($okEnv) { 'PASS (local; the CI run is the owner''s)' } else { 'FAIL (local)' })) ([ordered]@{
        environment_needed = $okEnv; per_environment = $envVerdict; items = $table })
    Save-Results
}

# ==========================================================================================
# S0.2: s02-lil-qt-route.ps1, run in a scratch worktree twice: with a dashed package name (the
# workspace's convention, `lil-qt-probe`) and with an underscore one (`lil_qt_probe`), because
# cxx-qt-cmake 0.10.0 and cxx-qt-build disagree on dashed names. Existing results are reused.
# ==========================================================================================
if (Want 'S0.2') {
    $variants = [ordered]@{ dashed = 'lil-qt-probe'; underscore = 'lil_qt_probe' }
    $r02 = [ordered]@{}
    foreach ($k in $variants.Keys) {
        $f = Join-Path $Results "s02-$k.json"
        if (-not (Test-Path $f)) { & pwsh -NoProfile -File (Join-Path $Root 's02-lil-qt-route.ps1') -Pkg $variants[$k] -Out $f }
        $r02[$k] = Get-Content $f -Raw | ConvertFrom-Json
    }
    $verdict = if ($r02.dashed.verdict -eq 'PASS') { 'PASS' }
               elseif ($r02.underscore.verdict -eq 'PASS') { 'PASS (with an underscore package name; a dashed one fails to link)' }
               else { 'FAIL' }
    Set-Check 'S0.2' $verdict $r02
    Save-Results
}

Save-Results
$failed = @($checks.Values | Where-Object { $_.verdict -notmatch '^PASS' })
Write-Host ("done in {0:N1} min; not passing: {1}" -f ((Get-Date) - $started).TotalMinutes, (($failed | ForEach-Object check) -join ', '))
