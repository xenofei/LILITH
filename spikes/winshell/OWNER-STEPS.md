# Spike 11: the owner-present steps

Plan 4a, Task 1. The agent built the spike and ran only its unit tests: nothing below has run yet. Each step changes something on this PC (listed under "What it writes"), so each waits for your go.

Run everything from the repository root, in a **normal (not elevated) Windows Terminal** with PowerShell 7. Keep that terminal in front unless a step asks otherwise: the scripts ask you yes-or-no questions in it.

## Before you start

- The **Rust toolchain** on `PATH` (`cargo`, the repository's `rust-toolchain.toml` picks 1.99.0), as for any build in this repository.
- For Step 2b: **Visual Studio Build Tools** with the C++ workload, so `vswhere.exe` finds the VC++ runtime DLLs under `VC\Redist\MSVC` (the bundle copies them).
- **Windows Hello** is set up for your account (a PIN at least), for S1.8.
- For S1.6: a **fullscreen Direct3D game or program** you can start quickly (any game in exclusive or borderless fullscreen), a **video** you can play fullscreen in a browser, and, if you have one, a **presentation** to run as a slide show (optional).
- For S1.7: the PC may be put to **sleep** once, and left **untouched** for the sleep timeout plus about 6 minutes. The script reads the sleep timeout and Modern Standby from `powercfg` output in **English**; on another display language the stay-awake check is reported as skipped ("timeout unknown"), not run.
- For Step 2b: Task 0's Step 2 has finished, so `spikes/app-shell/build/deploy` exists.

## Step 2: the host run (about 1 hour, plus the sleep timeout)

```powershell
pwsh -NoProfile -File spikes/winshell/run-spike.ps1
```

It builds the spike (`cargo build --release --locked`, about 2 minutes the first time), registers the shortcut and the `lilspike` scheme, runs S1.1–S1.8 in order and removes everything at the end. Results: `spikes/winshell/results/checks.json`. To repeat single checks later: `-Only S1.5,S1.6` (setup and cleanup still run).

| Check | Time | What you see and do |
|---|---|---|
| S1.1 | 2–3 min | Nothing: a clean release build, warnings counted. If it fails, the script stops. |
| S1.2 | 3 min | Three toasts "LILITH spike / Toast n of 3" pop bottom right. **Don't click them.** Answer `y` or `n` for each. Then one more toast with the shortcut removed (recorded only): answer whether it popped. If S1.2 fails, the script stops. |
| S1.3 | 8 min, +4 for the recorded checks | Six toasts with an "Open" button. Click the **body** of the first three and the **Open button** of the last three (each toast says which). If a toast has already gone, open the notification centre (Win+N) and click it there. Each click opens a small "winshell-app" window; the script closes it. Then, if you agree: Edge opens a local page; click the link and choose **Open** in Edge's prompt, **without ticking "Always allow"**. Then the Run dialog twice: the script copies a `lilspike:open/…` link to the clipboard; press Win+R, Ctrl+V, Enter. |
| S1.4 | 12 min | Ten toasts. Five times: a "winshell-app" window opens, then a "winshell blocker" window in front of it, then a toast; **click the toast's body** and leave the windows alone. Five more times with only the blocker window. Each trial first checks that the blocker window really took the front; if it didn't, the script says so and repeats that trial (up to 3 times) before any toast. If the first five don't all pass, five more run with the fallback recipe. |
| S1.5 | 4 min | A progress toast "Spike job running" pops and fills for 20 s: watch whether it **pops a second time** (it should not), then answer. Open the notification centre (Win+N) and **dismiss it with its X**. Then a "Spike job finished" toast should pop: answer. Then a "Quiet toast" should **not** pop: answer, then look in the notification centre (Win+N) for it: answer. |
| S1.6 | 5 min | (a) 30 s at the desktop: change nothing. (b) When asked, **start a fullscreen Direct3D game or program**; you have 20 s to get it fullscreen; stay in it until you hear a beep (60 s), then come back. (c) Optional: the same with a **browser video in fullscreen**. (d) Optional: the same with a **slide show**. |
| S1.7 | 15 min plus the sleep timeout | The script reads `powercfg /a` and the sleep timeout itself. When asked, open an **elevated** terminal (Win+X, then Terminal (Admin)) and run `powercfg /requests`: answer whether "LILITH spike 11 keep-awake check" is listed under SYSTEM, and type anything **else** listed under SYSTEM (another program's request could keep the PC awake by itself); the script clears the request; run `powercfg /requests` again and answer whether it is gone. Then, if the sleep timeout isn't "never", **leave the PC untouched** until the time the script shows (the timeout plus about 5.5 minutes; the display may turn off). Then **put the PC to sleep** (Start, Power, Sleep), wait about a minute, wake it and sign in; the script waits up to 15 minutes. If the first checks fail, they repeat with `SetThreadExecutionState`; that request carries no reason text, so look for `[PROCESS] ...\winshell-core.exe` under SYSTEM instead. Besides the spike's own log, the stay-awake verdict reads the System event log's Kernel-Power sleep and standby entries. |
| S1.8 | 5 min | Windows Hello prompts, each announced first: (b) create the credential "LILITH-spike-kek"; (c) up to ten signing prompts, five from each of two processes; (e) one verification prompt. For (b) and (e), answer whether the prompt was **in front of the terminal**; complete every prompt. (f) deletes the credential without a prompt. |
| Cleanup | 1 min | Nothing: it removes everything listed below and checks it is gone. `Cleanup -> PASS` ends the run. |

### What Step 2 writes, and how cleanup removes it

| Written | When | Removed by |
|---|---|---|
| `%APPDATA%\Microsoft\Windows\Start Menu\Programs\LILITH Spike.lnk` (target `winshell-app.exe`, AUMID `LILITH.Spike.Notify`) | setup | deleted; checked |
| `HKCU\Software\Classes\lilspike` (`URL:LILITH (Spike)`, `URL Protocol`, `DefaultIcon`, `shell\open\command`) | setup | deleted with its subkeys; checked |
| `HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify` (`DisplayName`) | S1.2's recorded check only | deleted at once after that check, and again at cleanup; checked. Its parent `AppUserModelId` is removed too when the script created it and it is empty |
| The Windows Hello credential `LILITH-spike-kek` | S1.8 (b) | deleted by S1.8 (f); cleanup deletes it if still there; checked: the cleanup passes only when Windows Hello answers "not found" (or Hello isn't supported at all), never on an error |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\LILITH.Spike.Notify` | by Windows, on the first toast | deleted |
| `lilspike` values under `HKCU\Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts`, if Windows adds any | by Windows, after registration | deleted |
| The spike's toasts in the notification centre | S1.2–S1.5 | cleared |
| `%LOCALAPPDATA%\LILITH-spike\winshell\` (activation log, Edge test page, power flags) | throughout | copied to `results\raw\events.jsonl`, then deleted (and `LILITH-spike\` if empty) |
| The clipboard (a `lilspike:open/…` link) | S1.3's Run-dialog check | your text clipboard is saved first and put back after the check (an image, or an empty clipboard, isn't restored: the link stays) |
| `spikes/winshell/target/` and `spikes/winshell/results/` | build, every check | kept; both are ignored by git (only images in `results/` would be committed) |

The cleanup runs in a `finally` block, so it also runs after an error or Ctrl+C. Each of its steps runs even when an earlier one fails, and it also checks that no `winshell-core` is left running (so no power request is left held). If the terminal itself was closed mid-run, run `pwsh -NoProfile -File spikes/winshell/run-spike.ps1 -CleanupOnly`, which registers nothing, removes what is left and reports what remains. It works without the built exes too, but then it can't ask Windows Hello and reports the credential as "unknown".

## Step 2b: Windows Sandbox (about 1 hour 15 minutes, after the feature is on)

### Turn on Windows Sandbox (once, about 10 minutes with the restart)

The agent changes no system setting; this one is yours.

1. Check that virtualization is on: Task Manager, Performance, CPU: "Virtualization: Enabled". If it says Disabled, turn on Intel VT-x or AMD-V (SVM) in the firmware setup first.
2. Settings, System, Optional features, "More Windows features" (at the bottom). This opens "Turn Windows features on or off".
3. Tick **Windows Sandbox**, press OK, and restart when asked.

The same from an elevated PowerShell, if you prefer: `Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All`, then restart.

To turn it off again later: untick it in the same dialog (or `Disable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM`), then restart.

### The three unattended runs (about 30 minutes)

```powershell
pwsh -NoProfile -File spikes/winshell/run-sandbox.ps1 -Runs 3
```

**Close any Windows Sandbox of your own first**: the script refuses to start while one runs, and it only ever stops the Sandbox it started itself (by its id with the `wsb` CLI, otherwise by the processes that appeared when it started it). Ctrl+C stops that Sandbox too.

Each run opens the Windows Sandbox window, which works by itself for 3–8 minutes (a console, a toast, small windows, the Qt spike app) and then closes itself. **Don't type or click inside the Sandbox window**, and leave it where it is. The host terminal prints each item's PASS or FAIL per run. Results: `spikes/winshell/results/sandbox-checks.json` and `results/sandbox/run-<n>/`.

If Task 0's app takes a different command line for its offscreen grab than `--grab <png>`, pass it as `-QtGrabArgs '<args with {png}>'`; if the deploy folder holds more than one exe, name the app with `-QtExe <name>.exe`.

### The host-state observation (about 15 minutes, owner present)

```powershell
pwsh -NoProfile -File spikes/winshell/run-sandbox.ps1 -Observe
```

1. Watch the Sandbox window: a toast should pop **inside it** within a minute or two. Answer in the host terminal.
2. When the Sandbox console goes quiet, press Enter, then **put other windows over the Sandbox window** (don't minimise it). The toast, foreground and Qt items run again.
3. When the terminal says round 2 is done, press Enter, then **lock the PC (Win+L) within 30 s**, stay locked about 4 minutes, and sign in again. The items run a third time; then the Sandbox closes itself.

Results: `spikes/winshell/results/sandbox-observe.json`. These are recorded, not bars.

### What Step 2b writes

On the host: only `spikes/winshell/build/sandbox/` (the bundles, a copy of Task 0's deploy folder, the `.wsb` files and the results folders, each results folder granted SYSTEM modify when SYSTEM isn't already on its access list). The `.wsb` turns off networking, clipboard sharing, audio and video input and printer sharing and `spikes/winshell/results/sandbox*`. Both are ignored by git. Delete `spikes/winshell/build/` when you like.

`inside.ps1` runs only inside the Sandbox: it checks for the Sandbox's own account (`WDAGUtilityAccount`) and for a marker file that only the mapped bundle holds, and otherwise exits before doing anything (so starting it by mistake on this PC changes nothing and never shuts it down).

Inside the Sandbox: the shortcut, the `lilspike` key, a scheduled task `LilSpikeLimited` (only when the logon is elevated) and the logs. All of it is discarded when the Sandbox closes.

The Windows Sandbox feature itself stays on until you turn it off.
