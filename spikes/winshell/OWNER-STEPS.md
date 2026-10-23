# Spike 11: how it runs

Plan 4a, Task 1. Since the owner's round 12 answers (2026-10-23) this is **one command, unattended**. No one need be at the PC: every check decides itself, so there are no clicks, no watching, no Windows Hello gesture, no sleep and no lock to do. Toasts may pop and a Windows Hello prompt may appear and close by itself while it runs; a pop-up warns before any stretch that moves the mouse or presses keys, and nothing acts on your own windows.

The coordinator starts it; this doc is here so anyone can see what it does and re-run it.

## Before the first run (5 minutes, once)

1. **Rust is installed.** In Windows Terminal, `cargo --version` prints a version number.
2. **Windows Hello is set up**: a PIN, finger or face (Settings, Accounts, Sign-in options).
3. **Windows Sandbox is on.** Type `Windows Sandbox` in Start: if it is listed, it is on. If not, see "Turn on Windows Sandbox" at the end.
4. **Visual Studio Build Tools** with "Desktop development with C++" (the Sandbox part copies three Microsoft runtime files from it).
5. **Task 0's app is built**: Task 0's worktree has the folder `spikes\app-shell\build\deploy`.

## The one command

From a normal **Windows Terminal** (not "Run as administrator": Windows hides toasts from elevated programs), in this repository's folder:

```powershell
pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Task0Repo "<folder of Task 0's worktree>"
```

Put the folder of Task 0's worktree in the quotes (or set the environment variable `LILITH_TASK0_REPO` to it). If you leave it out and the Sandbox part can't find the deploy folder, the run does the host checks and prints how to add the folder for the Sandbox part.

It runs two stages, in order, with no input:

| Stage | What happens |
|---|---|
| Part B (host desktop) | the toast, activation, foreground, progress-toast, keep-awake and Windows Hello checks. Toasts pop; a Hello prompt may appear and close by itself (it is cancelled, so there is nothing to complete); small windows open and close. Each check decides itself and records how. |
| Part C (Windows Sandbox) | a Windows Sandbox window opens and closes 3 times by itself. The PC is kept awake for the runs (a power request, released at the end). A run during which the PC slept anyway is recorded as interrupted, not as a failure. |

**The hands-off pop-up** still appears at the top centre whenever the script is about to move the mouse or press keys:

1. **Amber, at least 10 seconds ahead:** "In N s: HANDS OFF the mouse and keyboard", and what is coming.
2. **Red:** "HANDS OFF", with a countdown until you can use the PC again.
3. **Green:** "Done: you can use the PC again". Then it closes.

It never takes the focus, and clicks pass through it. If you do use the mouse or keyboard, the script holds back and tries again. **Ctrl+C** stops it at any time and still cleans up.

**No Windows Hello gesture.** The Hello check verifies only that the prompt comes to the front of other apps, including a fullscreen one, and then cancels the prompt itself. The key creation and signatures were measured once, with you present, and are not repeated; the route decision is recorded from that.

When it ends, the terminal prints each check's verdict (PASS, FAIL, INCOMPLETE or RECORDED) and the cleanup's result. Tell the agent the run is done. The results are in `spikes\winshell\results\`: `checks.json` for the host checks and `sandbox-checks.json` for the Sandbox.

To re-run only some checks, add `-Only` with the names, commas and no spaces, for example `-Only S1.5,S1.9`.
## What the session changes, and how it is undone

Each part removes what it added and checks that it is gone (the line "Cleanup -> PASS").

| What | When | Undone |
|---|---|---|
| A Start-menu shortcut "LILITH Spike" | Part B and the stay-awake wait | deleted and checked |
| The link type `lilspike:` (`HKCU\Software\Classes\lilspike`) | same | deleted and checked |
| The spike's toast activator (`HKCU\Software\Classes\CLSID\{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}`) | from S1.2p on | deleted and checked |
| `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` | only if S1.2 is run | deleted at once, and checked |
| Windows' own notification setting for "LILITH Spike", and any `lilspike` entries under `ApplicationAssociationToasts` | added by Windows | deleted |
| The Windows Hello key "LILITH-spike-kek" | the owner's earlier run only; the automated run no longer creates it | deleted; the cleanup passes only when Hello says "not found" |
| The spike's toasts in the notification centre | throughout | cleared |
| The power request "LILITH spike 11 keep-awake check" | S1.7, and the Sandbox runs | released by the script; if the terminal is closed instead, the program holding it notices and releases it within a second; the cleanup checks no `winshell-core` is left running |
| `%LOCALAPPDATA%\LILITH-spike\winshell` (a log) | throughout | copied into the results, then deleted |
| The hands-off pop-up (a small window at the top centre) | before each hands-off stretch | it closes by itself when the stretch is done, and when the script stops |
| The mouse pointer | the S1.8 placement probe | it is put back where it was after each synthesized move |
| `spikes\winshell\build\` and `spikes\winshell\results\` | throughout | kept for the agent; git ignores both; delete them whenever you like |
| Inside the Sandbox: a shortcut, the link type, logs | Part C | gone when the Sandbox closes. The Sandbox has no network, clipboard, microphone, camera or printer; only two folders connect it to this PC |

The script inside the Sandbox (`inside.ps1`) first checks that it really is inside the Sandbox; on this PC it stops at once and changes nothing. No system or power setting is changed by any script.

## If something looks wrong

- **You want to stop:** press **Ctrl+C**. The part that is running cleans up, and the Sandbox it started (only that one) is closed.
- **A check ends as INCOMPLETE:** something couldn't be decided automatically. Nothing is wrong with the PC. Tell the agent, who will say what to do.
- **A check stops with an error** (a red line "... stopped with an error ... Going on"): nothing to do; the error is recorded and the run goes on.
- **"Task 0's deploy folder is missing":** add `-Task0Repo "<folder of Task 0's worktree>"`, or wait for Task 0's run. Without it the host checks still run and only the Sandbox part is left out.
- **"A Windows Sandbox is already running":** close your own Sandbox window, then run the command again.
- **The terminal closed or the PC restarted in the middle:** in a normal terminal in the repository folder, type `pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -CleanupOnly`. It adds nothing, removes what is left and prints what remains.
- **To repeat only some checks:** add `-Only` with the names, commas and no spaces, for example `-Only S1.5,S1.9` (S1.9 is the Sandbox part). A misspelt name stops the script at once, before it does anything.
- **Earlier results are kept:** the run moves the previous `checks.json` and `raw\` into `results\run-<date and time>\` first. The console is in `results\raw\console.log`.

## Turn on Windows Sandbox (only if it isn't on; about 10 minutes with a restart)

1. Task Manager (Ctrl+Shift+Esc), Performance, CPU: "Virtualization: Enabled". If it says Disabled, it must be turned on in the PC's firmware setup first (Intel VT-x or AMD-V/SVM); stop and ask if unsure.
2. Settings, System, Optional features, **More Windows features**: tick **Windows Sandbox**, OK, restart when asked.
3. To turn it off later: untick it in the same list and restart.
