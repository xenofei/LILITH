<#
.SYNOPSIS
  What only a real window can prove about the confirm window's gate (P4a-R10), against a test-channel
  core:
    pwsh -NoProfile -File tools/sandbox/confirm-check.ps1 [-DryRun] [-NoHand]

.DESCRIPTION
  It must not run until the controller says so.

  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown). When a person
     is needed it turns blue and says, in large words, what to do now, and never "hands off"; the console
     isn't needed. Beeps add to that: one high beep for the first stage; two lower ones, a clear gap apart,
     for the second.
  2. A test channel under a new temporary root (LIL_CHANNEL=test-confirmcheck, LIL_TEST_ROOT). It refuses
     a channel that already has a core. It makes its own confirmations there, with `lil call
     test.destructive` from a `cli` session, and starts the deployed lil-image with --confirm.

  How a check proves anything: the answer comes from the core (the call's retry with its key: `pending`,
  `denied` or it runs), never from the window, and where input came from comes from the app's own log: one
  line per press or release on a button (`origin`, `device`, `in_send`, `physically_down`,
  `by_messages_down`, and the counters `seen`, `real` and `sent`), and one per arrival on a button. No
  check reads lines older than its own step, so none can pass on what another step left. A refusal's words
  must also be on screen, and must be the ones for that step. Each block runs in its own `try`, so a throw
  in one is recorded and the next still runs: the gesture happens whatever the checks before it did.

  With no hand:
    (a) UI Automation's Invoke on Approve answers nothing, says why, and reaches no press or release.
    (b) Space and Enter on Approve answer nothing, and say why.
    (c) A window move restarts the clock: Invoke on Deny right after a SetWindowPos is refused as too soon.
    (e) Enter on Deny denies; UI Automation's Invoke on Deny denies.
    (s) The sweep: 20 injected press and release pairs on Approve at each hold of 5, 10, 20, 40 and 80 ms.
        Injected input is refused by the source whatever else happens; this measures what the physical
        button reads when the GUI thread gets to the message, so how often that factor would refuse a
        click of that length. A table goes to the output and to -DiagDir\sweep.txt.

  The gesture (the only part that needs a person), with a real rest on Approve in place: the script
  checks the cursor was off Approve, moved onto it by a real pointer move (the log's `origin`), and stayed
  for 300 ms. It warps nothing.
    (f) A posted press and release: `pending` from the core, and the log's press and release must read
        origin Unavailable. Anything else, Hardware or System, means a posted message can inherit a source:
        the check FAILS, loudly (tell the controller at once).
    (g) A sent press and release, by SendMessageTimeout and by SendNotifyMessage: `pending`, no press or
        release line in the log (they skip the filter), and the counter `sent` rose by 2 for each.
    (h) An injected press and release (mouse_event, no warp), only if the cursor is still inside Approve:
        `pending`, and the log's origin isn't Hardware.
    (i) A real click: `approved`, and the log's press and release say origin Hardware, physically down at
        the press and up at the release. A refused try shows why on the pop-up, and can be tried again.
  Then, with no hand again:
    (d) A change to the raise amount by UI Automation's value shows on the button. A posted WM_CHAR is
        measured and reported, not judged. It restarts the core with a cap, so it comes after the gesture.
  On any failure the window's reason, the log's lines for that step and the UIA tree go to -DiagDir first.

  Real input goes only while the app's window is in front and under the point, and an injected click only
  while the cursor is inside Approve's own rectangle, checked right before each input: a drifted hand is
  never clicked onto Deny. 3. lil-image is closed, the test core stopped (only the PIDs recorded here, only
  on this channel), the channel's registry keys and temporary root removed, and the pop-up told it is done.
  Anything left behind fails the run. Exit 0 only when every check passed and the cleanup was complete:
  -NoHand can't pass.

  -DryRun starts no lil-image and shows no window or pop-up, and sends no input. It runs the guards' self
  test and the log's parsing, then every command-line step on the test channel (a core, which has no
  window: its confirmations, the retry that reads them, the cap and the core's restart), and cleans up.

  What it can't check: a see-through (layered) window under a real click, a fake overlay above the card, and
  a touchpad tap (the sweep's short holds are only a guess at one).
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
# The notification path, reachable with no real toast.
$desktopArgs = '{"handlers": true, "shortcut": true, "toasts": "ready"}'
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
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll", EntryPoint = "SendNotifyMessageW")] public static extern bool SendNotifyMessage(System.IntPtr hwnd, uint msg, System.IntPtr wp, System.IntPtr lp);
[DllImport("user32.dll")] public static extern System.IntPtr SetThreadDpiAwarenessContext(System.IntPtr context);
[DllImport("user32.dll")] public static extern uint GetDpiForSystem();
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
        $stamp = (Get-Date).ToUniversalTime().ToString('HHmmssfff')
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

# Adds the cap the (d) check needs to the test channel's suite.toml, and starts the core again on it.
function Set-CapAndRestart {
    $suite = Get-ChildItem -LiteralPath (Join-Path $root 'roaming') -Recurse -Force -Filter 'suite.toml' | Select-Object -First 1
    if (-not $suite) { throw 'no suite.toml under the test root' }
    Add-Content -LiteralPath $suite.FullName -Value "`n[money]`ndisplay_currency = `"USD`"`n[caps]`nmonthly = `"1`"`n"
    $old = Get-CorePid
    [void](Invoke-Lil @('core', 'stop'))
    $end = (Get-Date).AddSeconds(15)
    while ($old -and (Get-Process -Id $old -ErrorAction SilentlyContinue)) {
        if ((Get-Date) -gt $end) { throw 'the test core didn''t stop' }
        Start-Sleep -Milliseconds 200
    }
    [void](Invoke-Lil @('call', 'test.desktop', $desktopArgs))
    [void](Get-CorePid)
}
function Test-InRect($rect, $point) {
    return $point.X -ge $rect.Left -and $point.X -lt $rect.Right -and $point.Y -ge $rect.Top -and $point.Y -lt $rect.Bottom
}
# Nothing here starts lil-image or shows a window: the guards, the log's parsing, and every step of the
# command line the real run needs, on the test channel, with a core that has no window.
if ($DryRun) {
    Self-Test
    $rect = [pscustomobject]@{ Left = 10; Right = 20; Top = 10; Bottom = 20 }
    Check ((Test-InRect $rect ([pscustomobject]@{ X = 10; Y = 19 })) -and -not (Test-InRect $rect ([pscustomobject]@{ X = 20; Y = 15 }))) 'dry run: Test-InRect keeps to the rectangle'
    $sample = '{"timestamp":"t","level":"INFO","fields":{"message":"a press or release on a confirmation''s button","kind":"Press","origin":"Hardware","physically_down":true,"sent":3},"target":"lil_qt"}'
    $parsed = ($sample | ConvertFrom-Json).fields
    Check ($parsed.kind -eq 'Press' -and $parsed.origin -eq 'Hardware' -and $parsed.physically_down -eq $true) 'dry run: a log line of that shape parses'
    Enter-TestChannel
    try {
        [void](Invoke-Lil @('workspace', 'init', (Join-Path $root 'places\harbor'), '--default'))
        $id = Get-CorePid
        Check ($id -gt 0) "dry run: the test core runs, pid $id"
        Check (Test-Path -LiteralPath $image) "dry run: $image is deployed"
        Check (Test-Path -LiteralPath (Join-Path $Repo 'tools\sandbox\hands-off.ps1')) 'dry run: hands-off.ps1 is there'
        [void](Invoke-Lil @('call', 'test.desktop', $desktopArgs))
        $c = New-Confirmation
        Check ($c -like 'cf_*') "dry run: a confirmation waits: $c"
        Check ((Get-Answer $c) -eq 'pending') 'dry run: its retry says pending'
        Check (Test-Path -LiteralPath (Get-LogFolder)) "dry run: the log folder is where the script reads it: $(Get-LogFolder)"
        Set-CapAndRestart
        $c = New-Confirmation '{"cost": "5"}'
        Check ($c -like 'cf_*') "dry run: on the cap, a confirmation that also passes it waits: $c"
        $c2 = New-Confirmation
        Check ($c2 -like 'cf_*') 'dry run: a plain one still waits'
    } catch { Fail "dry run: $_" } finally { Exit-TestChannel }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-confirmcheck-{0}.state" -f $PID)
# What a person must do now, in large words on the pop-up, when it has any text.
$cueFile = "$state.cue"
$what = if ($NoHand) { 'checks the confirm window (a window, synthesized clicks and keys)' }
        else { 'checks the confirm window (a window, synthesized clicks and keys). When this turns blue it needs you' }
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', $what, '-WarnSec', '10', '-RunSec', '240',
               '-CapSec', '600', '-StateFile', $state, '-CueFile', $cueFile, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
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
# Per-monitor physical pixels, as the app and the desktop test use: UI Automation's rectangles and the
# cursor agree on a scaled display.
[void][LilConfirmCheck.Win]::SetThreadDpiAwarenessContext([System.IntPtr](-4))
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
    # The window may have gone: an answered confirmation can't throw the check that reads this.
    try {
        foreach ($p in 'That ', 'No key answers', 'Move the', 'What ', 'Wait a moment', 'Press and release', 'Bring this window', 'This window is covered', 'LILITH couldn') {
            $e = Find-Starting $p
            if ($e) { return $e.Current.Name }
        }
    } catch { }
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
    Move-ClearOfPopUp
}
# The pop-up is click-through, but it mustn't hide the buttons: it sits at the top of the primary screen,
# and takes up to 284 of the system's scaled pixels when it says what to do. The whole window stays on
# the screen, or its clicks wouldn't count.
function Move-ClearOfPopUp {
    $work = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $b = Get-Rect $script:approve
    $clear = $work.Top + [int](300 * [LilConfirmCheck.Win]::GetDpiForSystem() / 96)
    if ($b.Top -ge $clear -or $b.Left -ge $work.Right -or $b.Right -le $work.Left) { return }
    $w = Get-Rect $script:main
    $newTop = [math]::Min($w.Top + ($clear - $b.Top), $work.Bottom - $w.Height)
    if ($newTop -le $w.Top) { return }
    $hwnd =[System.IntPtr]$script:main.Current.NativeWindowHandle
    [void][LilConfirmCheck.Win]::SetWindowPos($hwnd, [System.IntPtr]::Zero, [int]$w.Left, [int]$newTop, 0, 0, 0x0015)
    # The answer gate's half second starts again with a move.
    Start-Sleep -Milliseconds 800
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
$script:id = ''

# A block of checks: a throw in it is recorded, and the next block still runs.
function Step([string]$name, [scriptblock]$body) {
    try { . $body } catch { Fail "$name stopped: $_" }
}
# What a person sees: the pop-up's large blue text, and the console for whoever has one.
# Whole or not at all: the pop-up reads the file four times a second.
function Set-Cue([string]$text) {
    try {
        Set-Content -LiteralPath "$cueFile.new" -Value $text -Encoding utf8
        Move-Item -LiteralPath "$cueFile.new" -Destination $cueFile -Force
    } catch { Write-Host "the cue wasn't written: $_" }
}
function Clear-Cue { foreach ($f in $cueFile, "$cueFile.new") { try { Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue } catch { } } }
# There may be no sound device, and no console to beep through.
function Try-Beep([int]$hz, [int]$ms) { try { [Console]::Beep($hz, $ms) } catch { } }
function Get-Moves {
    foreach ($line in Get-LogSinceStep) {
        if ($line -notlike "*a move onto a confirmation's button*") { continue }
        $o = $line | ConvertFrom-Json
        if ($o.fields) { $o.fields } else { $o }
    }
}
# The cursor is inside Approve's rectangle as it is now: only then may a click be injected.
function Test-OnApprove { return (Test-InRect (Get-Rect $script:approve) (Get-Cursor)) }
function Hold-Ms([int]$ms) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $ms) { }
}

try {
    $channelUp = $false
    try {
        Enter-TestChannel
        [void](Invoke-Lil @('workspace', 'init', (Join-Path $root 'places\harbor'), '--default'))
        [void](Get-CorePid)
        [void](Invoke-Lil @('call', 'test.desktop', $desktopArgs))
        $channelUp = $true
    } catch { Fail "the test channel didn't come up: $_" }

    if ($channelUp) {
        Step '(a)' {
            $script:id = New-Confirmation
            Open-Window $script:id
            Pass 'lil-image --confirm is up on the test channel, on its own confirmation'
            Start-Step
            Invoke-Uia $script:approve
            Start-Sleep -Milliseconds 300
            Check ((Get-Answer $script:id) -eq 'pending') '(a) Invoke on Approve answered nothing'
            Check (@(Get-PointerLines).Count -eq 0) '(a) and no press or release reached the gate'
            Check (@(Get-Refusals | Where-Object { $_ -like '*not_pointer*' }).Count -eq 1) '(a) one refusal, not_pointer, in the log'
            Check ((Get-WindowReason) -like 'That click didn''t come from*') "(a) and the window says why: $(Get-WindowReason)"
        }
        Step '(b)' {
            Start-Step
            $script:approve.SetFocus()
            Start-Sleep -Milliseconds 200
            Send-Key ' '
            Send-Key '{ENTER}'
            Check ((Get-Answer $script:id) -eq 'pending') '(b) Space and Enter on Approve answered nothing'
            Check (@(Get-Refusals | Where-Object { $_ -like '*"key"*' }).Count -ge 1) '(b) refusals named key in the log'
            Check ((Get-WindowReason) -like 'No key answers Approve*') "(b) and the window says why: $(Get-WindowReason)"
        }
        Step '(c)' {
            # Deny's gate is the same clock.
            Start-Step
            $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
            $r = Get-Rect $script:main
            Assert-AppInFront
            [void][LilConfirmCheck.Win]::SetWindowPos($hwnd, [System.IntPtr]::Zero, [int]$r.X + 30, [int]$r.Y, 0, 0, 0x0015)
            Invoke-Uia $script:deny
            Check (@(Get-Refusals | Where-Object { $_ -like '*too_soon*' }).Count -eq 1) '(c) right after a move the clock had started again (too_soon)'
            Check ((Get-Answer $script:id) -eq 'pending') '(c) and nothing was answered'
            Start-Sleep -Milliseconds 700
        }
        Step '(e) Enter' {
            Start-Step
            $script:deny.SetFocus()
            Start-Sleep -Milliseconds 200
            Send-Key '{ENTER}'
            $answer = Wait-Answer $script:id 10
            Check ($answer -eq 'denied') "(e) Enter on Deny denied it (the core says: $answer)"
        }
        Step '(e) Invoke' {
            $script:id = New-Confirmation
            Open-Window $script:id
            Start-Step
            Invoke-Uia $script:deny
            $answer = Wait-Answer $script:id 10
            Check ($answer -eq 'denied') "(e) Invoke on Deny denied it (the core says: $answer)"
        }

        # (s) How often the physical-button read would refuse a click of each length. Injected input
        # is refused by the source whatever this reads: only the read is being measured.
        Step '(s)' {
            $script:id = New-Confirmation
            Open-Window $script:id
            $r = Get-Rect $script:approve
            [void][LilConfirmCheck.Win]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
            Start-Sleep -Milliseconds 300
            $table = [System.Collections.Generic.List[string]]::new()
            $table.Add('hold ms | pairs | presses read | press read as up | would refuse % | releases read | release read as down')
            $stopped = $false
            foreach ($hold in 5, 10, 20, 40, 80) {
                if ($stopped) { break }
                Start-Step
                $pairs = 0
                for ($i = 0; $i -lt 20; ++$i) {
                    # Never a click that can land anywhere but Approve.
                    if (-not (Test-OnApprove)) {
                        Fail "(s) stopped at a hold of $hold ms: the cursor isn't inside Approve, so nothing more was injected"
                        $stopped = $true
                        break
                    }
                    Assert-AppInFront
                    $c = Get-Cursor
                    Assert-AppAt $c.X $c.Y
                    [LilConfirmCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
                    Hold-Ms $hold
                    [LilConfirmCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
                    ++$pairs
                    Start-Sleep -Milliseconds 40
                }
                Start-Sleep -Milliseconds 400
                $lines = @(Get-PointerLines)
                $presses = @($lines | Where-Object { $_.kind -eq 'Press' })
                $releases = @($lines | Where-Object { $_.kind -eq 'Release' })
                $up = @($presses | Where-Object { $_.physically_down -eq $false }).Count
                $down = @($releases | Where-Object { $_.physically_down -eq $true }).Count
                $share = if ($presses.Count) { [math]::Round(100 * $up / $presses.Count) } else { 'n/a' }
                $table.Add(('{0,7} | {1,5} | {2,12} | {3,16} | {4,14} | {5,13} | {6}' -f $hold, $pairs, $presses.Count, $up, $share, $releases.Count, $down))
                $sources = @($lines | Where-Object { $_.origin -eq 'Hardware' }).Count
                if ($sources -gt 0) { Fail "(s) $sources injected lines at a hold of $hold ms read as hardware" }
            }
            New-Item -ItemType Directory -Force $DiagDir | Out-Null
            Set-Content -LiteralPath (Join-Path $DiagDir 'sweep.txt') -Value $table -Encoding utf8
            $table | ForEach-Object { Write-Host "(s) $_"; $results.Add("(s) $_") }
            Check ((Get-Answer $script:id) -eq 'pending') '(s) none of the injected pairs answered anything'
            # Off the button, so the gesture starts from somewhere else.
            $w = Get-Rect $script:main
            [void][LilConfirmCheck.Win]::SetCursorPos([int]$w.X + 20, [int]$w.Y + 10)
        }

        # The gesture: before (d), so the suite file's edit and the core's restart can't touch it.
        if ($NoHand) {
            Fail 'the gesture (f, g, h, i) was NOT checked: -NoHand'
        } else {
            Step 'the gesture' {
                $script:id = New-Confirmation
                Open-Window $script:id
                Start-Step
                $rest = $null
                Set-Cue 'Now: rest the mouse on Approve'
                Try-Beep 1047 400
                Write-Host "NOW: rest the mouse on Approve, in the LILITH - Confirm window. Don't click. ($HandSec s)"
                $end = (Get-Date).AddSeconds($HandSec)
                $wasOff = $false
                $restSince = $null
                $last = $null
                while ((Get-Date) -lt $end) {
                    $p = Get-Cursor
                    $on = Test-InRect (Get-Rect $script:approve) $p
                    if (-not $on) { $wasOff = $true; $restSince = $null }
                    elseif ($wasOff -and $last -and $last.X -eq $p.X -and $last.Y -eq $p.Y) {
                        if (-not $restSince) { $restSince = Get-Date }
                        if (((Get-Date) - $restSince).TotalMilliseconds -ge 300) { $rest = $p; break }
                    } else { $restSince = $null }
                    $last = $p
                    Start-Sleep -Milliseconds 30
                    $left = [int]($end - (Get-Date)).TotalSeconds
                    if ($left % 5 -eq 0) {
                        $hint = if ($wasOff -or -not $on) { 'Now: rest the mouse on Approve' } else { 'Now: move off Approve, then rest the mouse on it' }
                        Set-Cue "$hint`n($left s left)"
                    }
                }
                if (-not $rest) {
                    Set-Cue 'Timed out: the mouse never rested on Approve. The check goes on to the click.'
                    Fail "the first stage timed out after $HandSec s: the cursor was seen off Approve: $wasOff; it never stayed on it for 300 ms. (f), (g) and (h) did not run"
                    Start-Sleep -Seconds 3
                } else {
                    Set-Cue 'Keep the mouse still for a moment. Don''t click yet.'
                    $moves = @(Get-Moves)
                    $realMove = @($moves | Where-Object { $_.origin -eq 'Hardware' }).Count -gt 0
                    Write-Host "measured: the moves onto Approve read: $(($moves | ForEach-Object { $_.origin }) -join ', ')"
                    if (-not $realMove) {
                        Fail '(f) NOT measured: no real pointer move onto Approve reached the gate, so a posted message had nothing to inherit'
                    }
                    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
                    $lp = Get-ClientLParam $rest.X $rest.Y

                    # (f) A posted pair, on a real rest.
                    Step '(f)' {
                        Start-Step
                        Assert-AppInFront
                        Assert-AppAt $rest.X $rest.Y
                        if (Test-OnApprove) {
                            [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0201, [System.IntPtr]1, $lp)
                            [void][LilConfirmCheck.Win]::PostMessage($hwnd, 0x0202, [System.IntPtr]::Zero, $lp)
                            Start-Sleep -Milliseconds 500
                            Check ((Get-Answer $script:id) -eq 'pending') '(f) a posted pair on a real rest answered nothing'
                            $lines = @(Get-PointerLines)
                            Write-Host "(f) measured: $(($lines | ForEach-Object { "$($_.kind) origin=$($_.origin) physically_down=$($_.physically_down) by_messages_down=$($_.by_messages_down)" }) -join '; ')"
                            Check ($lines.Count -eq 2) "(f) the gate read both ($($lines.Count) lines)"
                            $inherited = @($lines | Where-Object { $_.origin -ne 'Unavailable' })
                            if ($lines.Count -gt 0 -and $inherited.Count -gt 0) {
                                Fail "(f) *** A POSTED MESSAGE READ AS $(($inherited | ForEach-Object { $_.origin }) -join ', '): it can inherit a source. Tell the controller at once. ***"
                            } elseif ($lines.Count -gt 0 -and $realMove) {
                                Pass '(f) both read Unavailable, on a real rest: no source was inherited'
                            }
                        } else {
                            Fail '(f) skipped: the cursor had left Approve, so nothing was posted'
                        }
                    }
                    # (g) Sent pairs: the filter never sees them, and the hook must.
                    Step '(g)' {
                        Start-Step
                        Assert-AppInFront
                        Assert-AppAt $rest.X $rest.Y
                        $res = [System.IntPtr]::Zero
                        # The app logs the counter at most once a second, so each reading is one more
                        # sent release, on its own, after a wait: its line holds the count up to it.
                        $read = {
                            Start-Sleep -Milliseconds 1100
                            [void][LilConfirmCheck.Win]::SendMessageTimeout($hwnd, 0x0202, [System.IntPtr]::Zero, $lp, 2, 2000, [ref]$res)
                            Start-Sleep -Milliseconds 300
                            Get-Counts
                        }
                        $start = & $read
                        [void][LilConfirmCheck.Win]::SendMessageTimeout($hwnd, 0x0201, [System.IntPtr]1, $lp, 2, 2000, [ref]$res)
                        [void][LilConfirmCheck.Win]::SendMessageTimeout($hwnd, 0x0202, [System.IntPtr]::Zero, $lp, 2, 2000, [ref]$res)
                        $first = & $read
                        [void][LilConfirmCheck.Win]::SendNotifyMessage($hwnd, 0x0201, [System.IntPtr]1, $lp)
                        [void][LilConfirmCheck.Win]::SendNotifyMessage($hwnd, 0x0202, [System.IntPtr]::Zero, $lp)
                        $second = & $read
                        Check ((Get-Answer $script:id) -eq 'pending') '(g) sent pairs answered nothing'
                        Check (@(Get-PointerLines).Count -eq 0) '(g) and no press or release reached the filter'
                        Write-Host "(g) measured: the counter sent read $($start.sent), $($first.sent) after SendMessageTimeout and $($second.sent) after SendNotifyMessage, each with its reading's one"
                        Check (([int]$first.sent - [int]$start.sent) -eq 3) '(g) the hook saw the SendMessageTimeout pair'
                        Check (([int]$second.sent - [int]$first.sent) -eq 3) '(g) the hook saw the SendNotifyMessage pair'
                    }
                    # (h) An injected pair, no warp, only with the cursor inside Approve.
                    Step '(h)' {
                        Start-Step
                        if (-not (Test-OnApprove)) {
                            Fail '(h) skipped: the cursor had left Approve, so nothing was injected'
                        } else {
                            $now = Get-Cursor
                            Assert-AppInFront
                            Assert-AppAt $now.X $now.Y
                            [LilConfirmCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
                            Hold-Ms 60
                            [LilConfirmCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
                            Start-Sleep -Milliseconds 500
                            Check ((Get-Answer $script:id) -eq 'pending') '(h) an injected pair on the button answered nothing'
                            $lines = @(Get-PointerLines)
                            Check ($lines.Count -eq 2) "(h) the gate read both ($($lines.Count) lines)"
                            Check (@($lines | Where-Object { $_.origin -eq 'Hardware' }).Count -eq 0) "(h) the source isn't hardware: $(($lines | ForEach-Object { $_.origin }) -join ', ')"
                            Check ((Get-WindowReason) -ne '') "(h) and the window says why: $(Get-WindowReason)"
                        }
                    }
                }

                # (i) The real click.
                Start-Step
                Set-Cue 'Now: move off Approve and back, wait a moment, click once'
                Try-Beep 659 250
                Start-Sleep -Milliseconds 250
                Try-Beep 659 250
                Write-Host "NOW: move off Approve and back, wait a moment, click once. ($HandSec s)"
                $end = (Get-Date).AddSeconds($HandSec)
                $answer = 'pending'
                $said = ''
                $refusedSoFar = 0
                while ((Get-Date) -lt $end) {
                    $answer = Get-Answer $script:id
                    if ($answer -ne 'pending') { break }
                    $left = [int]($end - (Get-Date)).TotalSeconds
                    # UI Automation only after a refusal: its calls keep the app's GUI thread busy, and
                    # that mustn't delay the read of a real press.
                    $refusedNow = @(Get-Refusals).Count
                    if ($refusedNow -ne $refusedSoFar) { $refusedSoFar = $refusedNow; $said = Get-WindowReason }
                    $text = "Now: move off Approve and back, wait a moment, click once`n($left s left)"
                    if ($said) { $text += "`nLast try: $said" }
                    Set-Cue $text
                    Start-Sleep -Milliseconds 300
                }
                if ($answer -eq 'pending') {
                    Set-Cue 'Timed out: no click on Approve counted. The evidence is saved.'
                } else {
                    Set-Cue 'That was the last thing needed from you. Stay off the mouse until this says the PC is free.'
                }
                $lines = @(Get-PointerLines)
                Write-Host "(i) measured: $(($lines | ForEach-Object { "$($_.kind) origin=$($_.origin) device=$($_.device) physically_down=$($_.physically_down) by_messages_down=$($_.by_messages_down)" }) -join '; ')"
                $press = $lines | Where-Object { $_.kind -eq 'Press' } | Select-Object -Last 1
                $release = $lines | Where-Object { $_.kind -eq 'Release' } | Select-Object -Last 1
                $refused = @(Get-Refusals).Count
                Check ($answer -eq 'approved') "(i) a real click on Approve approved it (the core says: $answer; refused clicks before it: $refused; the window: $(Get-WindowReason))"
                Check ($press -and $press.origin -eq 'Hardware' -and $press.physically_down -eq $true) '(i) the press reads as hardware, the button physically down'
                Check ($release -and $release.origin -eq 'Hardware' -and $release.physically_down -eq $false) '(i) the release reads as hardware, the button physically up'
                Start-Sleep -Seconds 3
                Clear-Cue
            }
            Clear-Cue
        }

        # (d) The raise amount, on a confirmation that also passes a cap. The suite's cap: 1 USD.
        Step '(d)' {
            Close-App
            Set-CapAndRestart
            $script:id = New-Confirmation '{"cost": "5"}'
            Open-Window $script:id 'Allow once'
            $field = Wait-For { Find-Named 'New limit for The monthly cap' } 10 'the amount field'
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
            $measured = if ($b) { "a posted WM_CHAR reached the field, and the button shows it: $($b.Current.Name)" } else { 'a posted WM_CHAR did not change the field' }
            Write-Host "(d) measured: $measured"
            $results.Add("(d) measured: $measured")
        }
    }
} catch {
    Fail "the confirm check stopped: $_"
} finally {
    Clear-Cue
    try { Close-App } catch { Fail "cleanup: closing lil-image: $_" }
    try { Exit-TestChannel } catch { Fail "cleanup: $_" }
    try { Set-Content -LiteralPath $state -Value 'done' } catch { Write-Host "note: the pop-up wasn't told: $_" }
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} checks passed, {1} failed" -f @($results | Where-Object { $_ -like 'PASS*' }).Count, $failures.Count)
# The pop-up closes when this script does: give it time to turn green and say the PC is free.
Start-Sleep -Seconds 6
exit ([int]($failures.Count -gt 0))
