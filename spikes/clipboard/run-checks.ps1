<#
.SYNOPSIS
  Builds the clipboard spike and runs the automated checks (Task 7): criteria 1, 2 and the
  automatable part of 5. Writes spikes/clipboard/results/clipboard.json.

.DESCRIPTION
  Criteria (docs/spikes/07-clipboard.md section 4):
    1  after Copy, the clipboard lists PNG, CF_DIB or CF_DIBV5, FileNameW or CF_HDROP and the custom
       format application/x-worldwright-ref, and does not list CF_UNICODETEXT (nor CF_TEXT /
       CF_OEMTEXT, which Windows would synthesize from it).
    2  the PNG bytes on the clipboard have the same SHA-256 as assets/source.png.
    5a (automated part of 5) a synthetic drag on the app's own window runs the real drag code path:
       the QDrag's supported actions are exactly [copy], the default action is copy, the offered
       formats carry no text/plain, and the exported file still exists afterwards with the source
       PNG's SHA-256. (A helper thread posts an in-process WM_MOUSEMOVE so DoDragDrop ends, because no real mouse
       button is down; no desktop input is generated.)
    3, 4, 5b, 6 are owner checks (Discord, Chrome, Explorer, Paint): not scripted; their results are
    in results/owner-checks.json, and clipboard.json points there.
    5a also asserts that the exported file is not read-only (APP-070).

  The app overwrites the user's clipboard with its own data. Before that, nothing on the clipboard
  is read. inspect_clipboard.ps1 only reads data it can identify as the spike's own.

.EXAMPLE
  pwsh -File spikes/clipboard/run-checks.ps1 [-NoBuild] [-Clean]
#>
param(
    [switch]$NoBuild,
    [switch]$Clean
)
$ErrorActionPreference = 'Stop'

$Root    = $PSScriptRoot
$Repo    = (Resolve-Path (Join-Path $Root '..\..')).Path
$Build   = Join-Path $Root 'build'
$CmakeDir = Join-Path $Build 'cmake'
$BinDir  = Join-Path $CmakeDir 'bin'
$Deploy  = Join-Path $Build 'deploy'
$BuiltExe = Join-Path $BinDir 'clipboard_spike.exe'
$Exe     = Join-Path $Deploy 'clipboard_spike.exe'   # the checks run the deployed copy the owner uses
$Runs    = Join-Path $Build 'runs'
$Results = Join-Path $Root 'results'
$SrcPng  = Join-Path $Root 'assets\source.png'
$ExportDir = Join-Path ([IO.Path]::GetTempPath()) 'ww-spike-export'
$ExportFile = Join-Path $ExportDir 'worldwright-spike-image.png'
New-Item -ItemType Directory -Force -Path $Runs, $Results | Out-Null

$criteria = [ordered]@{}
function Record($id, [bool]$pass, $reason, $evidence = $null) {
    $script:criteria["$id"] = [ordered]@{ pass = $pass; reason = $reason; evidence = $evidence }
    "criterion {0,-3}: {1}  {2}" -f $id, $(if ($pass) { 'PASS' } else { 'FAIL' }), $reason
}
function Finish {
    # Owner checks are not scripted: their results live in results/owner-checks.json.
    $ownerFile = Join-Path $Results 'owner-checks.json'
    $owner = if (Test-Path $ownerFile) { Get-Content $ownerFile -Raw | ConvertFrom-Json } else { $null }
    foreach ($id in '3', '4', '5b', '6') {
        $script:criteria[$id] = if ($owner) {
            [ordered]@{ pass = $null; reason = "owner check, not scripted: $($owner.checks.$id.status) ($($owner.source), $($owner.date)); see results/owner-checks.json"; evidence = $null }
        } else {
            [ordered]@{ pass = $null; reason = 'owner check, not scripted; results/owner-checks.json is missing'; evidence = $null }
        }
    }
    $out = [ordered]@{
        date = (Get-Date -Format 'yyyy-MM-dd HH:mm'); source_png = 'assets/source.png'
        source_sha256 = $(if (Test-Path $SrcPng) { (Get-FileHash $SrcPng -Algorithm SHA256).Hash.ToLower() } else { $null })
        criteria = $script:criteria
    }
    $out | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $Results $script:ResultName) -Encoding utf8
    $failed = @($script:criteria.GetEnumerator() | Where-Object { $_.Value.pass -eq $false } | ForEach-Object { $_.Key })
    if ($failed.Count) { "FAILED criteria: $($failed -join ', ')"; exit 1 }
    'ALL AUTOMATED CRITERIA PASSED (owner checks 3, 4, 5b, 6: see results/owner-checks.json)'; exit 0
}
$script:ResultName = 'clipboard.json'

# --- build -------------------------------------------------------------------------------------
if (-not (Test-Path (Join-Path $Root 'CMakeLists.txt'))) {
    Record 1 $false "no CMake project: $Root\CMakeLists.txt does not exist"
    Record 2 $false 'not run'
    Record '5a' $false 'not run'
    $script:ResultName = 'clipboard-red.json'; Finish
}
. (Join-Path $Repo 'spikes\toolchain\env.ps1')
if ($Clean -and (Test-Path $CmakeDir)) { Remove-Item -Recurse -Force $CmakeDir }
if (-not $NoBuild) {
    cmake -S $Root -B $CmakeDir -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo 2>&1 | Out-Host
    if ($LASTEXITCODE) { Record 1 $false "cmake configure exit $LASTEXITCODE"; Finish }
    $log = cmake --build $CmakeDir 2>&1
    $log | Select-Object -Last 5 | Out-Host
    if ($LASTEXITCODE) { Record 1 $false "build exit $LASTEXITCODE"; Finish }
    $warnings = @($log | Where-Object { $_ -match 'warning' }).Count
    "build warnings: $warnings"
}
if (-not (Test-Path $BuiltExe)) { Record 1 $false "no exe at $BuiltExe"; Finish }

# --- deploy: self-contained copy (windeployqt) for the owner checks ---------------------------
if (Test-Path $Deploy) { Remove-Item -LiteralPath $Deploy -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Deploy | Out-Null
Copy-Item $BuiltExe, (Join-Path $BinDir 'spike_shim.dll') $Deploy   # windeployqt does not copy our own DLL
windeployqt --release --qmldir (Join-Path $Root 'qml') $Exe 2>&1 | Select-Object -Last 2 | Out-Host
if ($LASTEXITCODE) { Record 1 $false "windeployqt exit $LASTEXITCODE"; Finish }

# Start the app with the Qt, MSVC and Windows Kits directories removed from PATH.
$FullPath = $env:PATH
$StrippedPath = ($FullPath -split ';' | Where-Object { $_ -and $_ -notmatch '(?i)\\Qt\\|Visual Studio|MSVC|Windows Kits' }) -join ';'
function Start-Stripped($appArgs) {
    $env:PATH = $StrippedPath
    try { Start-Process $Exe -ArgumentList $appArgs -PassThru -WorkingDirectory $env:TEMP } finally { $env:PATH = $FullPath }
}
$leak = @($StrippedPath -split ';' | Where-Object { $_ -and ((Test-Path (Join-Path $_ 'Qt6Core.dll')) -or (Test-Path (Join-Path $_ 'cl.exe'))) })
"stripped PATH leaks Qt6Core.dll or cl.exe from: $(if ($leak.Count) { $leak -join '; ' } else { 'nowhere' })"

function Wait-LogEvent($path, $event, $timeoutMs) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $timeoutMs) {
        if (Test-Path $path) {
            $hit = Get-Content $path -ErrorAction SilentlyContinue | Where-Object { $_ -match "`"event`":`"$event`"" }
            if ($hit) { return ($hit | Select-Object -First 1 | ConvertFrom-Json) }
        }
        Start-Sleep -Milliseconds 100
    }
    return $null
}
function Read-Log($path) { Get-Content $path | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json } }

$srcSha = (Get-FileHash $SrcPng -Algorithm SHA256).Hash.ToLower()
if (Test-Path $ExportDir) { Remove-Item -LiteralPath $ExportFile -ErrorAction SilentlyContinue }

# --- criteria 1 and 2: Copy, then inspect ------------------------------------------------------
$copyLog = Join-Path $Runs 'copy.jsonl'
$p = Start-Stripped @('--log', "`"$copyLog`"", '--auto-copy')
try {
    $copied = Wait-LogEvent $copyLog 'auto_copy' 20000
    if (-not $copied) { Record 1 $false 'app never logged auto_copy'; Record 2 $false 'not run'; Finish }
    Start-Sleep -Milliseconds 700
    $inspectJson = Join-Path $Runs 'inspect.json'
    $clipPng = Join-Path $Runs 'clipboard-png.bin'
    & pwsh -NoProfile -File (Join-Path $Root 'inspect_clipboard.ps1') -Json $inspectJson -PngOut $clipPng | Out-Host
    $ins = Get-Content $inspectJson -Raw | ConvertFrom-Json
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
}

$names = @($ins.formats | ForEach-Object { $_.name })
if (-not $ins.ours) {
    Record 1 $false 'the clipboard did not hold the spike data (no application/x-worldwright-ref)' $names
} else {
    $missing = @()
    if (-not $ins.has_png)       { $missing += 'PNG' }
    if (-not $ins.has_dib)       { $missing += 'CF_DIB/CF_DIBV5' }
    if (-not $ins.has_file_drop) { $missing += 'FileNameW/CF_HDROP' }
    if (-not $ins.has_ref)       { $missing += 'application/x-worldwright-ref' }
    $extra = @(); if ($ins.has_unicodetext) { $extra += 'text format present (CF_UNICODETEXT/CF_TEXT/CF_OEMTEXT)' }
    $ok = ($missing.Count -eq 0 -and $extra.Count -eq 0)
    $why = if ($ok) { 'all required formats present, no text format' } else { "missing: [$($missing -join ', ')]; unexpected: [$($extra -join ', ')]" }
    Record 1 $ok $why $ins.formats
}
$clipSha = if ($ins.png) { $ins.png.sha256 } else { $null }
Record 2 ($clipSha -eq $srcSha) ("clipboard PNG sha256 = $clipSha ($($ins.png.bytes) bytes); source = $srcSha") `
    ([ordered]@{ clipboard = $clipSha; source = $srcSha })

# --- criterion 5a: synthetic drag on our own window --------------------------------------------
$dragLog = Join-Path $Runs 'drag.jsonl'
$d = Start-Stripped @('--log', "`"$dragLog`"", '--selftest-drag', '--quit-after', '4000')
$null = Wait-LogEvent $dragLog 'exit' 30000
if (-not $d.HasExited) { $null = $d.WaitForExit(5000) }
if (-not $d.HasExited) { Stop-Process -Id $d.Id -Force }
$ev = Read-Log $dragLog
$ds = $ev | Where-Object { $_.event -eq 'drag_start' } | Select-Object -First 1
$de = $ev | Where-Object { $_.event -eq 'drag_end' } | Select-Object -First 1
if (-not $ds -or -not $de) {
    Record '5a' $false 'the synthetic drag never reached QDrag::exec (no drag_start/drag_end)' ($ev | Where-Object { $_.event -in 'error', 'qt_msg' })
} else {
    $actions = @($de.supported_actions)
    $exSha = if (Test-Path $ExportFile) { (Get-FileHash $ExportFile -Algorithm SHA256).Hash.ToLower() } else { $null }
    $bad = @()
    if (-not ($actions.Count -eq 1 -and $actions[0] -eq 'copy')) { $bad += "supported actions [$($actions -join ',')] != [copy]" }
    if ($de.default_action -ne 'copy') { $bad += "default action $($de.default_action)" }
    if (@($ds.formats) -contains 'text/plain') { $bad += 'drag offers text/plain' }
    if (-not $de.export_exists_after) { $bad += 'exported file missing after the drag' }
    if ($exSha -ne $srcSha) { $bad += "exported file sha256 $exSha != source" }
    $readOnly = if (Test-Path $ExportFile) { (Get-Item $ExportFile).IsReadOnly } else { $null }
    if ($readOnly -ne $false) { $bad += "exported file read-only flag is '$readOnly', expected False (APP-070)" }
    $why = if ($bad.Count) { $bad -join '; ' } else {
        "QDrag supportedActions=[copy], defaultAction=copy, drag result=$($de.result) (cancelled: no real button down), export file exists after with source sha256" }
    Record '5a' ($bad.Count -eq 0) $why ([ordered]@{ drag_start = $ds; drag_end = $de; export_sha256 = $exSha; export_is_read_only = $readOnly })
}
Finish

