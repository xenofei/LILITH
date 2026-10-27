<#
.SYNOPSIS
  QA-077: the installed Qt pair is Qt 6.12.0 with Qt WebEngine 6.140.0, and nothing else was
  installed in its place. Exit 0 when it is, 1 otherwise.

.DESCRIPTION
  Checks, in -QtDir (default LIL_QT_DIR, else C:\Qt\6.12.0\msvc2022_64):
  - bin\Qt6Core.dll's file version is 6.12.0 (it reads 6.12.0.0: the first three parts count);
  - bin\Qt6WebEngineCore.dll's file version is 6.140.0 (6.140.0.0), and
    modules\WebEngineCore.json says "version": "6.140.0" (QA-078: naqt's default pick is an older
    WebEngine build, so the pair is checked, not assumed);
  - bin\Qt6Core5Compat.dll is absent (the suite uses no Qt 5 compatibility module).
#>
param([string]$QtDir = $(if ($env:LIL_QT_DIR) { $env:LIL_QT_DIR } else { 'C:\Qt\6.12.0\msvc2022_64' }))
$ErrorActionPreference = 'Stop'
$failures = [System.Collections.Generic.List[string]]::new()

function Get-Version3([string]$dll) {
    if (-not (Test-Path -LiteralPath $dll -PathType Leaf)) { return $null }
    $v = (Get-Item -LiteralPath $dll).VersionInfo
    '{0}.{1}.{2}' -f $v.FileMajorPart, $v.FileMinorPart, $v.FileBuildPart
}

$core = Get-Version3 (Join-Path $QtDir 'bin\Qt6Core.dll')
$web = Get-Version3 (Join-Path $QtDir 'bin\Qt6WebEngineCore.dll')
$json = Join-Path $QtDir 'modules\WebEngineCore.json'
$jsonVersion = if (Test-Path -LiteralPath $json) { (Get-Content -LiteralPath $json -Raw | ConvertFrom-Json).version } else { $null }
$compat = Test-Path -LiteralPath (Join-Path $QtDir 'bin\Qt6Core5Compat.dll')

Write-Host ("Qt pair in {0}: Qt6Core.dll {1}, Qt6WebEngineCore.dll {2}, WebEngineCore.json {3}, Qt6Core5Compat.dll {4}" -f `
    $QtDir, $core, $web, $jsonVersion, $(if ($compat) { 'present' } else { 'absent' }))
if ($core -ne '6.12.0') { $failures.Add("Qt6Core.dll is $(if ($core) { $core } else { 'missing' }), not 6.12.0") }
if ($web -ne '6.140.0') { $failures.Add("Qt6WebEngineCore.dll is $(if ($web) { $web } else { 'missing' }), not 6.140.0") }
if ($jsonVersion -ne '6.140.0') { $failures.Add("modules\WebEngineCore.json says $(if ($jsonVersion) { $jsonVersion } else { 'nothing' }), not 6.140.0") }
if ($compat) { $failures.Add('Qt6Core5Compat.dll is installed') }

if ($failures.Count) {
    $failures | ForEach-Object { Write-Host "check-qt-pair: $_" }
    exit 1
}
Write-Host 'check-qt-pair: the pair is Qt 6.12.0 + Qt WebEngine 6.140.0'
exit 0
