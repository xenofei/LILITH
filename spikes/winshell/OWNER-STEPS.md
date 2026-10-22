# Spike 11: what you run, and what you do

Plan 4a, Task 1. The agent built this spike and ran only its own tests; nothing below has run on this PC yet. Each part changes something on the PC for a while (the lists under "What it changes" say what, and how it is undone), so nothing starts until you decide to start it.

There are three parts:

- **Part A: before you start** (5 minutes): a short checklist.
- **Part B: the main run on this PC** (about 1 hour, plus your PC's sleep timeout): toasts, links, windows, fullscreen, sleep and Windows Hello.
- **Part C: the runs inside Windows Sandbox** (about 1 hour 15 minutes, plus 10 minutes once to turn the Sandbox on). Do this part only after Task 0's run has finished.

You answer the scripts' questions by typing `y` or `n` and pressing Enter, or just Enter when they say "press Enter". Nothing is ever typed for you.

## Part A: before you start

1. **Rust is installed.** Open Windows Terminal and type `cargo --version`. You should see a version number. If you see "not recognized", install Rust first (rustup), then open a new terminal.
2. **Windows Hello is set up.** Settings, Accounts, Sign-in options: a PIN (or a fingerprint or face) is set. Part B's last check uses it.
3. **Have these ready for the fullscreen check** (Part B, check 6):
   - a game or 3D program you can start quickly and switch to fullscreen;
   - a video you can play fullscreen in a web browser (optional);
   - a presentation you can start as a slide show (optional).
4. **Plan for one sleep.** Part B puts the PC to sleep once, and asks you to leave it untouched for your sleep timeout plus about 6 minutes. Save your work in other programs first.
5. **The display language.** The keep-awake check reads Windows' `powercfg` output in English. If Windows is in another language, that one part is reported as "skipped" rather than run; everything else still works.
6. **For Part C only:** Visual Studio Build Tools with the "Desktop development with C++" workload (the script copies three Microsoft runtime files from it), and Task 0's run must have produced the folder `spikes\app-shell\build\deploy`.

## Part B: the main run on this PC (about 1 hour, plus the sleep timeout)

> **Rerun after run 1 (2026-10-22).** Checks 1 and 2 passed in run 1, so the rerun starts at the new check 2b. Use the command under "Rerunning after run 1" at the end of Part B instead of the one in step 3 below. Everything else in Part B is the same.

### Start it

1. Open **Windows Terminal normally**: Start, type `Terminal`, press Enter. Don't choose "Run as administrator": the script refuses to run elevated, because Windows doesn't show toasts from elevated programs.
2. Go to the folder where this repository is checked out: type `cd ` followed by that folder's path, then press Enter.
3. Type this and press Enter:

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-spike.ps1
   ```

4. Keep this terminal window where you can see it. It prints a heading for each check (`==== S1.2 ...`) and asks its questions there. After each check it prints a line such as `S1.2 -> PASS` in green or `-> FAIL` in red.

### The checks, in order

Each heading gives the time the check takes.

**Check 1: the build (2–3 minutes).**

1. Nothing to do. The script compiles the spike and counts warnings.
2. If it prints `S1.1 -> FAIL`, the script stops there. Tell the agent; the result is in `spikes\winshell\results\checks.json`.

**Setup (under a minute).** The script adds a Start-menu shortcut "LILITH Spike" and registers the link type `lilspike:`. Nothing to do.

**Check 2: toasts from a background program (3 minutes).**

1. The script says "Ready for toast 1 of 3?". Press Enter.
2. A notification ("LILITH spike / Toast 1 of 3") should slide in at the bottom right. **Don't click it.**
3. Answer `y` if you saw it, `n` if you didn't.
4. The same again for toasts 2 and 3.
5. Then one extra toast with the shortcut removed (this one is only recorded). Press Enter, watch, and answer whether it appeared.
6. If any of the three answers is `n`, the script stops with `S1.2 -> FAIL` and cleans up. Tell the agent.

**Check 2b: does a missed toast stay in the notification centre? (about 3 minutes, recorded only)**

Run 1 showed that a toast you missed could not be found in Win+N afterwards. This check measures that.

1. Two toasts come, one at a time, each saying "Persistence check (...): leave this toast alone". Press Enter for each when asked.
2. **Don't click it and don't close it.** Let it slide away by itself.
3. The script waits about 20 seconds, then asks you to press **Win+N** and say whether that toast is listed.
4. Close the notification centre again (press Esc) before the next one.

**Check 3: clicking toasts (about 8 minutes, plus 4 for the optional extras).**

1. Six toasts come, one at a time, each with an "Open" button. Each toast's text says what to click.
2. For the first three, click the **toast itself** (its text), not the button.
3. For the last three, click the **Open button**.
4. Each click opens a small window titled "winshell-app"; the script closes it a moment later.
5. If a toast disappears before you click it, press **Win+N** and click it there if it is listed.
6. **If you missed a toast and can't find it**, just wait. After 60 seconds the script asks "Did you miss the toast?":
   1. Answer `y`.
   2. The same toast is sent again (up to 3 times); click it this time.
   3. A missed toast that you then click counts as a normal trial. If a trial never gets a click at all, check 3 ends as **INCOMPLETE** rather than FAIL; tell the agent, who will give you a command to repeat just that check.
7. Optional extra 1, Edge: if you answer `y`, Edge opens a small local page.
   1. Click the link "Open the spike ref".
   2. Edge asks whether to open the app: click **Open**. Leave the "Always allow" box **unticked**.
8. Optional extra 2, the Run box (twice, once for each link): if you answer `y`, the script puts a link on the clipboard.
   1. **Close the Edge tab with the test page first.** In run 1 its link was clicked again by mistake, and the Run box's link never arrived.
   2. Press **Win+R**.
   3. Press **Ctrl+V**, then Enter.
   4. If what arrives isn't the Run box's link, the script says so and asks once more.
   5. Your earlier clipboard text is put back afterwards.

**Check 4: which window comes to the front (about 12 minutes).**

1. In each trial two small windows open: "winshell-app" and then "winshell blocker" on top of it.
2. **If the script says "Click once on the title bar of the window 'winshell blocker'"**, do that within 60 seconds: click the blocker's title bar, not "winshell-app". If the blocker is hidden behind other windows, click its button on the **taskbar** instead. In run 1 the blocker often couldn't come to the front by itself, and the trial needs it in front.
3. A toast comes: click the **toast itself**. A missed toast works as in check 3: wait, answer `y`, and it comes again.
4. There are ten trials: five with both windows, five with only the blocker.
5. If a trial with both windows fails, the script runs five more with a different method. Same steps.

**Check 5: progress toasts (about 4 minutes).**

1. Press Enter when asked. A toast "Spike job running" appears and its progress bar fills over 20 seconds.
2. Watch: it should slide in **once** and then just update. Answer `y` if it appeared only once.
3. Press **Win+N**, find "Spike job running", and close it with its **X**. Press Enter in the terminal.
4. Press Enter again: a toast "Spike job finished" should appear. Answer whether it did.
5. Press Enter again: a "Quiet toast" is sent, and this one should **not** slide in. Answer `y` if nothing appeared.
6. Press **Win+N** and look for "Quiet toast" in the list. Answer whether it is there.

**Check 6: fullscreen (about 5 minutes).**

1. (a) Press Enter, then leave the desktop as it is for 30 seconds: ordinary windows, nothing fullscreen.
2. (b) Press Enter, then within 20 seconds start your game or 3D program and make it fullscreen. Stay in it until you hear a **beep** (60 seconds in all), then come back to the terminal.
3. (c) Optional: answer `y`, then do the same with a browser video in fullscreen until the beep.
4. (d) Optional: answer `y` if you have a presentation, then do the same with the slide show until the beep.

**Check 7: keeping the PC awake (about 15 minutes, plus your sleep timeout).**

1. The script prints your sleep timeout (or "never").
2. When it asks, open a **second** terminal **as administrator**: right-click Start, choose "Terminal (Admin)", and say Yes.
3. In that admin terminal, type `powercfg /requests` and press Enter.
4. Look under the heading **SYSTEM**. You should see a line containing "LILITH spike 11 keep-awake check" (with `winshell-core.exe`). Answer in the first terminal.
5. The script then asks whether anything **else** is listed under SYSTEM. Type what it is (for example a program's name), or just press Enter if nothing else is there.
6. The script releases the request. Run `powercfg /requests` again in the admin terminal and answer whether the LILITH line is gone.
7. If your sleep timeout isn't "never": press Enter, then **don't touch the mouse or keyboard** until the time the script shows (your timeout plus about 5.5 minutes). The screen may turn off; that is fine. The PC should **not** go to sleep. The script carries on by itself afterwards.
8. If the script says "PowerSetRequest failed a check", it repeats steps 3 to 7 with a second method. This time, look under SYSTEM for a line `[PROCESS] ...\winshell-core.exe`, which has no description text.
9. Sleep and wake:
   1. Press Enter.
   2. Put the PC to sleep: Start, Power, **Sleep**.
   3. Wait about a minute, then wake it and sign in.
   4. The script notices the wake and carries on by itself. It waits up to 15 minutes.

**Check 8: Windows Hello (about 5 minutes).** Keep the terminal in front for this check.

1. The script prints three short results: whether Hello is supported, the security chip (TPM) version, and a data-protection test.
2. (b) Press Enter. A Windows Hello prompt appears to create a key called "LILITH-spike-kek".
   1. Notice whether the prompt appeared **in front of** the terminal.
   2. Complete it (PIN, finger or face).
   3. Answer the question.
3. (c) Press Enter. Up to ten Hello prompts follow, five and then five. Complete each one.
4. (e) Press Enter. One more Hello prompt, to confirm it's you.
   1. Notice whether it appeared in front of the terminal.
   2. Complete it.
   3. Answer the question.
5. The key is then deleted without a prompt.

**Cleanup (about 1 minute).**

1. Nothing to do.
2. The script removes everything listed below and checks that it is gone.
3. The run ends with `Cleanup -> PASS` in green. If it says `FAIL` in red, see "If something looks wrong" below.

### What Part B changes, and how it is undone

All of it is removed by the cleanup at the end, and the cleanup checks each item.

| What | When | How it is undone |
|---|---|---|
| A Start-menu shortcut "LILITH Spike" (`%APPDATA%\Microsoft\Windows\Start Menu\Programs\LILITH Spike.lnk`) | setup | deleted, then checked |
| The link type `lilspike:` (registry key `HKCU\Software\Classes\lilspike`) | setup | deleted with everything under it, then checked |
| The spike's toast activator `HKCU\Software\Classes\CLSID\{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}` (it names `winshell-app.exe`), and the same ID on the shortcut | check 2b on | deleted, then checked |
| The key `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` | check 2's extra toast only | deleted straight after that toast, and again at cleanup, then checked. Its parent `AppUserModelId` is removed too if the script created it and it is empty |
| The Windows Hello key "LILITH-spike-kek" | check 8 | deleted at the end of check 8, and again at cleanup if still there. The cleanup passes only when Windows Hello confirms the key is "not found" |
| Windows' own notification setting for "LILITH Spike" (`HKCU\...\Notifications\Settings\LILITH.Spike.Notify`) | added by Windows on the first toast | deleted |
| Any `lilspike` entries Windows adds under `HKCU\...\ApplicationAssociationToasts` | added by Windows | deleted |
| The spike's toasts in the notification centre | checks 2–5 | cleared |
| The folder `%LOCALAPPDATA%\LILITH-spike\winshell` (a log, the Edge test page) | throughout | the log is copied into the results first; then the folder is deleted (and `LILITH-spike` too, if empty) |
| Your clipboard | check 3, Run-box extra | your text is put back afterwards. An image, or an empty clipboard, is not restored: the link stays on it |
| The power request "LILITH spike 11 keep-awake check" | check 7 | released by the script; the cleanup also checks that no `winshell-core` program is still running |
| `spikes\winshell\target\` (the built programs) and `spikes\winshell\results\` (the results) | throughout | kept for the agent to read. Git ignores both. Delete them whenever you like |

### If something looks wrong in Part B

- **A check ends as INCOMPLETE** (for example `S1.4 -> INCOMPLETE`): some trials couldn't be done (a click that never came, or a blocker window that never got to the front), and they could still change the result. Nothing is wrong with the PC. Tell the agent, who will give you a command to repeat just that check (for example `-Only S1.4`).
- **A check stops with an error** (a red line such as "S1.4 stopped with an error ... Going on to the next check"): nothing to do. The script records the error for that check and carries on with the next one.
- **A toast didn't appear or a window didn't come up:** just answer `n` (or wait: the script gives each click up to 2 minutes). Don't try to fix it. A failure is a result too.
- **You want to stop:** press **Ctrl+C**. The cleanup still runs and prints `Cleanup -> PASS` or `FAIL`.
- **The terminal was closed, or the PC restarted, in the middle:** open a normal terminal in the repository folder and type:

  ```powershell
  pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -CleanupOnly
  ```

  It adds nothing, removes whatever is left and prints what remains.
- **`Cleanup -> FAIL`:** the line above it lists what is still there. Run the `-CleanupOnly` command once more. If it still fails, tell the agent; the details are in `spikes\winshell\results\checks.json`.
- **To repeat only some checks later**, name them, for example:

  ```powershell
  pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -Only S1.5,S1.6
  ```

  The setup and the cleanup run each time.
- **Earlier results are kept.** Each run moves the previous run's `checks.json` and `raw\` into `spikes\winshell\results\run-<date and time>\` before it starts. The whole console, with your answers, is kept in `results\raw\console.log`.

### Rerunning after run 1

Run 1 (2026-10-22) passed checks 1 and 2 and stopped during check 4. The rerun skips checks 1 and 2 and starts with the new check 2b:

1. Open Windows Terminal normally and go to the repository folder (steps 1 and 2 under "Start it").
2. Type this and press Enter:

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -Only S1.2p,S1.3,S1.4,S1.5,S1.6,S1.7,S1.8
   ```

3. The list after `-Only` is typed exactly as shown, with commas and no spaces. A misspelt name makes the script stop at once with "Unknown check name", before it does anything.
4. Then follow check 2b and checks 3 to 8 and the cleanup as above. It takes about 50 minutes, plus your sleep timeout.

## Part C: the runs inside Windows Sandbox

Windows Sandbox is a throwaway Windows that runs in a window and is wiped when it closes. The agent changes no system setting itself, so turning the Sandbox on is your step.

### C1. Turn on Windows Sandbox (once, about 10 minutes including a restart)

1. Check that virtualization is on.
   1. Press Ctrl+Shift+Esc to open Task Manager, then open the Performance tab and select CPU.
   2. Look for "Virtualization: Enabled".
   3. If it says Disabled, it has to be turned on in the PC's firmware (BIOS/UEFI) setup first, where it is called Intel VT-x or AMD-V/SVM. Stop here if you're unsure, and ask.
2. Open Settings, System, Optional features. At the bottom, click **More Windows features**. A list called "Turn Windows features on or off" opens.
3. Tick **Windows Sandbox**, click OK, and restart the PC when Windows asks.
4. After the restart, type `Windows Sandbox` in Start. If it is listed, the feature is on. You don't need to open it.

To turn it off again later: untick it in the same list and restart.

### C2. Three unattended runs (about 30 minutes)

1. **Close any Windows Sandbox window you have open yourself.** The script refuses to start while one is running, so that it never touches yours.
2. In a normal terminal in the repository folder, type:

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-sandbox.ps1 -Runs 3
   ```

3. The script builds and prepares its files (a few minutes the first time), then opens a Windows Sandbox window.
4. Inside that window things happen by themselves for 3–8 minutes: a blue console, a toast, small windows, and the Qt test app from Task 0. **Don't click or type inside the Sandbox window**, and leave it where it is.
5. The Sandbox window closes by itself. The terminal prints each item with PASS or FAIL, and then the next run starts. There are three runs in all.
6. At the end the terminal prints `S1.9 run: PASS` or `FAIL`.

### C3. Watching one run (about 15 minutes, you present)

1. Type:

   ```powershell
   pwsh -NoProfile -File spikes/winshell/run-sandbox.ps1 -Observe
   ```

2. Watch the Sandbox window: within a minute or two a toast should slide in **inside the Sandbox window**. Answer `y` or `n` in your terminal.
3. Wait until the console inside the Sandbox stops changing (its first round is done), then press Enter in your terminal.
4. Now drag other windows **over** the Sandbox window so that it is covered (don't minimise it). The script repeats some items. Wait.
5. When the terminal says "Round 2 done", press Enter, then **lock the PC** with Win+L within 30 seconds.
6. Stay locked for about 4 minutes, then sign in again.
7. The Sandbox finishes and closes by itself. The terminal prints the results.

### What Part C changes

- **On this PC**, only two folders inside the repository, both ignored by git. Delete them whenever you like.
  - `spikes\winshell\build\sandbox\`: the files handed to the Sandbox, a copy of Task 0's app folder, the Sandbox settings files, and the folders the Sandbox writes its results to. If a results folder doesn't already give Windows' SYSTEM account write access, the script adds it, because the Sandbox writes through that account.
  - `spikes\winshell\results\sandbox*`: the copied results.
- **The Sandbox's settings** turn off its network, clipboard sharing, microphone, camera and printer sharing. Only those two folders connect it to this PC.
- **Inside the Sandbox:** a shortcut, the `lilspike:` link type, a scheduled task (only if needed) and logs. All of it disappears when the Sandbox closes.
- **The script inside the Sandbox** (`inside.ps1`) checks first that it is really inside the Sandbox. If it were started on this PC by mistake, it stops at once and changes nothing.
- **The Windows Sandbox feature itself** stays on until you turn it off (C1).

### If something looks wrong in Part C

- **"A Windows Sandbox is already running":** close your own Sandbox window, then run the command again.
- **"Windows Sandbox is not turned on":** do C1 first.
- **"Task 0's deploy folder is missing":** Task 0's run hasn't happened yet. Wait for it.
- **The script asks for `-QtExe` or `-QtGrabArgs`:** Task 0's app folder isn't what the script expected. Tell the agent, who will give you the exact command.
- **You want to stop:** press **Ctrl+C**. The script closes the Sandbox it started, and only that one.
- **The Sandbox window stays open for more than 10 minutes:** wait. The script closes it within a few minutes after its time limit, and records that run as a failure.
