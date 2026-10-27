<#
.SYNOPSIS
  Runs a command in the MSVC 14.44 x64 developer environment with the Qt pair first on PATH,
  on the PC and in CI (Ruling Q6):
    pwsh -NoProfile -File tools/dev-shell.ps1 cmake --preset dev
    pwsh -NoProfile -File tools/dev-shell.ps1 cmake --build --preset dev
    pwsh -NoProfile -File tools/dev-shell.ps1 ctest --preset qml

.DESCRIPTION
  1. Finds Visual Studio with vswhere (`-products *`, so Build Tools count), whatever its version
     folder: the windows-2025 runner image is VS 2026, which still ships the 14.44 toolset.
  2. Runs its vcvars64.bat with -vcvars_ver=14.44 and takes the environment it sets, then checks
     that `cl` is 19.44.x (Qt 6.12 supports MSVC 2022's toolset for x64, not 2026's): it exits 2
     otherwise.
  3. The Qt pair's folder is LIL_QT_DIR (CMakePresets.json reads it as CMAKE_PREFIX_PATH); when
     it isn't set, C:\Qt\6.12.0\msvc2022_64, where the installer and CI's naqt put it. Its bin\
     goes first on PATH.
  4. Runs the command and exits with its exit code. With no command, it only checks.
  On a GitHub runner it prints the runner image's name and version first.
#>
$ErrorActionPreference = 'Stop'
$Command = @($args)

if ($env:ImageOS) {
    Write-Host "Runner image: $env:ImageOS $env:ImageVersion"
}

# 1. Visual Studio, whichever version has the x64 C++ tools.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) {
    Write-Host "dev-shell: vswhere.exe not found at $vswhere (is Visual Studio or its Build Tools installed?)"
    exit 2
}
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) {
    Write-Host 'dev-shell: no Visual Studio with the x64 C++ tools (Microsoft.VisualStudio.Component.VC.Tools.x86.x64)'
    exit 2
}
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'

# 2. The 14.44 toolset's environment, once per process.
if (-not $env:VCToolsVersion -or -not $env:VCToolsVersion.StartsWith('14.44.')) {
    $vars = & cmd.exe /d /c "`"$vcvars`" -vcvars_ver=14.44 >nul 2>&1 && set"
    if ($LASTEXITCODE) {
        Write-Host "dev-shell: vcvars64.bat -vcvars_ver=14.44 failed (exit $LASTEXITCODE): is the MSVC 14.44 toolset installed?"
        exit 2
    }
    foreach ($line in $vars) {
        if ($line -match '^([^=]+)=(.*)$') { Set-Item -LiteralPath "Env:$($Matches[1])" -Value $Matches[2] }
    }
}
$banner = (& cmd.exe /d /c 'cl 2>&1' | Select-Object -First 1) -join ''
Write-Host "dev-shell: $banner"
if ($banner -notmatch 'Version 19\.44\.') {
    Write-Host 'dev-shell: cl is not 19.44.x (MSVC 14.44), the toolset Qt 6.12 supports'
    exit 2
}

# 3. The Qt pair.
if (-not $env:LIL_QT_DIR) { $env:LIL_QT_DIR = 'C:\Qt\6.12.0\msvc2022_64' }
if (-not (Test-Path -LiteralPath (Join-Path $env:LIL_QT_DIR 'bin\qmake.exe'))) {
    Write-Host "dev-shell: no Qt at LIL_QT_DIR ($env:LIL_QT_DIR)"
    exit 2
}
$qtBin = Join-Path $env:LIL_QT_DIR 'bin'
$env:PATH = $qtBin + ';' + (($env:PATH -split ';' | Where-Object { $_ -and $_ -ne $qtBin }) -join ';')

# 4. The command.
if ($Command.Count -eq 0) { exit 0 }
$exe = $Command[0]
$rest = @($Command | Select-Object -Skip 1)
& $exe @rest
exit $LASTEXITCODE
