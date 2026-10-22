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

Run it from a normal (not elevated) Windows Terminal. See OWNER-STEPS.md.

.PARAMETER Only
Runs only these checks, for example the rerun after run 1:
  -Only S1.2p,S1.3,S1.4,S1.5,S1.6,S1.7,S1.8
Setup and cleanup always run. An earlier checks.json and raw\ are moved into
results\run-<UTC time>\ first, never overwritten.

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
    [switch]$LibraryOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

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
$ToastClsid = '{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}'
$ClsidKey = "HKCU:\Software\Classes\CLSID\$ToastClsid"

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

# ---------------------------------------------------------------- registration

$Lnk = $null

# The shortcut: plain (the AUMID only), or with the spike's ToastActivatorCLSID and its COM
# registration, whose LocalServer32 is winshell-app.exe (S1.2p and everything after it).
$script:ShortcutForm = 'plain'

function Write-Shortcut([switch]$ToastClsid) {
    $extra = @()
    if ($ToastClsid) {
        New-Item -Path "$ClsidKey\LocalServer32" -Value "`"$App`"" -Force | Out-Null
        $extra = @('--toast-clsid')
    } elseif (Test-Path $ClsidKey) {
        Remove-Item -Path $ClsidKey -Recurse -Force
    }
    $w = Tool shortcut-write --path $Lnk --target $App --aumid $Aumid @extra
    if (-not (P $w 'written')) { throw "shortcut not written: $(P $w 'error')" }
    $r = Tool shortcut-read --path $Lnk
    if ((P $r 'aumid') -ne $Aumid) { throw "shortcut AUMID reads back as '$(P $r 'aumid')'" }
    if ($ToastClsid -and (P $r 'toast_clsid') -ne $ToastClsid) { throw "shortcut ToastActivatorCLSID reads back as '$(P $r 'toast_clsid')'" }
    $script:ShortcutForm = $(if ($ToastClsid) { 'toast_clsid' } else { 'plain' })
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
                Copy-Item $Events (Join-Path $Raw 'events.jsonl') -Force
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
    Say 'Watch the bottom-right corner of the screen. Do not click these toasts.'
    $trials = @()
    for ($i = 1; $i -le 3; $i++) {
        Wait-Owner "Ready for toast $i of 3?"
        $r = Post-Toast "s12-$i" @('--tag', "s12-$i", '--title', 'LILITH spike', '--body', "Toast $i of 3 (S1.2)")
        $seen = Ask "Did toast $i of 3 pop?"
        $hist = Tool history --tag "s12-$i"
        $trials += [ordered]@{ trial = $i; posted = (P $r 'posted'); setting = (P $r 'setting'); error = (P $r 'error'); seen = $seen; in_history = (P $hist 'has_tag') }
    }
    $pass = @($trials | Where-Object { $_.posted -and $_.seen -and $_.setting -eq 'Enabled' }).Count -eq 3

    Say 'Recorded, not a bar: the shortcut removed, only HKCU\Software\Classes\AppUserModelId present.'
    Remove-Item -LiteralPath $Lnk -Force
    $script:AumidParentExisted = [bool](Test-Path $AumidParent)
    New-Item -Path $AumidKey -Force | Out-Null
    New-ItemProperty -Path $AumidKey -Name 'DisplayName' -Value 'LILITH Spike' -PropertyType String -Force | Out-Null
    Start-Sleep -Seconds 3
    Wait-Owner 'Ready for one more toast (no shortcut now)?'
    $r = Post-Toast 's12-regonly' @('--tag', 's12-regonly', '--title', 'LILITH spike', '--body', 'Registry AUMID only (S1.2, recorded)')
    $seenRegOnly = Ask 'Did that toast pop?'
    Remove-Item -Path $AumidKey -Recurse -Force
    Write-Shortcut

    Set-Check 'S1.2' $(if ($pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
        trials = $trials
        recorded_registry_only = [ordered]@{ posted = (P $r 'posted'); setting = (P $r 'setting'); error = (P $r 'error'); seen = $seenRegOnly }
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
# time after the banner has gone, and by the owner opening the notification centre.
function Check-S12p {
    Step 'S1.2p recorded: does a missed toast stay in the notification centre?'
    $banner = Get-BannerSeconds
    $wait = [math]::Max(20, $banner + 15)
    Say "Two toasts follow, one at a time. Do NOT click them and do NOT close them: let each one slide away by itself (about $banner s)."
    $forms = [ordered]@{}
    foreach ($form in 'plain', 'toast_clsid') {
        if ($form -eq 'toast_clsid') { Write-Shortcut -ToastClsid } else { Write-Shortcut }
        Wait-Owner "Ready for the '$form' toast?"
        $tag = "persist-$form"
        $r = Post-Toast "s12p-$form" @('--tag', $tag, '--launch', 'lilspike:task/j_0123456789abcdef',
                '--title', 'LILITH spike', '--body', "Persistence check ($form): leave this toast alone")
        $h0 = Tool history --tag $tag
        Say "Waiting $wait s for the banner to go..."
        Start-Sleep -Seconds $wait
        $h1 = Tool history --tag $tag
        $seen = Ask "Now press Win+N. Is 'Persistence check ($form)' listed in the notification centre?"
        $forms[$form] = [ordered]@{
            posted = (P $r 'posted'); setting = (P $r 'setting')
            in_history_at_once = (P $h0 'has_tag'); in_history_after_banner = (P $h1 'has_tag')
            owner_sees_it_in_win_n = $seen; waited_s = $wait; notify_settings = (Get-NotifySettings)
        }
    }
    Set-Check 'S1.2p' 'RECORDED' ([ordered]@{ banner_seconds = $banner; forms = $forms; shortcut_used_from_now = $script:ShortcutForm })
}

# One click trial: posts the toast and waits for winshell-app to start. When no click arrives
# within 60 s the owner is asked whether the toast was missed; a missed toast is posted again
# (up to 3 posts) instead of failing the trial (Part B run 1: a missed toast couldn't be found).
function Click-Trial([string]$Name, [string[]]$ToastArgs, [string]$Instruction, [scriptblock]$Prepare, [scriptblock]$Await) {
    $posts = @()
    for ($post = 1; $post -le 3; $post++) {
        $ctx = & $Prepare
        if ($ctx -is [System.Collections.IDictionary] -and $ctx.Contains('invalid')) { return $ctx }
        $n = @(Get-Events).Count
        $r = Post-Toast "$Name-$post" $ToastArgs
        Say $Instruction
        $result = & $Await $n $ctx 60
        if ($null -ne $result) {
            return [ordered]@{ result = $result; context = $ctx; posts = $post; missed_before = $posts }
        }
        $missed = Ask 'No click arrived within 60 s. Did you miss the toast (or not find it)? Answer y to get it again'
        $posts += [ordered]@{ post = $post; toast = $r; owner_says_missed = $missed; shortcut = $script:ShortcutForm }
        if (-not $missed) {
            $late = & $Await $n $ctx 60
            if ($null -ne $late) { return [ordered]@{ result = $late; context = $ctx; posts = $post; missed_before = $posts } }
            break
        }
        Stop-SpikeProcesses
    }
    [ordered]@{ result = $null; context = $null; posts = $posts.Count; missed_before = $posts }
}

function Check-S13 {
    Step 'S1.3 protocol activation: the raw command line'
    if ($script:ShortcutForm -ne 'toast_clsid') { Write-Shortcut -ToastClsid }
    $expected = "`"$App`" `"$ConfirmUrl`""
    $trials = @()
    $targets = @('body', 'body', 'body', 'button', 'button', 'button')
    for ($i = 0; $i -lt $targets.Count; $i++) {
        $where = $targets[$i]
        $what = $(if ($where -eq 'body') { 'BODY of this toast' } else { 'Open button' })
        $t = Click-Trial "s13-$($i + 1)" @('--tag', "s13-$($i + 1)", '--launch', $ConfirmUrl, '--button', $ConfirmUrl,
                '--title', 'LILITH spike', '--body', "Click the $what (S1.3, $($i + 1) of 6)") `
            "Click the toast's $(if ($where -eq 'body') { 'BODY (not the button)' } else { 'Open BUTTON' }). If it has gone, open the notification centre (Win+N) and click it there." `
            { Stop-SpikeProcesses; [ordered]@{} } `
            { param($n, $ctx, $timeout) Wait-SpikeEvent 'start' $n $timeout }
        $cmd = P $t.result 'cmdline'
        $trials += [ordered]@{ trial = $i + 1; clicked = $where; cmdline = $cmd; exact = ($cmd -ceq $expected); posts = $t.posts; missed = $t.missed_before }
        Start-Sleep -Seconds 2
    }
    Stop-SpikeProcesses
    $pass = @($trials | Where-Object { $_.exact }).Count -eq 6

    # Recorded: Edge opening a raw ref from a local HTML file.
    $hex = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
    $ref = "lilspike://0123456789abcdef/sketchbook/lantern@1.0.0#sha256=$hex"
    $html = Join-Path $LogDir 'links.html'
    Set-Content -Encoding utf8 $html "<!doctype html><meta charset=`"utf-8`"><title>spike 11</title><p><a href=`"$ref`">Open the spike ref</a></p>"
    $recorded = [ordered]@{}
    if (Ask 'Recorded check: open a link in Edge now? (Edge asks before opening the app; do NOT tick "Always allow")') {
        $n = @(Get-Events).Count
        Start-Process msedge -ArgumentList "`"$html`""
        Say 'In Edge: click "Open the spike ref", then Open in Edge''s prompt.'
        $ev = Wait-SpikeEvent 'start' $n 180
        $recorded.edge = [ordered]@{ sent = $ref; cmdline = $(if ($ev) { $ev.cmdline } else { $null }) }
        Start-Sleep -Seconds 2
        Stop-SpikeProcesses
    }
    # Recorded: the Run dialog with lilspike:open/<base64url>, plain and with a trailing '/'.
    $target = 'lil://0123456789abcdef/sketchbook/lantern@1.0.0?~é'
    $b64 = (Tool b64url $target).b64url
    # The owner's clipboard is put back after the Run-dialog checks (fix round 1, M7).
    $savedClipboard = $null
    try { $savedClipboard = Get-Clipboard -Raw } catch { }
    foreach ($suffix in '', '/') {
        $url = "lilspike:open/$b64$suffix"
        if (Ask "Recorded check: the Run dialog with $url ? (it is copied to the clipboard)") {
            # Run 1: both "Run dialog" arrivals carried the Edge page's link instead, so what
            # arrives is now checked, and the owner gets up to 3 tries (Part B run 1 analysis).
            $tries = @()
            for ($try = 1; $try -le 3; $try++) {
                Set-Clipboard -Value $url
                $n = @(Get-Events).Count
                Say 'Close the Edge tab with the test page if it is still open. Then press Win+R, paste with Ctrl+V, press Enter.'
                $ev = Wait-SpikeEvent 'start' $n 180
                $cmd = P $ev 'cmdline'
                $fromRun = [bool]($cmd -and $cmd.Contains("open/$b64"))
                $tries += [ordered]@{ try = $try; cmdline = $cmd; came_from_the_run_box = $fromRun }
                Start-Sleep -Seconds 2
                Stop-SpikeProcesses
                if ($fromRun -or -not $cmd) { break }
                Say 'That start did not carry the Run box link (it came from somewhere else, perhaps the Edge page). Once more, please.' 'Yellow'
            }
            $last = $tries[-1]
            $recorded["run$(if ($suffix) { '_trailing_slash' } else { '' })"] = [ordered]@{ sent = $url; cmdline = $(if ($last.came_from_the_run_box) { $last.cmdline } else { $null }); tries = $tries }
        }
    }
    if ($null -ne $savedClipboard) { try { Set-Clipboard -Value $savedClipboard } catch { } }
    Set-Check 'S1.3' $(if ($pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
        expected = $expected; shortcut = $script:ShortcutForm; trials = $trials; recorded = $recorded; base64url_target = $target
    })
}

# The precondition of every S1.4 trial: the blocker window holds the front before the toast is
# posted. Run 1 showed that a blocker started by the script often can't take the front once A
# has it (SetForegroundWindow false: the script's terminal is no longer the foreground process).
# Then the owner clicks the blocker's title bar once, which gives it the front as a person
# would; the script checks the foreground window itself (Part B run 1 analysis).
function Ensure-BlockerFront($Blocker, $Check) {
    if (P $Check 'is_foreground') { return [ordered]@{ how = 'blocker took it'; ok = $true } }
    Say "Click once on the title bar of the window 'winshell blocker' (not on 'winshell-app'), then wait." 'Yellow'
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $deadline) {
        $fg = Tool foreground
        if ((P $fg 'fg_pid') -eq $Blocker.Id) { return [ordered]@{ how = 'owner clicked it'; ok = $true; foreground = $fg } }
        Start-Sleep -Milliseconds 300
    }
    [ordered]@{ how = 'not in front after 60 s'; ok = $false }
}

function Trial-WithA([int]$I, [string]$Recipe) {
    $t = Click-Trial "s14-a-$Recipe-$I" @('--tag', "s14-a-$I", '--launch', $ConfirmUrl, '--title', 'LILITH spike',
            '--body', "Click this toast's body (S1.4 with A, $I of 5)") `
        "Click the toast's BODY (S1.4, A running behind the blocker window, $I of 5)." `
        {
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
        { param($n, $ctx, $timeout) Wait-SpikeEvent 'front_check' $n $timeout { param($e) $e.pid -eq $ctx.a_pid -and $e.phase -eq 'activate' } }
    Stop-SpikeProcesses
    if ($t.Contains('invalid')) { return [ordered]@{ trial = $I; recipe = $Recipe; valid = $false; pass = $false; detail = $t } }
    $chk = $t.result
    $hand = @(Get-Events) | Where-Object { $_.kind -eq 'handoff' } | Select-Object -Last 1
    $clicked = $null -ne $chk
    [ordered]@{
        trial = $I; recipe = $Recipe; valid = $clicked; clicked = $clicked
        pass = [bool]((P $chk 'is_foreground') -and (P $chk 'has_focus'))
        a_pid = (P $t.context 'a_pid'); precondition = (P $t.context 'precondition'); handoff = $hand; check = $chk
        posts = $t.posts; missed = $t.missed_before
    }
}

function Trial-NoA([int]$I) {
    $t = Click-Trial "s14-noa-$I" @('--tag', "s14-noa-$I", '--launch', $ConfirmUrl, '--title', 'LILITH spike',
            '--body', "Click this toast's body (S1.4 no A, $I of 5)") `
        "Click the toast's BODY (S1.4, no A running, $I of 5)." `
        {
            Stop-SpikeProcesses
            $n = @(Get-Events).Count
            $b = Start-Process -FilePath $App -ArgumentList '--blocker', '--exit-after-secs', '300' -PassThru
            $blk = Wait-SpikeEvent 'front_check' $n 20 { param($e) $e.pid -eq $b.Id }
            $front = Ensure-BlockerFront $b $blk
            if (-not $front.ok) { return [ordered]@{ invalid = $true; blocker_front = $blk; precondition = $front } }
            [ordered]@{ blocker_pid = $b.Id; blocker_front = $blk; precondition = $front }
        } `
        { param($n, $ctx, $timeout) Wait-SpikeEvent 'front_check' $n $timeout { param($e) $e.pid -ne $ctx.blocker_pid -and $e.phase -eq 'start' } }
    Stop-SpikeProcesses
    if ($t.Contains('invalid')) { return [ordered]@{ trial = $I; valid = $false; pass = $false; detail = $t } }
    $chk = $t.result
    $clicked = $null -ne $chk
    [ordered]@{
        trial = $I; valid = $clicked; clicked = $clicked
        pass = [bool]((P $chk 'is_foreground') -and (P $chk 'has_focus'))
        precondition = (P $t.context 'precondition'); check = $chk; posts = $t.posts; missed = $t.missed_before
    }
}

function Check-S14 {
    Step 'S1.4 foreground: a toast click hands off to the running instance'
    if ($script:ShortcutForm -ne 'toast_clsid') { Write-Shortcut -ToastClsid }
    Say 'Each trial opens two small windows (A, then a "winshell blocker" in front). Leave them; click only the toast, or the blocker when asked.'
    $withA = @(); for ($i = 1; $i -le 5; $i++) { $withA += Trial-WithA $i 'basic' }
    $withAPass = @($withA | Where-Object { $_.pass }).Count -eq 5
    # The fallback recipe is the FAIL path of a recipe that failed a valid trial, not of a trial
    # whose precondition or click never happened.
    $validFailed = @($withA | Where-Object { $_.valid -and -not $_.pass }).Count
    $fallback = @()
    if ($validFailed -gt 0) {
        Say 'The basic recipe failed a valid trial: the FAIL path runs the same 5 trials with the fallback recipe.' 'Red'
        for ($i = 1; $i -le 5; $i++) { $fallback += Trial-WithA $i 'fallback' }
    }
    $fallbackPass = $fallback.Count -gt 0 -and @($fallback | Where-Object { $_.pass }).Count -eq 5
    $noA = @(); for ($i = 1; $i -le 5; $i++) { $noA += Trial-NoA $i }
    $noAPass = @($noA | Where-Object { $_.pass }).Count -eq 5
    $invalid = @(@($withA) + @($fallback) + @($noA) | Where-Object { -not $_.valid }).Count
    $verdict = if ($withAPass -and $noAPass) { 'PASS' } elseif ($fallbackPass -and $noAPass) { 'PASS (fallback recipe)' } else { 'FAIL' }
    if ($verdict -eq 'FAIL' -and $invalid -gt 0) { $verdict = 'INCOMPLETE (invalid trials: rerun S1.4)' }
    Set-Check 'S1.4' $verdict ([ordered]@{ invalid_trials = $invalid; shortcut = $script:ShortcutForm; with_a = $withA; with_a_fallback = $fallback; no_a = $noA })
}

function Check-S15 {
    Step 'S1.5 progress toasts'
    [void](Tool history-clear)
    Wait-Owner 'A progress toast pops next and updates once a second for 20 s. Watch whether it pops a SECOND time. Ready?'
    $p = Wait-Core (Start-Core 's15-progress' @('progress', '--secs', '20')) 90
    $noRepop = Ask 'Did the progress toast pop only once (no second pop while it updated)?'
    Wait-Owner 'Now open the notification centre (Win+N) and dismiss the "Spike job running" toast with its X. Then come back here'
    $u = Wait-Core (Start-Core 's15-update-after-dismiss' @('update', '--seq', '100')) 30
    Wait-Owner 'Next: Remove(tag, group), then a new "Spike job finished" toast. Ready?'
    $rr = Wait-Core (Start-Core 's15-remove-repost' @('remove-repost')) 30
    $repostPopped = Ask 'Did the "Spike job finished" toast pop?'
    Wait-Owner 'Next: a quiet toast (SuppressPopup = true). It should NOT pop. Ready?'
    $q = Post-Toast 's15-quiet' @('--tag', 's15-quiet', '--suppress', '--title', 'Quiet toast', '--body', 'SuppressPopup = true (S1.5)')
    $quietNoPop = Ask 'Did the quiet toast stay hidden (no pop)?'
    $quietInCentre = Ask 'Open the notification centre (Win+N): is "Quiet toast" there?'
    $pass = (P $p 'posted') -and (P $p 'all_succeeded') -and $noRepop -and (P $u 'result') -eq 'NotificationNotFound' -and
        (P $rr 'removed') -and (P $rr 'posted') -and $repostPopped -and (P $q 'posted') -and $quietNoPop -and $quietInCentre
    Set-Check 'S1.5' $(if ($pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
        progress = $p; no_second_pop = $noRepop; update_after_dismiss = (P $u 'result')
        remove_repost = $rr; repost_popped = $repostPopped
        quiet = [ordered]@{ posted = (P $q 'posted'); no_pop = $quietNoPop; in_centre = $quietInCentre }
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
        c_video = $(if ($c) { [ordered]@{ states = Count-States $c.samples; samples = $c.samples } } else { 'skipped' })
        d_presentation = $(if ($d) { [ordered]@{ states = Count-States $d.samples; samples = $d.samples } } else { 'skipped' })
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

function Power-Listing([string]$Api) {
    $c = Start-Core "s17-$Api-listing" @('power', '--api', $Api, '--max-secs', '3600')
    $set = Wait-PowerEvent $c 'set' 20
    Say "Open an ELEVATED terminal (Win+X, then Terminal (Admin)) and run:  powercfg /requests"
    # SetThreadExecutionState carries no reason string: it shows as the process alone (M4).
    $what = if ($Api -eq 'ste') { "[PROCESS] ...\winshell-core.exe (no reason text)" } else { "'$PowerReason' (winshell-core.exe)" }
    $listed = Ask "Is $what listed under SYSTEM?"
    # Other requesters could keep the PC awake by themselves during the stay-awake check (M6).
    $others = Read-Host 'Is anything ELSE listed under SYSTEM? Type what it is, or press Enter for nothing'

    New-Item -ItemType File -Force -Path (Join-Path $LogDir 'power-clear.flag') | Out-Null
    $cleared = Wait-PowerEvent $c 'cleared' 20
    Say 'The request is cleared. Run  powercfg /requests  again in the elevated terminal.'
    $gone = Ask 'Is it gone from the list now?'
    $doc = Stop-Power $c
    [ordered]@{ api = $Api; set = $set; listed_while_held = $listed; other_system_requests = $others; cleared = $cleared; gone_after_clear = $gone; core = $doc }
}

function Power-StayAwake([string]$Api, $Timeout) {
    if ($null -eq $Timeout.used_s) { return [ordered]@{ api = $Api; result = 'skipped'; why = 'timeout unknown (powercfg output not parsed)' } }
    if ($Timeout.used_s -eq 0) { return [ordered]@{ api = $Api; result = 'skipped'; why = 'sleep timeout is never' } }
    $waitS = [int]$Timeout.used_s + 300
    $until = (Get-Date).AddSeconds($waitS + 30)
    Wait-Owner ("Stay-awake check: after Enter, leave the PC alone (no mouse, no keys) until {0:HH:mm} ({1} min). The display may turn off; that is fine" -f $until, [math]::Ceiling(($waitS + 30) / 60))
    $c = Start-Core "s17-$Api-stayawake" @('power', '--api', $Api, '--max-secs', '7200')
    [void](Wait-PowerEvent $c 'set' 20)
    $t0 = Get-Date
    Start-Sleep -Seconds ($waitS + 30)
    $elapsed = ((Get-Date) - $t0).TotalSeconds
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
    [ordered]@{
        api = $Api; timeout_s = $Timeout.used_s; waited_s = [int]$elapsed; slept_events = $slept
        kernel_power_events = $kernel
        pass = ($slept.Count -eq 0 -and $kernelSlept.Count -eq 0 -and @($kernel | Where-Object { P $_ 'error' }).Count -eq 0)
        core = $doc
    }
}

function Check-S17 {
    Step 'S1.7 keep-awake'
    $a = (& powercfg /a) -join "`n"
    $availablePart = ($a -split 'The following sleep states are not available')[0]
    $modernStandby = $availablePart -match 'S0 Low Power Idle'
    $timeout = Get-SleepTimeoutSec
    Say ("Sleep timeout in use: {0}" -f $(if ($null -eq $timeout.used_s) { 'unknown (powercfg output not parsed)' } elseif ($timeout.used_s) { "$($timeout.used_s) s" } else { 'never' }))

    $listing = Power-Listing 'request'
    $stay = Power-StayAwake 'request' $timeout
    $ste = $null
    $requestOk = $listing.listed_while_held -and $listing.gone_after_clear -and ($stay.result -eq 'skipped' -or $stay.pass)
    if (-not $requestOk) {
        Say 'PowerSetRequest failed a check: the FAIL path repeats them with SetThreadExecutionState.' 'Red'
        $ste = [ordered]@{ listing = Power-Listing 'ste'; stay_awake = Power-StayAwake 'ste' $timeout }
    }

    Wait-Owner 'Sleep-and-wake check: after Enter, put the PC to sleep (Start, Power, Sleep), wait about a minute, wake it and sign in. The script waits up to 15 minutes'
    $c = Start-Core 's17-sleep' @('power', '--api', 'none', '--max-secs', '3600')
    [void](Wait-PowerEvent $c 'set' 20)
    $resumed = Wait-PowerEvent $c 'power_broadcast' 900 { param($e) $e.pbt -eq 'PBT_APMRESUMEAUTOMATIC' }
    Start-Sleep -Seconds 35   # one more 30 s clock tick after the wake
    $doc = Stop-Power $c
    $pbts = @(P $doc 'events' | Where-Object { $_.kind -eq 'power_broadcast' } | ForEach-Object { $_.pbt })
    $sleepPass = ($pbts -contains 'PBT_APMSUSPEND') -and ($pbts -contains 'PBT_APMRESUMEAUTOMATIC')

    $steOk = $ste -and $ste.listing.listed_while_held -and $ste.listing.gone_after_clear -and
        ($ste.stay_awake.result -eq 'skipped' -or $ste.stay_awake.pass)
    $api = if ($requestOk) { 'PowerSetRequest' } elseif ($steOk) { 'SetThreadExecutionState' } else { 'none' }
    $verdict = if ($requestOk -and $sleepPass) { 'PASS' } elseif ($steOk -and $sleepPass) { 'PASS (SetThreadExecutionState)' } else { 'FAIL' }
    Set-Check 'S1.7' $verdict ([ordered]@{
        api = $api; listing = $listing; stay_awake = $stay; ste = $ste
        sleep_wake = [ordered]@{ pass = $sleepPass; broadcasts = $pbts; resumed = $resumed; core = $doc }
        sleep_gap_fallback_needed = -not $sleepPass
        modern_standby = $modernStandby; powercfg_a = $a; sleep_timeout = $timeout
    })
}

function Check-S18 {
    Step 'S1.8 Windows Hello (keep this terminal in front)'
    $a = HelloCmd supported
    $d = HelloCmd tpm
    $dp = HelloCmd dpapi
    Say "(a) supported: $(P $a 'supported'); (d) TPM: $(P $d 'tpm_version'); DPAPI round trip: $(P $dp 'round_trip')"
    $b = $null; $bFront = $false; $c1 = $null; $c2 = $null
    if (P $a 'supported') {
        Wait-Owner '(b) Next, Windows Hello asks you to create the spike credential. Watch whether its prompt is IN FRONT of this terminal, then complete it. Ready?'
        $b = HelloCmd create
        $bFront = Ask 'Was the Hello prompt in front of the terminal?'
        Wait-Owner '(c) Next, 5 signatures, then 5 more from a second process. Complete each Hello prompt. Ready?'
        $c1 = HelloCmd sign --count 5
        $c2 = HelloCmd sign --count 5
    }
    Wait-Owner '(e) Next, a Windows Hello verification prompt. Watch whether it is IN FRONT of this terminal, then complete it. Ready?'
    $e = HelloCmd verify
    $eFront = Ask 'Was the verification prompt in front of the terminal?'
    $f = $null; $fGone = $null
    if (P $a 'supported') { $f = HelloCmd delete; $fGone = HelloCmd exists }

    $sigs = @()
    foreach ($run in @($c1, $c2)) { $sigs += @(P $run 'signatures' | Where-Object { $_ }) }
    $hexes = @($sigs | Where-Object { (P $_ 'status') -eq 'Success' } | ForEach-Object { P $_ 'signature_hex' })
    $cPass = $hexes.Count -eq 10 -and @($hexes | Select-Object -Unique).Count -eq 1
    $bPass = (P $b 'status') -eq 'Success' -and $bFront
    $fPass = (P $f 'deleted') -eq $true -and (P $fGone 'status') -eq 'NotFound'
    $ePass = (P $e 'result') -eq 'Verified' -and $eFront
    $route = if ((P $a 'supported') -and $bPass -and $cPass -and (P $d 'tpm_2_0') -and $fPass) { 'KEK' }
             elseif ($ePass -and (P $dp 'round_trip')) { 'DPAPI with the Hello gate' }
             else { 'passphrase only' }
    $verdict = if ($route -eq 'passphrase only') { 'FAIL (passphrase only: report to the owner)' } else { 'PASS' }
    Set-Check 'S1.8' $verdict ([ordered]@{
        route = $route
        a_supported = $a; b_create = $b; b_prompt_in_front = $bFront
        c_identical = $cPass; c_count = $hexes.Count; c_process1 = $c1; c_process2 = $c2
        d_tpm = $d; e_verify = $e; e_prompt_in_front = $eFront; f_delete = $f; f_after = $fGone
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
if (Test-Path (Join-Path $Results 'checks.json')) {
    $old = Get-Content -Raw (Join-Path $Results 'checks.json') | ConvertFrom-Json
    $stamp = ([datetime](P $old 'started')).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
    $keep = Join-Path $Results "run-$stamp"
    New-Item -ItemType Directory -Force -Path $keep | Out-Null
    Move-Item (Join-Path $Results 'checks.json') $keep -Force
    if (Test-Path $Raw) { Move-Item $Raw (Join-Path $keep 'raw') -Force }
    Say "The earlier run's results were moved to $keep"
}
New-Item -ItemType Directory -Force -Path $Raw | Out-Null
# The console, with every question and answer, kept beside the results (git ignores it).
Start-Transcript -Path (Join-Path $Raw 'console.log') -IncludeInvocationHeader | Out-Null

if (-not $SkipBuild -and -not (Want 'S1.1')) {
    Push-Location $Spike
    try { & cargo build --release --locked; if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' } } finally { Pop-Location }
}
if (-not $SkipBuild) { Invoke-Check 'S1.1' { Check-S11 } }

$Report.setup = [ordered]@{
    os_build   = [Environment]::OSVersion.Version.ToString()
    os_display = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').DisplayVersion
    rustc      = (& rustc --version)
    pwsh       = $PSVersionTable.PSVersion.ToString()
    token      = (Tool token)
}
$Lnk = Join-Path (Get-ProgramsDir) 'LILITH Spike.lnk'

try {
    Step 'Setup: clearing anything left from an earlier run, then registering'
    $pre = Invoke-Cleanup
    $Report.setup.pre_cleanup = $pre
    New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
    Write-Shortcut
    Register-Scheme
    $Report.setup.shortcut = Tool shortcut-read --path $Lnk
    $Report.setup.setting = (Tool setting).setting
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
    $Report.finished = (Get-Date).ToUniversalTime().ToString('o')
    Save-Report
    Say ("Cleanup -> {0}" -f $(if ($Report.cleanup.pass) { 'PASS' } else { 'FAIL' })) $(if ($Report.cleanup.pass) { 'Green' } else { 'Red' })
    Say "Results: $(Join-Path $Results 'checks.json')"
    try { Stop-Transcript | Out-Null } catch { }
}
