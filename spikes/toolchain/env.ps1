<#
.SYNOPSIS
  Dot-source this to get Rust, CMake, Ninja, MSVC and Qt in the current PowerShell 7 shell:
    . spikes/toolchain/env.ps1
  Sets QTDIR, CMAKE_PREFIX_PATH, PATH and the MSVC x64 developer environment (vcvars64.bat).
  Safe to dot-source more than once. Pinned versions: docs/spikes/01-toolchain.md.
#>

# --- Pins (keep in sync with docs/spikes/01-toolchain.md and QA-077) ----------------------
$script:WwQtVersion = '6.12.0'
$script:WwQtRoot    = 'C:\Qt'
$script:WwQtArch    = 'msvc2022_64'
$script:WwVcvars    = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'

function script:Add-PathFront([string]$dir) {
    if (-not $dir -or -not (Test-Path -LiteralPath $dir)) { return }
    $parts = @($env:PATH -split ';' | Where-Object { $_ })
    if ($parts -notcontains $dir) { $env:PATH = $dir + ';' + $env:PATH }
}

# 1. Pick up PATH entries added to the registry by installers (winget, rustup) that this
#    shell has not seen yet; keep what the shell already has.
$regPath = @(
    [Environment]::GetEnvironmentVariable('PATH', 'Machine'),
    [Environment]::GetEnvironmentVariable('PATH', 'User')
) -join ';'
foreach ($d in ($regPath -split ';' | Where-Object { $_ })) {
    if (@($env:PATH -split ';') -notcontains $d) { $env:PATH += ';' + $d }
}

# 2. Rust.
script:Add-PathFront (Join-Path $env:USERPROFILE '.cargo\bin')

# 3. Qt.
$qtDir = Join-Path $script:WwQtRoot "$script:WwQtVersion\$script:WwQtArch"
if (-not (Test-Path -LiteralPath (Join-Path $qtDir 'bin\qmake.exe'))) {
    Write-Warning "Qt $script:WwQtVersion not found at $qtDir"
} else {
    $env:QTDIR = $qtDir
    $env:CMAKE_PREFIX_PATH = $qtDir
    script:Add-PathFront (Join-Path $qtDir 'bin')
}

# 4. MSVC developer environment (once per shell; vcvars64.bat prepends to PATH, INCLUDE, LIB).
if (-not $env:VCToolsInstallDir) {
    if (-not (Test-Path -LiteralPath $script:WwVcvars)) {
        Write-Warning "vcvars64.bat not found at $script:WwVcvars"
    } else {
        $vars = cmd /c "`"$script:WwVcvars`" >nul 2>&1 && set"
        foreach ($line in $vars) {
            if ($line -match '^([^=]+)=(.*)$') {
                Set-Item -LiteralPath "Env:$($Matches[1])" -Value $Matches[2]
            }
        }
    }
}

# 5. Keep Qt's bin first so Qt DLLs win over anything else on PATH.
if ($env:QTDIR) {
    $qtBin = Join-Path $env:QTDIR 'bin'
    $env:PATH = $qtBin + ';' + (($env:PATH -split ';' | Where-Object { $_ -and $_ -ne $qtBin }) -join ';')
}
