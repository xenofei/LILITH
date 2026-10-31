<#
.SYNOPSIS
  What only a real window can prove about the confirm window's gate (P4a-R10), against a test-channel
  core:
    pwsh -NoProfile -File tools/sandbox/confirm-check.ps1 [-DryRun]

.DESCRIPTION
  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown). One step
     needs a hand: the pop-up says so, and the window's title and this console say when.
  2. A test channel under a new temporary root (LIL_CHANNEL=test-confirmcheck, LIL_TEST_ROOT). It makes
     its own confirmations there, with `lil call test.destructive` from a `cli` session, and starts
     the deployed lil-image with --confirm. Each check reads the answer from the core (that call's
     retry with its key), not from the window:
       (a) UI Automation's Invoke on Approve answers nothing, and the window says why.
       (b) A synthesized click (SendInput: SetCursorPos, then mouse_event) on Approve answers nothing,
           and the window says why.
       (c) A posted WM_LBUTTONDOWN and WM_LBUTTONUP at Approve answer nothing, while the cursor rests
           on it, right after a real pointer's moves if there were any: the source mustn't be
           inherited (the Windows Hello window's Phase B check 2, on this window).
       (d) Space and Enter with the focus on Approve answer nothing.
       (e) A refusal shows its reason next to the buttons.
       (f) Enter with the focus on Deny denies.
       (g) UI Automation's Invoke on Deny denies.
       (h) A real click on Approve answers. A person has to make it: with -NoHand this check is
           skipped and said to be, and the run can't pass as complete.
  On a failure, the window's UIA tree and the test channel's logs go to -DiagDir first (this app's
  own elements only). Real input goes only while the app's window is in front and under the point;
  otherwise the check fails and sends nothing.
  3. lil-image is closed, the test core stopped (only the PIDs recorded here, only on this channel),
     the channel's registry keys and temporary root removed, and the pop-up told it is done. Anything
     left behind fails the run. Exit 0 only when every check passed and the cleanup was complete.
  -DryRun does step 2's channel set-up and the guards' self-test only: no pop-up, no window, no input.
  It must not run until the controller says so.
#>
param(
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    [string]$DiagDir = '',
    # No person at the PC: check (h) is skipped, and the run says it isn't complete.
    [switch]$NoHand,
    # How long check (h) waits for the real click.
    [int]$HandSec = 90,
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
function Fail([string]$what) { $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what" }
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-confirmcheck'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-confirmcheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$corePids = [System.Collections.Generic.HashSet[int]]::new()
function Enter-TestChannel {
    # A channel already taken is refused: nothing here may touch a core this script didn't start.
    $env:LIL_CHANNEL = $channel
    $env:LIL_TEST_ROOT = $root
    New-Item -ItemType Directory $root | Out-Null
    foreach ($s in 'roaming', 'local', 'profile', 'places') { New-Item -ItemType Directory (Join-Path $root $s) | Out-Null }
    Set-Location -LiteralPath $root
    if ((Get-CorePid) -ne 0) { throw "$channel already has a core running: not touching it" }
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
    try { [void](Get-CorePid) } catch { }
    try { [void](& $lil core stop 2>&1) } catch { }
    Start-Sleep -Milliseconds 500
    try { Stop-RecordedCores } catch { Fail "cleanup: stopping the test core: $_" }
    try { Set-Location -LiteralPath $Repo } catch { }
    Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
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
$keys = @{}
function New-Confirmation {
    $key = [guid]::NewGuid().ToString('N')
    $o = & $lil call test.destructive '{}' --idempotency-key $key 2>&1
    if ($LASTEXITCODE -ne 4) { throw "test.destructive didn't wait for an answer (exit $LASTEXITCODE): $($o -join ' ')" }
    $id = (($o -join "`n") | ConvertFrom-Json).confirmation_id
    $keys[$id] = $key
    return $id
}
# From the core itself, the window's word never being taken for an answer: the call's retry with its
# key is `pending` until it is answered, `denied`, or runs (`approved`).
function Get-Answer([string]$id) {
    $o = & $lil call test.destructive '{}' --idempotency-key $keys[$id] 2>&1
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
function Is-StillPending([string]$id) { return (Get-Answer $id) -eq 'pending' }
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
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern bool ScreenToClient(System.IntPtr hwnd, ref POINT point);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, System.UIntPtr extra);
[DllImport("user32.dll", EntryPoint = "PostMessageW")] public static extern bool PostMessage(System.IntPtr hwnd, uint msg, System.IntPtr wp, System.IntPtr lp);
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
$what = if ($NoHand) { 'checks the confirm window (a window, a synthesized click and keys)' }
        else { 'checks the confirm window (a window, a synthesized click and keys). One step needs YOUR real click on Approve: the window''s title will ask' }
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
function Show-App {
    [void][LilConfirmCheck.Win]::SetForegroundWindow([System.IntPtr]$script:main.Current.NativeWindowHandle)
    # The answer gate's half second, in front.
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
# The confirm window on `id`, and the buttons of the card shown. One confirmation at a time: the
# window is closed and started again for each.
function Open-Window([string]$id) {
    Close-App
    $script:app = Start-Process -FilePath $image -ArgumentList @('--confirm', $id) -WorkingDirectory $root -PassThru
    $script:main = Wait-For {
        $AE::RootElement.FindFirst($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id))
    } 40 'the confirm window'
    $script:approve = Wait-For { Find-Named 'Approve' } 30 'Approve'
    $script:deny = Wait-For { Find-Named 'Deny' } 10 'Deny'
    Wait-For { if ($script:deny.Current.IsEnabled) { $true } } 30 'the buttons to turn on' | Out-Null
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
function Get-Center($e) {
    $r = $e.Current.BoundingRectangle
    return [pscustomobject]@{ X = [int]($r.X + $r.Width / 2); Y = [int]($r.Y + $r.Height / 2) }
}
function Reason-Shows { return $null -ne (Find-Starting 'That click') -or $null -ne (Find-Starting 'No key answers') -or $null -ne (Find-Starting 'Move the pointer') }

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

    # (a) UI Automation's Invoke.
    Invoke-Uia $script:approve
    Start-Sleep -Milliseconds 500
    Check (Is-StillPending $id) '(a) UI Automation''s Invoke on Approve answered nothing'
    Check (Reason-Shows) '(a, e) and the window says why'

    # (b) A synthesized click, moved there first as a program would.
    $c = Get-Center $script:approve
    Assert-AppInFront
    Assert-AppAt $c.X $c.Y
    [void][LilConfirmCheck.Win]::SetCursorPos($c.X, $c.Y)
    Start-Sleep -Milliseconds 400
    [LilConfirmCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
    Start-Sleep -Milliseconds 50
    [LilConfirmCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
    Check (Is-StillPending $id) '(b) a synthesized click on Approve answered nothing'
    Check (Reason-Shows) '(b, e) and the window says why'

    # (c) Posted messages at Approve, with the cursor resting on it.
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
    $point = [LilConfirmCheck.Win+POINT]::new()
    $point.X = $c.X
    $point.Y = $c.Y
    [void][LilConfirmCheck.Win]::ScreenToClient($hwnd, [ref]$point)
    $lp = [System.IntPtr](($point.Y -shl 16) -bor ($point.X -band 0xffff))
    [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0200, [System.IntPtr]::Zero, $lp)
    [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0201, [System.IntPtr]1, $lp)
    [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0202, [System.IntPtr]::Zero, $lp)
    Start-Sleep -Milliseconds 500
    Check (Is-StillPending $id) '(c) posted messages at Approve answered nothing'

    # (d) Keys with the focus on Approve.
    Assert-AppInFront
    $script:approve.SetFocus()
    Start-Sleep -Milliseconds 200
    Assert-AppInFront
    [System.Windows.Forms.SendKeys]::SendWait(' ')
    Start-Sleep -Milliseconds 200
    Assert-AppInFront
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
    Start-Sleep -Milliseconds 500
    Check (Is-StillPending $id) '(d) Space and Enter on Approve answered nothing'
    Check ($null -ne (Find-Starting 'No key answers Approve')) '(d, e) and the window says why'

    # (f) Enter on Deny.
    $script:deny.SetFocus()
    Start-Sleep -Milliseconds 200
    Assert-AppInFront
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
    $answer = Wait-Answer $id 10
    Check ($answer -eq 'denied') "(f) Enter on Deny denied it (the core says: $answer)"

    # (g) UI Automation's Invoke on Deny.
    $id = New-Confirmation
    Open-Window $id
    Invoke-Uia $script:deny
    $answer = Wait-Answer $id 10
    Check ($answer -eq 'denied') "(g) UI Automation's Invoke on Deny denied it (the core says: $answer)"

    # (h) A real click: a person's.
    if ($NoHand) {
        Fail '(h) a real click on Approve was NOT checked: -NoHand'
    } else {
        $id = New-Confirmation
        Open-Window $id
        [Console]::Beep(880, 300)
        Write-Host "(h) NEEDS YOU: move the mouse onto Approve in the LILITH — Confirm window, pause a moment and click it, within $HandSec s"
        $answer = Wait-Answer $id $HandSec
        Check ($answer -eq 'approved') "(h) a real click on Approve approved it (the core says: $answer)"
        # (c) again, straight after a real pointer's moves: the source must not be inherited.
        $id = New-Confirmation
        Open-Window $id
        $c = Get-Center $script:approve
        Write-Host '(c2) NEEDS YOU: move the mouse onto Approve and leave it there, without clicking, for 5 s'
        Start-Sleep -Seconds 5
        $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
        $point = [LilConfirmCheck.Win+POINT]::new()
        $point.X = $c.X
        $point.Y = $c.Y
        [void][LilConfirmCheck.Win]::ScreenToClient($hwnd, [ref]$point)
        $lp = [System.IntPtr](($point.Y -shl 16) -bor ($point.X -band 0xffff))
        [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0201, [System.IntPtr]1, $lp)
        [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0202, [System.IntPtr]::Zero, $lp)
        Start-Sleep -Milliseconds 500
        Check (Is-StillPending $id) '(c2) posted messages after a real pointer''s moves answered nothing'
    }
} catch {
    Fail "the confirm check stopped: $_"
    try {
        New-Item -ItemType Directory -Force $DiagDir | Out-Null
        if ($script:app -and -not $script:app.HasExited -and $script:main) { Save-UiaTree (Join-Path $DiagDir 'uia-tree.txt') }
        $logs = Join-Path $root "local\LILITH-$channel\logs"
        if (Test-Path -LiteralPath $logs) { Copy-Item -LiteralPath $logs -Destination (Join-Path $DiagDir 'logs') -Recurse -Force }
        Write-Host "diagnostics in $DiagDir"
    } catch { Write-Host "the diagnostics weren't saved: $_" }
} finally {
    try { Close-App } catch { Fail "cleanup: closing lil-image: $_" }
    try { Exit-TestChannel } catch { Fail "cleanup: $_" }
    try { Set-Content -LiteralPath $state -Value 'done' } catch { Write-Host "note: the pop-up wasn't told: $_" }
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
exit ([int]($failures.Count -gt 0))
