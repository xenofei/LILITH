# Spike 10: the owner's steps (plan 4a, Task 0, Step 3)

These checks need you present, or have an outside effect (ComfyUI, a reboot, a Defender scan, Narrator, a workflow push), so the agent prepared them and stopped. Run them in a PowerShell 7 window from the repository root, in this order. Each one writes its result under `spikes/app-shell/results/` (git-ignored); hand the files back to the agent for Step 4.

Before any of them: `spikes/app-shell/build/deploy/` must exist. If it doesn't, rebuild and redeploy first (about 5 minutes on an idle machine; it opens and closes spike windows without taking focus):

```powershell
pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Only S0.4
```

## 1. S0.5 (b): the first start after a reboot (about 10 minutes with the reboot)

1. Reboot. Log in, and wait until the desktop has settled (about 1 minute).
2. Start nothing else, then run:

   ```powershell
   pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label after-reboot
   ```

   It starts the deployed spike once (`--no-activate`), quits it after 2 s, and writes `results/s05-after-reboot.json` with `first_frame` and `interactive`. PASS = `interactive` < 2,000 ms. If the window takes longer than 30 s to appear, leave it alone: the script keeps its log and the stack dump, which go to the agent at once (spike 02's 145 s stall).

## 2. S0.5 (c): the first start of a fresh copy after a Defender scan (about 5 minutes)

```powershell
$copy = Join-Path $env:TEMP 'lilith-spike10-fresh'
Remove-Item -Recurse -Force $copy -ErrorAction SilentlyContinue
Copy-Item -Recurse spikes/app-shell/build/deploy $copy
& "$env:ProgramFiles\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File $copy
pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label after-defender-scan -Folder $copy
Remove-Item -Recurse -Force $copy
```

The scan needs an elevated window on some setups; if `MpCmdRun` says access is denied, run just that line in an administrator PowerShell. PASS = `interactive` < 2,000 ms.

## 3. S0.6: GUI stalls under ComfyUI load (about 10 minutes; say "go" first)

This starts ComfyUI on `127.0.0.1:8188` (it refuses if something already answers there), runs the spike's own Z-Image Turbo int8 loop (1024×1024, batch 2, 8 steps, preview images only, nothing saved to ComfyUI's output folder), waits until NVML GPU utilisation has been ≥ 95 % for 10 s, then runs the spike for 5 s of warm-up plus 60 s measured, and stops ComfyUI. The spike window stays on top for that minute; please don't touch the mouse or keyboard while it runs.

```powershell
pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1
```

Result: `results/s06.json`. PASS = heartbeat lateness p99 < 16 ms and frame interval p99 < 16 ms, with the GPU ≥ 95 % throughout (else the run is marked INVALID; run it again). It also records the spike's dedicated VRAM (`Local Usage`) with the web dock shown, for P4a-R15. If it FAILs, run the fallback once:

```powershell
pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1 -RateHz 5 -ChartsBatched
```

## 4. S0.7: Narrator smoke check (about 10 minutes)

1. Start the spike normally (it takes focus this time, which is what Narrator needs):

   ```powershell
   pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label narrator -Interactive
   ```

2. Turn Narrator on (Ctrl+Win+Enter). Click into the `main-a` window, then press Tab and Shift+Tab through it, and through `main-b` and the floating Counter window.
3. Write yes or no for each, in `spikes/app-shell/results/s07.txt` (or just tell the agent):
   - a KDDockWidgets title bar (for example "Jobs" or "Grid") is read;
   - a dock tab ("Grid" / "Web" in `main-a`) is read;
   - the counter button is read by its accessible name, "Increment the counter";
   - a chart tile in `main-b` is read as a chart with its name (for example "GPU load chart, latest 70.0 %");
   - pressing the counter button (Space while it has focus) makes Narrator say "Count is now 1" (the `Accessible.announce()` message).
4. Turn Narrator off (Ctrl+Win+Enter) and close the spike's windows.

## 5. S0.9 and S0.10: the scratch CI run (about 60 minutes, mostly waiting)

Agents' tokens can't change workflow files (P4a-R32), so this one is yours to push. The workflow is ready at `spikes/app-shell/ci/spike10-qt.yml`.

1. Copy it into a scratch branch and push it (never to `main`):

   ```powershell
   git switch -c spike10-ci
   Copy-Item spikes/app-shell/ci/spike10-qt.yml .github/workflows/spike10-qt.yml
   git add .github/workflows/spike10-qt.yml
   git commit -m "spike 10: scratch CI run (S0.9, S0.10)"
   git push -u origin spike10-ci
   ```

2. In GitHub, open Actions → "spike10-qt" and run it on `spike10-ci` (it also runs on the push).
3. When it finishes, note the total job time and download the `spike10-results` artifact; hand both to the agent.
4. Delete the branch afterwards: `git push origin --delete spike10-ci`, then `git switch -` and `git branch -D spike10-ci`.

PASS (S0.9) = every item of the offscreen Qt Quick Test passes on `windows-2025`. PASS (S0.10) = the whole job (the `pr-ready` steps plus Qt) stays under 50 minutes.
