<#
.SYNOPSIS
  S0.2 (P4a-R1): can `lil-qt` be a workspace member with its Qt parts behind a `qt` feature?

.DESCRIPTION
  In a scratch git worktree under %TEMP% (detached, deleted afterwards), this script:
    1. adds a throwaway member crates/lil-qt-probe: Qt-free logic always; a cxx-qt counter, the
       `cxx`, `cxx-qt`, `cxx-qt-lib` dependencies and the `cxx-qt-build` build-dependency only
       under the `qt` feature (all four from [workspace.dependencies], as Task 15 would add them);
    2. lets cargo add the member to the workspace Cargo.lock (`cargo metadata`, no other update);
    3. with every Qt folder removed from PATH and QMAKE, QTDIR and CMAKE_PREFIX_PATH unset:
       `cargo build --workspace --locked` and
       `cargo clippy -p $Pkg --all-targets --locked -- -D warnings`;
    4. with Qt: a small CMake project imports the member with
       cxx_qt_import_crate(MANIFEST_PATH ../crates/lil-qt-probe/Cargo.toml CRATES lil-qt-probe
                           LOCKED FEATURES qt QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml),
       builds, and runs a QML counter round trip (no window; QT_QPA_PLATFORM=offscreen);
    5. checks the workspace Cargo.lock was not changed by the CMake build (LOCKED);
    6. removes the worktree. Writes -Out (JSON).

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/s02-lil-qt-route.ps1 -Out spikes/app-shell/results/s02.json
#>
param(
    [Parameter(Mandatory)] [string]$Out,
    [string]$Pkg = 'lil-qt-probe',   # the throwaway package's name (try lil_qt_probe too)
    [switch]$Keep   # keep the scratch worktree (debugging only)
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
. (Join-Path $Repo 'spikes\toolchain\env.ps1')
# Missing-DLL failures become exit codes, never dialogs, in this process and every child.
Add-Type -Namespace SpikeS02 -Name K -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint m);'
[void][SpikeS02.K]::SetErrorMode(0x0001 -bor 0x8000)

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$wt = Join-Path $env:TEMP "lil-s02-$stamp"
$res = [ordered]@{ check = 'S0.2'; started = (Get-Date).ToString('o'); worktree = '%TEMP%\' + (Split-Path $wt -Leaf) }
$sw = [Diagnostics.Stopwatch]::StartNew()

function Write-Text([string]$path) { process { [IO.File]::WriteAllText($path, ($_ -replace "`r", "")) } }
function Invoke-Logged([string]$name, [scriptblock]$cmd) {
    $log = Join-Path $wt "s02-$name.log"
    $t = [Diagnostics.Stopwatch]::StartNew()
    & $cmd *>&1 | Out-File -Encoding utf8 $log
    $code = $LASTEXITCODE
    $tail = @(Get-Content $log -Tail 25)
    [ordered]@{ exit = $code; seconds = [math]::Round($t.Elapsed.TotalSeconds, 1)
                warnings = @(Get-Content $log | Where-Object { $_ -match '^\s*warning' } | Select-Object -Unique -First 30)
                tail = if ($code -ne 0) { $tail } else { @() } }
}

try {
    & git -C $Repo worktree add --detach $wt HEAD 2>&1 | Out-Null
    if ($LASTEXITCODE) { throw "git worktree add failed" }
    $res.head = (& git -C $wt rev-parse --short HEAD).Trim()

    # ---- 1. the throwaway member -------------------------------------------------------------
    $memberDir = Join-Path $wt "crates\$Pkg"
    $libTarget = $Pkg.Replace('-', '_')
    $res.member = $Pkg
    New-Item -ItemType Directory -Force (Join-Path $memberDir 'src') | Out-Null
    $rootToml = Join-Path $wt 'Cargo.toml'
    $toml = Get-Content $rootToml -Raw
    $deps = @'
[workspace.dependencies]
# S0.2 probe: cxx-qt for lil-qt (P4a-R1), optional in the member.
cxx = "1.0.176"
cxx-qt = "=0.10.0"
cxx-qt-lib = "=0.10.0"
cxx-qt-build = "=0.10.0"
'@
    $toml = [regex]::new('\[workspace\.dependencies\]\r?\n').Replace($toml, ($deps -replace '\r', '') + "`n", 1)
    if ($toml -notmatch 'cxx-qt-build = "=0\.10\.0"') { throw 'could not add the cxx-qt workspace dependencies' }
    [IO.File]::WriteAllText($rootToml, $toml)
    @'
[package]
name = "@MEMBER@"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

# Throwaway (spike 10, S0.2): Qt-free logic always, the Qt bridge only under `qt`.
[lib]
crate-type = ["rlib", "staticlib"]

[features]
default = []
qt = ["dep:cxx", "dep:cxx-qt", "dep:cxx-qt-lib", "dep:cxx-qt-build"]

[dependencies]
cxx = { workspace = true, optional = true }
cxx-qt = { workspace = true, optional = true }
cxx-qt-lib = { workspace = true, optional = true }

[build-dependencies]
cxx-qt-build = { workspace = true, optional = true }

[lints]
workspace = true
'@.Replace('@MEMBER@', $Pkg) | Write-Text (Join-Path $memberDir 'Cargo.toml')
    @'
fn main() {
    // The Qt bridge is generated only with the `qt` feature; without it this does nothing, so
    // the workspace builds with no Qt installed.
    #[cfg(feature = "qt")]
    cxx_qt_build::CxxQtBuilder::new_qml_module(cxx_qt_build::QmlModule::new("LilQtProbe"))
        .qt_module("Qml")
        .files(["src/qt_bridge.rs"])
        .build();
}
'@ | Write-Text (Join-Path $memberDir 'build.rs')
    @'
//! Throwaway probe for spike 10, S0.2: Qt-free logic, and a cxx-qt bridge behind `qt`.

/// The Qt-free part: what the counter's next value is.
pub fn next_count(count: i32) -> i32 {
    count.saturating_add(1)
}

#[cfg(feature = "qt")]
extern crate cxx_qt_lib;

#[cfg(feature = "qt")]
pub mod qt_bridge;

#[cfg(test)]
mod tests {
    #[test]
    fn next_count_adds_one_and_saturates() {
        assert_eq!(super::next_count(1), 2);
        assert_eq!(super::next_count(i32::MAX), i32::MAX);
    }
}
'@ | Write-Text (Join-Path $memberDir 'src\lib.rs')
    @'
//! `ProbeCounter`: `count` and `increment()`, a QML round trip through the Qt-free logic.

#[cxx_qt::bridge]
pub mod qobject {
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        type ProbeCounter = super::ProbeCounterRust;

        #[qinvokable]
        fn increment(self: Pin<&mut Self>);
    }
}

use core::pin::Pin;

#[derive(Default)]
pub struct ProbeCounterRust {
    count: i32,
}

impl qobject::ProbeCounter {
    pub fn increment(self: Pin<&mut Self>) {
        let next = crate::next_count(*self.count());
        self.set_count(next);
    }
}
'@ | Write-Text (Join-Path $memberDir 'src\qt_bridge.rs')

    # ---- 2. the member's entries in the workspace Cargo.lock --------------------------------
    $lock = Join-Path $wt 'Cargo.lock'
    $lockBefore = (Get-FileHash $lock).Hash
    $res.lock_update = Invoke-Logged 'lock' { cargo metadata --format-version 1 --manifest-path $rootToml | Out-Null }
    $lockText = Get-Content $lock -Raw
    $res.lock_has_member = $lockText.Contains("name = `"$Pkg`"")
    $res.lock_has_cxx_qt = $lockText -match 'name = "cxx-qt"\r?\nversion = "0\.10\.0"'
    $diff = & git -C $wt diff --numstat -- Cargo.lock
    $res.lock_diff_numstat = "$diff".Trim()
    $lockAfterMeta = (Get-FileHash $lock).Hash

    # ---- 3. Qt-free: workspace build and the member's clippy --------------------------------
    $saved = @{ PATH = $env:PATH; QMAKE = $env:QMAKE; QTDIR = $env:QTDIR; CMAKE_PREFIX_PATH = $env:CMAKE_PREFIX_PATH }
    $env:PATH = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch '\\Qt\\|\\Qt$|Qt6' }) -join ';'
    Remove-Item Env:QMAKE, Env:QTDIR, Env:CMAKE_PREFIX_PATH -ErrorAction SilentlyContinue
    $env:CARGO_TARGET_DIR = Join-Path $wt 'target'
    $res.qt_free_env = [ordered]@{
        qt_dirs_on_path = @(($env:PATH -split ';') | Where-Object { $_ -and (Test-Path (Join-Path $_ 'Qt6Core.dll')) })
        qmake_on_path = [bool](Get-Command qmake -ErrorAction SilentlyContinue)
        QMAKE = $env:QMAKE; QTDIR = $env:QTDIR }
    Push-Location $wt
    try {
        $res.workspace_build = Invoke-Logged 'workspace-build' { cargo build --workspace --locked }
        $res.member_clippy = Invoke-Logged 'clippy' { cargo clippy -p $Pkg --all-targets --locked -- -D warnings }
        $res.member_test = Invoke-Logged 'member-test' { cargo test -p $Pkg --locked }
        $treeOut = & cargo tree -p $Pkg --locked -e normal,build 2>&1
        $res.member_tree_without_qt_has_cxx_qt = [bool]($treeOut -match 'cxx-qt')
    } finally {
        Pop-Location
        foreach ($k in $saved.Keys) { if ($null -ne $saved[$k]) { Set-Item "Env:$k" $saved[$k] } }
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    }
    $lockAfterCargo = (Get-FileHash $lock).Hash

    # ---- 4. with Qt: the CMake import and the QML counter -----------------------------------
    $proj = Join-Path $wt 'qt-probe'
    New-Item -ItemType Directory -Force $proj | Out-Null
    @'
cmake_minimum_required(VERSION 3.24)
project(lil_qt_probe_app LANGUAGES CXX)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml)
include(FetchContent)
FetchContent_Declare(CxxQt
    GIT_REPOSITORY https://github.com/KDAB/cxx-qt-cmake.git
    GIT_TAG 0.10.0
    GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(CxxQt)
cxx_qt_import_crate(MANIFEST_PATH ../crates/@MEMBER@/Cargo.toml CRATES @MEMBER@ LOCKED FEATURES qt QT_MODULES Qt6::Core Qt6::Gui Qt6::Qml)
cxx_qt_import_qml_module(lil_qt_probe_qml URI "LilQtProbe" SOURCE_CRATE @LIBTARGET@)
qt_add_executable(probe main.cpp)
qt_add_resources(probe "probe_qml" PREFIX "/" FILES Probe.qml)
target_link_libraries(probe PRIVATE Qt6::Gui Qt6::Qml lil_qt_probe_qml)
'@.Replace('@MEMBER@', $Pkg).Replace('@LIBTARGET@', $libTarget) | Write-Text (Join-Path $proj 'CMakeLists.txt')
    @'
#include <QGuiApplication>
#include <QQmlApplicationEngine>

int main(int argc, char *argv[])
{
    QGuiApplication app(argc, argv);
    QQmlApplicationEngine engine;
    engine.load(QUrl(QStringLiteral("qrc:/Probe.qml")));
    if (engine.rootObjects().isEmpty())
        return 2;
    return app.exec();
}
'@ | Write-Text (Join-Path $proj 'main.cpp')
    @'
import QtQml
import LilQtProbe

QtObject {
    property ProbeCounter counter: ProbeCounter {}
    Component.onCompleted: {
        counter.increment(); counter.increment(); counter.increment()
        console.log("probe_count " + counter.count)
        Qt.exit(counter.count === 3 ? 0 : 1)
    }
}
'@ | Write-Text (Join-Path $proj 'Probe.qml')
    $pb = Join-Path $proj 'build'
    $res.cmake_configure = Invoke-Logged 'cmake-configure' { cmake -S $proj -B $pb -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo }
    if ($res.cmake_configure.exit -eq 0) {
        $res.cmake_build = Invoke-Logged 'cmake-build' { cmake --build $pb }
    }
    $probeExe = Join-Path $pb 'bin\probe.exe'
    if (Test-Path $probeExe) {
        $missing = @('Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll' | Where-Object { -not (Test-Path (Join-Path $env:QTDIR "bin\$_")) })
        if ($missing.Count) { throw "Qt DLLs missing from QTDIR\bin: $($missing -join ', ')" }
        $env:QT_QPA_PLATFORM = 'offscreen'
        $env:QT_FORCE_STDERR_LOGGING = '1'   # else Qt logs to the debugger when stderr isn't a console
        $p = Start-Process -FilePath $probeExe -NoNewWindow -PassThru -RedirectStandardError (Join-Path $wt 's02-probe.err') -RedirectStandardOutput (Join-Path $wt 's02-probe.out')
        if (-not $p.WaitForExit(60000)) { $p.Kill($true); $code = $null } else { $code = $p.ExitCode }
        Remove-Item Env:QT_QPA_PLATFORM, Env:QT_FORCE_STDERR_LOGGING
        $res.qml_roundtrip = [ordered]@{ exit = $code
            output = @((Get-Content (Join-Path $wt 's02-probe.err') -ErrorAction SilentlyContinue) + (Get-Content (Join-Path $wt 's02-probe.out') -ErrorAction SilentlyContinue) | Where-Object { $_ -match 'probe_count|rror' }) }
    } else {
        $res.qml_roundtrip = [ordered]@{ exit = $null; reason = 'probe.exe was not built' }
    }
    $lockAfterCmake = (Get-FileHash $lock).Hash
    $res.lock_unchanged_by_cargo_build = $lockAfterMeta -eq $lockAfterCargo
    $res.lock_unchanged_by_cmake_build = $lockAfterCargo -eq $lockAfterCmake

    $pass = $res.lock_has_member -and $res.lock_has_cxx_qt -and
            ($res.qt_free_env.qt_dirs_on_path.Count -eq 0) -and (-not $res.qt_free_env.qmake_on_path) -and
            ($res.workspace_build.exit -eq 0) -and ($res.member_clippy.exit -eq 0) -and
            ($res.cmake_configure.exit -eq 0) -and ($res.cmake_build.exit -eq 0) -and
            ($res.qml_roundtrip.exit -eq 0) -and ($res.qml_roundtrip.output -match 'probe_count 3') -and
            $res.lock_unchanged_by_cargo_build -and $res.lock_unchanged_by_cmake_build
    $res.verdict = if ($pass) { 'PASS' } else { 'FAIL' }
} catch {
    $res.verdict = 'FAIL'
    $res.error = "$($_.Exception.Message)"; $res.error_at = "$($_.InvocationInfo.PositionMessage)"
} finally {
    $res.minutes = [math]::Round($sw.Elapsed.TotalMinutes, 1)
    if (-not $Keep -and (Test-Path $wt)) {
        & git -C $Repo worktree remove --force $wt 2>&1 | Out-Null
        if (Test-Path $wt) { Remove-Item -Recurse -Force $wt -ErrorAction SilentlyContinue }
        & git -C $Repo worktree prune 2>&1 | Out-Null
    }
    $res.worktree_removed = -not (Test-Path $wt)
    New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
    $res | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 $Out
    Write-Host "S0.2: $($res.verdict) ($($res.minutes) min)"
}
