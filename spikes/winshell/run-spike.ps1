#requires -Version 7
<#
.SYNOPSIS
Spike 11 (plan 4a Task 1), Step 2: the owner-present Windows-integration run (S1.1–S1.8 and
the cleanup). Writes spikes/winshell/results/checks.json.

.DESCRIPTION
Writes, and removes again at the end (also after an error or Ctrl+C):
  - the shortcut <Programs>\LILITH Spike.lnk (System.AppUserModel.ID = LILITH.Spike.Notify)
  - HKCU\Software\Classes\lilspike (the protocol handler, Appendix G's shape)
  - HKCU\Software\Classes\AppUserModelId\LILITH.Spike.Notify (only during S1.2's recorded check)
  - HKCU\Software\Classes\CLSID\{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31} (the spike's toast
    activator, from the toast-persistence check S1.2p on)
  - the Windows Hello credential LILITH-spike-kek (S1.8)
  - %LOCALAPPDATA%\LILITH-spike\winshell (the activation log and the power flags)
Windows itself adds HKCU\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\
LILITH.Spike.Notify on the first toast; the cleanup removes that too.

Run it from a normal (not elevated) Windows Terminal. run-all.ps1 runs it for the owner's
session; see OWNER-STEPS.md.

Since the automation round (owner direction 2026-10-23) the script detects instead of asking
(uia.ps1): a toast's popup and its notification-centre entry by UI Automation, the notification
centre's content also by ToastNotificationManager.History, the foreground window by
GetForegroundWindow. Toast clicks in S1.3 are UIA InvokePattern or a synthesized click (the mouse
moves); S1.4's clicks stay the owner's, since its question is the foreground right a real click
gives. A yes/no question is asked only when detection is impossible, and every item records the
method that decided it.

.PARAMETER Only
Runs only these checks, for example -Only S1.3,S1.5. Setup and cleanup always run. An earlier
checks.json and raw\ are moved into results\run-<UTC time>\ first, never overwritten (unless
-Append).

.PARAMETER Append
Adds to the current results\checks.json instead of moving it aside (run-all.ps1's later stages).

.PARAMETER PowerPart
S1.7 in parts: 'sleep-wake' runs the sleep-and-wake check and leaves the verdict pending;
'stay-awake' (with -Append) runs the long no-input wait and decides the verdict; 'all' runs both.

.PARAMETER DryRun
Prints the parsed -Only list as JSON and stops, doing nothing else (tests/only_parse.rs).

.PARAMETER LibraryOnly
Defines the functions and returns, doing nothing else (the strict-mode cleanup test
tests/cleanup-strict.ps1 dot-sources the script this way).

.PARAMETER CleanupOnly
Registers nothing and runs no check: removes whatever an interrupted run left and reports
what is still there.
#>
param(
    [string[]]$Only = @(),
    [switch]$SkipBuild,
    [switch]$CleanupOnly,
    [switch]$LibraryOnly,
    [switch]$DryRun,
    [switch]$Append,
    [ValidateSet('all', 'sleep-wake', 'stay-awake')]
    [string]$PowerPart = 'all'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# -Only: `pwsh -File` hands "S1.2p,S1.3,..." over as one string (a comma is an array only in
# PowerShell's own parser), so the names are split on commas and trimmed here, and an unknown
# name stops the script before anything at all runs (rerun review C1).
$KnownChecks = @('S1.1', 'S1.2', 'S1.2p', 'S1.3', 'S1.4', 'S1.5', 'S1.6', 'S1.7', 'S1.8')
$Only = @($Only | ForEach-Object { "$_" -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ } |
    ForEach-Object { $n = $_; $k = @($KnownChecks | Where-Object { $_ -eq $n }); if ($k.Count -gt 0) { $k[0] } else { $n } })
$unknownChecks = @($Only | Where-Object { $_ -notin $KnownChecks })
if ($unknownChecks.Count -gt 0) {
    throw "Unknown check name in -Only: $($unknownChecks -join ', '). The names are: $($KnownChecks -join ', '). Nothing was done."
}
if ($DryRun) {
    [ordered]@{ only = $Only; runs = @($KnownChecks | Where-Object { $Only.Count -eq 0 -or $_ -in $Only }); append = [bool]$Append; power_part = $PowerPart } | ConvertTo-Json -Compress
    return
}

# The detection helpers (UI Automation, the foreground window, the lock state). Loading them
# only defines functions.
. (Join-Path $PSScriptRoot 'uia.ps1')

$Spike = $PSScriptRoot
$Bin = Join-Path $Spike 'target\release'
$Core = Join-Path $Bin 'winshell-core.exe'
$App = Join-Path $Bin 'winshell-app.exe'
$Hello = Join-Path $Bin 'winshell-hello.exe'
$ToolExe = Join-Path $Bin 'winshell-tool.exe'
$Results = Join-Path $Spike 'results'
$Raw = Join-Path $Results 'raw'
$LogDir = Join-Path $env:LOCALAPPDATA 'LILITH-spike\winshell'
$Events = Join-Path $LogDir 'events.jsonl'

$Aumid = 'LILITH.Spike.Notify'
$Scheme = 'lilspike'
$ConfirmUrl = 'lilspike:confirm/cf_0123456789abcdef'
$SchemeKey = "HKCU:\Software\Classes\$Scheme"
$AumidKey = "HKCU:\Software\Classes\AppUserModelId\$Aumid"
$NotifySettingsKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\$Aumid"
$AssocToastsKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts'
$PowerReason = 'LILITH spike 11 keep-awake check'
$ProgressTag = 'j_spike'   # winshell::PROGRESS_TAG
$ToastClsid = '{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}'
$ClsidKey = "HKCU:\Software\Classes\CLSID\$ToastClsid"
$ClsidParent = 'HKCU:\Software\Classes\CLSID'

$Report = [ordered]@{
    spike   = 11
    started = (Get-Date).ToUniversalTime().ToString('o')
    setup   = [ordered]@{}
    checks  = [ordered]@{}
    cleanup = $null
}

# ---------------------------------------------------------------- helpers

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }
function Step([string]$Text) { Write-Host ''; Write-Host "==== $Text" -ForegroundColor Yellow }

function Ask([string]$Question) {
    while ($true) {
        $a = Read-Host "$Question [y/n]"
        if ($a -match '^(y|yes)$') { return $true }
        if ($a -match '^(n|no)$') { return $false }
    }
}

function Wait-Owner([string]$Text) { [void](Read-Host "$Text  (press Enter)") }

function Want([string]$Id) { $Only.Count -eq 0 -or $Only -contains $Id }

function Save-Report {
    New-Item -ItemType Directory -Force -Path $Results | Out-Null
    $Report | ConvertTo-Json -Depth 12 | Set-Content -Encoding utf8 (Join-Path $Results 'checks.json')
}

# A check's results so far, saved while it runs: an error later in the check keeps them
# (rerun review M5).
function Save-Partial([string]$Id, $Details) {
    $Report.checks[$Id] = [ordered]@{ verdict = 'IN PROGRESS'; details = $Details }
    Save-Report
}

function Set-Check([string]$Id, [string]$Verdict, $Details) {
    $Report.checks[$Id] = [ordered]@{ verdict = $Verdict; details = $Details }
    Save-Report
    $color = switch ($Verdict) { 'PASS' { 'Green' } 'FAIL' { 'Red' } default { 'Gray' } }
    Say "$Id -> $Verdict" $color
}

function Tool {
    $out = & $ToolExe @args
    if ($LASTEXITCODE -ne 0) { throw "winshell-tool $($args -join ' ') exited $LASTEXITCODE" }
    ($out | Out-String) | ConvertFrom-Json
}

# A field of a recorded result, or $null when the result or the field is absent: strict mode
# would otherwise end the run on a field an errored command didn't write (fix round 1, M10).
function P($Obj, [string]$Name) {
    if ($null -eq $Obj) { return $null }
    if ($Obj -is [System.Collections.IDictionary]) { return $Obj[$Name] }
    $p = $Obj.PSObject.Properties[$Name]
    if ($p) { $p.Value } else { $null }
}

# winshell-hello's one JSON line; a missing exe or unreadable output becomes a result with
# status "unknown" or "error" instead of a throw (I1, M9).
function HelloCmd {
    if (-not (Test-Path -LiteralPath $Hello)) {
        return [pscustomobject]@{ status = 'unknown'; error = 'winshell-hello.exe is not built' }
    }
    $out = & $Hello @args
    try {
        $r = ($out | Out-String) | ConvertFrom-Json
        if ($null -ne $r) { return $r }
    } catch { }
    [pscustomobject]@{ status = 'error'; error = "unreadable output: $($out | Out-String)" }
}

# FOLDERID_Programs through winshell-tool, or .NET's Programs folder when the exe isn't built.
function Get-ProgramsDir {
    if (Test-Path -LiteralPath $ToolExe) {
        try { $d = P (Tool programs-dir) 'programs'; if ($d) { return $d } } catch { }
    }
    [Environment]::GetFolderPath('Programs')
}

# Starts winshell-core detached (DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP) through
# winshell-tool, as lil-client starts the core. Its result lands in results\raw\<name>.json.
function Start-Core([string]$Name, [string[]]$CoreArgs) {
    $out = Join-Path $Raw "$Name.json"
    Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
    $r = Tool spawn '--' $Core @CoreArgs --out $out   # a quoted '--': a bare one is PowerShell's
    if (-not $r.PSObject.Properties['pid']) { throw "spawn failed: $($r | ConvertTo-Json -Compress)" }
    [pscustomobject]@{ Name = $Name; Pid = [int]$r.pid; Out = $out }
}

function Wait-Core($C, [int]$TimeoutSec = 60) {
    try { Wait-Process -Id $C.Pid -Timeout $TimeoutSec -ErrorAction Stop } catch { }
    if (Get-Process -Id $C.Pid -ErrorAction SilentlyContinue) {
        throw "winshell-core $($C.Name) still running after $TimeoutSec s"
    }
    if (-not (Test-Path $C.Out)) { throw "winshell-core $($C.Name) wrote no result" }
    Get-Content -Raw $C.Out | ConvertFrom-Json
}

function Read-CoreNow($C) {
    if (Test-Path $C.Out) { try { return Get-Content -Raw $C.Out | ConvertFrom-Json } catch { } }
    $null
}

# A line caught while winshell-app is still writing it would fail to parse; it is skipped and
# read whole on the next poll, never a terminating error (Part B run 1 analysis).
function Get-Events {
    if (-not (Test-Path $Events)) { return @() }
    $lines = @(Get-Content $Events -ErrorAction SilentlyContinue | Where-Object { $_ })
    $out = [System.Collections.Generic.List[object]]::new()
    foreach ($l in $lines) {
        try { $out.Add(($l | ConvertFrom-Json)) } catch { break }
    }
    $out.ToArray()   # callers wrap it in @()
}

# Waits for the first event after index $After that matches $Kind and $Filter.
function Wait-SpikeEvent([string]$Kind, [int]$After, [int]$TimeoutSec = 90, [scriptblock]$Filter = { $true }) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $ev = @(Get-Events)
        if ($ev.Count -gt $After) {
            foreach ($e in $ev[$After..($ev.Count - 1)]) {
                if ($e.kind -eq $Kind -and (& $Filter $e)) { return $e }
            }
        }
        Start-Sleep -Milliseconds 300
    }
    $null
}

function Stop-SpikeProcesses {
    foreach ($name in 'winshell-app', 'winshell-core') {
        Get-Process -Name $name -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $_.Path.StartsWith($Bin, [StringComparison]::OrdinalIgnoreCase) } |
            Stop-Process -Force -ErrorAction SilentlyContinue
    }
}

function Post-Toast([string]$Name, [string[]]$ToastArgs) {
    Wait-Core (Start-Core $Name (@('toast') + $ToastArgs)) 30
}

# ---------------------------------------------------------------- detection (automation round)

# A short random token. Each toast's text carries one, so a UIA search finds that toast only.
function New-Nonce { -join (1..6 | ForEach-Object { '{0:x}' -f (Get-Random -Maximum 16) }) }

# Posts a toast and watches for its popup for $WatchSec (uia.ps1).
function Post-Watched([string]$Name, [string[]]$ToastArgs, [string]$Needle, [double]$WatchSec = 8) {
    $before = Get-UiaSnapshot
    $r = Post-Toast $Name $ToastArgs
    $w = Watch-Needle -Needle $Needle -Before $before -Seconds $WatchSec
    [pscustomobject]@{ Toast = $r; Watch = $w }
}

# Whether a toast popped, from a watch. UIA decides once it has found a toast in this run (then
# "not found" means it did not pop); before that, or without UIA, the owner is asked.
function Resolve-Popped($Watch, [string]$Question) {
    $apps = @(P $Watch.Record 'appearances' | Where-Object { $null -ne $_ })
    if ($apps.Count -gt 0) { return [ordered]@{ value = $true; method = 'uia'; appearances = $apps.Count; watch = $Watch.Record } }
    if ($script:Uia.proven) { return [ordered]@{ value = $false; method = 'uia'; appearances = 0; watch = $Watch.Record } }
    [ordered]@{ value = (Ask $Question); method = 'owner (UIA has not found a toast in this run)'; appearances = $null; watch = $Watch.Record }
}

# One calibration toast at setup: proves that UIA sees this PC's toasts, and learns where they
# are hosted. If UIA can't find it, the owner says once whether it showed; the checks then ask
# the owner wherever they would have detected.
function Invoke-UiaCalibration {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ ready = $false; error = $script:Uia.error } }
    $n = New-Nonce
    $w = Post-Watched 'calibrate' @('--tag', 'calibrate', '--title', 'LILITH spike', '--body', "Calibration toast: nothing to do #$n") "#$n"
    $found = $null -ne $w.Watch.Hit
    $asked = $null
    if (-not $found) { $asked = Ask 'Calibration: did a toast "Calibration toast" slide in at the bottom right just now?' }
    try { [void](Tool history-clear) } catch { }
    [ordered]@{
        ready = $true; found = $found; owner_saw_it = $asked; watch = $w.Watch.Record; posted = (P $w.Toast 'posted')
        result = $(if ($found) { 'UIA finds this run''s toasts: detection decides' }
                   elseif ($asked) { 'the toast showed but UIA did not find it: the owner answers instead' }
                   else { 'no toast showed' })
    }
}

# Whether the dialog prompt came in front during a Hello call: the foreground moved to a window
# of another process than the one in front before the call (the terminal). $null when the
# foreground before is unknown. Pure; tests/strict-results.ps1 covers it.
function Get-PromptInFront($Before, $Seen) {
    if ($null -eq $Before) { return $null }
    $other = @(@($Seen) | Where-Object { $null -ne $_ -and $_['pid'] -ne $Before['pid'] -and $_['hwnd'] -ne $Before['hwnd'] })
    $other.Count -gt 0
}

function Read-HelloOutput([string]$Text) {
    try {
        $r = $Text | ConvertFrom-Json
        if ($null -ne $r) { return $r }
    } catch { }
    [pscustomobject]@{ status = 'error'; error = "unreadable output: $Text" }
}

# A winshell-hello command whose prompt should come in front: the foreground window is polled
# while it runs. Falls back to the owner's answer when the foreground can't be read.
function HelloWatched([string[]]$HelloArgs, [string]$Question) {
    if (-not (Test-Path -LiteralPath $Hello)) {
        return [pscustomobject]@{ Result = [pscustomobject]@{ status = 'unknown'; error = 'winshell-hello.exe is not built' }; Front = [ordered]@{ value = $false; method = 'none' } }
    }
    $pre = Get-ForegroundNow
    $outFile = Join-Path $Raw ("hello-{0}-{1}.json" -f $HelloArgs[0], (New-Nonce))
    $p = Start-Process -FilePath $Hello -ArgumentList $HelloArgs -NoNewWindow -PassThru -RedirectStandardOutput $outFile
    $seen = [ordered]@{}
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while (-not $p.HasExited) {
        $f = $null
        try { $f = Get-ForegroundNow } catch { }
        if ($null -ne $f -and -not $seen.Contains("$($f.hwnd)")) {
            $f.first_s = [math]::Round($sw.Elapsed.TotalSeconds, 2)
            $seen["$($f.hwnd)"] = $f
        }
        Start-Sleep -Milliseconds 150
    }
    $p.WaitForExit()
    $r = Read-HelloOutput ((Get-Content -Raw -LiteralPath $outFile -ErrorAction SilentlyContinue) | Out-String)
    $inFront = Get-PromptInFront $pre @($seen.Values)
    $front = [ordered]@{ value = $inFront; method = 'GetForegroundWindow every 150 ms while the call ran'; before = $pre; seen = @($seen.Values) }
    if ($null -eq $inFront) { $front.value = Ask $Question; $front.method = 'owner (the foreground window could not be read)' }
    [pscustomobject]@{ Result = $r; Front = $front }
}

# The Run box (Win+R) with $Url typed into it by UIA ValuePattern, then Enter: no clipboard.
function Invoke-RunBox([string]$Url) {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ method = 'none'; ok = $false; why = "UI Automation not available: $($script:Uia.error)" } }
    $before = Get-UiaSnapshot
    Send-Keys @(0x5B, 0x52)   # Win+R
    $editCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
    $box = $null
    $deadline = (Get-Date).AddSeconds(5)
    while ($null -eq $box -and (Get-Date) -lt $deadline) {
        foreach ($t in Get-UiaTop) {
            try {
                if ($before.ContainsKey([int64]$t.Current.NativeWindowHandle)) { continue }
                $edit = $t.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $editCond)
                if ($null -ne $edit) { $box = [pscustomobject]@{ Top = $t; Edit = $edit }; break }
            } catch { }
        }
        if ($null -eq $box) { Start-Sleep -Milliseconds 250 }
    }
    if ($null -eq $box) { return [ordered]@{ method = 'uia (Win+R)'; ok = $false; why = 'no new window with an edit box within 5 s of Win+R' } }
    $info = Get-TopInfo $box.Top
    $rec = [ordered]@{ method = 'uia (Win+R, ValuePattern.SetValue, Enter)'; ok = $false; window = [ordered]@{ process = $info.process; class = $info.class } }
    $vp = $null
    $fg = $null
    if ($box.Edit.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) {
        $vp.SetValue($Url)
        Start-Sleep -Milliseconds 300
        $fg = Get-ForegroundNow
    }
    if ($null -ne $fg -and $fg.hwnd -eq $info.hwnd) {
        Send-Keys @(0x0D)   # Enter
        $rec.ok = $true
    } else {
        $rec.why = $(if ($null -eq $vp) { 'the edit box has no ValuePattern' } else { 'the Run box is not the foreground window: Enter not sent' })
        $wp = $null
        try { if ($box.Top.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$wp)) { $wp.Close() } } catch { }
    }
    $rec
}

# ---------------------------------------------------------------- registration

$Lnk = $null

# The shortcut: plain (the AUMID only), or with the spike's ToastActivatorCLSID and its COM
# registration, whose LocalServer32 is winshell-app.exe (S1.2p and everything after it).
$script:ShortcutForm = 'plain'

# Whether a shortcut-read result matches what was written; $null when it does, else the reason.
# $WantClsid is the CLSID string expected on the shortcut, or $null for the plain form.
function Test-ShortcutReadback($Read, [string]$WantAumid, $WantClsid) {
    if ((P $Read 'aumid') -ne $WantAumid) { return "shortcut AUMID reads back as '$(P $Read 'aumid')'" }
    if ($null -ne $WantClsid -and "$(P $Read 'toast_clsid')" -ne "$WantClsid") {
        return "shortcut ToastActivatorCLSID reads back as '$(P $Read 'toast_clsid')', expected '$WantClsid'"
    }
    $null
}

# The switch is -WithToastClsid, not -ToastClsid: a parameter named $ToastClsid hid the script's
# $ToastClsid string inside this function, so the read-back compared the CLSID with "True" and
# stopped S1.2p, S1.3 and S1.4 in the rerun of 2026-10-23 (tests/strict-results.ps1).
function Write-Shortcut([switch]$WithToastClsid) {
    $extra = @()
    if ($WithToastClsid) {
        # Whether ...\Classes\CLSID existed before: the cleanup removes it only if the script
        # created it and it is empty (rerun review M8).
        if ($null -eq $script:ClsidParentExisted) { $script:ClsidParentExisted = [bool](Test-Path $ClsidParent) }
        New-Item -Path "$ClsidKey\LocalServer32" -Value "`"$App`"" -Force | Out-Null
        $extra = @('--toast-clsid')
    } elseif (Test-Path $ClsidKey) {
        Remove-Item -Path $ClsidKey -Recurse -Force
    }
    $w = Tool shortcut-write --path $Lnk --target $App --aumid $Aumid @extra
    if (-not (P $w 'written')) { throw "shortcut not written: $(P $w 'error')" }
    $r = Tool shortcut-read --path $Lnk
    $bad = Test-ShortcutReadback $r $Aumid $(if ($WithToastClsid) { $script:ToastClsid } else { $null })
    if ($bad) { throw $bad }
    $script:ShortcutForm = $(if ($WithToastClsid) { 'toast_clsid' } else { 'plain' })
    Start-Sleep -Seconds 3   # let the Start menu index it before the next toast
}

# Appendix G's registration, with scheme lilspike and winshell-app.exe as <exe>.
function Register-Scheme {
    New-Item -Path $SchemeKey -Value 'URL:LILITH (Spike)' -Force | Out-Null
    New-ItemProperty -Path $SchemeKey -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
    New-Item -Path "$SchemeKey\DefaultIcon" -Value "`"$App`",0" -Force | Out-Null
    New-Item -Path "$SchemeKey\shell\open\command" -Value "`"$App`" `"%1`"" -Force | Out-Null
}

# ---------------------------------------------------------------- cleanup

# Whether HKCU\Software\Classes\AppUserModelId existed before S1.2 created its child; $null
# while unknown (then it is never removed) (fix round 1, M8).
$script:AumidParentExisted = $null
# The name the event log is kept under in raw\ (a later -Append stage keeps its own).
$script:EventsKeepName = 'events.jsonl'
$script:ClsidParentExisted = $null
$AumidParent = 'HKCU:\Software\Classes\AppUserModelId'

# The credential counts as gone only when OpenAsync says NotFound, or Hello isn't supported
# at all; any other answer (an error, no output) is "unknown", which fails the cleanup (I1).
function Get-CredentialState {
    $cred = HelloCmd exists
    $status = P $cred 'status'
    if ($status -eq 'NotFound') { return 'gone' }
    if ($status -eq 'Success') { return 'present' }
    $sup = HelloCmd supported
    if ((P $sup 'supported') -eq $false -and -not (P $sup 'error')) { return 'gone (Hello not supported)' }
    "unknown ($status)"
}

function Get-SpikeCores {
    @(Get-Process -Name 'winshell-core' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($Bin, [StringComparison]::OrdinalIgnoreCase) })
}

function Get-CleanupState {
    $cred = Get-CredentialState
    [ordered]@{
        shortcut_present     = [bool]($Lnk -and (Test-Path -LiteralPath $Lnk))
        scheme_key_present   = [bool](Test-Path $SchemeKey)
        aumid_key_present    = [bool](Test-Path $AumidKey)
        credential           = $cred
        credential_gone      = $cred.StartsWith('gone')
        core_running         = @(Get-SpikeCores).Count -gt 0
        clsid_key_present    = [bool](Test-Path $ClsidKey)
        notify_settings_key  = [bool](Test-Path $NotifySettingsKey)
        log_dir_present      = [bool](Test-Path $LogDir)
    }
}

# Every step runs even when an earlier one fails; failures are recorded (I1).
function Invoke-Cleanup([switch]$Final) {
    $errors = [ordered]@{}
    $assocRemoved = @()
    $deleted = $null
    $steps = [ordered]@{
        'stop processes'        = {
            Stop-SpikeProcesses
            $until = (Get-Date).AddSeconds(5)
            while (@(Get-SpikeCores).Count -gt 0 -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 200 }
        }
        'clear history'         = { if (Test-Path -LiteralPath $ToolExe) { [void](Tool history-clear) } }
        'shortcut'              = { if ($Lnk -and (Test-Path -LiteralPath $Lnk)) { Remove-Item -LiteralPath $Lnk -Force -ErrorAction Stop } }
        'scheme key'            = { if (Test-Path $SchemeKey) { Remove-Item -Path $SchemeKey -Recurse -Force -ErrorAction Stop } }
        'AUMID key'             = { if (Test-Path $AumidKey) { Remove-Item -Path $AumidKey -Recurse -Force -ErrorAction Stop } }
        'toast activator key'   = { if (Test-Path $ClsidKey) { Remove-Item -Path $ClsidKey -Recurse -Force -ErrorAction Stop } }
        'AUMID parent'          = {
            if ($script:AumidParentExisted -eq $false -and (Test-Path $AumidParent)) {
                $k = Get-Item $AumidParent
                if ($k.SubKeyCount -eq 0 -and $k.ValueCount -eq 0) { Remove-Item -Path $AumidParent -Force -ErrorAction Stop }
            }
        }
        'toast activator parent' = {
            if ($script:ClsidParentExisted -eq $false -and (Test-Path $ClsidParent)) {
                $k = Get-Item $ClsidParent
                if ($k.SubKeyCount -eq 0 -and $k.ValueCount -eq 0) { Remove-Item -Path $ClsidParent -Force -ErrorAction Stop }
            }
        }
        'notification settings' = { if (Test-Path $NotifySettingsKey) { Remove-Item -Path $NotifySettingsKey -Recurse -Force -ErrorAction Stop } }
        'association toasts'    = {
            $assoc = Get-Item -Path $AssocToastsKey -ErrorAction SilentlyContinue
            if ($assoc) {
                foreach ($v in $assoc.GetValueNames() | Where-Object { $_ -match $Scheme }) {
                    Remove-ItemProperty -Path $AssocToastsKey -Name $v -ErrorAction Stop
                    $script:assocRemovedNow += $v
                }
            }
        }
        'credential'            = { if ((Get-CredentialState) -eq 'present') { $script:credDeletedNow = HelloCmd delete } }
        'keep the event log'    = {
            if ($Final -and (Test-Path $Events)) {
                New-Item -ItemType Directory -Force -Path $Raw | Out-Null
                Copy-Item $Events (Join-Path $Raw $script:EventsKeepName) -Force
            }
        }
        'log folder'            = {
            if (Test-Path $LogDir) { Remove-Item -LiteralPath $LogDir -Recurse -Force -ErrorAction Stop }
            $parent = Split-Path $LogDir
            if ((Test-Path $parent) -and -not (Get-ChildItem $parent -Force)) { Remove-Item $parent -Force -ErrorAction Stop }
        }
    }
    $script:assocRemovedNow = @()
    $script:credDeletedNow = $null
    foreach ($name in $steps.Keys) {
        try { & $steps[$name] } catch { $errors[$name] = $_.Exception.Message }
    }
    $assocRemoved = $script:assocRemovedNow
    $deleted = $script:credDeletedNow
    $state = $null
    try { $state = Get-CleanupState } catch { $errors['state'] = $_.Exception.Message }
    [ordered]@{
        state              = $state
        errors             = $errors
        association_toasts = $assocRemoved
        credential_deleted = $deleted
        pass               = [bool]($state -and -not ($state.shortcut_present -or $state.scheme_key_present -or
                                     $state.aumid_key_present -or $state.clsid_key_present -or $state.core_running) -and
                                     $state.credential_gone -and $errors.Count -eq 0)
    }
}

# ---------------------------------------------------------------- checks

function Check-S11 {
    Step 'S1.1 crate surface: release build with 0 warnings'
    Push-Location $Spike
    $ErrorActionPreference = 'Continue'   # cargo writes its progress to stderr
    try {
        & cargo clean --release -p winshell 2>&1 | Out-Null
        $log = & cargo build --release --locked 2>&1 | ForEach-Object { "$_" }
        $exit = $LASTEXITCODE
    } finally { Pop-Location }
    $warnings = @($log | Where-Object { $_ -match '^warning' })
    $toml = Get-Content -Raw (Join-Path $Spike 'Cargo.toml')
    $features = @([regex]::Matches($toml, '(?m)^\s*"([A-Za-z0-9_]+)",') | ForEach-Object { $_.Groups[1].Value })
    $verdict = if ($exit -eq 0 -and $warnings.Count -eq 0) { 'PASS' } else { 'FAIL' }
    Set-Check 'S1.1' $verdict ([ordered]@{
        cargo_exit = $exit; warnings = $warnings.Count; warning_lines = $warnings
        windows_crate = '0.62.2'; features = $features
    })
    if ($verdict -ne 'PASS') { throw 'STOP: S1.1 failed: the result goes to the owner before Task 2 (APP-022).' }
}

function Check-S12 {
    Step 'S1.2 windowless sender: 3 toasts from the detached winshell-core'
    Say 'Three toasts follow; the script watches for each one itself. Do not click them.'
    $trials = @()
    for ($i = 1; $i -le 3; $i++) {
        $n = New-Nonce
        $w = Post-Watched "s12-$i" @('--tag', "s12-$i", '--title', 'LILITH spike', '--body', "Toast $i of 3 (S1.2) #$n") "#$n"
        $r = $w.Toast
        $seen = Resolve-Popped $w.Watch "Did toast $i of 3 pop?"
        $hist = Tool history --tag "s12-$i"
        $trials += [ordered]@{ trial = $i; posted = (P $r 'posted'); setting = (P $r 'setting'); error = (P $r 'error'); seen = $seen.value; seen_method = $seen.method; popup = $seen.watch; in_history = (P $hist 'has_tag') }
    }
    $pass = @($trials | Where-Object { $_.posted -and $_.seen -and $_.setting -eq 'Enabled' }).Count -eq 3

    Say 'Recorded, not a bar: the shortcut removed, only HKCU\Software\Classes\AppUserModelId present.'
    Remove-Item -LiteralPath $Lnk -Force
    $script:AumidParentExisted = [bool](Test-Path $AumidParent)
    New-Item -Path $AumidKey -Force | Out-Null
    New-ItemProperty -Path $AumidKey -Name 'DisplayName' -Value 'LILITH Spike' -PropertyType String -Force | Out-Null
    Start-Sleep -Seconds 3
    $n = New-Nonce
    $w = Post-Watched 's12-regonly' @('--tag', 's12-regonly', '--title', 'LILITH spike', '--body', "Registry AUMID only (S1.2, recorded) #$n") "#$n"
    $r = $w.Toast
    $seenRegOnly = Resolve-Popped $w.Watch 'Did that toast pop?'
    Remove-Item -Path $AumidKey -Recurse -Force
    Write-Shortcut

    Set-Check 'S1.2' $(if ($pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
        trials = $trials
        recorded_registry_only = [ordered]@{ posted = (P $r 'posted'); setting = (P $r 'setting'); error = (P $r 'error'); seen = $seenRegOnly.value; seen_method = $seenRegOnly.method; popup = $seenRegOnly.watch }
    })
    if (-not $pass) { throw 'STOP: S1.2 failed: the result goes to the owner before Task 2 (APP-022).' }
}

# How long Windows shows a toast banner (Settings, Accessibility, Visual effects, "Dismiss
# notifications after this amount of time"); 5 s by default.
function Get-BannerSeconds {
    $v = (Get-ItemProperty 'HKCU:\Control Panel\Accessibility' -Name MessageDuration -ErrorAction SilentlyContinue)
    if ($v -and $v.MessageDuration) { [int]$v.MessageDuration } else { 5 }
}

# The AUMID's notification settings as Windows keeps them (read only).
function Get-NotifySettings {
    $k = Get-ItemProperty -Path $NotifySettingsKey -ErrorAction SilentlyContinue
    if (-not $k) { return $null }
    $h = [ordered]@{}
    foreach ($p in $k.PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' }) { $h[$p.Name] = $p.Value }
    $h
}

# S1.2p (recorded, Part B run 1 analysis): does a toast nobody clicks stay in the notification
# centre after its banner times out? Once with the plain shortcut (the AUMID only), once with
# the spike's ToastActivatorCLSID on the shortcut and registered. Measured from History some
# time after the banner has gone, and by UIA in the notification centre, which the script
# opens with Win+N and closes with Esc (automation round: no question).
function Check-S12p {
    Step 'S1.2p recorded: does a missed toast stay in the notification centre?'
    $banner = Get-BannerSeconds
    $wait = [math]::Max(20, $banner + 15)
    Say "Two toasts follow, one at a time, and the script opens and closes the notification centre itself. Hands off: let each toast slide away by itself (about $banner s)."
    $forms = [ordered]@{}
    foreach ($form in 'plain', 'toast_clsid') {
        if ($form -eq 'toast_clsid') { Write-Shortcut -WithToastClsid } else { Write-Shortcut }
        $tag = "persist-$form"
        $n = New-Nonce
        $before = Get-UiaSnapshot
        $r = Post-Toast "s12p-$form" @('--tag', $tag, '--launch', 'lilspike:task/j_0123456789abcdef',
                '--title', 'LILITH spike', '--body', "Persistence check ($form): leave this toast alone #$n")
        $h0 = Tool history --tag $tag
        Say "Watching the banner for $wait s..."
        $w = Watch-Needle -Needle "#$n" -Before $before -Seconds $wait
        $h1 = Tool history --tag $tag
        $centre = Test-InNotificationCentre "#$n"
        $apps = @(P $w.Record 'appearances' | Where-Object { $null -ne $_ })
        $forms[$form] = [ordered]@{
            posted = (P $r 'posted'); setting = (P $r 'setting')
            popup = $w.Record; banner_gone_after_s = $(if ($apps.Count -gt 0) { $apps[-1]['to_s'] } else { $null })
            in_history_at_once = (P $h0 'has_tag'); in_history_after_banner = (P $h1 'has_tag')
            in_notification_centre = $centre; waited_s = $wait; notify_settings = (Get-NotifySettings)
        }
        Save-Partial 'S1.2p' ([ordered]@{ banner_seconds = $banner; forms = $forms })
    }
    # Its two toasts would otherwise wait in the notification centre while S1.3 asks for clicks
    # there (rerun review M2).
    $cleared = $true
    try { [void](Tool history-clear) } catch { $cleared = $false }
    Set-Check 'S1.2p' 'RECORDED' ([ordered]@{
        banner_seconds = $banner; forms = $forms; shortcut_used_from_now = $script:ShortcutForm
        history_cleared_after = $cleared
        caveat = 'Both forms share one AUMID and one .lnk path, rewritten in place; "only the CLSID form persists" is informative, "neither persists" may be the shell keeping the earlier registration.'
    })
}

# One click trial (S1.3, S1.4): posts the toast (its body carries a nonce) and waits for the
# launch the click starts. $Click says who clicks:
#   'auto'      UIA InvokePattern first, then a synthesized click on the toast; when UIA has not
#               found a toast in this run, the owner clicks instead (and that is recorded)
#   'synthetic' a synthesized click only (S1.4's recorded set)
#   'owner'     the owner clicks (S1.4's bar: the foreground right a real click gives)
# $Target is 'body' (the toast's text) or 'button' (its "Open" action). A post that gets no
# launch is posted again, up to 3 posts; nothing is asked (automation round).
function Click-Trial([string]$Name, [string]$Tag, [string]$Body, [switch]$WithButton, [string]$Target = 'body',
        [string]$Click = 'auto', [string]$Instruction, [scriptblock]$Prepare, [scriptblock]$Await) {
    $posts = @()
    for ($post = 1; $post -le 3; $post++) {
        $ctx = & $Prepare
        if ($ctx -is [System.Collections.IDictionary] -and $ctx.Contains('invalid')) { return $ctx }
        $n = @(Get-Events).Count
        $nonce = New-Nonce
        $toastArgs = @('--tag', $Tag, '--launch', $ConfirmUrl, '--title', 'LILITH spike', '--body', "$Body #$nonce")
        if ($WithButton) { $toastArgs += @('--button', $ConfirmUrl) }
        $before = Get-UiaSnapshot
        $r = Post-Toast "$Name-$post" $toastArgs
        $attempt = [ordered]@{ post = $post; posted = (P $r 'posted'); shortcut = $script:ShortcutForm; found = $null; clicks = @(); clicked_by = $null }
        $result = $null
        $mode = $Click
        if ($mode -ne 'owner') {
            $w = Watch-Needle -Needle "#$nonce" -Before $before -Seconds 8 -UntilFound
            $attempt.found = $w.Record
            if ($null -eq $w.Hit) {
                if ($script:Uia.proven) { $mode = 'none' }
                else { $mode = 'owner'; $attempt.fallback = 'UIA has not found a toast in this run: the owner clicks' }
            } else {
                $top = [int64]$w.Hit.TopInfo.hwnd
                $el = $w.Hit.Element
                if ($Target -eq 'button') {
                    $b = Find-ButtonNamed $w.Hit.Top 'Open'
                    if ($null -ne $b) { $el = $b } else { $attempt.button_missing = $true }
                }
                $ways = $(if ($mode -eq 'synthetic') { @('mouse') } else { @('invoke', 'mouse') })
                foreach ($way in $ways) {
                    if ($Target -eq 'button' -and $attempt.Contains('button_missing')) { break }
                    $c = $(if ($way -eq 'invoke') { Invoke-UiaElement $el $top } else { Click-ElementCentre $el $top })
                    $attempt.clicks += $c
                    if ($c.ok) { $result = & $Await $n $ctx 6 }
                    if ($null -ne $result) { $attempt.clicked_by = $c.mode; break }
                }
            }
        }
        if ($mode -eq 'owner') {
            Say $Instruction
            $result = & $Await $n $ctx 60
            if ($null -ne $result) { $attempt.clicked_by = 'owner' }
            else { Say 'No click within 60 s: the toast comes again.' 'Yellow' }
        }
        $posts += $attempt
        if ($null -ne $result) {
            return [ordered]@{ result = $result; context = $ctx; posts = $post; attempts = $posts; clicked_by = $attempt.clicked_by }
        }
        Stop-SpikeProcesses
    }
    [ordered]@{ result = $null; context = $null; posts = $posts.Count; attempts = $posts; clicked_by = $null }
}

# S1.3's verdict: FAIL when a click that arrived was not exact; INCOMPLETE when every click
# that arrived was exact but some trial never got one (rerun review M4); else PASS.
function Get-S13Verdict($Trials) {
    $all = @($Trials)
    $clicked = @($all | Where-Object { $_.got_click })
    $inexact = @($clicked | Where-Object { -not $_.exact }).Count
    $never = $all.Count - $clicked.Count
    if ($inexact -gt 0) { return 'FAIL' }
    if ($never -gt 0) { return "INCOMPLETE ($never trial(s) never got a click: rerun S1.3)" }
    'PASS'
}

function Check-S13 {
    Step 'S1.3 protocol activation: the raw command line'
    if ($script:ShortcutForm -ne 'toast_clsid') { Write-Shortcut -WithToastClsid }
    $expected = "`"$App`" `"$ConfirmUrl`""
    $trials = @()
    $targets = @('body', 'body', 'body', 'button', 'button', 'button')
    for ($i = 0; $i -lt $targets.Count; $i++) {
        $where = $targets[$i]
        $what = $(if ($where -eq 'body') { 'BODY of this toast' } else { 'Open button' })
        $t = Click-Trial -Name "s13-$($i + 1)" -Tag "s13-$($i + 1)" -Body "Click the $what (S1.3, $($i + 1) of 6)" -WithButton `
            -Target $where -Click 'auto' `
            -Instruction "Click the toast's $(if ($where -eq 'body') { 'BODY (not the button)' } else { 'Open BUTTON' }). If it has gone, open the notification centre (Win+N) and click it there." `
            -Prepare { Stop-SpikeProcesses; [ordered]@{} } `
            -Await { param($n, $ctx, $timeout) Wait-SpikeEvent 'start' $n $timeout { param($e) -not "$($e.cmdline)".Contains('lilspike:task/') } }
        $cmd = P $t.result 'cmdline'
        $trials += [ordered]@{ trial = $i + 1; clicked = $where; got_click = ($null -ne $cmd); cmdline = $cmd; exact = ($cmd -ceq $expected)
                               click_method = $t.clicked_by; posts = $t.posts; attempts = $t.attempts }
        Save-Partial 'S1.3' ([ordered]@{ expected = $expected; trials = $trials })
        Start-Sleep -Seconds 2
    }
    Stop-SpikeProcesses
    $verdict = Get-S13Verdict $trials

    # Recorded: Edge opening a raw ref from a local HTML file. Run 1 recorded it (one quoted
    # argument, unchanged); it needs a click in Edge's own prompt, so it is not repeated.
    $recorded = [ordered]@{ edge = 'not repeated: recorded in run 1 (results\run-20261022T033715Z)' }
    # Recorded: the Run dialog with lilspike:open/<base64url>, plain and with a trailing '/'. The
    # script opens the Run box itself and types the link with UIA (no clipboard).
    $target = 'lil://0123456789abcdef/sketchbook/lantern@1.0.0?~é'
    $b64 = (Tool b64url $target).b64url
    foreach ($suffix in '', '/') {
        $url = "lilspike:open/$b64$suffix"
        Stop-SpikeProcesses
        $n = @(Get-Events).Count
        $box = Invoke-RunBox $url
        $ev = $null
        if ($box.ok) { $ev = Wait-SpikeEvent 'start' $n 20 }
        $cmd = P $ev 'cmdline'
        $fromRun = [bool]($cmd -and $cmd.Contains("open/$b64"))
        $recorded["run$(if ($suffix) { '_trailing_slash' } else { '' })"] = [ordered]@{ sent = $url; cmdline = $(if ($fromRun) { $cmd } else { $null }); start_seen = $cmd; run_box = $box }
        Start-Sleep -Seconds 2
        Stop-SpikeProcesses
    }
    Set-Check 'S1.3' $verdict ([ordered]@{
        expected = $expected; shortcut = $script:ShortcutForm; trials = $trials; recorded = $recorded; base64url_target = $target
    })
}

# The precondition of every S1.4 trial: the blocker window holds the front before the toast is
# posted. Run 1 showed that a blocker started by the script often can't take the front once A
# has it (SetForegroundWindow false: the script's terminal is no longer the foreground process).
# Then the owner clicks the blocker's title bar once, which gives it the front as a person
# would; the script checks the foreground window itself (Part B run 1 analysis).
# Automation round: before asking, the script clicks a visible part of the blocker itself (a
# synthesized click; this is only the trial's precondition, not what S1.4 measures).
function Ensure-BlockerFront($Blocker, $Check) {
    if (P $Check 'is_foreground') { return [ordered]@{ how = 'blocker took it'; ok = $true } }
    $hwnd = [int64](P $Check 'hwnd')
    $synthetic = $null
    if ($hwnd -ne 0 -and (Initialize-SpikeUi)) {
        $synthetic = Click-WindowVisible $hwnd
        $deadline = (Get-Date).AddSeconds(3)
        while ($synthetic.ok -and (Get-Date) -lt $deadline) {
            $fg = $null
            try { $fg = Get-ForegroundNow } catch { }
            if ($null -ne $fg -and $fg.pid -eq $Blocker.Id) { return [ordered]@{ how = 'synthetic click on the blocker'; ok = $true; click = $synthetic } }
            Start-Sleep -Milliseconds 200
        }
    }
    Say "Click once on the title bar of the window 'winshell blocker' (not on 'winshell-app'), then wait." 'Yellow'
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $deadline) {
        # One failed poll is just retried: it must not end S1.4 (rerun review M5).
        $fg = $null
        try { $fg = Tool foreground } catch { }
        if ((P $fg 'fg_pid') -eq $Blocker.Id) { return [ordered]@{ how = 'owner clicked it'; ok = $true; foreground = $fg; synthetic_tried = $synthetic } }
        Start-Sleep -Milliseconds 300
    }
    [ordered]@{ how = 'not in front after 60 s'; ok = $false; synthetic_tried = $synthetic }
}

# A click counts as soon as the second launch (B) starts, whatever happens next: the trial is
# then valid, and passes only on A's own check after the hand-off. A B that starts while A
# never logs an activation is a FAIL of the recipe, so the FAIL path can run (rerun review I2).
function Trial-WithA([int]$I, [string]$Recipe, [string]$Click = 'owner') {
    $t = Click-Trial -Name "s14-a-$Recipe-$Click-$I" -Tag "s14-a-$I" -Body "Click this toast's body (S1.4 with A, $I of 5)" `
        -Target 'body' -Click $Click `
        -Instruction "YOUR CLICK: click the toast's BODY (S1.4, A running behind the blocker window, $I of 5)." `
        -Prepare {
            Stop-SpikeProcesses
            $n = @(Get-Events).Count
            $a = Start-Process -FilePath $App -ArgumentList '--recipe', $Recipe, '--exit-after-secs', '300' -PassThru
            [void](Wait-SpikeEvent 'window' $n 20 { param($e) $e.pid -eq $a.Id })
            $b = Start-Process -FilePath $App -ArgumentList '--blocker', '--exit-after-secs', '300' -PassThru
            $blk = Wait-SpikeEvent 'front_check' $n 20 { param($e) $e.pid -eq $b.Id }
            $front = Ensure-BlockerFront $b $blk
            if (-not $front.ok) { return [ordered]@{ invalid = $true; blocker_front = $blk; precondition = $front } }
            [ordered]@{ a_pid = $a.Id; blocker_pid = $b.Id; blocker_front = $blk; precondition = $front }
        } `
        -Await {
            param($n, $ctx, $timeout)
            $bs = Wait-SpikeEvent 'start' $n $timeout { param($e) $e.pid -ne $ctx.a_pid -and $e.pid -ne $ctx.blocker_pid }
            if ($null -eq $bs) { return $null }
            $chk = Wait-SpikeEvent 'front_check' $n 10 { param($e) $e.pid -eq $ctx.a_pid -and $e.phase -eq 'activate' }
            [ordered]@{ b_start = $bs; check = $chk }
        }
    Stop-SpikeProcesses
    if ($t.Contains('invalid')) { return [ordered]@{ trial = $I; recipe = $Recipe; valid = $false; pass = $false; detail = $t } }
    $bs = P $t.result 'b_start'
    $chk = P $t.result 'check'
    $bPid = P $bs 'pid'
    $hand = @(Get-Events) | Where-Object { $_.kind -eq 'handoff' -and $_.pid -eq $bPid } | Select-Object -Last 1
    $clicked = $null -ne $bs
    [ordered]@{
        trial = $I; recipe = $Recipe; valid = $clicked; clicked = $clicked
        pass = [bool]($clicked -and (P $chk 'is_foreground') -and (P $chk 'has_focus'))
        b_started = $clicked; b_cmdline = (P $bs 'cmdline'); a_activated = ($null -ne $chk)
        a_pid = (P $t.context 'a_pid'); precondition = (P $t.context 'precondition'); handoff = $hand; check = $chk
        click_method = $t.clicked_by; posts = $t.posts; attempts = $t.attempts
    }
}

function Trial-NoA([int]$I) {
    $t = Click-Trial -Name "s14-noa-$I" -Tag "s14-noa-$I" -Body "Click this toast's body (S1.4 no A, $I of 5)" `
        -Target 'body' -Click 'owner' `
        -Instruction "YOUR CLICK: click the toast's BODY (S1.4, no A running, $I of 5)." `
        -Prepare {
            Stop-SpikeProcesses
            $n = @(Get-Events).Count
            $b = Start-Process -FilePath $App -ArgumentList '--blocker', '--exit-after-secs', '300' -PassThru
            $blk = Wait-SpikeEvent 'front_check' $n 20 { param($e) $e.pid -eq $b.Id }
            $front = Ensure-BlockerFront $b $blk
            if (-not $front.ok) { return [ordered]@{ invalid = $true; blocker_front = $blk; precondition = $front } }
            [ordered]@{ blocker_pid = $b.Id; blocker_front = $blk; precondition = $front }
        } `
        -Await {
            param($n, $ctx, $timeout)
            $bs = Wait-SpikeEvent 'start' $n $timeout { param($e) $e.pid -ne $ctx.blocker_pid }
            if ($null -eq $bs) { return $null }
            $chk = Wait-SpikeEvent 'front_check' $n 10 { param($e) $e.pid -eq $bs.pid -and $e.phase -eq 'start' }
            [ordered]@{ b_start = $bs; check = $chk }
        }
    Stop-SpikeProcesses
    if ($t.Contains('invalid')) { return [ordered]@{ trial = $I; valid = $false; pass = $false; detail = $t } }
    $bs = P $t.result 'b_start'
    $chk = P $t.result 'check'
    $clicked = $null -ne $bs
    [ordered]@{
        trial = $I; valid = $clicked; clicked = $clicked
        pass = [bool]($clicked -and (P $chk 'is_foreground') -and (P $chk 'has_focus'))
        b_started = $clicked; b_cmdline = (P $bs 'cmdline')
        precondition = (P $t.context 'precondition'); check = $chk; click_method = $t.clicked_by; posts = $t.posts; attempts = $t.attempts
    }
}

# S1.4's verdict from its trials (rerun review M1). FAIL is decided by a valid failure: of a
# no-A trial, or of the with-A trials once the fallback recipe also failed a valid trial.
# PASS needs 5 passes without A and 5 passes with A (basic, or else fallback). Anything else
# is INCOMPLETE: invalid trials that could still change the verdict.
function Get-S14Verdict($WithA, $Fallback, $NoA) {
    $basic = @($WithA); $fb = @($Fallback); $none = @($NoA)
    $validFail = { param($set) @(@($set) | Where-Object { $_.valid -and -not $_.pass }).Count -gt 0 }
    $allPass = { param($set) @(@($set) | Where-Object { $_.pass }).Count -eq 5 }
    $withAFailed = (& $validFail $basic) -and ($fb.Count -eq 0 -or (& $validFail $fb))
    if ((& $validFail $none) -or $withAFailed) { return 'FAIL' }
    $noAPass = & $allPass $none
    if ($noAPass -and (& $allPass $basic)) { return 'PASS' }
    if ($noAPass -and (& $allPass $fb)) { return 'PASS (fallback recipe)' }
    'INCOMPLETE (invalid trials: rerun S1.4)'
}

function Check-S14 {
    Step 'S1.4 foreground: a toast click hands off to the running instance'
    if ($script:ShortcutForm -ne 'toast_clsid') { Write-Shortcut -WithToastClsid }
    Say 'YOUR TURN: each trial opens two small windows (A, then a "winshell blocker" that the script clicks to the front). When a toast appears, click its text yourself: S1.4 measures what a real click allows. Keep your hand off the mouse until the toast is there.' 'Yellow'
    $withA = @(); $fallback = @(); $noA = @(); $synthetic = @()
    $partial = { Save-Partial 'S1.4' ([ordered]@{ shortcut = $script:ShortcutForm; with_a = $withA; with_a_fallback = $fallback; no_a = $noA; with_a_synthetic = $synthetic }) }
    for ($i = 1; $i -le 5; $i++) { $withA += Trial-WithA $i 'basic'; & $partial }
    # The fallback recipe is the FAIL path of a recipe that failed a valid trial, not of a trial
    # whose precondition or click never happened.
    $validFailed = @($withA | Where-Object { $_.valid -and -not $_.pass }).Count
    if ($validFailed -gt 0) {
        Say 'The basic recipe failed a valid trial: the FAIL path runs the same 5 trials with the fallback recipe.' 'Red'
        for ($i = 1; $i -le 5; $i++) { $fallback += Trial-WithA $i 'fallback'; & $partial }
    }
    for ($i = 1; $i -le 5; $i++) { $noA += Trial-NoA $i; & $partial }
    $invalid = @(@($withA) + @($fallback) + @($noA) | Where-Object { -not $_.valid }).Count
    $verdict = Get-S14Verdict $withA $fallback $noA
    # Recorded, not the bar: the same with-A trial clicked by a synthesized mouse click, to show
    # whether a test runner could stand in for the real click (Task 30).
    Say 'HANDS OFF: 5 recorded trials follow in which the script clicks the toasts itself.' 'Yellow'
    for ($i = 1; $i -le 5; $i++) { $synthetic += Trial-WithA $i 'basic' 'synthetic'; & $partial }
    Set-Check 'S1.4' $verdict ([ordered]@{
        invalid_trials = $invalid; shortcut = $script:ShortcutForm; click_for_the_bar = 'owner (a real click)'
        with_a = $withA; with_a_fallback = $fallback; no_a = $noA
        with_a_synthetic = [ordered]@{ recorded_only = $true; passes = @($synthetic | Where-Object { $_.pass }).Count; trials = $synthetic }
    })
}

# S1.5 (automation round): the popups are counted by UIA, the notification centre is read from
# History (and UIA), and the dismissal is done in the notification centre by UIA; the owner
# dismisses it only when UIA can't, and the script sees that by itself (History).
function Check-S15 {
    Step 'S1.5 progress toasts (detected; hands off the mouse)'
    [void](Tool history-clear)
    $before = Get-UiaSnapshot
    $c = Start-Core 's15-progress' @('progress', '--secs', '20')
    Say 'A progress toast updates once a second for 20 s; the script counts how often it pops.'
    $w = Watch-Needle -Needle 'Progress toast (S1.5)' -Before $before -Seconds 26
    $p = Wait-Core $c 90
    $pops = @(P $w.Record 'appearances' | Where-Object { $null -ne $_ }).Count
    $noRepop = [ordered]@{ value = ($pops -le 1); method = 'uia'; popups = $pops; watch = $w.Record }
    if ($pops -eq 0 -and -not $script:Uia.proven) {
        $noRepop.value = Ask 'Did the progress toast pop only once (no second pop while it updated)?'
        $noRepop.method = 'owner (UIA has not found a toast in this run)'
    }

    $dismiss = Dismiss-InNotificationCentre 'Progress toast (S1.5)'
    $gone = $false
    for ($k = 0; $k -lt 10 -and -not $gone; $k++) { Start-Sleep -Milliseconds 500; $gone = (P (Tool history --tag $ProgressTag) 'has_tag') -eq $false }
    $dismissedBy = 'uia'
    if (-not $gone) {
        $dismissedBy = 'owner (detected by History)'
        Say 'YOUR TURN: open the notification centre (Win+N) and close "Spike job running" with its X. The script carries on by itself once it is gone (up to 3 minutes).' 'Yellow'
        for ($k = 0; $k -lt 360 -and -not $gone; $k++) { Start-Sleep -Milliseconds 500; $gone = (P (Tool history --tag $ProgressTag) 'has_tag') -eq $false }
        if (-not $gone) { $dismissedBy = 'not dismissed within 3 minutes' }
    }
    $u = Wait-Core (Start-Core 's15-update-after-dismiss' @('update', '--seq', '100')) 30

    $before = Get-UiaSnapshot
    $c = Start-Core 's15-remove-repost' @('remove-repost')
    $wr = Watch-Needle -Needle 'Posted after Remove(tag, group) (S1.5)' -Before $before -Seconds 8
    $rr = Wait-Core $c 30
    $repost = Resolve-Popped $wr 'Did the "Spike job finished" toast pop?'

    $n = New-Nonce
    $wq = Post-Watched 's15-quiet' @('--tag', 's15-quiet', '--suppress', '--title', 'Quiet toast', '--body', "SuppressPopup = true (S1.5) #$n") "#$n" 8
    $q = $wq.Toast
    $quietPopped = Resolve-Popped $wq.Watch 'Did the quiet toast pop up (slide in)?'
    $qh = Tool history --tag 's15-quiet'
    $quietCentre = Test-InNotificationCentre "#$n"
    $inCentre = (P $qh 'has_tag') -eq $true

    $pass = (P $p 'posted') -and (P $p 'all_succeeded') -and $noRepop.value -and (P $u 'result') -eq 'NotificationNotFound' -and
        (P $rr 'removed') -and (P $rr 'posted') -and $repost.value -and (P $q 'posted') -and -not $quietPopped.value -and $inCentre
    Set-Check 'S1.5' $(if ($pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
        progress = $p; no_second_pop = $noRepop.value; no_second_pop_detail = $noRepop
        dismissal = [ordered]@{ by = $dismissedBy; gone_from_history = $gone; uia = $dismiss }
        update_after_dismiss = (P $u 'result')
        remove_repost = $rr; repost_popped = $repost.value; repost_detail = $repost
        quiet = [ordered]@{ posted = (P $q 'posted'); no_pop = -not $quietPopped.value; no_pop_detail = $quietPopped
                            in_centre = $inCentre; in_centre_method = 'ToastNotificationManager.History'; in_centre_uia = $quietCentre }
    })
}

# The longest run of consecutive samples whose state is in $States.
function Longest-Run($Samples, [string[]]$States) {
    $best = 0; $cur = 0
    foreach ($s in $Samples) { if ($States -contains $s.state) { $cur++; if ($cur -gt $best) { $best = $cur } } else { $cur = 0 } }
    $best
}

function Count-States($Samples) {
    $h = [ordered]@{}
    foreach ($s in $Samples) { if ($h.Contains($s.state)) { $h[$s.state]++ } else { $h[$s.state] = 1 } }
    $h
}

function Check-S16 {
    Step 'S1.6 fullscreen poll (SHQueryUserNotificationState every 2 s, from the detached core)'
    Wait-Owner '(a) Stay at the desktop with ordinary windows for 30 s: nothing fullscreen. Ready?'
    $a = Wait-Core (Start-Core 's16-a-desktop' @('fullscreen', '--secs', '30', '--timing', '100')) 90
    $aSamples = @(P $a 'samples')
    $aPass = $aSamples.Count -gt 0 -and @($aSamples | Where-Object { $_.state -ne 'QUNS_ACCEPTS_NOTIFICATIONS' }).Count -eq 0
    $p99 = P (P $a 'call_ms') 'p99'
    Wait-Owner '(b) Next: start a fullscreen Direct3D game or program. You have 20 s to get it fullscreen; keep it there until you hear a beep (60 s). Ready?'
    $b = Wait-Core (Start-Core 's16-b-d3d' @('fullscreen', '--secs', '60')) 120
    [console]::Beep(880, 400)
    # PASS for (b): at least 5 polls in a row (10 s) read D3D fullscreen or busy.
    $bSamples = @(P $b 'samples')
    $bRun = Longest-Run $bSamples @('QUNS_RUNNING_D3D_FULL_SCREEN', 'QUNS_BUSY')
    $bFirst = @($bSamples | Where-Object { $_.state -in 'QUNS_RUNNING_D3D_FULL_SCREEN', 'QUNS_BUSY' } | Select-Object -First 1)
    $c = $null; $d = $null
    if (Ask '(c) Recorded: play a video fullscreen in a browser next (20 s to set it up, 60 s total, until the beep)?') {
        $c = Wait-Core (Start-Core 's16-c-video' @('fullscreen', '--secs', '60')) 120
        [console]::Beep(880, 400)
    }
    if (Ask '(d) Recorded: is a presentation at hand to run as a slide show (same timing)?') {
        $d = Wait-Core (Start-Core 's16-d-presentation' @('fullscreen', '--secs', '60')) 120
        [console]::Beep(880, 400)
    }
    $pass = $aPass -and $bRun -ge 5 -and $null -ne $p99 -and $p99 -lt 1.0
    Set-Check 'S1.6' $(if ($pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
        a_desktop = [ordered]@{ pass = $aPass; states = Count-States $aSamples; samples = $aSamples }
        call_ms = (P $a 'call_ms')
        b_d3d = [ordered]@{ longest_run = $bRun; first_fullscreen_s = $(if ($bFirst) { $bFirst[0].t_s } else { $null }); states = Count-States $bSamples; samples = $bSamples }
        c_video = $(if ($c) { [ordered]@{ states = Count-States @(P $c 'samples'); samples = (P $c 'samples') } } else { 'skipped' })
        d_presentation = $(if ($d) { [ordered]@{ states = Count-States @(P $d 'samples'); samples = (P $d 'samples') } } else { 'skipped' })
    })
}

function Get-SleepTimeoutSec {
    $q = (& powercfg /query SCHEME_CURRENT SUB_SLEEP STANDBYIDLE) -join "`n"
    $ac = [regex]::Match($q, 'Current AC Power Setting Index:\s*0x([0-9a-fA-F]+)')
    $dc = [regex]::Match($q, 'Current DC Power Setting Index:\s*0x([0-9a-fA-F]+)')
    $onBattery = $false
    $bat = Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue
    if ($bat) { $onBattery = @($bat | Where-Object { $_.BatteryStatus -eq 1 }).Count -gt 0 }
    $use = if ($onBattery) { $dc } else { $ac }
    [ordered]@{
        ac_s = $(if ($ac.Success) { [Convert]::ToInt64($ac.Groups[1].Value, 16) } else { $null })
        dc_s = $(if ($dc.Success) { [Convert]::ToInt64($dc.Groups[1].Value, 16) } else { $null })
        on_battery = $onBattery
        used_s = $(if ($use.Success) { [Convert]::ToInt64($use.Groups[1].Value, 16) } else { $null })
    }
}

function Wait-PowerEvent($C, [string]$Kind, [int]$TimeoutSec, [scriptblock]$Filter = { $true }) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $doc = Read-CoreNow $C
        if ($doc -and $doc.PSObject.Properties['events']) {
            $e = @($doc.events | Where-Object { $_.kind -eq $Kind -and (& $Filter $_) }) | Select-Object -First 1
            if ($e) { return $e }
        }
        Start-Sleep -Milliseconds 500
    }
    $null
}

function Stop-Power($C) {
    New-Item -ItemType File -Force -Path (Join-Path $LogDir 'power-quit.flag') | Out-Null
    $doc = Wait-Core $C 30
    Remove-Item (Join-Path $LogDir 'power-*.flag') -ErrorAction SilentlyContinue
    $doc
}

# The owner's elevated `powercfg /requests` listing of the PowerSetRequest from S1.7's details
# (or the partial details of an interrupted S1.7), with where it came from; $null if none.
function Get-ListingFrom($CheckRecord, [string]$Source) {
    $d = P $CheckRecord 'details'
    foreach ($holder in @($d, (P $d 'partial'))) {
        $l = P $holder 'listing'
        if ($null -ne $l -and (P $l 'api') -eq 'request' -and (P $l 'listed_while_held') -is [bool] -and (P $l 'gone_after_clear') -is [bool]) {
            $src = P $holder 'listing_source'
            return [ordered]@{ listing = $l; source = $(if ($src) { $src } else { $Source }) }
        }
    }
    $null
}

# Owner direction 2026-10-23: the elevated powercfg /requests step is not repeated. The listing
# the owner made in an earlier run (results\run-*\checks.json, newest first) stands for the bar.
function Find-EarlierListing {
    $cur = Get-ListingFrom (P $Report.checks 'S1.7') 'this session'
    if ($null -ne $cur) { return $cur }
    $files = @(Get-ChildItem -Path $Results -Directory -Filter 'run-*' -ErrorAction SilentlyContinue | Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'checks.json' } | Where-Object { Test-Path -LiteralPath $_ })
    foreach ($f in $files) {
        $doc = $null
        try { $doc = Get-Content -Raw -LiteralPath $f | ConvertFrom-Json } catch { continue }
        $hit = Get-ListingFrom (P (P $doc 'checks') 'S1.7') "results\$(Split-Path (Split-Path $f) -Leaf)"
        if ($null -ne $hit) { return $hit }
    }
    $null
}

# The no-input wait while the request is held. GetLastInputInfo shows whether anyone touched
# the PC during it; a touched wait proves nothing and is recorded as invalid.
function Power-StayAwake([string]$Api, $Timeout) {
    if ($null -eq $Timeout.used_s) { return [ordered]@{ api = $Api; result = 'skipped'; why = 'timeout unknown (powercfg output not parsed)' } }
    if ($Timeout.used_s -eq 0) { return [ordered]@{ api = $Api; result = 'skipped'; why = 'sleep timeout is never' } }
    $waitS = [int]$Timeout.used_s + 300
    $until = (Get-Date).AddSeconds($waitS + 30)
    Say ("Stay-awake check: leave the PC alone (no mouse, no keys) until {0:HH:mm} ({1} min). The display may turn off; that is fine." -f $until, [math]::Ceiling(($waitS + 30) / 60)) 'Yellow'
    # The core holds the request well past the wait: it must not clear it by itself (its own
    # limit was 7200 s, shorter than a 2-hour timeout's wait).
    $c = Start-Core "s17-$Api-stayawake" @('power', '--api', $Api, '--max-secs', "$($waitS + 900)")
    [void](Wait-PowerEvent $c 'set' 20)
    $t0 = Get-Date
    Start-Sleep -Seconds ($waitS + 30)
    $elapsed = ((Get-Date) - $t0).TotalSeconds
    $idle = Get-IdleSeconds
    $doc = Stop-Power $c
    $slept = @(P $doc 'events' | Where-Object { ($_.kind -eq 'power_broadcast' -and $_.pbt -eq 'PBT_APMSUSPEND') -or $_.kind -eq 'clock_jump' })
    # An independent witness: the System log's Kernel-Power sleep, resume and Modern Standby
    # entries since the request was set (42 sleep, 107 resume, 506/507 standby enter/exit) (M6).
    $kernel = @()
    try {
        $kernel = @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = 'Microsoft-Windows-Kernel-Power'; Id = 42, 107, 506, 507; StartTime = $t0 } -ErrorAction Stop |
            ForEach-Object { [ordered]@{ id = $_.Id; at = $_.TimeCreated.ToUniversalTime().ToString('o') } })
    } catch [Exception] {
        if ($_.Exception.Message -notmatch 'No events were found') { $kernel = @([ordered]@{ error = $_.Exception.Message }) }
    }
    $kernelSlept = @($kernel | Where-Object { (P $_ 'id') -in 42, 506 })
    $r = [ordered]@{
        api = $Api; timeout_s = $Timeout.used_s; waited_s = [int]$elapsed; slept_events = $slept
        kernel_power_events = $kernel
        idle_s_at_end = $idle; idle_method = 'GetLastInputInfo'
        pass = ($slept.Count -eq 0 -and $kernelSlept.Count -eq 0 -and @($kernel | Where-Object { P $_ 'error' }).Count -eq 0)
        core = $doc
    }
    # Input during the wait: the PC stayed awake for that reason, not the request.
    if ($null -ne $idle -and $idle -lt ($elapsed - 5) -and $slept.Count -eq 0 -and $kernelSlept.Count -eq 0) {
        $r.result = 'invalid'; $r.why = "input $idle s before the end of the wait"; $r.pass = $null
    }
    $r
}

# Whether one power API passed its checks: listed while held, gone after the clear, and the
# stay-awake wait passed or was skipped. Every field is read through P: Power-StayAwake's
# result has `result` only when skipped or invalid and `pass` only when run, and strict mode
# throws on a missing dictionary key (rerun review I1; tests/strict-results.ps1).
# -ListingOptional: no listing is needed (SetThreadExecutionState, whose listing would need the
# elevated step the owner direction of 2026-10-23 dropped), but a listing that is there counts.
function Test-PowerApiOk($Listing, $Stay, [switch]$ListingOptional) {
    $listed = (P $Listing 'listed_while_held') -eq $true -and (P $Listing 'gone_after_clear') -eq $true
    if ($ListingOptional -and $null -eq $Listing) { $listed = $true }
    [bool]($listed -and ((P $Stay 'result') -eq 'skipped' -or (P $Stay 'pass') -eq $true))
}

# $SleepPass is $true or $false from the sleep-and-wake check, $null when it hasn't run.
function Get-S17Verdict($Listing, $Stay, $Ste, $SleepPass) {
    $requestOk = Test-PowerApiOk $Listing $Stay
    $steOk = [bool]($null -ne $Ste -and (Test-PowerApiOk (P $Ste 'listing') (P $Ste 'stay_awake') -ListingOptional))
    $sleepOk = $SleepPass -eq $true
    $verdict = if ($requestOk -and $sleepOk) { 'PASS' }
        elseif ($steOk -and $sleepOk) { 'PASS (SetThreadExecutionState)' }
        elseif ($null -eq $SleepPass -and ($requestOk -or $steOk)) { 'INCOMPLETE (the sleep-and-wake check has not run)' }
        elseif ((P $Stay 'result') -eq 'invalid' -and $null -eq $Ste) { 'INCOMPLETE (input during the stay-awake wait: rerun it)' }
        elseif ($null -eq $Listing -and $null -eq $Ste -and (Test-PowerApiOk $null $Stay -ListingOptional) -and $sleepOk) { 'INCOMPLETE (no powercfg listing on record)' }
        else { 'FAIL' }
    [ordered]@{
        request_ok = $requestOk; ste_ok = $steOk
        api = $(if ($requestOk) { 'PowerSetRequest' } elseif ($steOk) { 'SetThreadExecutionState' } else { 'none' })
        verdict = $verdict
    }
}

# The FAIL path (SetThreadExecutionState) runs only on a PowerSetRequest result that failed: a
# listing that says not listed or not gone, or a stay-awake wait in which the PC slept.
function Test-NeedSte($Listing, $Stay) {
    $listingFailed = $null -ne $Listing -and -not ((P $Listing 'listed_while_held') -eq $true -and (P $Listing 'gone_after_clear') -eq $true)
    [bool]($listingFailed -or (P $Stay 'pass') -eq $false)
}

# The sleep-and-wake check (a body step: the owner sleeps and wakes the PC). The script sees the
# sleep and the wake through WM_POWERBROADCAST on the core's hidden window.
function Invoke-SleepWake {
    $c = Start-Core 's17-sleep' @('power', '--api', 'none', '--max-secs', '3600')
    [void](Wait-PowerEvent $c 'set' 20)
    Say 'YOUR TURN, sleep and wake: put the PC to sleep now (Start, Power, Sleep), wait about a minute, then wake it and sign in. The script notices the wake by itself (it waits up to 15 minutes).' 'Yellow'
    $resumed = Wait-PowerEvent $c 'power_broadcast' 900 { param($e) $e.pbt -eq 'PBT_APMRESUMEAUTOMATIC' }
    Start-Sleep -Seconds 35   # one more 30 s clock tick after the wake
    $doc = Stop-Power $c
    $pbts = @(P $doc 'events' | Where-Object { $_.kind -eq 'power_broadcast' } | ForEach-Object { $_.pbt })
    [ordered]@{
        pass = (($pbts -contains 'PBT_APMSUSPEND') -and ($pbts -contains 'PBT_APMRESUMEAUTOMATIC'))
        method = 'WM_POWERBROADCAST on the core''s hidden window'; broadcasts = $pbts; resumed = $resumed; core = $doc
    }
}

# S1.7 in parts ($PowerPart): 'sleep-wake' in the owner-present stretch, 'stay-awake' at the
# very end of the session (run-all.ps1, with -Append), or 'all'.
function Check-S17 {
    Step "S1.7 keep-awake ($PowerPart)"
    $prevDetails = P (P $Report.checks 'S1.7') 'details'
    $a = (& powercfg /a) -join "`n"
    $availablePart = ($a -split 'The following sleep states are not available')[0]
    $modernStandby = $availablePart -match 'S0 Low Power Idle'
    $timeout = Get-SleepTimeoutSec
    Say ("Sleep timeout in use: {0}" -f $(if ($null -eq $timeout.used_s) { 'unknown (powercfg output not parsed)' } elseif ($timeout.used_s) { "$($timeout.used_s) s" } else { 'never' }))

    $earlier = Find-EarlierListing
    $listing = P $earlier 'listing'
    $base = [ordered]@{
        modern_standby = $modernStandby; powercfg_a = $a; sleep_timeout = $timeout
        listing = $listing; listing_source = (P $earlier 'source')
        listing_method = 'the owner''s elevated powercfg /requests, from an earlier run; not repeated (owner direction 2026-10-23)'
    }
    $sleep = P $prevDetails 'sleep_wake'
    if ($PowerPart -ne 'stay-awake') {
        $sleep = Invoke-SleepWake
    }
    $base.sleep_wake = $sleep
    if ($PowerPart -eq 'sleep-wake') {
        $base.stay_awake = 'runs last in the session (run-all.ps1)'
        Set-Check 'S1.7' 'PENDING (the stay-awake wait runs last)' $base
        return
    }
    Save-Partial 'S1.7' $base
    $stay = Power-StayAwake 'request' $timeout
    $base.stay_awake = $stay; Save-Partial 'S1.7' $base
    $ste = $null
    if (Test-NeedSte $listing $stay) {
        Say 'PowerSetRequest failed a check: the FAIL path repeats the stay-awake wait with SetThreadExecutionState.' 'Red'
        $ste = [ordered]@{ listing = $null; listing_note = 'not run: it needs the elevated step that the owner direction of 2026-10-23 dropped'; stay_awake = (Power-StayAwake 'ste' $timeout) }
        $base.ste = $ste; Save-Partial 'S1.7' $base
    }

    $sleepPass = $(if ($null -eq $sleep) { $null } else { [bool](P $sleep 'pass') })
    $v = Get-S17Verdict $listing $stay $ste $sleepPass
    $base.api = $v.api
    $base.sleep_gap_fallback_needed = $(if ($null -eq $sleepPass) { $null } else { -not $sleepPass })
    Set-Check 'S1.7' $v.verdict $base
}

function Check-S18 {
    Step 'S1.8 Windows Hello (keep this terminal in front)'
    $a = HelloCmd supported
    $d = HelloCmd tpm
    $dp = HelloCmd dpapi
    Say "(a) supported: $(P $a 'supported'); (d) TPM: $(P $d 'tpm_version'); DPAPI round trip: $(P $dp 'round_trip')"
    $b = $null; $bFront = $null; $c1 = $null; $c2 = $null
    if (P $a 'supported') {
        Say 'YOUR TURN, Windows Hello: complete each prompt (PIN, finger or face). First one prompt to create the spike key; the script checks by itself whether the prompt comes in front.' 'Yellow'
        $bw = HelloWatched @('create') 'Was the Hello prompt in front of the terminal?'
        $b = $bw.Result; $bFront = $bw.Front
        Say 'Next: up to 10 prompts, 5 and then 5 more from a second process. Complete each one.' 'Yellow'
        $c1 = HelloCmd sign --count 5
        $c2 = HelloCmd sign --count 5
    }
    Say 'Last: one verification prompt. Complete it.' 'Yellow'
    $ew = HelloWatched @('verify') 'Was the verification prompt in front of the terminal?'
    $e = $ew.Result; $eFront = $ew.Front
    $f = $null; $fGone = $null
    if (P $a 'supported') { $f = HelloCmd delete; $fGone = HelloCmd exists }

    $sigs = @()
    foreach ($run in @($c1, $c2)) { $sigs += @(P $run 'signatures' | Where-Object { $_ }) }
    $hexes = @($sigs | Where-Object { (P $_ 'status') -eq 'Success' } | ForEach-Object { P $_ 'signature_hex' })
    $cPass = $hexes.Count -eq 10 -and @($hexes | Select-Object -Unique).Count -eq 1
    $bPass = (P $b 'status') -eq 'Success' -and (P $bFront 'value') -eq $true
    $fPass = (P $f 'deleted') -eq $true -and (P $fGone 'status') -eq 'NotFound'
    $ePass = (P $e 'result') -eq 'Verified' -and (P $eFront 'value') -eq $true
    $route = if ((P $a 'supported') -and $bPass -and $cPass -and (P $d 'tpm_2_0') -and $fPass) { 'KEK' }
             elseif ($ePass -and (P $dp 'round_trip')) { 'DPAPI with the Hello gate' }
             else { 'passphrase only' }
    $verdict = if ($route -eq 'passphrase only') { 'FAIL (passphrase only: report to the owner)' } else { 'PASS' }
    Set-Check 'S1.8' $verdict ([ordered]@{
        route = $route
        a_supported = $a; b_create = $b; b_prompt_in_front = (P $bFront 'value'); b_front_detail = $bFront
        c_identical = $cPass; c_count = $hexes.Count; c_process1 = $c1; c_process2 = $c2
        d_tpm = $d; e_verify = $e; e_prompt_in_front = (P $eFront 'value'); e_front_detail = $eFront; f_delete = $f; f_after = $fGone
        dpapi = $dp
    })
}

# ---------------------------------------------------------------- main

# Runs one check. An error inside it is recorded as that check's ERROR verdict, with the
# message and line, and the run goes on to the next check; only the S1.1 and S1.2 stop rule
# (an error starting "STOP:") ends the run (Part B run 1 analysis).
function Invoke-Check([string]$Id, [scriptblock]$Body) {
    if (-not (Want $Id)) { return }
    try { & $Body }
    catch {
        if ($_.Exception.Message -like 'STOP:*') { throw }
        $err = [ordered]@{
            message = $_.Exception.Message
            line = $_.InvocationInfo.ScriptLineNumber
            statement = "$($_.InvocationInfo.Line)".Trim()
            stack = "$($_.ScriptStackTrace)"
        }
        Say "$Id stopped with an error: $($err.message) (line $($err.line)). Going on to the next check." 'Red'
        if ($Report.checks.Contains($Id)) { $err.partial = P $Report.checks[$Id] 'details' }
        Set-Check $Id 'ERROR' $err
        try { Stop-SpikeProcesses } catch { }
    }
}

if ($LibraryOnly) { return }

$identity = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from a normal (not elevated) terminal: an elevated sender''s toasts do not show.'
}
if ($CleanupOnly) {
    $Lnk = Join-Path (Get-ProgramsDir) 'LILITH Spike.lnk'
    $c = Invoke-Cleanup
    $c | ConvertTo-Json -Depth 6
    Say ("Cleanup -> {0}" -f $(if ($c.pass) { 'PASS' } else { 'FAIL' })) $(if ($c.pass) { 'Green' } else { 'Red' })
    return
}

# A rerun never overwrites an earlier run's results: they move to results\run-<UTC time>\.
# raw\ moves first, then checks.json; if either move fails the script stops here, before
# anything is registered, so the next run never writes into the old raw\ (rerun review M7).
$prevChecks = Join-Path $Results 'checks.json'
$stageStarted = (Get-Date).ToUniversalTime().ToString('o')
if ($Append -and (Test-Path $prevChecks)) {
    # A later stage of the same session (run-all.ps1): the checks so far stay, and this stage
    # adds to them. Its event log is kept under its own name.
    $old = Get-Content -Raw $prevChecks | ConvertFrom-Json
    $Report.started = P $old 'started'
    $Report.setup = P $old 'setup'
    $Report.cleanup = P $old 'cleanup'
    foreach ($prop in @((P $old 'checks').PSObject.Properties)) { $Report.checks[$prop.Name] = $prop.Value }
    $Report.stages = @(P $old 'stages' | Where-Object { $null -ne $_ })
    $script:EventsKeepName = 'events-' + (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ') + '.jsonl'
    Say 'Adding to the results of this session (results\checks.json).'
} elseif (Test-Path $prevChecks) {
    $stamp = $null
    try {
        $old = Get-Content -Raw $prevChecks | ConvertFrom-Json
        $stamp = ([datetime](P $old 'started')).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
    } catch { }
    if (-not $stamp) { $stamp = (Get-Item -LiteralPath $prevChecks).LastWriteTimeUtc.ToString('yyyyMMddTHHmmssZ') }
    $keep = Join-Path $Results "run-$stamp"
    if (Test-Path $keep) { $keep = "$keep-moved-" + (Get-Date).ToUniversalTime().ToString('HHmmss') }
    try {
        New-Item -ItemType Directory -Force -Path $keep | Out-Null
        if (Test-Path $Raw) { Move-Item -LiteralPath $Raw -Destination (Join-Path $keep 'raw') -ErrorAction Stop }
        Move-Item -LiteralPath $prevChecks -Destination $keep -ErrorAction Stop
    } catch {
        throw "Could not move the earlier results into $keep ($($_.Exception.Message)). Close anything that has a file in spikes\winshell\results open, then run the command again. Nothing was registered."
    }
    Say "The earlier run's results were moved to $keep"
}
New-Item -ItemType Directory -Force -Path $Raw | Out-Null
# The console, with every message and any answer, kept beside the results (git ignores it).
Start-Transcript -Path (Join-Path $Raw 'console.log') -IncludeInvocationHeader -Append:$Append | Out-Null

if (-not $SkipBuild -and -not (Want 'S1.1')) {
    Push-Location $Spike
    try { & cargo build --release --locked; if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' } } finally { Pop-Location }
}
if (-not $SkipBuild) { Invoke-Check 'S1.1' { Check-S11 } }

$setup = [ordered]@{
    os_build   = [Environment]::OSVersion.Version.ToString()
    os_display = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').DisplayVersion
    rustc      = (& rustc --version)
    pwsh       = $PSVersionTable.PSVersion.ToString()
    token      = (Tool token)
}
if (-not $Append) { $Report.setup = $setup }
$stage = [ordered]@{ started = $stageStarted; only = $Only; power_part = $PowerPart; append = [bool]$Append; setup = $setup }
$Report.stages = @(P $Report 'stages' | Where-Object { $null -ne $_ }) + @($stage)
$Lnk = Join-Path (Get-ProgramsDir) 'LILITH Spike.lnk'

try {
    Step 'Setup: clearing anything left from an earlier run, then registering'
    $pre = Invoke-Cleanup
    $setup.pre_cleanup = $pre
    New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
    Write-Shortcut
    Register-Scheme
    $setup.shortcut = Tool shortcut-read --path $Lnk
    $setup.setting = (Tool setting).setting
    # The calibration toast proves UIA can see this PC's toasts before any check relies on it.
    if (@('S1.2', 'S1.2p', 'S1.3', 'S1.4', 'S1.5' | Where-Object { Want $_ }).Count -gt 0) {
        Say 'One calibration toast: the script checks that it can see toasts on this PC. Hands off.'
        $setup.uia = Invoke-UiaCalibration
        Say ("Detection: {0}" -f $setup.uia.result)
    }
    Save-Report

    Invoke-Check 'S1.2' { Check-S12 }
    Invoke-Check 'S1.2p' { Check-S12p }
    Invoke-Check 'S1.3' { Check-S13 }
    Invoke-Check 'S1.4' { Check-S14 }
    Invoke-Check 'S1.5' { Check-S15 }
    Invoke-Check 'S1.6' { Check-S16 }
    Invoke-Check 'S1.7' { Check-S17 }
    Invoke-Check 'S1.8' { Check-S18 }
}
finally {
    Step 'Cleanup'
    $Report.cleanup = Invoke-Cleanup -Final
    $stage.cleanup = $Report.cleanup
    $stage.uia_hosts = $script:Uia.hosts
    $Report.finished = (Get-Date).ToUniversalTime().ToString('o')
    Save-Report
    Say ("Cleanup -> {0}" -f $(if ($Report.cleanup.pass) { 'PASS' } else { 'FAIL' })) $(if ($Report.cleanup.pass) { 'Green' } else { 'Red' })
    Say "Results: $(Join-Path $Results 'checks.json')"
    try { Stop-Transcript | Out-Null } catch { }
}
