<#
.SYNOPSIS
  What only a real window can prove about the confirm window's gate (P4a-R10), against a test-channel
  core:
    pwsh -NoProfile -File tools/sandbox/confirm-check.ps1 [-DryRun] [-NoHand]

.DESCRIPTION
  It must not run until the controller says so.

  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown). It says one
     step needs a hand. That step is one gesture, about 15 seconds; this console says when, with beeps.
  2. A test channel under a new temporary root (LIL_CHANNEL=test-confirmcheck, LIL_TEST_ROOT). It refuses
     a channel that already has a core. It makes its own confirmations there, with `lil call
     test.destructive` from a `cli` session, and starts the deployed lil-image with --confirm.

  How a check proves anything: the answer comes from the core (the call's retry with its key: `pending`,
  `denied` or it runs), never from the window, and where input came from comes from the app's own log,
  one line per press or release on a button: `origin`, `device`, `in_send`, `physically_down`,
  `by_messages_down`, and the counters `seen`, `real` and `sent`. No check reads lines older than its own
  step, so none can pass on what another step left. A refusal's words must also be on screen, and must be
  the ones for that step.

  With no hand:
    (a) UI Automation's Invoke on Approve answers nothing, says why, and reaches no press or release.
    (b) Space and Enter on Approve answer nothing, and say why.
    (c) A window move restarts the clock: Invoke on Deny right after a SetWindowPos is refused as too soon.
    (d) A change to the raise amount, by UI Automation's value and by posted WM_CHAR, shows on the button.
    (e) Enter on Deny denies; UI Automation's Invoke on Deny denies.

  The gesture, with a real rest on Approve in place (the script checks the cursor has moved onto it and
  stayed for 300 ms, and it warps nothing):
    (f) A posted press and release: `pending` from the core, and the log's press and release do NOT say
        origin Hardware. If they do, the source alone would have approved, and only the physical button
        stopped it: the check FAILS, and says so loudly (I1: tell the controller at once).
    (g) A sent press and release: `pending`, and the counter `sent` rose by 2 with `seen` not rising for them.
    (h) An injected press and release (mouse_event, no warp): `pending`, and the log's origin isn't Hardware.
    (i) Your real click ("Nudge the mouse, then click"): `approved`, and the log's press and release say
        origin Hardware, physically down at the press and up at the release.
  On any failure the window's reason, the log's lines for that step and the UIA tree go to -DiagDir first.

  Real input goes only while the app's window is in front and under the point, checked right before each
  input. 3. lil-image is closed, the test core stopped (only the PIDs recorded here, only on this channel),
  the channel's registry keys and temporary root removed, and the pop-up told it is done. Anything left
  behind fails the run. Exit 0 only when every check passed and the cleanup was complete: -NoHand can't pass.

  -DryRun does the channel set-up and the guards' self-test only: no pop-up, no window, no input.

  What it can't check: a see-through (layered) window under a real click, and a fake overlay above the card.
#>
param(
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    [string]$DiagDir = '',
    # No person at the PC: the gesture's checks are skipped, and the run fails.
    [switch]$NoHand,
    # How long each part of the gesture waits.
    [int]$HandSec = 60,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $Deploy) { $Deploy = Join-Path $Repo 'build\dev\deploy' }
if (-not $DiagDir) { $DiagDir = Join-Path $Repo 'build\dev\confirm-check' }
$lil = Join-Path $Deploy 'lil.exe'
$image = Join-Path $Deploy 'lil-image.exe'
$results = [System.Collections.Generic.List[string]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
function Pass([string]$what) { $results.Add("PASS $what"); Write-Host "PASS $what" }
function Fail([string]$what) {
    $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what"
    Save-Evidence $what
}
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-confirmcheck'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-confirmcheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$corePids = [System.Collections.Generic.HashSet[int]]::new()
# True once this script knows the channel is its own: nothing else is ever stopped.
$script:channelMine = $false
function Enter-TestChannel {
    $env:LIL_CHANNEL = $channel
    $env:LIL_TEST_ROOT = $root
    New-Item -ItemType Directory $root | Out-Null
    foreach ($s in 'roaming', 'local', 'profile', 'places') { New-Item -ItemType Directory (Join-Path $root $s) | Out-Null }
    Set-Location -LiteralPath $root
    # With a fresh root no core of ours can answer; one that does isn't ours to touch.
    $o = & $lil --no-start status 2>&1
    if ($LASTEXITCODE -eq 0 -and ($o -join "`n") -match 'pid \d+') { throw "$channel already has a core running: not touching it" }
    $script:channelMine = $true
}
function Invoke-Lil([string[]]$a) {
    $o = & $lil @a 2>&1
    if ($LASTEXITCODE -ne 0) { throw "lil $($a -join ' ') exited $LASTEXITCODE`: $($o -join ' ')" }
    return ($o -join "`n")
}
function Get-CorePid {
    $o = & $lil --no-start status 2>&1
    if ($LASTEXITCODE -ne 0) { return 0 }
    $m = [regex]::Match(($o -join "`n"), 'pid (\d+), channel (\S+?)\)')
    if (-not $m.Success -or $m.Groups[2].Value -notlike '*confirmcheck*') { return 0 }
    $id = [int]$m.Groups[1].Value
    [void]$corePids.Add($id)
    return $id
}
function Stop-RecordedCores {
    foreach ($id in $corePids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p -and $p.ProcessName -eq 'lil-core' -and $p.Path -and $p.Path.StartsWith($Deploy, [StringComparison]::OrdinalIgnoreCase)) {
            Write-Host "stopping the test core, pid $id"
            Stop-Process -Id $id -Force
        }
    }
}
function Exit-TestChannel {
    # Returns at once unless this shell is on the test channel: `lil core stop` would reach the
    # channel it defaults to.
    if ($env:LIL_CHANNEL -ne $channel) { Write-Host 'the test channel was never entered: nothing to clean up'; return }
    if ($script:channelMine) {
        try { [void](Get-CorePid) } catch { }
        try { [void](& $lil core stop 2>&1) } catch { }
        Start-Sleep -Milliseconds 500
        try { Stop-RecordedCores } catch { Fail "cleanup: stopping the test core: $_" }
    } else {
        Write-Host 'the channel had a core that is not this script''s: it is left alone'
    }
    try { Set-Location -LiteralPath $Repo } catch { }
    Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
    if ($script:channelMine) {
        $capitalised = $channel.Substring(0, 1).ToUpperInvariant() + $channel.Substring(1)
        $keys = "HKCU:\Software\LILITH-$channel", "HKCU:\Software\Classes\lil-$channel",
                "HKCU:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\LILITH.Suite.$capitalised"
        foreach ($k in $keys) {
            if (Test-Path $k) {
                Write-Host "removing the test channel's $k"
                try { Remove-Item -Recurse -Force $k } catch { Write-Host "note: $_" }
            }
            if (Test-Path $k) { Fail "cleanup: $k is still there" }
        }
    }
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $root); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $root } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path -LiteralPath $root) { Fail "cleanup: $root is still there" }
    foreach ($id in $corePids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p -and $p.ProcessName -eq 'lil-core' -and $p.Path -and $p.Path.StartsWith($Deploy, [StringComparison]::OrdinalIgnoreCase)) {
            if (-not $p.WaitForExit(5000)) { Fail "cleanup: the test core, pid $id, is still running" }
        }
    }
}
# A new confirmation: a `cli` call of test.destructive with a key of its own, which comes back
# pending (exit 4) with the confirmation's id. test.desktop has made the notification path reachable,
# with no real toast.
$keys_ = @{}
function New-Confirmation([string]$args_ = '{}', [string]$tool = 'test.destructive') {
    $key = [guid]::NewGuid().ToString('N')
    $o = & $lil call $tool $args_ --idempotency-key $key 2>&1
    if ($LASTEXITCODE -ne 4) { throw "$tool didn't wait for an answer (exit $LASTEXITCODE): $($o -join ' ')" }
    $id = (($o -join "`n") | ConvertFrom-Json).confirmation_id
    $keys_[$id] = @{ key = $key; args = $args_; tool = $tool }
    return $id
}
# From the core itself: the call's retry with its key is `pending` until it is answered, `denied`, or
# runs (`approved`).
function Get-Answer([string]$id) {
    $c = $keys_[$id]
    $o = & $lil call $c.tool $c.args --idempotency-key $c.key 2>&1
    switch ($LASTEXITCODE) {
        4 { return 'pending' }
        0 { return 'approved' }
        default {
            $text = $o -join ' '
            if ($text -match 'confirm\.denied') { return 'denied' }
            return "other: $text"
        }
    }
}
function Wait-Answer([string]$id, [int]$sec) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) {
        $a = Get-Answer $id
        if ($a -ne 'pending') { return $a }
        Start-Sleep -Milliseconds 300
    }
    return 'pending'
}

if (-not ('LilConfirmCheck.Win' -as [type])) {
    Add-Type -Namespace LilConfirmCheck -Name Win -MemberDefinition @'
public struct POINT { public int X; public int Y; }
[DllImport("user32.dll")] public static extern System.IntPtr WindowFromPoint(POINT point);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint processId);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT point);
[DllImport("user32.dll")] public static extern bool ScreenToClient(System.IntPtr hwnd, ref POINT point);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, System.UIntPtr extra);
[DllImport("user32.dll", EntryPoint = "PostMessageW")] public static extern bool PostMessage(System.IntPtr hwnd, uint msg, System.IntPtr wp, System.IntPtr lp);
[DllImport("user32.dll", EntryPoint = "SendMessageTimeoutW")] public static extern System.IntPtr SendMessageTimeout(System.IntPtr hwnd, uint msg, System.IntPtr wp, System.IntPtr lp, uint flags, uint ms, out System.IntPtr result);
[DllImport("user32.dll")] public static extern bool SetWindowPos(System.IntPtr hwnd, System.IntPtr after, int x, int y, int cx, int cy, uint flags);
'@
}
function Get-WindowProcessId([System.IntPtr]$hwnd) {
    [uint32]$owner = 0
    [void][LilConfirmCheck.Win]::GetWindowThreadProcessId($hwnd, [ref]$owner)
    return [int]$owner
}
function Assert-AppInFront {
    for ($i = 0; $i -lt 5; ++$i) {
        if ((Get-WindowProcessId ([LilConfirmCheck.Win]::GetForegroundWindow())) -eq $script:app.Id) { return }
        Start-Sleep -Milliseconds 200
    }
    throw 'no input sent: another program''s window is in front'
}
function Assert-AppAt([int]$x, [int]$y) {
    $point = [LilConfirmCheck.Win+POINT]::new()
    $point.X = $x
    $point.Y = $y
    if ((Get-WindowProcessId ([LilConfirmCheck.Win]::WindowFromPoint($point))) -ne $script:app.Id) {
        throw "no input sent: another program's window is at $x,$y"
    }
}
function Self-Test {
    $script:app = [pscustomobject]@{ Id = -1 }
    $refused = 0
    try { Assert-AppInFront } catch { if ("$_" -like 'no input sent*') { ++$refused } }
    try { Assert-AppAt 0 0 } catch { if ("$_" -like 'no input sent*') { ++$refused } }
    $script:app = $null
    Check ($refused -eq 2) "self-test: the guards refuse a window that isn't the app's ($refused of 2)"
}
function Save-Evidence([string]$what) {
    # Never for a dry run, and never able to mask the failure it records.
    if ($DryRun -or -not $script:main) { return }
    try {
        New-Item -ItemType Directory -Force $DiagDir | Out-Null
        $stamp = (Get-Date).ToUniversalTime().ToString('HHmmss')
        $lines = @("failed: $what", "the window said: $(Get-WindowReason)", 'the log since this step:') + @(Get-LogSinceStep)
        Set-Content -LiteralPath (Join-Path $DiagDir "failure-$stamp-evidence.txt") -Value $lines -Encoding utf8
        Save-UiaTree (Join-Path $DiagDir "failure-$stamp-uia.txt")
        $logs = Get-LogFolder
        if (Test-Path -LiteralPath $logs) { Copy-Item -LiteralPath $logs -Destination (Join-Path $DiagDir "failure-$stamp-logs") -Recurse -Force }
        Write-Host "evidence in $DiagDir"
    } catch { Write-Host "the evidence wasn't saved: $_" }
}
# The app's log, from the test root only.
function Get-LogFolder { return (Join-Path $root "local\LILITH-$channel\logs") }
function Get-LogLines {
    $folder = Get-LogFolder
    if (-not (Test-Path -LiteralPath $folder)) { return @() }
    $out = [System.Collections.Generic.List[string]]::new()
    foreach ($f in Get-ChildItem -LiteralPath $folder -Filter 'lil-image.*.log' | Sort-Object Name) {
        # The app has the file open for appending: read it shared.
        $s = [IO.File]::Open($f.FullName, 'Open', 'Read', 'ReadWrite')
        try { $r = [IO.StreamReader]::new($s); while (-not $r.EndOfStream) { $out.Add($r.ReadLine()) } } finally { $s.Dispose() }
    }
    return $out
}
$script:stepStart = 0
function Start-Step { $script:stepStart = @(Get-LogLines).Count }
function Get-LogSinceStep { return @(Get-LogLines) | Select-Object -Skip $script:stepStart }
# The new lines on a press or release, in order, as objects: {kind, origin, in_send, physically_down, ...}.
function Get-PointerLines {
    foreach ($line in Get-LogSinceStep) {
        if ($line -notlike "*a press or release on a confirmation's button*") { continue }
        $o = $line | ConvertFrom-Json
        if ($o.fields) { $o.fields } else { $o }
    }
}
function Get-Counts {
    $last = @(Get-LogLines | Where-Object { $_ -like '*"sent":*' }) | Select-Object -Last 1
    if (-not $last) { return [pscustomobject]@{ seen = 0; real = 0; sent = 0 } }
    $o = $last | ConvertFrom-Json
    return $(if ($o.fields) { $o.fields } else { $o })
}
function Get-Refusals { @(Get-LogSinceStep | Where-Object { $_ -like "*an answer to a confirmation didn't count*" }) }

if ($DryRun) {
    Self-Test
    Enter-TestChannel
    try {
        [void](Invoke-Lil @('workspace', 'init', (Join-Path $root 'places\harbor'), '--default'))
        $id = Get-CorePid
        Check ($id -gt 0) "dry run: the test core runs, pid $id"
        Check (Test-Path -LiteralPath $image) "dry run: $image is deployed"
    } catch { Fail "dry run: $_" } finally { Exit-TestChannel }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-confirmcheck-{0}.state" -f $PID)
$what = if ($NoHand) { 'checks the confirm window (a window, synthesized clicks and keys). Stay off the mouse and keyboard' }
        else { 'checks the confirm window (a window, synthesized clicks and keys). Stay off the mouse and keyboard until the beep and the console ask for one gesture on Approve' }
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', $what, '-WarnSec', '10', '-RunSec', '90',
               '-CapSec', '420', '-StateFile', $state, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
[void][System.Diagnostics.Process]::Start($psi)
$deadline = (Get-Date).AddSeconds(30)
while (-not (Test-Path -LiteralPath "$state.shown")) {
    if (Test-Path -LiteralPath "$state.error") { throw "the hands-off pop-up failed: $(Get-Content -Raw "$state.error")" }
    if ((Get-Date) -gt $deadline) { throw 'the hands-off pop-up was not on screen within 30 s, so nothing ran' }
    Start-Sleep -Milliseconds 250
}
Start-Sleep -Seconds 10

# --- 2. The check --------------------------------------------------------------------------------
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$AE = [System.Windows.Automation.AutomationElement]
$TS = [System.Windows.Automation.TreeScope]
function Prop($p, $v) { [System.Windows.Automation.PropertyCondition]::new($p, $v) }
function Wait-For([scriptblock]$find, [int]$sec, [string]$what) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) {
        $e = & $find
        if ($e) { return $e }
        Start-Sleep -Milliseconds 200
    }
    throw "timed out after $sec s waiting for $what"
}
function Find-Named([string]$name) {
    return $script:main.FindFirst($TS::Descendants, (Prop $AE::NameProperty $name))
}
function Find-Starting([string]$prefix) {
    foreach ($e in $script:main.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
        try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { }
    }
    return $null
}
# A button whose name starts `prefix`: the notes beside the buttons start with the same words.
function Find-Button([string]$prefix) {
    $buttons = Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Button)
    foreach ($e in $script:main.FindAll($TS::Descendants, $buttons)) {
        try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { }
    }
    return $null
}
# What the reason line says now, if anything of the kinds it can say.
function Get-WindowReason {
    if (-not $script:main) { return '' }
    foreach ($p in 'That click', 'No key answers', 'Move the pointer', 'What is shown', 'Wait a moment', 'Press and release', 'Bring this window', 'This window is covered') {
        $e = Find-Starting $p
        if ($e) { return $e.Current.Name }
    }
    return ''
}
function Show-App {
    [void][LilConfirmCheck.Win]::SetForegroundWindow([System.IntPtr]$script:main.Current.NativeWindowHandle)
    Start-Sleep -Milliseconds 800
}
function Save-UiaTree([string]$path) {
    $lines = [System.Collections.Generic.List[string]]::new()
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $walk = {
        param($e, [int]$depth)
        if ($lines.Count -ge 4000) { return }
        try {
            $c = $e.Current
            $name = if ($c.Name.Length -gt 100) { $c.Name.Substring(0, 100) + '…' } else { $c.Name }
            $lines.Add(('{0}{1} "{2}" enabled={3}' -f ('  ' * $depth), $c.ControlType.ProgrammaticName, $name, $c.IsEnabled))
        } catch { $lines.Add(('{0}(gone)' -f ('  ' * $depth))); return }
        $child = $walker.GetFirstChild($e)
        while ($child) { & $walk $child ($depth + 1); $child = $walker.GetNextSibling($child) }
    }
    & $walk $script:main 0
    Set-Content -LiteralPath $path -Value $lines -Encoding utf8
}
# The confirm window on `id`. One confirmation at a time: it is closed and started again for each.
function Open-Window([string]$id, [string]$button = 'Approve') {
    Close-App
    $script:app = Start-Process -FilePath $image -ArgumentList @('--confirm', $id) -WorkingDirectory $root -PassThru
    $script:main = Wait-For {
        $AE::RootElement.FindFirst($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id))
    } 40 'the confirm window'
    $script:approve = Wait-For { Find-Button $button } 30 $button
    $script:deny = Wait-For { $b = Find-Named 'Deny'; if (-not $b) { $b = Find-Named 'Cancel' }; $b } 10 'Deny'
    [void](Wait-For { if ($script:deny.Current.IsEnabled) { $true } } 30 'the buttons to turn on')
    Show-App
}
function Close-App {
    if ($script:app -and -not $script:app.HasExited) {
        try {
            $wp = $null
            if ($script:main -and $script:main.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$wp)) { $wp.Close() }
            if (-not $script:app.WaitForExit(10000)) { Stop-Process -Id $script:app.Id -Force }
        } catch { Stop-Process -Id $script:app.Id -Force -ErrorAction SilentlyContinue }
    }
    $script:app = $null
    $script:main = $null
}
function Invoke-Uia($e) {
    $p = $null
    if (-not $e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { throw 'it has no Invoke pattern' }
    $p.Invoke()
    Start-Sleep -Milliseconds 300
}
function Get-Rect($e) { return $e.Current.BoundingRectangle }
function Get-Cursor {
    $p = [LilConfirmCheck.Win+POINT]::new()
    [void][LilConfirmCheck.Win]::GetCursorPos([ref]$p)
    return $p
}
function Get-ClientLParam([int]$x, [int]$y) {
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
    $point = [LilConfirmCheck.Win+POINT]::new()
    $point.X = $x
    $point.Y = $y
    [void][LilConfirmCheck.Win]::ScreenToClient($hwnd, [ref]$point)
    return [System.IntPtr](($point.Y -shl 16) -bor ($point.X -band 0xffff))
}
function Send-Key([string]$keys) {
    Assert-AppInFront
    [System.Windows.Forms.SendKeys]::SendWait($keys)
    Start-Sleep -Milliseconds 300
}

$script:app = $null
$script:main = $null
try {
    Enter-TestChannel
    [void](Invoke-Lil @('workspace', 'init', (Join-Path $root 'places\harbor'), '--default'))
    [void](Get-CorePid)
    [void](Invoke-Lil @('call', 'test.desktop', '{"handlers": true, "shortcut": true, "toasts": "ready"}'))

    $id = New-Confirmation
    Open-Window $id
    Pass 'lil-image --confirm is up on the test channel, on its own confirmation'

    # (a) UI Automation's Invoke on Approve.
    Start-Step
    Invoke-Uia $script:approve
    Start-Sleep -Milliseconds 300
    Check ((Get-Answer $id) -eq 'pending') '(a) Invoke on Approve answered nothing'
    Check (@(Get-PointerLines).Count -eq 0) '(a) and no press or release reached the gate'
    Check (@(Get-Refusals | Where-Object { $_ -like '*not_pointer*' }).Count -eq 1) '(a) one refusal, not_pointer, in the log'
    Check ((Get-WindowReason) -like 'That click didn''t come from*') "(a) and the window says why: $(Get-WindowReason)"

    # (b) Keys with the focus on Approve.
    Start-Step
    $script:approve.SetFocus()
    Start-Sleep -Milliseconds 200
    Send-Key ' '
    Send-Key '{ENTER}'
    Check ((Get-Answer $id) -eq 'pending') '(b) Space and Enter on Approve answered nothing'
    Check (@(Get-Refusals | Where-Object { $_ -like '*"key"*' -or $_ -like '*refusal=key*' }).Count -ge 1) '(b) refusals named key in the log'
    Check ((Get-WindowReason) -like 'No key answers Approve*') "(b) and the window says why: $(Get-WindowReason)"

    # (c) A window move restarts the clock: Deny's gate is the same clock.
    Start-Step
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
    $r = Get-Rect $script:main
    Assert-AppInFront
    [void][LilConfirmCheck.Win]::SetWindowPos($hwnd, [System.IntPtr]::Zero, [int]$r.X + 30, [int]$r.Y + 20, 0, 0, 0x0015)
    Invoke-Uia $script:deny
    Check (@(Get-Refusals | Where-Object { $_ -like '*too_soon*' }).Count -eq 1) '(c) right after a move the clock had started again (too_soon)'
    Check ((Get-Answer $id) -eq 'pending') '(c) and nothing was answered'
    Start-Sleep -Milliseconds 700

    # (e) Enter on Deny.
    Start-Step
    $script:deny.SetFocus()
    Start-Sleep -Milliseconds 200
    Send-Key '{ENTER}'
    $answer = Wait-Answer $id 10
    Check ($answer -eq 'denied') "(e) Enter on Deny denied it (the core says: $answer)"

    # (e) UI Automation's Invoke on Deny.
    $id = New-Confirmation
    Open-Window $id
    Start-Step
    Invoke-Uia $script:deny
    $answer = Wait-Answer $id 10
    Check ($answer -eq 'denied') "(e) Invoke on Deny denied it (the core says: $answer)"

    # (d) The raise amount, on a confirmation that also passes a cap. The suite's cap: 1 USD.
    Close-App
    $suite = Get-ChildItem -LiteralPath (Join-Path $root 'roaming') -Recurse -Force -Filter 'suite.toml' | Select-Object -First 1
    Add-Content -LiteralPath $suite.FullName -Value "`n[money]`ndisplay_currency = `"USD`"`n[caps]`nmonthly = `"1`"`n"
    $victim = Get-CorePid
    [void](Invoke-Lil @('core', 'stop'))
    [void](Wait-For { if (-not (Get-Process -Id $victim -ErrorAction SilentlyContinue)) { $true } } 15 'the test core to stop')
    [void](Invoke-Lil @('call', 'test.desktop', '{"handlers": true, "shortcut": true, "toasts": "ready"}'))
    [void](Get-CorePid)
    $id = New-Confirmation '{"cost": "5"}'
    Open-Window $id 'Allow once'
    $field = Wait-For { Find-Named ('New limit for The monthly cap') } 10 'the amount field'
    $vp = $null
    if (-not $field.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) { throw 'the field has no Value pattern' }
    $vp.SetValue('25')
    $b = Wait-For { Find-Button 'Raise the cap to 25' } 5 'the button to show the amount'
    Pass "(d) a program's value on the field shows on the button: $($b.Current.Name)"
    $field.SetFocus()
    Start-Sleep -Milliseconds 200
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
    [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0102, [System.IntPtr]0x37, [System.IntPtr]::Zero)
    Start-Sleep -Milliseconds 500
    $b = Find-Button 'Raise the cap to 257'
    if ($b) { Pass "(d) a posted WM_CHAR, which Qt took, shows on the button: $($b.Current.Name)" }
    else { Pass '(d) a posted WM_CHAR did not change the field' }

    if ($NoHand) {
        Fail 'the gesture (f, g, h, i) was NOT checked: -NoHand'
    } else {
        # The gesture. A plain confirmation, with no cap to answer: a fresh core's cap only covers calls that cost.
        $id = New-Confirmation
        Open-Window $id
        $rect = Get-Rect $script:approve
        [Console]::Beep(880, 300)
        Write-Host ''
        Write-Host "NEEDS YOU: move the mouse onto Approve in the LILITH — Confirm window and stop there, without clicking. ($HandSec s)"
        $end = (Get-Date).AddSeconds($HandSec)
        $restSince = $null
        $last = $null
        while ($true) {
            if ((Get-Date) -gt $end) { throw 'the cursor never rested on Approve' }
            $p = Get-Cursor
            $on = $p.X -ge $rect.Left -and $p.X -lt $rect.Right -and $p.Y -ge $rect.Top -and $p.Y -lt $rect.Bottom
            if ($on -and $last -and $last.X -eq $p.X -and $last.Y -eq $p.Y) {
                if (-not $restSince) { $restSince = Get-Date }
                if (((Get-Date) - $restSince).TotalMilliseconds -ge 300) { break }
            } else {
                $restSince = $null
            }
            $last = $p
            Start-Sleep -Milliseconds 30
        }
        Write-Host 'Keep still: a few things are tried now, and none of them may answer.'
        $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
        $lp = Get-ClientLParam $last.X $last.Y

        # (f) A posted pair, with a real rest in place and the cursor on the button.
        Start-Step
        Assert-AppInFront
        Assert-AppAt $last.X $last.Y
        [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0201, [System.IntPtr]1, $lp)
        [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0202, [System.IntPtr]::Zero, $lp)
        Start-Sleep -Milliseconds 500
        Check ((Get-Answer $id) -eq 'pending') '(f) a posted pair on a real rest answered nothing'
        $lines = @(Get-PointerLines)
        Check ($lines.Count -eq 2) "(f) the gate read both ($($lines.Count) lines)"
        $hardware = @($lines | Where-Object { $_.origin -eq 'Hardware' })
        if ($hardware.Count -gt 0) {
            Fail '(f) *** A POSTED MESSAGE READS AS HARDWARE: only the physical button stopped it. Tell the controller at once. ***'
        } else {
            Pass "(f) the source isn't hardware: $(($lines | ForEach-Object { $_.origin }) -join ', ')"
        }
        Write-Host "(f) measured: $(($lines | ForEach-Object { "$($_.kind) origin=$($_.origin) physically_down=$($_.physically_down) by_messages_down=$($_.by_messages_down)" }) -join '; ')"

        # (g) A sent pair: the filter never sees it, and the hook must.
        Start-Step
        $before = Get-Counts
        Assert-AppInFront
        Assert-AppAt $last.X $last.Y
        $res = [System.IntPtr]::Zero
        [void][LilConfirmCheck.Win]::SendMessageTimeout($hwnd, 0x0201, [System.IntPtr]1, $lp, 2, 2000, [ref]$res)
        [void][LilConfirmCheck.Win]::SendMessageTimeout($hwnd, 0x0202, [System.IntPtr]::Zero, $lp, 2, 2000, [ref]$res)
        Start-Sleep -Milliseconds 500
        Check ((Get-Answer $id) -eq 'pending') '(g) a sent pair answered nothing'
        $after = Get-Counts
        Check (([int]$after.sent - [int]$before.sent) -eq 2) "(g) the hook saw both (sent $($before.sent) to $($after.sent))"

        # (h) An injected pair, no warp: the cursor is where the hand left it.
        Start-Step
        $now = Get-Cursor
        Assert-AppInFront
        Assert-AppAt $now.X $now.Y
        [LilConfirmCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
        Start-Sleep -Milliseconds 50
        Assert-AppAt $now.X $now.Y
        [LilConfirmCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
        Start-Sleep -Milliseconds 500
        Check ((Get-Answer $id) -eq 'pending') '(h) an injected pair on the button answered nothing'
        $lines = @(Get-PointerLines)
        Check ($lines.Count -eq 2) "(h) the gate read both ($($lines.Count) lines)"
        Check (@($lines | Where-Object { $_.origin -eq 'Hardware' }).Count -eq 0) "(h) the source isn't hardware: $(($lines | ForEach-Object { $_.origin }) -join ', ')"
        Check ((Get-WindowReason) -ne '') "(h) and the window says why: $(Get-WindowReason)"

        # (i) The real click.
        Start-Step
        [Console]::Beep(880, 200); [Console]::Beep(880, 200)
        Write-Host "NEEDS YOU: nudge the mouse off Approve and back, pause a moment, then click it. ($HandSec s)"
        $answer = Wait-Answer $id $HandSec
        $lines = @(Get-PointerLines)
        Write-Host "(i) measured: $(($lines | ForEach-Object { "$($_.kind) origin=$($_.origin) device=$($_.device) physically_down=$($_.physically_down) by_messages_down=$($_.by_messages_down)" }) -join '; ')"
        $press = $lines | Where-Object { $_.kind -eq 'Press' } | Select-Object -Last 1
        $release = $lines | Where-Object { $_.kind -eq 'Release' } | Select-Object -Last 1
        Check ($answer -eq 'approved') "(i) a real click on Approve approved it (the core says: $answer; the window: $(Get-WindowReason))"
        Check ($press -and $press.origin -eq 'Hardware' -and $press.physically_down -eq $true) '(i) the press reads as hardware, the button physically down'
        Check ($release -and $release.origin -eq 'Hardware' -and $release.physically_down -eq $false) '(i) the release reads as hardware, the button physically up'
    }
} catch {
    Fail "the confirm check stopped: $_"
} finally {
    try { Close-App } catch { Fail "cleanup: closing lil-image: $_" }
    try { Exit-TestChannel } catch { Fail "cleanup: $_" }
    try { Set-Content -LiteralPath $state -Value 'done' } catch { Write-Host "note: the pop-up wasn't told: $_" }
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
exit ([int]($failures.Count -gt 0))
