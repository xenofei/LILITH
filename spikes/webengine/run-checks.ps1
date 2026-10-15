<#
.SYNOPSIS
  Builds the webengine spike and runs its checks (Task 6, criteria 1-7). Writes
  spikes/webengine/results/webengine.json.

.DESCRIPTION
  Phases (each one is a separate run of webengine.exe):
    hidden   the visible view stays on about:blank; the hidden off-the-record page converts the two
             fixtures 10 times (alternating). ComfyUI's user\default folder is snapshotted before and
             after this run only (criterion 4), so nothing else touches ComfyUI in between.
    visible  the visible view (named persistent profile under build\profile) loads ComfyUI; each
             fixture is stamped (extra.worldwright), loaded with loadGraph, read back with readGraph
             and exported with the frontend's own "Export (API)" command (download blocked, the blob
             captured in the page); one plain "Export" too; then off-origin navigation attempts.
    down     ComfyUI stopped: loadGraph must end in error:unreachable or a logged 5 s timeout, and the
             app must stay responsive.
    firstboot (informational) a scratch ComfyUI on 127.0.0.1:8189 with its own empty
             --user-directory / --temp-directory under build\scratch-comfy; two app runs with one
             hidden conversion each; records what a first frontend boot writes. Stopped afterwards.
  Controls (details.controls): the snapshot diff detects an edit and an added file in a scratch
  folder; the real user\ folder is identical from before any ComfyUI start to the end.
  Criterion evaluation:
    1  loadGraph: flag "ok" within 3 s; one more workflow tab; every tab open before is still open
       and the previously active tab's nodes and links are unchanged; PageDriver's load_graph_call
       line shows script_ok/sent and base64_chars == base64 length of the stamped fixture bytes.
       Negative probe: 5 hostile arguments (quote, backslash, '<', raw JSON, trailing newline) are
       each refused ("argument rejected", nothing sent, no flag) and the tabs stay unchanged.
    2  readGraph: node ids (top level and subgraphs) and links equal the stamped fixture, and
       extra.worldwright survives.
    3  hidden output == the visible tab's Export (API) for both fixtures (JSON with sorted keys).
    4  user\default snapshot (file list, sha256, comfy.settings.json content) identical before and
       after the hidden phase. Recorded either way (decision input for IMG-124).
    5  every export download is cancelled in downloadRequested, downloadAttempts counts them, no new
       file in the user's Downloads folder, and the profile's own download folder stays empty.
    6  https://example.com refused in the visible view (navigationRequested) and in the hidden page
       (acceptNavigationRequest override); both stay on 127.0.0.1:8188; window.open refused too.
    7  ComfyUI down: flag error:unreachable, or no flag and a load_graph_timeout log line after 5 s;
       heartbeats keep coming; exit code 0.

  ComfyUI is started on 127.0.0.1:8188 only if it is not already running, and only a process this
  script started is stopped. If ComfyUI was already running, the down phase is not run
  (criterion 7: not_verified).

.EXAMPLE
  pwsh -File spikes/webengine/run-checks.ps1
#>
param(
    [string]$ComfyPython  = (Join-Path $env:USERPROFILE 'ComfyUI\.venv\Scripts\python.exe'),
    [string]$ComfyMain    = (Join-Path $env:USERPROFILE 'ComfyUI\ComfyUI\main.py'),
    # The brief names %USERPROFILE%\ComfyUI\user\default; ComfyUI's root is ComfyUI\ComfyUI, so the
    # real folder is the one below. Both are recorded in the results.
    [string]$ComfyUserDir = (Join-Path $env:USERPROFILE 'ComfyUI\ComfyUI\user'),
    [string]$BriefUserDefault = (Join-Path $env:USERPROFILE 'ComfyUI\user\default'),
    [int]$Conversions = 10,
    [switch]$NoBuild,
    [switch]$Clean
)
$ErrorActionPreference = 'Stop'

$Root     = $PSScriptRoot
$Repo     = (Resolve-Path (Join-Path $Root '..\..')).Path
$Build    = Join-Path $Root 'build'
$BinDir   = Join-Path $Build 'cmake\bin'
$Runs     = Join-Path $Build 'runs'
$Results  = Join-Path $Root 'results'
$Fixtures = Join-Path $Root 'fixtures'
$ProfileDir  = Join-Path $Build 'profile'
$ExeName  = 'webengine.exe'
$ComfyUrl = 'http://127.0.0.1:8188'
$Downloads = Join-Path $env:USERPROFILE 'Downloads'
$FixtureNames = @('zimage_t2i', 'subgraph_muted')

. (Join-Path $Repo 'spikes\toolchain\env.ps1')
New-Item -ItemType Directory -Force $Build, $Runs, $Results | Out-Null

$checks = [ordered]@{}
$extra = [ordered]@{}
function Set-Check([int]$n, $pass, [System.Collections.IDictionary]$detail) {
    $o = [ordered]@{ criterion = $n; pass = $pass }
    foreach ($k in $detail.Keys) { $o[$k] = $detail[$k] }
    $script:checks["$n"] = $o
    $tag = if ($pass -eq $true) { 'PASS' } elseif ($pass -eq $false) { 'FAIL' } else { "$pass" }
    Write-Host "criterion $n : $tag"
}

function Write-Results {
    $all = foreach ($n in 1..7) {
        if ($checks.Contains("$n")) { $checks["$n"] }
        else { [ordered]@{ criterion = $n; pass = $false; not_run = $true } }
    }
    $doc = [ordered]@{
        generated  = (Get-Date).ToString('o')
        qt_dir     = $env:QTDIR
        qt_version = (& qmake -query QT_VERSION 2>$null)
        checks     = @($all)
        details    = $extra
    }
    $doc | ConvertTo-Json -Depth 40 | Set-Content -Encoding utf8 (Join-Path $Results 'webengine.json')
    # Criterion 4 is a decision input: it is recorded but does not fail the run.
    $failed = @($all | Where-Object { $_.criterion -ne 4 -and $_.pass -ne $true } | ForEach-Object { $_.criterion })
    if ($failed.Count) { Write-Host "FAILED criteria: $($failed -join ', ')"; exit 1 }
    Write-Host 'ALL CRITERIA PASSED (criterion 4 recorded as a decision input)'; exit 0
}

function Read-Log([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content $path -Encoding utf8 | Where-Object { $_.Trim() } | ForEach-Object {
        try { $_ | ConvertFrom-Json -Depth 60 } catch { $null }
    } | Where-Object { $_ })
}
function Get-Events($log, [string]$name) { @($log | Where-Object { $_.event -eq $name }) }

function Test-Comfy {
    try { (Invoke-WebRequest "$ComfyUrl/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 }
    catch { $false }
}

# Starts the app, polls Process.Responding once a second, kills it after $TimeoutSec.
function Invoke-App([string]$exe, [string[]]$appArgs, [int]$TimeoutSec = 240) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = Split-Path $exe
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

# Every folder this script deletes must be inside the spike's own build folder.
function Assert-UnderBuild([string]$path) {
    $full = [System.IO.Path]::GetFullPath($path)
    $base = [System.IO.Path]::GetFullPath($Build).TrimEnd('\') + '\'
    if (-not $full.StartsWith($base, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to delete '$full': it is not under '$base'"
    }
}
function Remove-BuildDir([string]$path) {
    Assert-UnderBuild $path
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force }
}

function Invoke-Phase([string]$phase, [string[]]$more = @(), [string]$tag = $phase) {
    $log = Join-Path $Runs "$tag.jsonl"
    $out = Join-Path $Runs $tag
    Remove-Item $log -ErrorAction SilentlyContinue
    Remove-BuildDir $out
    New-Item -ItemType Directory -Force $out | Out-Null
    $shot = Join-Path $Runs "$tag.png"
    $appArgs = @('--log', $log, '--phase', $phase, '--fixtures', $Fixtures, '--out', $out,
                 '--profile-dir', $ProfileDir, '--screenshot', $shot, '--heartbeat', '250') + $more
    $run = Invoke-App (Join-Path $BinDir $ExeName) $appArgs
    [ordered]@{ log = $log; out = $out; screenshot = $shot; run = $run; events = (Read-Log $log) }
}

# --- snapshots ------------------------------------------------------------------------------
function Get-DirSnapshot([string]$dir) {
    if (-not (Test-Path $dir)) { return [ordered]@{ path = $dir; exists = $false; files = @() } }
    $files = @(Get-ChildItem -LiteralPath $dir -Recurse -File -Force | Sort-Object FullName | ForEach-Object {
        [ordered]@{ path = $_.FullName.Substring($dir.Length).TrimStart('\')
                    size = $_.Length
                    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
                    mtime_utc = $_.LastWriteTimeUtc.ToString('o') }
    })
    $settings = Join-Path $dir 'comfy.settings.json'
    [ordered]@{ path = $dir; exists = $true; taken = (Get-Date).ToString('o'); files = $files
                comfy_settings = if (Test-Path $settings) { Get-Content -Raw -Encoding utf8 $settings } else { $null } }
}
function Compare-Snapshots($a, $b) {
    $ia = @{}; foreach ($f in $a.files) { $ia[$f.path] = $f }
    $ib = @{}; foreach ($f in $b.files) { $ib[$f.path] = $f }
    $added = @($ib.Keys | Where-Object { -not $ia.ContainsKey($_) } | Sort-Object)
    $removed = @($ia.Keys | Where-Object { -not $ib.ContainsKey($_) } | Sort-Object)
    $changed = @($ia.Keys | Where-Object { $ib.ContainsKey($_) -and ($ia[$_].sha256 -ne $ib[$_].sha256 -or $ia[$_].size -ne $ib[$_].size) } | Sort-Object |
        ForEach-Object { [ordered]@{ path = $_; before = $ia[$_]; after = $ib[$_] } })
    $touched = @($ia.Keys | Where-Object { $ib.ContainsKey($_) -and $ia[$_].sha256 -eq $ib[$_].sha256 -and $ia[$_].mtime_utc -ne $ib[$_].mtime_utc } | Sort-Object)
    [ordered]@{ identical = ($added.Count + $removed.Count + $changed.Count) -eq 0 -and $a.comfy_settings -eq $b.comfy_settings
                added = $added; removed = $removed; changed = $changed; mtime_only = $touched
                settings_equal = $a.comfy_settings -eq $b.comfy_settings }
}
# Downloads: only the names of files that are new (or rewritten) during the test are recorded.
function Get-DownloadsIndex {
    $h = @{}
    if (Test-Path $Downloads) {
        Get-ChildItem -LiteralPath $Downloads -Force -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object {
            $h[$_.FullName] = "$($_.Length)|$($_.LastWriteTimeUtc.Ticks)" }
    }
    $h
}
function Get-NewDownloads($before, $after) {
    @($after.Keys | Where-Object { -not $before.ContainsKey($_) -or $before[$_] -ne $after[$_] } |
        ForEach-Object { $_.Substring($Downloads.Length).TrimStart('\') } | Sort-Object)
}

# --- canonical JSON (sorted keys) -------------------------------------------------------------
function ConvertTo-Sorted($v) {
    if ($v -is [System.Collections.IDictionary]) {
        $o = [ordered]@{}
        foreach ($k in ($v.Keys | Sort-Object -CaseSensitive)) { $o[$k] = ConvertTo-Sorted $v[$k] }
        return $o
    }
    if ($v -is [System.Collections.IList] -and $v -isnot [string]) { return , @($v | ForEach-Object { ConvertTo-Sorted $_ }) }
    return $v
}
function Get-CanonicalJson([string]$text) {
    if (-not $text) { return $null }
    (ConvertTo-Sorted ($text | ConvertFrom-Json -AsHashtable -Depth 100)) | ConvertTo-Json -Compress -Depth 100
}
function Get-TextSha([string]$text) {
    if ($null -eq $text) { return $null }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    ([System.BitConverter]::ToString($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($text)))).Replace('-', '')
}

# --- graph structure (criterion 2) ------------------------------------------------------------
function Get-GraphShape($g) {
    $linkKey = { param($l)
        if ($l -is [System.Collections.IList]) { ($l[0..5] | ForEach-Object { "$_" }) -join ',' }
        else { (@($l.id, $l.origin_id, $l.origin_slot, $l.target_id, $l.target_slot, $l.type) | ForEach-Object { "$_" }) -join ',' } }
    $shape = [ordered]@{
        nodes = @($g.nodes | ForEach-Object { [string]$_.id } | Sort-Object)
        links = @($g.links | ForEach-Object { & $linkKey $_ } | Sort-Object)
        subgraphs = [ordered]@{}
    }
    foreach ($s in @($g.definitions.subgraphs)) {
        if (-not $s) { continue }
        $shape.subgraphs[[string]$s.id] = [ordered]@{
            nodes = @($s.nodes | ForEach-Object { [string]$_.id } | Sort-Object)
            links = @($s.links | ForEach-Object { & $linkKey $_ } | Sort-Object) }
    }
    $shape
}

# ==============================================================================================
# Build (part of criterion 1: no PageDriver, no criterion).
# ==============================================================================================
$cmakeLists = Join-Path $Root 'CMakeLists.txt'
$driverSrc = Join-Path $Root 'shim\PageDriver.cpp'
if (-not (Test-Path $cmakeLists) -or -not (Test-Path $driverSrc)) {
    Set-Check 1 $false @{ reason = "no PageDriver: $driverSrc / $cmakeLists not present" }
    Write-Results
}
$cmakeBuild = Join-Path $Build 'cmake'
if ($Clean) { Remove-BuildDir $cmakeBuild }
$buildInfo = [ordered]@{}
if (-not $NoBuild) {
    & cmake -S $Root -B $cmakeBuild -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo *>&1 | Tee-Object -FilePath (Join-Path $Build 'configure.log') | Out-Null
    $buildInfo.configure_exit = $LASTEXITCODE
    if ($buildInfo.configure_exit -eq 0) {
        & cmake --build $cmakeBuild *>&1 | Tee-Object -FilePath (Join-Path $Build 'build.log') | Out-Null
        $buildInfo.build_exit = $LASTEXITCODE
    }
    $buildInfo.warning_lines = @(Get-Content (Join-Path $Build 'build.log') -ErrorAction SilentlyContinue | Where-Object { $_ -match '\bwarning\b' } | Select-Object -Unique).Count
}
$extra.build = $buildInfo
if (-not (Test-Path (Join-Path $BinDir $ExeName))) {
    Set-Check 1 $false @{ reason = 'build failed: webengine.exe missing (no PageDriver)'; build = $buildInfo }
    Write-Results
}

# A fresh persistent profile per run (it is the spike's own folder), so the visible phase always
# starts from the frontend's default tab.
Remove-BuildDir $ProfileDir

$extra.paths = [ordered]@{ comfy_user_dir = $ComfyUserDir; brief_user_default = $BriefUserDefault
                           brief_user_default_exists = (Test-Path $BriefUserDefault)
                           user_default = (Join-Path $ComfyUserDir 'default'); profile_dir = $ProfileDir }

# ==============================================================================================
# ComfyUI up: hidden phase (criterion 4 brackets only this run), then the visible phase.
# ==============================================================================================
$snapRealStart = Get-DirSnapshot $ComfyUserDir
$started = $null
$comfyWasUp = Test-Comfy
if (-not $comfyWasUp) {
    Write-Host 'starting ComfyUI on 127.0.0.1:8188 ...'
    $started = Start-Process -FilePath $ComfyPython -ArgumentList @($ComfyMain, '--listen', '127.0.0.1', '--port', '8188') `
        -WorkingDirectory (Split-Path $ComfyMain) -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Runs 'comfy.out.log') -RedirectStandardError (Join-Path $Runs 'comfy.err.log')
    $deadline = (Get-Date).AddSeconds(240)
    while (-not (Test-Comfy) -and (Get-Date) -lt $deadline -and -not $started.HasExited) { Start-Sleep -Seconds 2 }
}
$extra.comfy = [ordered]@{ was_running = $comfyWasUp; started_by_script = [bool]$started }
$hidden = $null; $visible = $null
$userDefault = Join-Path $ComfyUserDir 'default'
try {
    if (-not (Test-Comfy)) { throw 'ComfyUI did not answer /system_stats on 127.0.0.1:8188' }
    $extra.comfy.system_stats = (Invoke-RestMethod "$ComfyUrl/system_stats" -TimeoutSec 5).system | Select-Object comfyui_version, python_version, pytorch_version

    $snap0 = Get-DirSnapshot $userDefault
    $snapAll0 = Get-DirSnapshot $ComfyUserDir
    $dl0 = Get-DownloadsIndex
    Write-Host "phase hidden ($Conversions conversions) ..."
    $hidden = Invoke-Phase 'hidden' @('--conversions', "$Conversions")
    $snap1 = Get-DirSnapshot $userDefault
    $snapAll1 = Get-DirSnapshot $ComfyUserDir
    Write-Host 'phase visible ...'
    $visible = Invoke-Phase 'visible'
    $snap2 = Get-DirSnapshot $userDefault
    $snapAll2 = Get-DirSnapshot $ComfyUserDir
    $dl1 = Get-DownloadsIndex
} finally {
    if ($started -and -not $started.HasExited) {
        & taskkill /PID $started.Id /T /F *>&1 | Out-Null
        Start-Sleep -Seconds 2
        Write-Host 'stopped the ComfyUI process this script started'
    }
}

# ==============================================================================================
# ComfyUI down: criterion 7.
# ==============================================================================================
$down = $null
if ($comfyWasUp) {
    Set-Check 7 'not_verified' @{ reason = 'ComfyUI was already running when the script started; it was not started by this script, so it was left alone' }
} elseif (Test-Comfy) {
    Set-Check 7 'not_verified' @{ reason = 'ComfyUI still answers after stopping the process this script started' }
} else {
    Write-Host 'phase down ...'
    $down = Invoke-Phase 'down'
}

# ==============================================================================================
# Evaluation.
# ==============================================================================================
$hev = $hidden.events; $vev = $visible.events
$extra.runs = [ordered]@{
    hidden = [ordered]@{ log = $hidden.log; exit_code = $hidden.run.exit_code; timed_out = $hidden.run.timed_out; wall_ms = $hidden.run.wall_ms }
    visible = [ordered]@{ log = $visible.log; exit_code = $visible.run.exit_code; timed_out = $visible.run.timed_out; wall_ms = $visible.run.wall_ms }
}
$extra.qt_warnings = [ordered]@{
    hidden = @(Get-Events $hev 'qt_msg' | ForEach-Object { "$($_.type) $($_.category): $($_.msg)" } | Select-Object -First 20)
    visible = @(Get-Events $vev 'qt_msg' | ForEach-Object { "$($_.type) $($_.category): $($_.msg)" } | Select-Object -First 20) }
$extra.page_console = [ordered]@{
    hidden = @(Get-Events $hev 'hidden_console' | ForEach-Object { "$($_.level): $($_.message)" } | Select-Object -First 30) }

# --- criterion 1 + 2 ---------------------------------------------------------------------------
$loads = Get-Events $vev 'load_graph'
$calls = Get-Events $vev 'load_graph_call'
$c1rows = @(); $c2rows = @()
foreach ($name in $FixtureNames) {
    $ev = $loads | Where-Object { $_.fixture -eq $name } | Select-Object -First 1
    if (-not $ev) { $c1rows += [ordered]@{ fixture = $name; pass = $false; reason = 'no load_graph event' }; $c2rows += [ordered]@{ fixture = $name; pass = $false; reason = 'no load_graph event' }; continue }
    $b = $ev.tabs_before; $a = $ev.tabs_after
    $beforePaths = @($b.open | ForEach-Object { $_.path })
    $afterPaths = @($a.open | ForEach-Object { $_.path })
    $prevActive = $b.active
    $prevBefore = $b.open | Where-Object { $_.path -eq $prevActive } | Select-Object -First 1
    $prevAfter = $a.open | Where-Object { $_.path -eq $prevActive } | Select-Object -First 1
    $kept = @($beforePaths | Where-Object { $afterPaths -notcontains $_ }).Count -eq 0
    $newTab = ($afterPaths.Count -eq $beforePaths.Count + 1) -and ($a.active -ne $prevActive) -and ($beforePaths -notcontains $a.active)
    $prevIntact = $prevBefore -and $prevAfter -and ($prevBefore.shape_hash -eq $prevAfter.shape_hash)
    # The call as PageDriver recorded it: the script was composed from the base64 argument and sent,
    # and the payload length equals the base64 of the stamped fixture bytes (computed here).
    $call = $calls | Where-Object { $_.request_id -eq $ev.request_id } | Select-Object -First 1
    $stampedFile = Join-Path $visible.out "$name.stamped.json"
    $expectedB64 = if (Test-Path $stampedFile) { [Convert]::ToBase64String([System.IO.File]::ReadAllBytes($stampedFile)).Length } else { -1 }
    $sentAsB64 = [bool]($call -and $call.script_ok -and $call.sent -and ($call.base64_chars -eq $ev.base64_chars) -and ($call.base64_chars -eq $expectedB64))
    $ok1 = ($ev.flag -eq 'ok') -and ($ev.flag_ms -le 3000) -and $kept -and $newTab -and $prevIntact -and $sentAsB64
    $c1rows += [ordered]@{ fixture = $name; pass = [bool]$ok1; request_id = $ev.request_id; flag = $ev.flag; flag_ms = $ev.flag_ms
                           tabs_before = $beforePaths; tabs_after = $afterPaths; active_before = $prevActive; active_after = $a.active
                           previous_tab_kept = $kept; new_tab = $newTab; previous_tab_shape_unchanged = [bool]$prevIntact
                           sent_as_base64 = $sentAsB64; load_graph_call = $call; expected_base64_chars = $expectedB64 }

    $stampedPath = Join-Path $visible.out "$name.stamped.json"
    $readPath = Join-Path $visible.out "$name.readgraph.json"
    if (-not (Test-Path $stampedPath) -or -not (Test-Path $readPath) -or (Get-Item $readPath).Length -eq 0) {
        $c2rows += [ordered]@{ fixture = $name; pass = $false; reason = 'stamped fixture or readGraph output missing' }; continue
    }
    $fx = Get-Content -Raw -Encoding utf8 $stampedPath | ConvertFrom-Json -Depth 100
    $rg = Get-Content -Raw -Encoding utf8 $readPath | ConvertFrom-Json -Depth 100
    $sf = Get-GraphShape $fx; $sr = Get-GraphShape $rg
    $jf = $sf | ConvertTo-Json -Depth 10 -Compress; $jr = $sr | ConvertTo-Json -Depth 10 -Compress
    $stampF = $fx.extra.worldwright | ConvertTo-Json -Compress; $stampR = $rg.extra.worldwright | ConvertTo-Json -Compress
    $ok2 = ($jf -eq $jr) -and ($null -ne $rg.extra.worldwright) -and ($stampF -eq $stampR)
    $c2rows += [ordered]@{ fixture = $name; pass = [bool]$ok2; shape_equal = ($jf -eq $jr); stamp_equal = ($stampF -eq $stampR)
                           stamp_sent = $fx.extra.worldwright; stamp_read = $rg.extra.worldwright
                           fixture_shape = $sf; readgraph_shape = if ($jf -eq $jr) { 'same as fixture_shape' } else { $sr }
                           readgraph_bytes = (Get-Item $readPath).Length }
}
# Negative probe: hostile loadGraph arguments must be rejected before any script is built
# (driver_error "argument rejected" written immediately before the load_graph_call line, nothing
# sent, no flag in the page), and the page's tabs must be unchanged.
$probe = Get-Events $vev 'hostile_probe' | Select-Object -First 1
$probeRows = @()
foreach ($case in @($probe.cases)) {
    $idx = -1
    for ($i = 0; $i -lt $vev.Count; $i++) { if ($vev[$i].event -eq 'load_graph_call' -and $vev[$i].request_id -eq $case.request_id) { $idx = $i; break } }
    $call = if ($idx -ge 0) { $vev[$idx] } else { $null }
    $prev = if ($idx -gt 0) { $vev[$idx - 1] } else { $null }
    $rejected = [bool]($prev -and $prev.event -eq 'driver_error' -and $prev.what -eq 'argument rejected' -and $prev.script -eq 'load_graph.js')
    $okCase = [bool]($call -and (-not $call.script_ok) -and (-not $call.sent) -and $rejected -and ("$($case.flag_after)" -eq ''))
    $probeRows += [ordered]@{ case = $case.name; pass = $okCase; request_id = $case.request_id; chars = $case.chars
                              argument_rejected_logged = $rejected; script_ok = $call.script_ok; sent = $call.sent; flag_after = $case.flag_after }
}
$probeOk = [bool]($probe -and ($probeRows.Count -eq 5) -and (@($probeRows | Where-Object { -not $_.pass }).Count -eq 0) -and $probe.tabs_unchanged)
$c1ok = ($c1rows.Count -eq 2) -and (@($c1rows | Where-Object { -not $_.pass }).Count -eq 0) -and $probeOk
Set-Check 1 ([bool]$c1ok) ([ordered]@{ fixtures = $c1rows
    hostile_probe = [ordered]@{ pass = $probeOk; cases = $probeRows; tabs_unchanged = $probe.tabs_unchanged
                                tabs_before = @($probe.tabs_before.open | ForEach-Object path); tabs_after = @($probe.tabs_after.open | ForEach-Object path) }
    run_exit_code = $visible.run.exit_code; view_ready = (Get-Events $vev 'view_ready' | Select-Object -First 1) })
Set-Check 2 ([bool](($c2rows.Count -eq 2) -and @($c2rows | Where-Object { -not $_.pass }).Count -eq 0)) @{ fixtures = $c2rows }

# --- criterion 3 --------------------------------------------------------------------------------
$convs = Get-Events $hev 'convert_hidden'
$c3rows = @()
foreach ($name in $FixtureNames) {
    $mine = @($convs | Where-Object { $_.fixture -eq $name })
    $hashes = @($mine | ForEach-Object { $_.canonical_sha256 } | Select-Object -Unique)
    $hiddenFile = Join-Path $hidden.out "$name.hidden.api.json"
    $exportFile = Join-Path $visible.out "$name.export_api.json"
    $hText = if (Test-Path $hiddenFile) { Get-Content -Raw -Encoding utf8 $hiddenFile } else { $null }
    $eText = if (Test-Path $exportFile) { Get-Content -Raw -Encoding utf8 $exportFile } else { $null }
    $hc = Get-CanonicalJson $hText; $ec = Get-CanonicalJson $eText
    # Informational: Task 4's API workflow (same template, converted in the browser pane).
    $t4 = $null
    if ($name -eq 'zimage_t2i') {
        $t4path = Join-Path $Repo 'spikes\comfy-measure\workflows\zimage_t2i.json'
        if (Test-Path $t4path) { $t4 = ((Get-CanonicalJson (Get-Content -Raw -Encoding utf8 $t4path)) -eq $hc) }
    }
    $ok3 = ($null -ne $hc) -and ($hc -eq $ec) -and ($mine.Count -ge 1) -and (@($mine | Where-Object { -not $_.ok }).Count -eq 0)
    $c3rows += [ordered]@{ fixture = $name; pass = [bool]$ok3; conversions = $mine.Count
                           conversions_ok = @($mine | Where-Object { $_.ok }).Count
                           conversion_ms = @($mine | ForEach-Object { $_.ms })
                           distinct_outputs = $hashes.Count
                           hidden_sha256_sorted = (Get-TextSha $hc); export_api_sha256_sorted = (Get-TextSha $ec)
                           equal_after_sorting_keys = ($null -ne $hc) -and ($hc -eq $ec)
                           api_node_ids = if ($hText) { @(($hText | ConvertFrom-Json -AsHashtable).Keys | Sort-Object) } else { @() }
                           equals_task4_zimage_t2i_json = $t4 }
}
$hiddenReady = Get-Events $hev 'hidden_page_ready' | Select-Object -First 1
Set-Check 3 ([bool](($c3rows.Count -eq 2) -and @($c3rows | Where-Object { -not $_.pass }).Count -eq 0) -and ($hidden.run.exit_code -eq 0)) @{
    fixtures = $c3rows; hidden_page_ready = $hiddenReady
    hidden_profile = (Get-Events $hev 'hidden_page_created' | Select-Object -First 1)
    hidden_run_exit_code = $hidden.run.exit_code }

# --- criterion 4 --------------------------------------------------------------------------------
$d01 = Compare-Snapshots $snap0 $snap1
$d12 = Compare-Snapshots $snap1 $snap2
$dAll01 = Compare-Snapshots $snapAll0 $snapAll1
$dAll12 = Compare-Snapshots $snapAll1 $snapAll2
$nconv = @($convs | Where-Object { $_.ok }).Count
Set-Check 4 ([bool]($d01.identical -and $nconv -ge $Conversions)) ([ordered]@{
    snapshot_dir = $userDefault; hidden_conversions_ok = $nconv
    before_hidden = $snap0; after_hidden = $snap1; diff_hidden = $d01
    user_folder_diff_hidden = $dAll01
    visible_phase_diff = $d12; visible_phase_user_folder_diff = $dAll12; after_visible = $snap2
    hidden_while_visible = (Get-Events $vev 'hidden_while_visible' | Select-Object -First 1 | ForEach-Object {
        [ordered]@{ ok = $_.ok; view_tabs_unchanged = $_.view_tabs_unchanged
                    tabs_before = @($_.tabs_before.open | ForEach-Object path); tabs_after = @($_.tabs_after.open | ForEach-Object path)
                    active_before = $_.tabs_before.active; active_after = $_.tabs_after.active } }) })

# --- criterion 5 --------------------------------------------------------------------------------
$newDl = @(Get-NewDownloads $dl0 $dl1)
$blocked = Get-Events $vev 'download_blocked'
$exports = Get-Events $vev 'export'
$profileDl = Join-Path $ProfileDir 'downloads'
$profileDlFiles = @(if (Test-Path $profileDl) { Get-ChildItem -LiteralPath $profileDl -Recurse -File | ForEach-Object Name })
$final = Get-Events $vev 'download_attempts_final' | Select-Object -Last 1
$expected = @($exports).Count
$ok5 = ($expected -ge 3) -and ($blocked.Count -eq $expected) -and (@($blocked | Where-Object { $_.state_after_cancel -ne 'cancelled' }).Count -eq 0) -and
       ($final.download_attempts -eq $expected) -and
       (@($exports | Where-Object { $_.attempts_after -ne $_.attempts_before + 1 }).Count -eq 0) -and
       ($newDl.Count -eq 0) -and ($profileDlFiles.Count -eq 0)
Set-Check 5 ([bool]$ok5) ([ordered]@{
    exports_triggered = @($exports | ForEach-Object { [ordered]@{ fixture = $_.fixture; command = $_.command; flag = $_.flag; attempts_before = $_.attempts_before; attempts_after = $_.attempts_after; bytes = $_.bytes } })
    downloads_blocked = @($blocked | ForEach-Object { [ordered]@{ t_ms = $_.t_ms; profile = $_.profile; suggested_file_name = $_.suggested_file_name; mime_type = $_.mime_type; url_scheme = $_.url_scheme; state_after_cancel = $_.state_after_cancel } })
    download_attempts_final = $final.download_attempts
    new_files_in_user_downloads = $newDl
    user_downloads_files_before = $dl0.Count; user_downloads_files_after = $dl1.Count
    profile_download_dir_files = $profileDlFiles })

# --- criterion 6 --------------------------------------------------------------------------------
$navV = Get-Events $vev 'nav_test' | Select-Object -Last 1
$navH = Get-Events $hev 'nav_test_hidden' | Select-Object -Last 1
$refV = @(Get-Events $vev 'navigation' | Where-Object { $_.url -like 'https://example.com*' })
$refH = @(Get-Events $hev 'hidden_navigation' | Where-Object { $_.url -like 'https://example.com*' })
$winV = @(Get-Events $vev 'new_window' | Where-Object { $_.url -like 'https://example.com*' })
$onOrigin = { param($u) "$u" -like "$ComfyUrl/*" -or "$u" -eq $ComfyUrl }
$ok6 = $navV -and $navH -and ($refV.Count -ge 1) -and (@($refV | Where-Object { $_.allowed }).Count -eq 0) -and
       ($refH.Count -ge 1) -and (@($refH | Where-Object { $_.allowed }).Count -eq 0) -and
       (& $onOrigin $navV.view_url_after) -and ($navV.page_origin_after -eq $ComfyUrl) -and $navV.app_alive_after -and
       (& $onOrigin $navH.url_after) -and ($winV.Count -ge 1) -and (@($winV | Where-Object { $_.opened }).Count -eq 0)
Set-Check 6 ([bool]$ok6) ([ordered]@{ visible = $navV; visible_navigation_events = $refV; visible_new_window_events = $winV
                                     hidden = $navH; hidden_navigation_events = $refH })

# --- criterion 7 --------------------------------------------------------------------------------
if ($down) {
    $dev = $down.events
    $dl = Get-Events $dev 'load_graph_down' | Select-Object -Last 1
    $to = Get-Events $dev 'load_graph_timeout' | Select-Object -Last 1
    $beats = @(Get-Events $dev 'heartbeat' | ForEach-Object { [long]$_.t_ms })
    $afterT = if ($dl) { [long]$dl.t_ms - [long]$dl.waited_ms } else { 0 }
    $beatsAfter = @($beats | Where-Object { $_ -ge $afterT })
    $maxGap = 0; for ($i = 1; $i -lt $beatsAfter.Count; $i++) { $maxGap = [Math]::Max($maxGap, $beatsAfter[$i] - $beatsAfter[$i - 1]) }
    $notResp = @($down.run.polls | Where-Object { -not $_.responding }).Count
    $flagOk = $dl -and (($dl.flag -eq 'error:unreachable') -or (($dl.flag -eq '' -or $null -eq $dl.flag) -and $to -and $dl.waited_ms -ge 5000 -and $dl.waited_ms -lt 6500))
    $ok7 = $flagOk -and ($down.run.exit_code -eq 0) -and (-not $down.run.timed_out) -and ($beatsAfter.Count -ge 10) -and ($maxGap -lt 1000) -and ($notResp -eq 0)
    Set-Check 7 ([bool]$ok7) ([ordered]@{ load_graph = $dl; timeout_line = $to; view_loads = @(Get-Events $dev 'web_load' | ForEach-Object { "$($_.status) $($_.url) $($_.error_string)".Trim() })
                                         heartbeats_after_call = $beatsAfter.Count; max_heartbeat_gap_ms = $maxGap; not_responding_polls = $notResp
                                         exit_code = $down.run.exit_code; timed_out = $down.run.timed_out; wall_ms = $down.run.wall_ms; log = $down.log })
}

# ==============================================================================================
# First boot against a scratch ComfyUI (informational, the basis of the IMG-124 caveat): its own
# --user-directory and --temp-directory under build\scratch-comfy, port 8189 on loopback. Two app
# runs, one hidden conversion each; the scratch user folder is snapshotted before ComfyUI starts,
# once it is up, and after each run. The real user folder is not involved; the process is stopped.
# ==============================================================================================
$ScratchUrl = 'http://127.0.0.1:8189'
$scratchRoot = Join-Path $Build 'scratch-comfy'
$scratchUser = Join-Path $scratchRoot 'user'
$scratchTemp = Join-Path $scratchRoot 'temp'
function Test-Scratch { try { (Invoke-WebRequest "$ScratchUrl/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 } catch { $false } }
function Get-SettingsText([string]$userDir) {
    $f = Join-Path $userDir 'default\comfy.settings.json'
    if (Test-Path $f) { Get-Content -Raw -Encoding utf8 $f } else { $null }
}
function Get-FileList($snap) { @($snap.files | ForEach-Object { "$($_.path) ($($_.size) B)" }) }
$firstBoot = [ordered]@{ scratch_user_dir = $scratchUser; origin = $ScratchUrl }
if ((Test-Scratch) -or (Get-NetTCPConnection -LocalPort 8189 -State Listen -ErrorAction SilentlyContinue)) {
    $firstBoot.not_run = 'port 8189 is in use'
} else {
    Remove-BuildDir $scratchRoot
    New-Item -ItemType Directory -Force $scratchUser, $scratchTemp | Out-Null
    $fbPre = Get-DirSnapshot $scratchUser
    Write-Host 'starting a scratch ComfyUI on 127.0.0.1:8189 (own user and temp directories) ...'
    $sp = Start-Process -FilePath $ComfyPython -ArgumentList @($ComfyMain, '--listen', '127.0.0.1', '--port', '8189',
            '--user-directory', $scratchUser, '--temp-directory', $scratchTemp) `
        -WorkingDirectory (Split-Path $ComfyMain) -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $Runs 'scratch-comfy.out.log') -RedirectStandardError (Join-Path $Runs 'scratch-comfy.err.log')
    try {
        $deadline = (Get-Date).AddSeconds(240)
        while (-not (Test-Scratch) -and (Get-Date) -lt $deadline -and -not $sp.HasExited) { Start-Sleep -Seconds 2 }
        if (-not (Test-Scratch)) { $firstBoot.not_run = 'scratch ComfyUI did not answer on 127.0.0.1:8189' }
        else {
            Start-Sleep -Seconds 2
            $fbUp = Get-DirSnapshot $scratchUser
            $firstBoot.server_start_diff = Compare-Snapshots $fbPre $fbUp
            $firstBoot.files_after_server_start = @(Get-FileList $fbUp)
            $firstBoot.settings_after_server_start = Get-SettingsText $scratchUser
            $fbRuns = @()
            $prevSnap = $fbUp
            foreach ($k in 1, 2) {
                Write-Host "phase firstboot (run $k) ..."
                $r = Invoke-Phase 'firstboot' @('--origin', $ScratchUrl) "firstboot-$k"
                Start-Sleep -Seconds 1
                $snapK = Get-DirSnapshot $scratchUser
                $conv = Get-Events $r.events 'firstboot_convert' | Select-Object -Last 1
                $fbRuns += [ordered]@{ run = $k; exit_code = $r.run.exit_code; convert_ok = $conv.ok; origin = $conv.origin
                                       diff = (Compare-Snapshots $prevSnap $snapK); settings_after = (Get-SettingsText $scratchUser)
                                       files_after = @(Get-FileList $snapK) }
                $prevSnap = $snapK
            }
            $firstBoot.runs = $fbRuns
        }
    } finally {
        if (-not $sp.HasExited) { & taskkill /PID $sp.Id /T /F *>&1 | Out-Null; Start-Sleep -Seconds 2 }
        $firstBoot.scratch_stopped = -not (Test-Scratch)
        Write-Host 'stopped the scratch ComfyUI'
    }
}
$extra.first_boot = $firstBoot

# ==============================================================================================
# Controls.
# ==============================================================================================
# 1. The snapshot/diff code detects a change: a scratch folder under build\ holding a settings
#    file; the settings file is edited and a workflow file added; then an unchanged re-snapshot.
$ctlDir = Join-Path $Build 'snapshot-control'
Remove-BuildDir $ctlDir
New-Item -ItemType Directory -Force (Join-Path $ctlDir 'workflows') | Out-Null
Set-Content -Encoding utf8 (Join-Path $ctlDir 'comfy.settings.json') '{ "Comfy.TutorialCompleted": true }'
$ctlA = Get-DirSnapshot $ctlDir
Set-Content -Encoding utf8 (Join-Path $ctlDir 'comfy.settings.json') '{ "Comfy.TutorialCompleted": true, "Comfy.InstalledVersion": "1.53.10" }'
Set-Content -Encoding utf8 (Join-Path $ctlDir 'workflows\x.json') '{}'
$ctlB = Get-DirSnapshot $ctlDir
$ctlDiff = Compare-Snapshots $ctlA $ctlB
$ctlSame = Compare-Snapshots $ctlB (Get-DirSnapshot $ctlDir)
# 2. The real ComfyUI user folder over the whole script (first snapshot before any ComfyUI start).
$snapRealEnd = Get-DirSnapshot $ComfyUserDir
$realDiff = Compare-Snapshots $snapRealStart $snapRealEnd
$extra.controls = [ordered]@{
    snapshot_diff_detects_change = [ordered]@{
        pass = [bool]((-not $ctlDiff.identical) -and ($ctlDiff.added -contains 'workflows\x.json') -and
                      (@($ctlDiff.changed | ForEach-Object path) -contains 'comfy.settings.json') -and (-not $ctlDiff.settings_equal) -and $ctlSame.identical)
        after_edit = [ordered]@{ identical = $ctlDiff.identical; added = $ctlDiff.added; changed = @($ctlDiff.changed | ForEach-Object path); settings_equal = $ctlDiff.settings_equal }
        unchanged_resnapshot_identical = $ctlSame.identical }
    real_user_folder_whole_run = [ordered]@{ path = $ComfyUserDir; identical = $realDiff.identical; diff = $realDiff
                                             files = @($snapRealEnd.files | ForEach-Object path) }
}

foreach ($n in 'hidden', 'visible', 'down') {
    $png = Join-Path $Runs "$n.png"
    if (Test-Path $png) { Copy-Item $png (Join-Path $Results "$n.png") -Force }
}
# The converted / exported API JSON and readGraph output, for the record.
$keep = Join-Path $Results 'outputs'
New-Item -ItemType Directory -Force $keep | Out-Null
foreach ($name in $FixtureNames) {
    foreach ($f in @((Join-Path $hidden.out "$name.hidden.api.json"), (Join-Path $visible.out "$name.export_api.json"), (Join-Path $visible.out "$name.readgraph.json"))) {
        if (Test-Path $f) { Copy-Item $f $keep -Force }
    }
}
Write-Results
