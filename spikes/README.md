# Spikes

Everything under `spikes/` is **throwaway**. A spike answers one question with a small program and a scripted measurement, then its result is written down. Spike code is never promoted to product code: the implementation plans re-implement what they need properly. Delete a spike folder freely once its results document is written.

## Layout

```
spikes/
  README.md                 this file
  toolchain/
    env.ps1                 dot-source to get Rust, CMake, Ninja, MSVC and Qt in one shell
    check-toolchain.ps1     prints one line per tool, exits non-zero if any is missing
  <name>/                   one folder per spike (e.g. 02-qt-stack)
    ...                     sources, scripts
    results/                output of the measurement script (only images are committed)
docs/spikes/
  TEMPLATE.md               copy this for every new results document
  <nn>-<name>.md            one results document per spike
```

## Rules

1. **Results go in `docs/spikes/`.** Copy `docs/spikes/TEMPLATE.md` to `docs/spikes/<nn>-<name>.md` and fill every section. The pass criteria are copied from the plan *before* the spike is run.
2. **Every spike starts from `env.ps1`:** `. spikes/toolchain/env.ps1` (PowerShell 7).
3. **Measurements** are written as JSON into `spikes/<name>/results/`. All timings are in milliseconds and all memory in MiB. Each series reports n, p50, p95, p99 and max, and the raw samples are kept in the JSON. Only image results are committed; raw JSON, logs and text outputs stay local and are kept privately.
4. **Versions at the top.** GPU, driver, ComfyUI and Qt versions go at the top of every results document.
5. **Isolation.** Spikes never write to `%APPDATA%\Worldwright`, `%LOCALAPPDATA%\Worldwright`, Credential Manager, HKCU suite keys or the user's workspace (`<workspace>`). Use your own temp folders. The only Task Scheduler task a spike may create is `WorldwrightSpikeTask`, deleted at the end.
6. **No secrets** in any file, log or commit.
7. **Ask the owner first** before any install, model download or download over 2 GB.
