# 09 Per-user scheduled task without admin rights

Date: 2026-10-12

Status: criteria 1, 2, 3 and 5 PASS (scripted). Criterion 4 (no console window flash) **FAIL, observed in the manual check**: a console window flashed with `pwsh -WindowStyle Hidden`. This is the finding OPS-078's windowless launcher addresses; see section 7.

## 1. Question

- **OPS-079:** can a non-elevated shell register and run a per-user Task Scheduler task ("run only when user is logged on", `StartWhenAvailable`, no admin, no stored password)?
- **OPS-078:** does the launch mechanism need a windowless launcher (does `pwsh -WindowStyle Hidden` flash a console)?

## 2. Setup

| Item | Version |
|---|---|
| OS | Windows 11 Pro 10.0.26200 |
| PowerShell | 7.6.6 (Store/MSIX install) |
| Shell integrity | `Mandatory Label\Medium Mandatory Level` (non-elevated, admin-group account) |
| Spike code | `spikes/winops/`, in the commit that adds this document |

## 3. Method

`spikes/winops/register-task.ps1` (refuses to run if the shell is not Medium integrity) does, in order:

1. `New-ScheduledTaskAction -Execute <abs path of pwsh.exe> -Argument '-NoProfile -WindowStyle Hidden -File "<repo>\spikes\winops\touch.ps1"' -WorkingDirectory <repo>`
2. `New-ScheduledTaskTrigger -Daily -At '03:00'`
3. `New-ScheduledTaskSettingsSet -StartWhenAvailable`
4. `New-ScheduledTaskPrincipal -UserId <DOMAIN\user> -LogonType Interactive -RunLevel Limited` (no `-Password`, nothing stored)
5. `Register-ScheduledTask -TaskName WorldwrightSpikeTask -Action ... -Trigger ... -Settings ... -Principal ...`
6. `Start-ScheduledTask -TaskName WorldwrightSpikeTask`, `Start-Sleep 10`, read `%TEMP%\ww-spike-task.log`, read `(Get-ScheduledTask ...).Settings.StartWhenAvailable`
7. `finally`: `Unregister-ScheduledTask -TaskName WorldwrightSpikeTask -Confirm:$false`, then confirm `Get-ScheduledTask` finds nothing.

`touch.ps1` appends one line to `%TEMP%\ww-spike-task.log` and writes nothing else. No other scheduled task is listed or touched.

Re-run: `pwsh -File spikes\winops\register-task.ps1` from a non-elevated PowerShell.

## 4. Pass criteria

1. Registration succeeds from a **non-elevated** shell (`whoami /groups` shows `Mandatory Label\Medium Mandatory Level`).
2. The task runs and writes the log line.
3. `StartWhenAvailable` reads back as `True`.
4. No console window flashes; a person watches while the task runs.
5. The task is removed afterwards; `Get-ScheduledTask WorldwrightSpikeTask` reports nothing.

A separate standard (non-admin) account was **not** tested. A non-elevated token from an admin account is what OPS-078 relies on in practice.

## 5. Measurements

| # | Criterion | Result |
|---|---|---|
| 1 | Non-elevated registration | PASS. Integrity `Medium Mandatory Level`; `Register-ScheduledTask` succeeded. Principal reads back `LogonType=Interactive RunLevel=Limited`. |
| 2 | Task runs, writes log | PASS. Log line `<timestamp> touch pid=<pid> user=<user>`; `LastTaskResult=0`. |
| 3 | `StartWhenAvailable` | PASS. Reads back `True`. |
| 4 | No console flash | FAIL, observed in the manual check: a console window flashed with `pwsh -NoProfile -WindowStyle Hidden`. |
| 5 | Removal | PASS. Script prints `removed`; `Get-ScheduledTask WorldwrightSpikeTask` returns nothing. |

Run history: the first run (script absent) failed with exit 64. The second run failed with `LastTaskResult=2147942402` (0x80070002, file not found) because the task action was the bare name `pwsh`, which is a Store execution alias that Task Scheduler does not resolve. The script now registers the absolute path of `pwsh.exe`; the third run passed.

## 6. Verdict

PARTIAL: criteria 1, 2, 3 and 5 PASS, criterion 4 FAIL. A non-elevated shell can register, run, read back and remove a per-user, run-only-when-logged-on task with `StartWhenAvailable`, with no admin rights and no stored password. But `pwsh -WindowStyle Hidden` is not windowless: a window flashes when the task starts. That is consistent with the console being created before the hide takes effect; the mechanism was not traced.

## 7. Decision

`OPS-079`: `Register-ScheduledTask` with `New-ScheduledTaskPrincipal -UserId <current user> -LogonType Interactive -RunLevel Limited`, `New-ScheduledTaskSettingsSet -StartWhenAvailable`, `New-ScheduledTaskTrigger -Daily`. No `-Password`.

`OPS-078` (windowless launcher): confirmed. The manual check saw a console flash, so `pwsh -WindowStyle Hidden` is not sufficient. The suite's scheduled backup must run through a windowless launcher registered at an **absolute path**, which OPS-078 already requires (a bare `pwsh` also fails with 0x80070002, see section 5). A GUI-subsystem exe, or `conhost --headless`-style hosting, should avoid the flash, but both are **untested hypotheses** for plan 7+ to verify (re-run the manual check below against the launcher). Plan 7+ builds the launcher; this spike does not.

## 8. Follow-ups

- Plan 7+ must register an **absolute** executable path. A bare `pwsh` does not run from Task Scheduler on a Store-installed PowerShell.
- The Store `pwsh.exe` path contains the package version (`...PowerShell_7.6.6.0_x64__...`) and changes on upgrade, so a task pointing at it would break. The real task should point at the suite's own launcher exe, not at `pwsh`.
- Standard-account (non-admin user) behaviour untested.

## Manual check (done: window flashed)

*Kept for re-testing the windowless launcher once plan 7+ builds it: point the task action at the launcher and repeat this check.*

Criterion 4 needs a person watching the screen. Run this one command from a normal (not "Run as administrator") PowerShell window in the repo root:

```powershell
pwsh -File spikes\winops\register-task.ps1
```

It registers `WorldwrightSpikeTask`, runs it once, waits 10 s, and removes it again.

Watch for: **any console window appearing, even for a fraction of a second**, when the script prints `ran` (the task starts then). Also glance at the taskbar for a briefly appearing icon.

Then record: "no window seen" or "window flashed (describe)". If a window flashed, that is the OPS-078 finding: plan 7+ ships the windowless launcher. Expected output ends with `log ok`, `StartWhenAvailable=True` and `removed`, exit code 0.

Result for criterion 4: a console window flashed. The command above is kept so the check can be repeated against the windowless launcher once plan 7+ builds it.
