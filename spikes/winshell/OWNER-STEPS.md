# Spike 11: what you run, and what you do

Plan 4a, Task 1. Since 2026-10-23 a session is **one command**. The scripts watch the screen themselves, so they no longer ask "did a toast appear?" or "is it in Win+N?". You do only what needs a person: a few clicks, Windows Hello, one sleep and one lock.

Nothing starts until you press Enter after the plan is printed.

## Before the first session (5 minutes, once)

1. **Rust is installed.** In Windows Terminal, `cargo --version` prints a version number.
2. **Windows Hello is set up**: a PIN, finger or face (Settings, Accounts, Sign-in options).
3. **Windows Sandbox is on.** Type `Windows Sandbox` in Start: if it is listed, it is on. If not, see "Turn on Windows Sandbox" at the end.
4. **Visual Studio Build Tools** with "Desktop development with C++" (the Sandbox part copies three Microsoft runtime files from it).
5. **Task 0's app is built**: Task 0's worktree has the folder `spikes\app-shell\build\deploy`.
6. **Save your work** in other programs. The PC sleeps once, and at the end it must sit untouched for a long time.

## The session

1. Open **Windows Terminal normally** (not "Run as administrator"; the script refuses an elevated terminal, because Windows hides toasts from elevated programs).
2. Go to this repository's folder: `cd ` and the folder's path.
3. Type, with the folder of Task 0's worktree in the quotes:

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Task0Repo "<folder of Task 0's worktree>"
   ```

   If you leave out `-Task0Repo`, the script looks in this worktree, and if Task 0's app isn't there it prints the exact command to type instead (it lists the worktrees that have it). Setting the environment variable `LILITH_TASK0_REPO` to that folder works too.
4. The script prints the plan and the time it will take on this PC, then waits for Enter.

### What happens, and when you act

**About 3 hours 25 minutes in all with a 2-hour sleep timeout** (the script prints the figure for your PC). The last 2 hours or so need nobody.

| Part | Time | What the PC shows | What you do |
|---|---|---|---|
| Calibration | under a minute | one toast, "Calibration toast" | nothing. If the script can't see it, it asks you once whether it showed; then it asks a few simple questions later instead of detecting |
| Toasts that stay (S1.2p) | about 2 min | two toasts; the notification centre opens and closes by itself | **hands off** |
| Clicking toasts (S1.3) | about 4 min | six toasts, clicked by the script; small "winshell-app" windows; the Run box opens and types a link by itself | **hands off**: the mouse moves by itself |
| Foreground (S1.4) | about 14 min | in each trial two small windows, "winshell-app" and "winshell blocker" (the script clicks the blocker to the front), then a toast | **YOUR TURN**: click the toast's text, 10 times (15 if the first method fails). A real click is what this check measures. Then 5 more trials that the script clicks itself: hands off |
| Progress toasts (S1.5) | about 3 min | a toast with a progress bar; the notification centre opens, the script closes that toast | hands off. Only if the script can't close it, it says YOUR TURN: close "Spike job running" in Win+N with its X; it notices by itself |
| Sleep and wake (S1.7) | about 5 min | — | **YOUR TURN**: Start, Power, **Sleep**; wait about a minute; wake the PC and sign in. The script notices the wake |
| Windows Hello (S1.8) | about 4 min | Windows Hello prompts: 1, then up to 10, then 1 | **YOUR TURN**: complete each prompt. Keep the terminal in front; the script checks by itself whether each prompt comes in front of it |
| Sandbox, watched run | about 15 min | a Windows Sandbox window with things happening inside; then one of the spike's windows is placed over it | when the script beeps and says YOUR TURN: **lock the PC (Win+L)**, stay locked about 4 minutes, then sign in. It sees the lock and the sign-in |
| Sandbox, 3 runs | about 30 min | the Sandbox window opens and closes 3 times | nothing. Don't click inside the Sandbox window |
| Stay-awake (S1.7) | your sleep timeout plus about 6 min | nothing; the screen may turn off | **nobody touches the PC**, and it must not go to sleep. You can walk away. If someone touches it, the script notices (Windows' last-input time) and marks that wait as needing a rerun |

When it ends, the terminal prints each check's verdict (PASS, FAIL, INCOMPLETE or RECORDED) and the cleanup's result. Tell the agent the session is done; the results are in `spikes\winshell\results\`.

Whenever a part says YOUR TURN it is printed in yellow; everything else needs nothing from you.

## What the session changes, and how it is undone

Each part removes what it added and checks that it is gone (the line "Cleanup -> PASS").

| What | When | Undone |
|---|---|---|
| A Start-menu shortcut "LILITH Spike" | Part B and the stay-awake wait | deleted and checked |
| The link type `lilspike:` (`HKCU\Software\Classes\lilspike`) | same | deleted and checked |
| The spike's toast activator (`HKCU\Software\Classes\CLSID\{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}`) | from S1.2p on | deleted and checked |
| `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` | only if S1.2 is run | deleted at once, and checked |
| Windows' own notification setting for "LILITH Spike", and any `lilspike` entries under `ApplicationAssociationToasts` | added by Windows | deleted |
| The Windows Hello key "LILITH-spike-kek" | S1.8 | deleted; the cleanup passes only when Hello says "not found" |
| The spike's toasts in the notification centre | throughout | cleared |
| The power request "LILITH spike 11 keep-awake check" | S1.7 | released; the cleanup checks no `winshell-core` is left running |
| `%LOCALAPPDATA%\LILITH-spike\winshell` (a log) | throughout | copied into the results, then deleted |
| The mouse pointer | S1.3, S1.4, S1.5, the Sandbox run | it is put back where it was after each synthesized click |
| `spikes\winshell\build\` and `spikes\winshell\results\` | throughout | kept for the agent; git ignores both; delete them whenever you like |
| Inside the Sandbox: a shortcut, the link type, logs | Part C | gone when the Sandbox closes. The Sandbox has no network, clipboard, microphone, camera or printer; only two folders connect it to this PC |

The script inside the Sandbox (`inside.ps1`) first checks that it really is inside the Sandbox; on this PC it stops at once and changes nothing. No system or power setting is changed by any script.

## If something looks wrong

- **You want to stop:** press **Ctrl+C**. The part that is running cleans up, and the Sandbox it started (only that one) is closed.
- **A check ends as INCOMPLETE:** something couldn't be decided (a click that never came, input during the stay-awake wait). Nothing is wrong with the PC. Tell the agent, who will give you a shorter command, for example `-Only S1.4`.
- **A check stops with an error** (a red line "... stopped with an error ... Going on"): nothing to do; the error is recorded and the session goes on.
- **"Task 0's deploy folder is missing":** type the command the script printed (it has the `-Task0Repo` folder in it), or wait for Task 0's run.
- **"A Windows Sandbox is already running":** close your own Sandbox window, then run the command again.
- **The terminal closed or the PC restarted in the middle:** in a normal terminal in the repository folder, type `pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -CleanupOnly`. It adds nothing, removes what is left and prints what remains.
- **To repeat only some checks:** add `-Only` with the names, commas and no spaces, for example `-Only S1.4,S1.9` (S1.9 is the Sandbox part). A misspelt name stops the script at once, before it does anything.
- **Earlier results are kept:** each session moves the previous `checks.json` and `raw\` into `results\run-<date and time>\` first. The console of each session is in `results\raw\console.log`.

## Turn on Windows Sandbox (only if it isn't on; about 10 minutes with a restart)

1. Task Manager (Ctrl+Shift+Esc), Performance, CPU: "Virtualization: Enabled". If it says Disabled, it must be turned on in the PC's firmware setup first (Intel VT-x or AMD-V/SVM); stop and ask if unsure.
2. Settings, System, Optional features, **More Windows features**: tick **Windows Sandbox**, OK, restart when asked.
3. To turn it off later: untick it in the same list and restart.
