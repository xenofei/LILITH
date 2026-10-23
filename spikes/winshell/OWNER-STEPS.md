# Spike 11: what you run, and what you do

Plan 4a, Task 1. Since 2026-10-23 a session is **two commands**: one for the part that needs you, one for the part that needs nobody. The scripts watch the screen themselves, so they no longer ask "did a toast appear?" or "is it in Win+N?". You do only what needs a person: a few clicks, Windows Hello, one sleep and one lock.

Nothing starts until you press Enter after the plan is printed.

## Before the first session (5 minutes, once)

1. **Rust is installed.** In Windows Terminal, `cargo --version` prints a version number.
2. **Windows Hello is set up**: a PIN, finger or face (Settings, Accounts, Sign-in options).
3. **Windows Sandbox is on.** Type `Windows Sandbox` in Start: if it is listed, it is on. If not, see "Turn on Windows Sandbox" at the end.
4. **Visual Studio Build Tools** with "Desktop development with C++" (the Sandbox part copies three Microsoft runtime files from it).
5. **Task 0's app is built**: Task 0's worktree has the folder `spikes\app-shell\build\deploy`.
6. **Save your work** in other programs. The PC sleeps once, and at the end it must sit untouched for a long time.

## The session: two blocks

A session is two commands (the two blocks below), run from a normal **Windows Terminal** (not "Run as administrator": Windows hides toasts from elevated programs) in this repository's folder. Put the folder of Task 0's worktree in the quotes. If you leave out `-Task0Repo`, the script prints the exact command to type instead; setting the environment variable `LILITH_TASK0_REPO` to that folder works too.

1. **The attended block, about 50 minutes, with you at the PC:**

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Block Attended -Task0Repo "<folder of Task 0's worktree>"
   ```

2. **The unattended block, with nobody at the PC.** Its length follows "Sleep after": about 45 minutes with it at 5 minutes (your plan), about 2 hours 40 minutes at 2 hours; the block prints its own figure before it starts. Start it when you leave. When the attended block ends it prints this command for you:

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Block Unattended -Task0Repo "<folder of Task 0's worktree>"
   ```

Each block prints its plan and its time on this PC, then waits for Enter. The unattended block then counts down 60 seconds so that you can leave.

**The hands-off pop-up.** A pop-up appears at the top centre of the screen whenever the script is about to move the mouse or press keys, and for the whole stay-awake wait:

1. **Amber, at least 10 seconds ahead:** "In N s: HANDS OFF the mouse and keyboard", and what is coming.
2. **Red:** "HANDS OFF", with a countdown until you can use the PC again.
3. **Green:** "Done: you can use the PC again". Then it closes.

**It stays out of the way.**
- It never takes the focus, and clicks pass through it, so it can't get in the way of what the script clicks or checks.
- The terminal also prints a yellow HANDS OFF line.

**If you touch the mouse or keyboard anyway,** the script holds back and tries again. After a few tries it hands that step to you.

**Ctrl+C** in the terminal stops it at any time. Whenever a step needs you, the script says **YOUR TURN** in yellow.

### The attended block

| Part | Time | What the PC shows | What you do |
|---|---|---|---|
| Calibration | under a minute | one toast, "Calibration toast" | nothing. If the script can't see it, it asks you once whether it showed, and then asks a few simple questions later instead of detecting |
| Toasts that stay (S1.2p) | about 2 min | two toasts; the notification centre opens and closes by itself | **hands off** |
| Clicking toasts (S1.3) | about 4 min | six toasts, clicked by the script; small "winshell-app" windows; the Run box opens and types a link by itself | **hands off**: the mouse moves by itself |
| Foreground (S1.4) | about 16 min | in each trial two small windows, "winshell-app" and "winshell blocker" (the script clicks the blocker to the front), then a toast | **YOUR TURN**: click the toast's text, 10 times (15 if the first method fails). A real click is what this check measures. Keep your hand off the mouse until the toast is there. Then 5 more trials that the script clicks itself: hands off |
| Progress toasts (S1.5) | about 4 min | a toast with a progress bar for 20 s; the notification centre opens, the script closes that toast | **YOUR TURN, watch only**: does the progress toast slide in a second time? The script asks you when it can't tell by itself. If it can't close the toast, it asks you to close it with its own X (never "Clear all") |
| Sleep and wake (S1.7) | about 5 min | — | **YOUR TURN**: Start, Power, **Sleep**; wait about a minute; wake the PC and sign in. The script carries on once the PC is awake again |
| Windows Hello (S1.8) | about 4 min | Windows Hello prompts: 1, then up to 10, then 1 | **YOUR TURN**: keep your hands off until each prompt shows, then complete it, **without clicking it first**. Keep the terminal in front. The script checks whether each prompt comes in front by itself, and a prompt you had to click or type into before it came forward counts as "owner-assisted", not as "in front" |
| Sandbox, watched run | about 15 min | a Windows Sandbox window with things happening inside; then one of the spike's windows is placed over it | when the script beeps and says YOUR TURN: **lock the PC (Win+L)**. Sign in again when you hear **two beeps** (about 4 minutes). The script sees the lock and the sign-in |

### The unattended block

| Part | Time | What happens |
|---|---|---|
| Stay-awake (S1.7) | "Sleep after" plus about 6 minutes (about 11 minutes at 5 minutes); longer if someone touches the PC or the first method fails | the hands-off pop-up counts down, and the screen may turn off. The PC must **not** go to sleep. Nobody touches the PC: a touch restarts the wait from that moment (up to 3 times) |
| Sandbox, 3 runs | about 30 minutes | the Sandbox window opens and closes 3 times by itself. The PC is kept awake meanwhile, and released at the end or as soon as the script stops. A run during which the PC slept anyway is recorded as interrupted, not as a failure |

**"Sleep after".** The wait follows this PC's "Sleep after" (on mains) setting in Settings, System, Power. The scripts only read it and never change it.
- **Your plan:** set it to 5 minutes yourself before the unattended block, and back afterwards.
- **At the start**, the unattended block prints the value it finds and how long the block will take.
- **At the end**, the terminal reminds you of the largest value the attended blocks noted, for example "set it back to 2 h 00 min".

**Nothing else may keep the PC awake during the wait.**
- **The script checks before you press Enter,** while you are still there. If something holds the PC awake, it says what to do and checks again each time you press Enter. Type `skip` to run only the Sandbox part.
- **The likely cause:** the Claude app's "Keep computer awake while Claude works" setting holds the PC awake while any Claude session is working. Pause your Claude sessions, or turn that setting off, for this test.
- **Other causes:** a playing video, a download, or another app with a keep-awake option.
- **If something starts keeping the PC awake during the wait,** the result is recorded as "not decisive" rather than as a pass.

When a block ends, the terminal prints each check's verdict (PASS, FAIL, INCOMPLETE, RECORDED or PENDING) and the cleanup's result. Tell the agent the block is done. The results are in `spikes\winshell\results\`: `checks.json` for Part B and the stay-awake wait, and `sandbox-observe.json` and `sandbox-checks.json` for the Sandbox.
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
| The power request "LILITH spike 11 keep-awake check" | S1.7 | released by the script; if the terminal is closed instead, the program holding it notices and releases it within a second; the cleanup checks no `winshell-core` is left running |
| `%LOCALAPPDATA%\LILITH-spike\winshell` (a log) | throughout | copied into the results, then deleted |
| The hands-off pop-up (a small window at the top centre) | before each hands-off stretch | it closes by itself when the stretch is done, and when the script stops |
| A power request that keeps the PC awake during the 3 unattended Sandbox runs | the unattended block | released at the end; if the terminal is closed instead, the program holding it notices and releases it within a second |
| The mouse pointer | S1.3, S1.4, S1.5, the Sandbox watched run | it is put back where it was after each synthesized click or hover |
| `spikes\winshell\build\` and `spikes\winshell\results\` | throughout | kept for the agent; git ignores both; delete them whenever you like |
| Inside the Sandbox: a shortcut, the link type, logs | Part C | gone when the Sandbox closes. The Sandbox has no network, clipboard, microphone, camera or printer; only two folders connect it to this PC |

The script inside the Sandbox (`inside.ps1`) first checks that it really is inside the Sandbox; on this PC it stops at once and changes nothing. No system or power setting is changed by any script.

## If something looks wrong

- **You want to stop:** press **Ctrl+C**. The part that is running cleans up, and the Sandbox it started (only that one) is closed.
- **A check ends as INCOMPLETE:** something couldn't be decided (a click that never came, too many touches during the stay-awake wait, another program keeping the PC awake). Nothing is wrong with the PC. Tell the agent, who will give you a shorter command, for example `-Only S1.4`.
- **A check stops with an error** (a red line "... stopped with an error ... Going on"): nothing to do; the error is recorded and the session goes on.
- **"Task 0's deploy folder is missing":** type the command the script printed (it has the `-Task0Repo` folder in it), or wait for Task 0's run.
- **"A Windows Sandbox is already running":** close your own Sandbox window, then run the command again.
- **The terminal closed or the PC restarted in the middle:** in a normal terminal in the repository folder, type `pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -CleanupOnly`. It adds nothing, removes what is left and prints what remains.
- **To repeat only some checks:** add `-Only` with the names, commas and no spaces, for example `-Block Attended -Only S1.4,S1.9` (S1.9 is the Sandbox part; the attended block runs its watched run, the unattended block its 3 runs). A misspelt name stops the script at once, before it does anything.
- **Earlier results are kept:** each attended block moves the previous `checks.json` and `raw\` into `results\run-<date and time>\` first; the unattended block adds to the attended block's results. The console is in `results\raw\console.log`.

## Turn on Windows Sandbox (only if it isn't on; about 10 minutes with a restart)

1. Task Manager (Ctrl+Shift+Esc), Performance, CPU: "Virtualization: Enabled". If it says Disabled, it must be turned on in the PC's firmware setup first (Intel VT-x or AMD-V/SVM); stop and ask if unsure.
2. Settings, System, Optional features, **More Windows features**: tick **Windows Sandbox**, OK, restart when asked.
3. To turn it off later: untick it in the same list and restart.
