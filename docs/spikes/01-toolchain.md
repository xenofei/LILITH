# 01 Toolchain, installer decisions and version pins

Date: 2026-10-12

## 1. Question

- **QA-078:** which installer does CI use for the Qt pair: `aqtinstall` (a version with the Qt 6.12 fix, PR #1048), `naqt`, or the official Qt installer CLI?
- **OPS-007:** where does the git-lfs upgrade (floor 3.8) come from: a newer Git for Windows that bundles git-lfs >= 3.8, or the standalone git-lfs installer?
- **QA-077 (pin):** the exact Qt 6.12 + Qt WebEngine pair.
- Side goal: install the toolchain every later spike dot-sources via `spikes/toolchain/env.ps1`.

## 2. Setup

Machine: the M0 test machine, Windows 11 Pro 10.0.26200, PowerShell 7.6.6. All installs were approved before they ran (2026-10-12).

| Tool | Version | Source | Path |
|---|---|---|---|
| rustc / cargo | 1.99.0 (b940084d7 2026-09-28) / 1.99.0 (5f94df478 2026-08-27) | rustup, `stable-x86_64-pc-windows-msvc` (updated from 1.93.1) | `%USERPROFILE%\.cargo\bin` |
| CMake | 4.4.4 | `winget install Kitware.CMake` | `C:\Program Files\CMake\bin` |
| Ninja | 1.13.2 | `winget install Ninja-build.Ninja` | WinGet Packages dir |
| MSVC (`cl`) | 19.44.35227 (toolset dir 14.44.35207, VS 2022 17.14 Build Tools) | pre-installed | `...\BuildTools\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64` |
| Windows SDK | 10.0.26100 | pre-installed | `C:\Program Files (x86)\Windows Kits\10` |
| **Qt** | **6.12.0** (`win64_msvc2022_64`, build 202609280346) | naqt, see section 7 | `C:\Qt\6.12.0\msvc2022_64` |
| **Qt WebEngine** | **6.140.0** (build 202609281005; Chromium 140 base per the Qt blog) | naqt `--extensions qtwebengine@6.140.0` | same prefix |
| Qt modules added | qtwebchannel, qtpositioning, qtshadertools, qtimageformats (all 6.12.0), plus the base set (qtbase, qtsvg, qtdeclarative, qttools, qttranslations, d3dcompiler_47, opengl32sw) | naqt | same prefix |
| `qmake` / `qsb` / `windeployqt` | 6.12.0 each | Qt | `C:\Qt\6.12.0\msvc2022_64\bin` |
| naqt | source commit `43da90f1` "Support Qt 6.12" (2026-09-04); prebuilt `naqt.zip` sha256 `59825326d65895c7f0d57716768e014c5e1f8fb6e1e10244ea1a00acb886cc8f`; needs the .NET 10 runtime (10.0.12 present) | https://github.com/jdpurcell/naqt/releases/tag/latest | `%USERPROFILE%\.worldwright\tools\naqt\bin` |
| Git | 2.56.0.windows.2 (was 2.55.0.windows.5) | Git for Windows installer, see section 7 | `C:\Program Files\Git\cmd` |
| git-lfs | 3.8.0 (was 3.7.1) | bundled with Git for Windows | `C:\Program Files\Git\cmd` |
| Python / uv / gh / dotnet SDK | 3.14 / present / present / 10.0.401 | pre-installed | |

Disk used by `C:\Qt`: about 3.6 GB.

Re-run the check:

```powershell
pwsh -Command ". spikes/toolchain/env.ps1; ./spikes/toolchain/check-toolchain.ps1"
```

## 3. Method

1. `check-toolchain.ps1` written first and run on the bare machine (RED).
2. Qt installer decision made by looking things up without installing (section 7).
3. Installs, then `env.ps1`, then the check again (GREEN).
4. A scratch CMake project (Ninja, Release) that does `find_package(Qt6 COMPONENTS Core Quick WebEngineQuick WebChannel Positioning ShaderTools)` and links `Qt6::WebEngineQuick` was configured, built and linked from a fresh `pwsh` after dot-sourcing `env.ps1`.
5. Git/git-lfs source decision from the Git for Windows release notes; upgrade performed.

## 4. Pass criteria

- `check-toolchain.ps1` exits non-zero on the bare machine and lists every missing tool.
- After install, from a fresh `pwsh` that dot-sources `env.ps1`, `check-toolchain.ps1` exits 0, prints a version and path for every tool, Qt reports 6.12.x and a Qt WebEngine version is printed.
- The Qt + WebEngine pair is pinned with exact versions.
- QA-078 and OPS-007 each have a recorded decision with evidence.

## 5. Measurements

No timings. Evidence:

| Check | Result |
|---|---|
| Bare machine (RED) | exit 1; missing: rustc, cargo, cmake, ninja, cl, qmake, qsb, windeployqt, qtwebengine; git 2.55.0, git-lfs 3.7.1 present |
| After install (GREEN) | exit 0; all 10 tools plus `qtwebengine 6.140.0` |
| Scratch CMake project | configure OK, `t.exe` built and linked against `Qt6::WebEngineQuick` |

`env.ps1` is idempotent: dot-sourcing twice leaves the same environment.

## 6. Verdict

**PASS.** Every pass criterion met. One caveat: a CMake configure that pulls in `Quick` prints warnings that `Qt6TaskTree` and Vulkan headers are not found (see Follow-ups); the build is unaffected.

## 7. Decision

### QA-078: **`naqt`**, pinned, with an explicit WebEngine version

Evidence (all looked up 2026-10-12, before installing):

- `aqtinstall`: PR #1048 "Qt 6.12 compatibility" (https://github.com/miurahr/aqtinstall/pull/1048) is **still open and unmerged** (`gh api repos/miurahr/aqtinstall/pulls/1048`: state open, merged false, updated 2026-10-05). The latest `aqtinstall` release on both GitHub and PyPI is **3.3.0 (2025-06-02)**, and no release notes mention 1048. So no `aqtinstall` release contains the fix, and the decision rule falls through to `naqt`.
- `naqt` (https://github.com/jdpurcell/naqt, MIT): commit `43da90f1` "Support Qt 6.12" (2026-09-04); the `latest` release asset was rebuilt 2026-09-05. Verified live:
  - `naqt list-qt windows desktop 6.12.0 win64_msvc2022_64` lists the modules.
  - `naqt install-qt ... --dryrun` resolves Qt 6.12.0 and the WebEngine extension.
  - The real install ran to completion (exit 0, 22 s) and produced a working tree (section 5).
- **Trap found:** Qt 6.12 publishes two WebEngine builds under `extensions/qtwebengine/6120/`. With `--modules qtwebengine` (or `--extensions qtwebengine` with no version), `naqt` picks the **older July 2026 build** (`6.12.0-0-202607081027`, a pre-release baseline). The matching GA build is `6120/61400/` (`6.12.0-0-202609281005`), which is Qt WebEngine **6.140.0**. It must be requested explicitly as `--extensions qtwebengine@6.140.0`. The Qt blog (https://www.qt.io/blog/qt-webengine-will-release-separate) says WebEngine is versioned separately from 6.12 on, that 6.140.0 releases alongside Qt 6.12.0 and keeps the Chromium 140 base. The installed `QtWebEngineProcess.exe` and `modules\WebEngineCore.json` both report 6.140.0.
- Official CLI (`qt-unified-windows-x64-online.exe`): not evaluated. The rule says use it only if neither `aqtinstall` nor `naqt` installs the matching WebEngine, and `naqt` does. It also needs a Qt account.

Command used (reproduce, and use in CI):

```powershell
dotnet naqt.dll install-qt windows desktop 6.12.0 win64_msvc2022_64 --outputdir C:\Qt `
  --modules qtwebchannel qtpositioning qtshadertools qtimageformats `
  --extensions qtwebengine@6.140.0
```

CI notes: pin `naqt` by source commit `43da90f1` (or the zip sha256 above), because the `latest` release tag is a rolling one. `jdpurcell/install-qt-action@v5` with `use-naqt: true` is the Actions wrapper. Re-check `aqtinstall` when PR #1048 merges; switching back is then optional.

### QA-077 pin: **Qt 6.12.0 + Qt WebEngine 6.140.0** (win64_msvc2022_64, MSVC 14.44).

### OPS-007: **Git for Windows >= 2.56.0.2** as the upgrade source

- Git for Windows v2.56.0(2) (https://github.com/git-for-windows/git/releases/tag/v2.56.0.windows.2, 2026-10-05): the release notes in `git-for-windows/build-extra/ReleaseNotes.md` say "Comes with Git LFS v3.8.0". 3.8.0 meets the floor (git-lfs 3.8.0 was released 2026-08-28).
- Decision: the source is "Git for Windows >= 2.56.0.2" (bundles git-lfs 3.8.0). Only 2.56.0(2) was verified, so the floor names that build. The standalone git-lfs installer is not needed.
- Caveat: **the winget `Git.Git` manifest is still at 2.55.0.5** (`winget upgrade --id Git.Git` finds no upgrade), so `ww doctor` can't rely on winget alone yet. On the test machine the upgrade was done with the official `Git-2.56.0.2-64-bit.exe` from the GitHub release, after checking its SHA-256 against the release notes (`52188f91...60f6`, matched) and its Authenticode signature (valid). It ran `/VERYSILENT /NORESTART`; the upgrade had been approved beforehand. Result: `git version 2.56.0.windows.2`, `git-lfs/3.8.0`.
- Side effect to know about: 2.56 moved to UCRT64, so `git.exe` is now `C:\Program Files\Git\cmd\git.exe`, not `...\mingw64\bin\git.exe`. Anything that hard-coded the old path breaks; use PATH lookup.

## 8. Follow-ups

- **Spec (QA-078 / QA-077):** record naqt + `qtwebengine@6.140.0` as the CI installer and the pair 6.12.0 / 6.140.0. Add a CI check that fails if the installed WebEngine is not 6.140.0 (the default naqt pick is the wrong, older build).
- **Spec (OPS-007):** the upgrade source is Git for Windows >= 2.56.0.2 (git-lfs 3.8.0). The doctor needs a fallback for winget lagging behind: the direct GitHub release installer with hash verification.
- **Plan:** the Qt 6.12 Qt Quick configure warns that `Qt6TaskTree` is missing (needed by the `QmlAssetDownloaderPrivate` QML plugin) and that Vulkan headers are missing (`WrapVulkanHeaders`). Both are harmless for D3D11. Install `qttasktree` if the warning becomes a nuisance.
- **Plan:** `cl` reports 19.44.35227 while the toolset directory is 14.44.35207; both are the same MSVC 17.14 toolset, noted here to avoid confusion when pinning.
- The Qt WebEngine Chromium patch level was not verified from the binary; the Qt wiki Chromium table still marks 6.12 as unreleased (140.0.7339.264 expected).
