# Spike 10: the owner's steps (plan 4a, Task 0, Step 3)

## The one command (about 10-16 minutes; at most about 30 with the fallback)

Close every spike window, and don't start ComfyUI yourself. Then, in a PowerShell 7 window at the repository root of the task worktree (branch `p4a-task0`), run:

```powershell
pwsh -NoProfile -File spikes/app-shell/run-owner.ps1
```

- **What it does:**
  1. It refuses at once, with nothing started, if a spike process (anything running from `spikes/app-shell/build`) or ComfyUI is already running.
  2. It prints a time estimate.
  3. It rebuilds and redeploys (S0.4).
  4. It runs the accessibility check (S0.7, with UI Automation, so there's no Narrator and nothing for you to do).
  5. It runs S0.6 under ComfyUI load (section 3 below) whenever the deploy matches the build, even if S0.4 failed for another reason, and the 5 Hz fallback only if S0.6 FAILs.
  6. It prints one line per check at the end.
- **During the run:**
  - Whenever you must keep off the keyboard and mouse, a small notice appears at least 10 s ahead (in the bottom-right corner of your main screen; it never takes the focus). It counts down until the PC is free, and then says "Done". That happens twice: S0.7's click (about 10 s), and S0.6's measured minute (about 70 s).
  - Spike windows open without taking focus. S0.7's takes it when it clicks the counter: about 0.1–0.2 s normally, at most about 2 s if something goes wrong. The ones S0.4 and S0.7 open close by themselves within a minute.
  - During S0.6, the spike window stays on top for about a minute, so don't use the mouse or keyboard then.
  - Ctrl+C or closing the window stops everything, ComfyUI included.
- **Afterwards:** hand `spikes/app-shell/results/` (`checks.json`, `s07.json`, `s06*.json`) back to the agent.

The sections below are for reruns and for the checks the one command doesn't cover. S0.5 (b) and (c) passed on 2026-10-22, and S0.9 and S0.10 passed on CI on 2026-10-23.

- Run them in a PowerShell 7 window from the repository root of the task worktree, in the order below.
- Close heavy work first: every result records the machine's CPU load beside its timing.
- Each script writes its result under `spikes/app-shell/results/`, and its logs to a new timestamped folder under `spikes/app-shell/build/runs/` (both git-ignored). Runs on another deploy folder (`-DeployName deploy-<name>`) and `comfy-load.ps1 -SamplersOnly` write to `results/agent-runs/<stamp>/` instead, so they never replace your result files; `-ResultsDir` picks a folder.

Before any of them, `spikes/app-shell/build/deploy/` must exist and match the build. If it doesn't, close every spike window, then rebuild and redeploy (about 5 minutes on an idle machine; spike windows open and close without taking focus). The harness, `start-once.ps1 -ColdCache` and `comfy-load.ps1` refuse to run while a spike window is open, and say which one:

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
- runs the spike's own Z-Image Turbo int8 loop (1024×1024, batch 2, 20 steps, preview images only, so nothing is saved to ComfyUI's output folder), keeping a second prompt queued so the GPU doesn't idle between prompts;
- samples GPU utilisation twice a second, and starts the measurement only once the last 20 s meet the GPU condition (at most 5 minutes; if that never happens, the spike isn't run and the result is INVALID);
- runs the spike for 5 s of warm-up plus 60 s measured, then stops ComfyUI.

Before you start it:
- **Close any spike window that's still open**, then redeploy. The script refuses both an open spike window and a deploy folder that is older than the build:

  ```powershell
  pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Only S0.4
  ```

- Run it on whichever display is primary. The result records that display's refresh rate, and the frame threshold follows from it.
- The spike window stays on top for that minute. Don't touch the mouse or keyboard while it runs: a small notice warns you 10 s ahead, counts down, and says when you're free again.
- To stop it early, press Ctrl+C or close its console window: either way ComfyUI, the GPU sampler, the load and the spike all stop (they're in a Windows job object that ends with the script).
- Under this load, the Web dock sits below Jobs and Grid, so the scrolled list stays visible throughout.

```powershell
pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1
```

Result: `results/s06.json`.
- **PASS** = heartbeat lateness p99 < 16 ms, and `main-a`'s frame-interval p99 under the frame threshold. The threshold is the larger of 16 ms and 1.5 vsync periods of the primary display: 16 ms at 144 Hz and above, 20 ms at 75 Hz, 25 ms at 60 Hz. The result also counts frames slower than 1.5 vsync periods (dropped frames).
- **FAIL** = a crash, a hang, a missing report, or a slower result; the stack files are named in the result.
- **INVALID** = the measurement conditions weren't met: GPU samples too sparse, fewer than 90 % of the GPU samples at ≥ 95 %, the list hidden, or no refresh rate. Fix the condition and run it again. Brief GPU dips between prompts are allowed; each one is listed in the result.
- It also records the spike's dedicated VRAM (`Local Usage`) with the web dock shown, for P4a-R15.

If it FAILs, run the fallback once:

```powershell
pwsh -NoProfile -File spikes/app-shell/comfy-load.ps1 -RateHz 5 -ChartsBatched
```

## 4. S0.7: the accessibility check (automated; about 1 minute)

No Narrator and nothing to do: it reads the spike's windows through UI Automation, the same data a screen reader gets, then clicks the counter through UI Automation and closes the spike. The spike window takes the focus at that click, which Qt does when a button is invoked: about 0.1–0.2 s normally, at most about 2 s if the announcement is missed (and, only if closing the spike fails twice, until it quits by itself at 25 s). `run-owner.ps1` runs it, and so does the harness on its own:

```powershell
pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Only S0.7
```

Result: `results/s07.json`, with PASS or FAIL per item and the UIA values:
- a dock title bar named "Jobs" or "Grid";
- the tabs "Grid" and "Web" in `main-a`;
- the button "Increment the counter";
- the chart tile "GPU load chart, latest … %", with its control type;
- the announcement "Count is now 1".

## 5. S0.9 and S0.10: the scratch CI run (done: run 39894748059 passed on 2026-10-23; for a rerun only, about 60–75 minutes, mostly waiting)

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
