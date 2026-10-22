# Spike 10: the owner's steps (plan 4a, Task 0, Step 3)

You run these checks yourself. Each one needs you present, or has an effect outside the spike: ComfyUI, a reboot, a Defender scan, Narrator or a workflow push. The agent prepared them and stopped.

- Run them in a PowerShell 7 window from the repository root of the task worktree (branch `p4a-task0`), in the order below.
- Close heavy work first: every result records the machine's CPU load beside its timing.
- Each script writes its result under `spikes/app-shell/results/`, and its logs to a new timestamped folder under `spikes/app-shell/build/runs/` (both git-ignored). Hand the result files back to the agent for Step 4.

Before any of them, `spikes/app-shell/build/deploy/` must exist. If it doesn't, rebuild and redeploy (about 5 minutes on an idle machine; spike windows open and close without taking focus):

```powershell
pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Only S0.4
```

## 1. S0.5 (b): the first start after a reboot (about 10 minutes with the reboot)

1. Reboot. Log in, and wait until the desktop has settled (about 1 minute).
2. Start nothing else, then run:

   ```powershell
   pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label after-reboot
   ```

   It starts the deployed spike once (`--no-activate`), quits it after 2 s, and writes `results/s05-after-reboot.json` with `first_frame`, `interactive` and the CPU load. PASS = `interactive` < 2,000 ms.

   If the window takes more than 30 s to appear, leave it alone. The script keeps its log and the stack dump, and they go to the agent at once (spike 02's 145 s stall).

## 2. S0.5 (c): the first start of a fresh copy after a Defender scan (about 5 minutes)

```powershell
$copy = Join-Path $env:TEMP 'lilith-spike10-fresh'
Remove-Item -Recurse -Force $copy -ErrorAction SilentlyContinue
Copy-Item -Recurse spikes/app-shell/build/deploy $copy
& "$env:ProgramFiles\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File $copy
pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label after-defender-scan -Folder $copy -ColdCache
Remove-Item -Recurse -Force $copy
```

- `-ColdCache` moves the spike's QML and pipeline caches (`%LOCALAPPDATA%\LILITH-spike\image\cache`) aside for this one start and puts them back afterwards. So this measures a first install: fresh files and empty caches.
- On some setups the scan needs an elevated window. If `MpCmdRun` says access is denied, run just that line in an administrator PowerShell.
- PASS = `interactive` < 2,000 ms.

## 3. S0.6: GUI stalls under ComfyUI load (about 10 minutes)

You run this script yourself, when you're ready; no agent starts it. It:
- starts ComfyUI on `127.0.0.1:8188`, and refuses if something already answers there;
- runs the spike's own Z-Image Turbo int8 loop (1024×1024, batch 2, 8 steps, preview images only, so nothing is saved to ComfyUI's output folder);
- samples GPU utilisation twice a second, and waits until it has been ≥ 95 % for 10 s;
- runs the spike for 5 s of warm-up plus 60 s measured, then stops ComfyUI.

Before you start it:
- **The 240 Hz monitor must be on and primary.** `main-a` opens at (40, 40) on the primary screen, and the result records that screen's refresh rate.
- The spike window stays on top for that minute. Don't touch the mouse or keyboard while it runs.
- Under this load, the Web dock sits below Jobs and Grid, so the scrolled list stays visible throughout.

```powershell
pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1
```

Result: `results/s06.json`.
- **PASS** = heartbeat lateness p99 < 16 ms, and frame interval p99 under the frame threshold. The threshold is 16 ms, or 1.5 vsync periods on a 60 Hz screen.
- **FAIL** = a crash, a hang, a missing report, or a slower result; the stack files are named in the result.
- **INVALID** = the measurement conditions weren't met: GPU samples too sparse, the GPU under 95 %, the list hidden, or no refresh rate. Fix the condition and run it again.
- It also records the spike's dedicated VRAM (`Local Usage`) with the web dock shown, for P4a-R15.

If it FAILs, run the fallback once:

```powershell
pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1 -RateHz 5 -ChartsBatched
```

## 4. S0.7: Narrator smoke check (about 10 minutes)

1. Start the spike normally. It takes focus this time, which Narrator needs:

   ```powershell
   pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label narrator -Interactive
   ```

2. Turn Narrator on (Ctrl+Win+Enter). Click into the `main-a` window. Then press Tab and Shift+Tab through it, through `main-b`, and through the floating Counter window.
3. Write yes or no for each item in `spikes/app-shell/results/s07.txt` (or just tell the agent):
   - Is a KDDockWidgets title bar read (for example "Jobs" or "Grid")?
   - Is a dock tab read ("Grid" / "Web" in `main-a`)?
   - Is the counter button read by its accessible name, "Increment the counter"?
   - Is a chart tile in `main-b` read as a chart with its name (for example "GPU load chart, latest 70.0 %")?
   - When you press the counter button (Space while it has focus), does Narrator say "Count is now 1" (the `Accessible.announce()` message)?
4. Turn Narrator off (Ctrl+Win+Enter) and close the spike's windows.

## 5. S0.9 and S0.10: the scratch CI run (about 60–75 minutes, mostly waiting)

Agents' tokens can't change workflow files (P4a-R32), so you push this one. The workflow is ready at `spikes/app-shell/ci/spike10-qt.yml`.

**Pushing publishes Task 0's commits** on `p4a-task0` (the spike code, the scripts, this file and the results document's §1–4) before their review is merged. The privacy scan of those commits was clean.

1. In the task worktree, make a scratch branch from `p4a-task0` (never `main`) and push it:

   ```powershell
   git switch -c spike10-ci p4a-task0
   Copy-Item spikes/app-shell/ci/spike10-qt.yml .github/workflows/spike10-qt.yml
   git add .github/workflows/spike10-qt.yml
   git commit -m "spike 10: scratch CI run (S0.9, S0.10)"
   git push -u origin spike10-ci
   ```

   The push starts the run. There is no Run button: a manually triggered workflow that isn't on the default branch doesn't get one.
2. In GitHub, open Actions, then the "spike10-qt (scratch)" run on `spike10-ci`.
3. When it finishes, note the job's total time from the run page. Copy the lines that start with `S0.9 [`, the "Qt install" and "Spike build" times, and the last step's "S0.10 job time" into `spikes/app-shell/results/s09-s10-ci.txt`, or give them to the agent. The run uploads no artifact; everything is in the log.
4. If the "Install the Qt pair with naqt" step fails with a sha256 mismatch, naqt's rolling `latest` build has changed since spike 01. Tell the agent; the pin needs a decision before the run can count.
5. Delete the branch afterwards: run `git push origin --delete spike10-ci`, then `git switch p4a-task0` and `git branch -D spike10-ci`.

- **S0.9 PASS** = every item of the offscreen Qt Quick Test passes on `windows-2025`.
- **S0.10 PASS** = the whole job (the `pr-ready` steps plus Qt) stays under 50 minutes.
