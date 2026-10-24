# Spike 11: how it runs

Plan 4a, Task 1. Since the scope cut (2026-10-23) the default run is **S1.9 only, unattended**: the desktop checks inside Windows Sandbox. Part B (S1.1 to S1.8, on the host desktop) is answered by the owner's three recorded runs (`docs/spikes/11-windows-integration.md`, §5) and is not re-run. Nobody need be at the PC: the run asks nothing, posts no toast on the host, creates no Windows Hello key, moves no mouse and presses no key on the host, and every wait has a time limit.

The coordinator starts it; this doc is here so anyone can see what it does and re-run it.

## Before the first run (once)

1. **Rust is installed.** In Windows Terminal, `cargo --version` prints a version number.
2. **Windows Sandbox is on.** Type `Windows Sandbox` in Start: if it is listed, it is on. If not, see "Turn on Windows Sandbox" at the end.
3. **Visual Studio Build Tools** with "Desktop development with C++" (the run copies three Microsoft runtime files from it into the Sandbox bundle).
4. **Task 0's app is built**: Task 0's worktree has the folder `spikes\app-shell\build\deploy`.

## The command

From a normal **Windows Terminal** (not "Run as administrator"), in this repository's folder:

```powershell
pwsh -NoProfile -NonInteractive -File spikes/winshell/run-all.ps1 -Task0Repo "<folder of Task 0's worktree>"
```

Put the folder of Task 0's worktree in the quotes (or set the environment variable `LILITH_TASK0_REPO` to it). If the deploy folder can't be found (or Windows Sandbox is off), the run stops before starting a Sandbox, says why, and exits with an error code.

It runs `run-sandbox.ps1 -Runs 3 -KeepAwake` and nothing else:

- it builds the spike (`cargo build --release`), copies Task 0's deploy folder as it is into `spikes\winshell\build\sandbox\deploy-copy`, and finds the three runtime files;
- a Windows Sandbox window opens 3 times. Each Sandbox shuts itself down at the end of its run, and the script then closes its window (the viewer `wsb connect` opens, which stays open otherwise). Each run is limited to 120 s to start and 600 s to finish; a run that goes over is recorded and its Sandbox closed;
- the PC is kept awake for the runs (a power request held by a spike program that watches the script, released at the end). A run during which the PC slept anyway is recorded as interrupted, not as a failure;
- whether the PC is locked is read (never changed) every few seconds during each run and recorded. The keep-awake doesn't stop the screen turning off or the PC locking; a toast, foreground or Qt item that fails while the PC was locked is recorded as "host locked: not judged" rather than as a failure.

The Qt item inside the Sandbox starts Task 0's app on the normal (on-screen) platform and grabs its window. The offscreen grab is **not tested**: the trimmed deploy ships no `qoffscreen.dll`, and CI's S0.9 covers offscreen. If the deploy has no `platforms\qwindows.dll`, the app is not started and that is recorded (Qt would otherwise show a blocking error box).

When it ends, the terminal prints the verdict per run and overall. The overall verdict is PASS only when all 3 runs finished and passed (it reads "PASS (Qt: on-screen grab only; offscreen not tested)"); fewer finished runs give INCOMPLETE. The results are in `spikes\winshell\results\`: `sandbox-checks.json`, and each run's files in `results\sandbox\run-<n>\`. **Ctrl+C** stops it at any time and still cleans up.

**Part B is not runnable.** Its code is unmaintained: it is the full-automation code that never ran on a real desktop, and its review findings are still open, so its verdicts would not be reliable. Part B's results are the owner's recorded runs. Asking for a Part B check stops the script before it does anything, and says why.

## What the default run changes, and how it is undone

| What | Undone |
|---|---|
| A Windows Sandbox (the `WindowsSandbox` processes this run started) and its window | the Sandbox shuts itself down after each run; the script then closes the run's own Sandbox window (`CloseMainWindow`, then it is ended if it doesn't close within 10 s), and anything still left is recorded in the run's `lifecycle.json`. On any other exit the script stops the Sandbox it started. Only the processes this run started are touched |
| The power request "LILITH spike 11 keep-awake check" | released by the script at the end; if the terminal is closed instead, the program holding it notices and releases it within a second |
| A permission for SYSTEM to write to `spikes\winshell\build\sandbox\run-<n>\results\` (the Sandbox writes its results through it), added only when that folder doesn't already inherit one | the folder is inside this worktree; delete it whenever you like |
| `spikes\winshell\build\` and `spikes\winshell\results\` | kept for the agent; git ignores both; delete them whenever you like |
| Inside the Sandbox: a shortcut, the link type `lilspike:`, logs | gone when the Sandbox closes. The Sandbox has no network, clipboard, microphone, camera or printer; only the bundle, the deploy copy (both read-only) and the run's results folder connect it to this PC |

Nothing else on the host is written: no shortcut, no registry entry, no Hello key, no toast, no setting. The script inside the Sandbox (`inside.ps1`) first checks that it really is inside the Sandbox; on this PC it stops at once and changes nothing.

## If something looks wrong

- **You want to stop:** press **Ctrl+C**. The Sandbox it started (only that one) is closed and the keep-awake is released.
- **"Task 0's deploy folder is missing":** add `-Task0Repo "<folder of Task 0's worktree>"`, or wait for Task 0's build.
- **"A Windows Sandbox is already running":** close your own Sandbox window, then run the command again.
- **"Windows Sandbox is not turned on":** see below.

## Turn on Windows Sandbox (only if it isn't on; about 10 minutes with a restart)

1. Task Manager (Ctrl+Shift+Esc), Performance, CPU: "Virtualization: Enabled". If it says Disabled, it must be turned on in the PC's firmware setup first (Intel VT-x or AMD-V/SVM); stop and ask if unsure.
2. Settings, System, Optional features, **More Windows features**: tick **Windows Sandbox**, OK, restart when asked.
3. To turn it off later: untick it in the same list and restart.
