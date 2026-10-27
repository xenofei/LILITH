<#
.SYNOPSIS
  CTest's deps_walk (label deploy): what the binaries import, read with dumpbin. Run it in the
  developer environment (tools/dev-shell.ps1), after the deploy target.

.DESCRIPTION
  Fails (exit 1) unless:
  - APP-137: `dumpbin /dependents` of -Exe (lil-image.exe) and -Shim (the shim DLL) lists no
    Qt6WebEngine* DLL, and the lil_qt static library (-RustLibrary) names WebEngine in neither
    its linker directives nor its symbols;
  - lil-image.exe imports SetDefaultDllDirectories and SetDllDirectoryW (the DLL search
    lockdown; deploy_runs_off_path checks that it works);
  - OPS-094 (Ruling Q8): every *140*.dll that `dumpbin /dependents` reports for any exe or DLL
    in the deploy folder is present in the deploy folder itself (System32 has them all, so the
    off-PATH run can't see a missing one), vcruntime140.dll, vcruntime140_1.dll and
    msvcp140.dll among them, and every VC++ runtime DLL in it has one and the same version, at
    least 14.44, the toolset that built the binaries (one redist folder, cmake/deploy.cmake);
  - the dynamic Lilith.Web module is deployed as Lilith\Web\ (its qmldir and plugin DLL).
#>
param(
    [Parameter(Mandatory)] [string]$Deploy,
    [Parameter(Mandatory)] [string]$Exe,
    [Parameter(Mandatory)] [string]$Shim,
    [Parameter(Mandatory)] [string]$RustLibrary
)
$ErrorActionPreference = 'Stop'
$failures = [System.Collections.Generic.List[string]]::new()

if (-not (Get-Command dumpbin -ErrorAction SilentlyContinue)) {
    Write-Host 'deps_walk: dumpbin is not on PATH; run it through tools/dev-shell.ps1'
    exit 1
}
if (-not (Test-Path -LiteralPath $Deploy -PathType Container)) {
    Write-Host "deps_walk: no deploy folder at $Deploy (build the deploy target first)"
    exit 1
}

function Get-Dependents([string]$file) {
    @(& dumpbin /nologo /dependents $file 2>&1 | ForEach-Object { "$_".Trim() } |
        Where-Object { $_ -match '^\S+\.dll$' })
}

# APP-137: nothing on the startup path imports WebEngine.
foreach ($bin in $Exe, $Shim) {
    $deps = Get-Dependents $bin
    if ($deps.Count -eq 0) { $failures.Add("$(Split-Path $bin -Leaf): dumpbin listed no dependents") }
    $web = @($deps | Where-Object { $_ -match '^Qt6WebEngine' })
    if ($web.Count) { $failures.Add("$(Split-Path $bin -Leaf) imports $($web -join ', ')") }
    Write-Host ("{0}: {1} dependents, WebEngine: {2}" -f (Split-Path $bin -Leaf), $deps.Count, $(if ($web.Count) { $web -join ', ' } else { 'none' }))
}
# The DLL search lockdown (main.cpp step 0) is in the exe: it imports both calls.
$imports = (& dumpbin /nologo /imports $Exe 2>&1) | Out-String
foreach ($fn in 'SetDefaultDllDirectories', 'SetDllDirectoryW') {
    if ($imports -notmatch "\b$fn\b") { $failures.Add("$(Split-Path $Exe -Leaf) doesn't import $fn") }
}
$directives = (& dumpbin /nologo /directives $RustLibrary 2>&1) | Out-String
$symbols = (& dumpbin /nologo /linkermember:1 $RustLibrary 2>&1) | Out-String
if ($symbols.Length -lt 1000) { $failures.Add("$(Split-Path $RustLibrary -Leaf): dumpbin read no symbols") }
if ($directives -match '(?i)webengine') { $failures.Add("$(Split-Path $RustLibrary -Leaf)'s linker directives name WebEngine") }
if ($symbols -match '(?i)webengine') { $failures.Add("$(Split-Path $RustLibrary -Leaf)'s symbols name WebEngine") }
Write-Host ("{0}: WebEngine in directives: {1}, in symbols: {2}" -f (Split-Path $RustLibrary -Leaf), ($directives -match '(?i)webengine'), ($symbols -match '(?i)webengine'))

# OPS-094: the VC++ runtime every deployed binary imports is app-local.
$files = @(Get-ChildItem -LiteralPath $Deploy -Recurse -File | Where-Object { $_.Extension -in '.exe', '.dll' })
$wanted = [System.Collections.Generic.SortedSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($name in 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll') { [void]$wanted.Add($name) }
foreach ($f in $files) {
    foreach ($dep in (Get-Dependents $f.FullName)) {
        if ($dep -match '140') { [void]$wanted.Add($dep) }
    }
}
$missing = @($wanted | Where-Object { -not (Test-Path -LiteralPath (Join-Path $Deploy $_) -PathType Leaf) })
foreach ($m in $missing) { $failures.Add("$m is imported but not in the deploy folder") }
Write-Host ("VC++ runtime imported by {0} binaries: {1}; missing app-local: {2}" -f $files.Count, ($wanted -join ', '), $(if ($missing.Count) { $missing -join ', ' } else { 'none' }))
# ...and it is one redist, never older than the 14.44 toolset that built the binaries: every
# runtime DLL in the folder has the same version, at least 14.44.
$runtime = @(Get-ChildItem -LiteralPath $Deploy -File -Filter '*.dll' | Where-Object { $_.Name -match '^(vcruntime|msvcp|concrt|vccorlib|vcomp|vcamp)140' })
$versions = @($runtime | ForEach-Object {
    $v = $_.VersionInfo
    [version]::new($v.FileMajorPart, $v.FileMinorPart, $v.FileBuildPart, $v.FilePrivatePart)
} | Sort-Object -Unique)
if ($runtime.Count -eq 0) { $failures.Add('no VC++ runtime DLL in the deploy folder') }
elseif ($versions.Count -ne 1) { $failures.Add("the VC++ runtime DLLs have $($versions.Count) versions ($($versions -join ', ')), not one") }
elseif ($versions[0] -lt [version]'14.44') { $failures.Add("the VC++ runtime is $($versions[0]), older than the 14.44 toolset") }
Write-Host ("VC++ runtime versions: {0}" -f (($runtime | ForEach-Object { "$($_.Name) $($_.VersionInfo.FileVersion)" }) -join ', '))

# The dynamic Lilith.Web module.
$web = Join-Path $Deploy 'Lilith\Web'
if (-not (Test-Path -LiteralPath (Join-Path $web 'qmldir') -PathType Leaf)) { $failures.Add('Lilith\Web\qmldir is not deployed') }
if (@(Get-ChildItem -LiteralPath $web -Filter '*.dll' -ErrorAction SilentlyContinue).Count -eq 0) { $failures.Add('Lilith\Web\ has no plugin DLL') }

if ($failures.Count) {
    Write-Host 'deps_walk: FAIL'
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host 'deps_walk: PASS'
exit 0
