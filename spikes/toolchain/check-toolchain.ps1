<#
.SYNOPSIS
  Prints one line per toolchain tool: "<tool> <version> <path>", then the Qt WebEngine
  version. Exits 1 if any tool is missing, 0 otherwise.
.NOTES
  Run after dot-sourcing env.ps1 in the same shell:
    pwsh -Command ". spikes/toolchain/env.ps1; ./spikes/toolchain/check-toolchain.ps1"
#>
$ErrorActionPreference = 'Continue'

function Get-FirstLine([string]$exe, [string[]]$toolArgs, [string]$pattern) {
    # Runs the tool, merges stderr (cl.exe writes its banner there) and returns the
    # first line matching $pattern; falls back to the first non-empty line.
    $out = & $exe @toolArgs 2>&1 | ForEach-Object { "$_" } | Where-Object { $_.Trim() }
    $hit = $out | Where-Object { $_ -match $pattern } | Select-Object -First 1
    if ($hit) { return $hit.Trim() }
    return ($out | Select-Object -First 1)
}

# name, command, args, regex that captures the version in group 1
$tools = @(
    @{ Name = 'rustc';       Cmd = 'rustc';       Args = @('--version');   Re = 'rustc (\S+)' },
    @{ Name = 'cargo';       Cmd = 'cargo';       Args = @('--version');   Re = 'cargo (\S+)' },
    @{ Name = 'cmake';       Cmd = 'cmake';       Args = @('--version');   Re = 'cmake version (\S+)' },
    @{ Name = 'ninja';       Cmd = 'ninja';       Args = @('--version');   Re = '^(\d\S*)' },
    @{ Name = 'cl';          Cmd = 'cl';          Args = @();              Re = 'Version (\d[\d.]*)' },
    @{ Name = 'qmake';       Cmd = 'qmake';       Args = @('-query', 'QT_VERSION'); Re = '^(\d\S*)' },
    @{ Name = 'qsb';         Cmd = 'qsb';         Args = @('--version');   Re = '(\d+\.\d+\.\d+)' },
    @{ Name = 'windeployqt'; Cmd = 'windeployqt'; Args = @('--version');   Re = '(\d+\.\d+\.\d+)' },
    @{ Name = 'git';         Cmd = 'git';         Args = @('--version');   Re = 'git version (\S+)' },
    @{ Name = 'git-lfs';     Cmd = 'git-lfs';     Args = @('--version');   Re = 'git-lfs/(\S+)' }
)

$missing = @()
foreach ($t in $tools) {
    $found = Get-Command $t.Cmd -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $found) {
        Write-Output ("{0} MISSING -" -f $t.Name)
        $missing += $t.Name
        continue
    }
    $line = Get-FirstLine $found.Source $t.Args $t.Re
    $version = if ($line -match $t.Re) { $Matches[1] } else { 'unknown' }
    Write-Output ("{0} {1} {2}" -f $t.Name, $version, $found.Source)
}

# Qt WebEngine: versioned separately from Qt (QA-077). Read it from the installed module metadata.
$webEngine = $null
$qmake = Get-Command qmake -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if ($qmake) {
    $qtPrefix = (& $qmake.Source -query QT_INSTALL_PREFIX 2>$null | Select-Object -First 1)
    $cfg = Join-Path $qtPrefix 'modules\WebEngineCore.json'
    if (Test-Path $cfg) {
        $webEngine = (Get-Content -Raw $cfg | ConvertFrom-Json).version
    }
}
if ($webEngine) {
    Write-Output ("qtwebengine {0} {1}" -f $webEngine, $cfg)
} else {
    Write-Output "qtwebengine MISSING -"
    $missing += 'qtwebengine'
}

if ($missing.Count -gt 0) {
    Write-Output ("FAIL: missing: {0}" -f ($missing -join ', '))
    exit 1
}
Write-Output 'OK: all tools present'
exit 0
