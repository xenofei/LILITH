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
    (s) The sweep: 5 injected press and release pairs on Approve at each hold of 5, 10, 20, 40 and 80 ms,
        a little over a second apart (a program's lines in the log are limited to one a second of each
        kind). Injected input is refused by the source whatever else happens; this measures what the
        button's state reads when the GUI thread gets to the message, so how often that factor would
        refuse a click of that length. A table goes to the output and to -DiagDir\sweep.txt.
    (m) InitializeTouchInjection and InjectTouchInput at Approve's centre, then InjectSyntheticPointerInput
        with a pen: `pending`, and what the log says is measured (`not_pointer`, if QML gets a click at
        all). An API that isn't there, or that fails, is recorded as not checked.
    (p) The Confirmations panel floated in the shell, and whether its window is layered, read without a
        click: what that would mean for `not_exposed` is measured, not judged.

  The gesture (the only part that needs a person), with a real rest on Approve in place: the script
  checks the cursor was off Approve, moved onto it by a real pointer move (the log's `origin`), and stayed
  for 300 ms. It warps nothing, here or in (j).
    (f) A posted press and release: `pending` from the core, and the log's press and release must read
        origin Unavailable. Anything else, Hardware or System, means a posted message can inherit a source:
        the check FAILS, loudly (tell the controller at once).
    (g) A sent press and release, by SendMessageTimeout and by SendNotifyMessage: `pending`, no press or
        release line in the log (they skip the filter), and the counter `sent` rose by 2 for each.
    (h) An injected press and release (mouse_event, no warp), only if the cursor is still inside Approve:
        `pending`, and the log's origin isn't Hardware.
    (i) A real click: `approved`, and the log's press and release say origin Hardware, physically down at
        the press and up at the release. A refused try shows why on the pop-up, and can be tried again.
    (l) No click, and no pass or fail: while a person keeps the mouse moving near Approve for 10 s, the
        script warps off it and back onto it every 500 ms, after the same front and point checks, and
        reports each arrival line's `origin` (with `/late` when the message's own point wasn't where the
        cursor then was), and how many read Hardware right after a warp. Any that do, on time, mean Windows
        can merge a warp into the hand's next move and report it as real, where the gate's warp rule can't
        see it. A program's arrival lines are limited to one a second; a hand's aren't.
    (j) A person's normal way back: click outside the window, click its title bar once, move onto Approve,
        rest, click. Coming to the front makes a new button to the gate, but the hand comes to it by
        moving, so the first click counts: `approved`, with no `brought_forward` and no `not_pointer`.
        Any other refusal (too soon, or the words to move off and back) is measured and can be retried.
    (q) Click outside, then click Approve straight on in the window that isn't in front: refused as
        `brought_forward`, with the words that say to move off and come back. Doing that approves. What
        the window said is reported.
  Then, with no hand again:
    (d) A change to the raise amount by UI Automation's value re-arms the gate: Invoke on Cancel right
        after it is refused as too soon, half a second after the window had settled, and the button shows
        the amount. A posted WM_CHAR is measured and reported, not judged. It restarts the core with a
        cap, so it comes after the gesture.
  On any failure the window's reason, the log's lines for that step and the UIA tree go to -DiagDir first.

  Real input goes only while the app's window is in front and under the point, and an injected click only
  while the cursor is inside Approve's own rectangle, checked right before each input: a drifted hand is
  never clicked onto Deny. 3. lil-image is closed, the test core stopped (only the PIDs recorded here, only
  on this channel), the channel's registry keys, temporary root and Qt cache folder removed, and the pop-up told it is done.
  Anything left behind fails the run. Exit 0 only when every check passed and the cleanup was complete:
  -NoHand can't pass.

  -DryRun starts no lil-image and shows no window or pop-up, and sends no input. It runs the guards' self
  test and the log's parsing, then every command-line step on the test channel (a core, which has no
  window: its confirmations, the retry that reads them, the cap and the core's restart), and cleans up.

  What it can't check: a see-through (layered) window under a real click, a fake overlay above the card, and
  a touchpad tap (the sweep's short holds are only a guess at one). It runs no overlay, reuses no other
  process's token, and changes no system setting.
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
    if ($script:channelMine) {
        # Qt's own cache goes by the application name and the channel: a folder in the real local data
        # folder, outside the test root. Exactly this channel's folder: on any other channel this
        # function has returned above.
        $cache = Join-Path $env:LOCALAPPDATA "LILITH-$channel"
        for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $cache); ++$i) {
            try { Remove-Item -Recurse -Force -LiteralPath $cache } catch { Start-Sleep -Milliseconds 500 }
        }
        if (Test-Path -LiteralPath $cache) { Fail "cleanup: $cache is still there" }
    }
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
[DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] public static extern System.IntPtr GetWindowLongPtr(System.IntPtr hwnd, int index);
[DllImport("user32.dll", SetLastError = true)] public static extern bool GetLayeredWindowAttributes(System.IntPtr hwnd, out uint key, out byte alpha, out uint flags);

// What (m) injects, as winuser.h lays it out on x64 (the dry run checks the sizes).
public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
public struct POINTER_INFO {
    public uint pointerType; public uint pointerId; public uint frameId; public uint pointerFlags;
    public System.IntPtr sourceDevice; public System.IntPtr hwndTarget;
    public POINT ptPixelLocation; public POINT ptHimetricLocation; public POINT ptPixelLocationRaw; public POINT ptHimetricLocationRaw;
    public uint dwTime; public uint historyCount; public int InputData; public uint dwKeyStates;
    public ulong PerformanceCount; public int ButtonChangeType;
}
public struct POINTER_TOUCH_INFO {
    public POINTER_INFO pointerInfo; public uint touchFlags; public uint touchMask;
    public RECT rcContact; public RECT rcContactRaw; public uint orientation; public uint pressure;
}
public struct POINTER_PEN_INFO {
    public POINTER_INFO pointerInfo; public uint penFlags; public uint penMask;
    public uint pressure; public uint rotation; public int tiltX; public int tiltY;
}
// POINTER_TYPE_INFO for a pen: its union is as long as the touch member, so the padding makes up the rest.
public struct PEN_TYPE_INFO { public uint type; public POINTER_PEN_INFO penInfo; public ulong pad0; public ulong pad1; public ulong pad2; }
public static int TouchInfoSize() { return Marshal.SizeOf(typeof(POINTER_TOUCH_INFO)); }
public static int PenTypeInfoSize() { return Marshal.SizeOf(typeof(PEN_TYPE_INFO)); }

[DllImport("user32.dll", SetLastError = true)] static extern bool InitializeTouchInjection(uint maxCount, uint mode);
[DllImport("user32.dll", SetLastError = true)] static extern bool InjectTouchInput(uint count, ref POINTER_TOUCH_INFO contact);
[DllImport("user32.dll", SetLastError = true)] static extern System.IntPtr CreateSyntheticPointerDevice(uint type, uint maxCount, uint mode);
[DllImport("user32.dll", SetLastError = true)] static extern bool InjectSyntheticPointerInput(System.IntPtr device, ref PEN_TYPE_INFO info, uint count);
[DllImport("user32.dll")] static extern void DestroySyntheticPointerDevice(System.IntPtr device);

const uint INRANGE = 0x2, INCONTACT = 0x4, DOWN = 0x10000, UP = 0x40000;

static POINTER_INFO PointerAt(uint type, int x, int y, uint flags) {
    POINTER_INFO p = new POINTER_INFO();
    p.pointerType = type;
    p.pointerFlags = flags;
    p.ptPixelLocation.X = x;
    p.ptPixelLocation.Y = y;
    return p;
}
// Each returns 0, or the error that stopped it.
public static int TouchTap(int x, int y, int holdMs) {
    if (!InitializeTouchInjection(1, 3)) return Marshal.GetLastWin32Error();
    POINTER_TOUCH_INFO t = new POINTER_TOUCH_INFO();
    t.pointerInfo = PointerAt(2, x, y, INRANGE | INCONTACT | DOWN);
    if (!InjectTouchInput(1, ref t)) return Marshal.GetLastWin32Error();
    System.Threading.Thread.Sleep(holdMs);
    t.pointerInfo = PointerAt(2, x, y, UP);
    if (!InjectTouchInput(1, ref t)) return Marshal.GetLastWin32Error();
    return 0;
}
public static int PenTap(int x, int y, int holdMs) {
    System.IntPtr device = CreateSyntheticPointerDevice(3, 1, 3);
    if (device == System.IntPtr.Zero) return Marshal.GetLastWin32Error();
    try {
        PEN_TYPE_INFO p = new PEN_TYPE_INFO();
        p.type = 3;
        p.penInfo.pointerInfo = PointerAt(3, x, y, INRANGE | INCONTACT | DOWN);
        if (!InjectSyntheticPointerInput(device, ref p, 1)) return Marshal.GetLastWin32Error();
        System.Threading.Thread.Sleep(holdMs);
        p.penInfo.pointerInfo = PointerAt(3, x, y, INRANGE | UP);
        if (!InjectSyntheticPointerInput(device, ref p, 1)) return Marshal.GetLastWin32Error();
        return 0;
    } finally { DestroySyntheticPointerDevice(device); }
}
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
# The app logs a program's press, release or arrival and any refusal at most once a second of each kind:
# the wait leaves this step's first line of each free to come.
function Start-Step {
    Start-Sleep -Milliseconds 1100
    $script:stepStart = @(Get-LogLines).Count
}
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
# What the gate was told of the front, by Windows, in this step: each line with its time and path, and
# whether each press read as the one that brought the window forward.
function Get-FrontStory([string[]]$lines = @(Get-LogSinceStep)) {
    $story = foreach ($line in $lines) {
        if ($line -notlike '*of the app*the front*' -and $line -notlike "*a press or release on a confirmation's button*") { continue }
        $o = $line | ConvertFrom-Json
        $f = $o.fields
        # PowerShell's ConvertFrom-Json reads the time as a DateTime.
        $at = ([datetime]$o.timestamp).ToUniversalTime().ToString('HH:mm:ss.fff')
        if ($f.kind) {
            "$at $($f.kind) tick=$($f.tick) came_forward=$($f.came_forward) since_front_ms=$($f.since_front_ms) in_front=$($f.in_front)"
        } else {
            "$at $($f.message) via=$($f.via) window=$($f.window) tick=$($f.tick) on_button=$($f.on_button) in_front=$($f.in_front)"
        }
    }
    return (@($story) -join '; ')
}

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
    $story = Get-FrontStory @(
        '{"timestamp":"2026-11-01T18:02:04.300000Z","fields":{"message":"a click brought a window of the app to the front","via":"hook","window":"0x1a2b","tick":1000,"on_button":true,"in_front":true}}',
        '{"timestamp":"2026-11-01T18:02:04.330091Z","fields":{"message":"a press or release on a confirmation''s button","kind":"Press","tick":990,"came_forward":true,"since_front_ms":-10,"in_front":true}}')
    Check ($story -eq '18:02:04.300 a click brought a window of the app to the front via=hook window=0x1a2b tick=1000 on_button=True in_front=True; 18:02:04.330 Press tick=990 came_forward=True since_front_ms=-10 in_front=True') "dry run: the front's story reads: $story"
    # winuser.h's sizes on x64: what InjectTouchInput and InjectSyntheticPointerInput will read.
    Check ([LilConfirmCheck.Win]::TouchInfoSize() -eq 144 -and [LilConfirmCheck.Win]::PenTypeInfoSize() -eq 152) "dry run: (m)'s structures have Windows' sizes ($([LilConfirmCheck.Win]::TouchInfoSize()), $([LilConfirmCheck.Win]::PenTypeInfoSize()))"
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
               '-What', $what, '-WarnSec', '10', '-RunSec', '900',
               '-CapSec', '1800', '-StateFile', $state, '-CueFile', $cueFile, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
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
# UI Automation can throw for a whole walk while the tree moves: such a walk is tried again.
function Retry-Walk([scriptblock]$walk) {
    for ($i = 0; ; ++$i) {
        try { return (& $walk) } catch { if ($i -ge 4) { throw }; Start-Sleep -Milliseconds 100 }
    }
}
function Find-Named([string]$name) {
    return Retry-Walk { $script:main.FindFirst($TS::Descendants, (Prop $AE::NameProperty $name)) }
}
function Find-Starting([string]$prefix) {
    $all = Retry-Walk { $script:main.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition) }
    foreach ($e in $all) {
        try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { }
    }
    return $null
}
# A button whose name starts `prefix`: the notes beside the buttons start with the same words.
function Find-Button([string]$prefix) {
    $buttons = Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Button)
    $all = Retry-Walk { $script:main.FindAll($TS::Descendants, $buttons) }
    foreach ($e in $all) {
        try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { }
    }
    return $null
}
# What the reason line says now, if anything of the kinds it can say.
function Get-WindowReason {
    if (-not $script:main) { return '' }
    # The window may have gone: an answered confirmation can't throw the check that reads this.
    try {
        foreach ($p in 'That ', 'No key answers', 'Approve just', 'Move the', 'What ', 'Wait a moment', 'Press and release', 'Bring this window', 'This window is covered', 'LILITH couldn', 'LILITH was') {
            $e = Find-Starting $p
            if ($e) { return $e.Current.Name }
        }
    } catch { }
    return ''
}
# Windows keeps the front from a program for a few minutes after the person's last input, and no input
# may be sent to get round that: keep asking until it lets go.
function Show-App {
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
    $end = (Get-Date).AddSeconds(240)
    $waited = $false
    while ($true) {
        [void][LilConfirmCheck.Win]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 800
        $holder = Get-WindowProcessId ([LilConfirmCheck.Win]::GetForegroundWindow())
        if ($holder -eq $script:app.Id) { break }
        if ((Get-Date) -gt $end) {
            $name = (Get-Process -Id $holder -ErrorAction SilentlyContinue).ProcessName
            throw "the window didn't get the front in 240 s: $name holds it"
        }
        if (-not $waited) {
            $waited = $true
            Write-Host "Windows keeps the front for now: $((Get-Process -Id $holder -ErrorAction SilentlyContinue).ProcessName) holds it"
            Set-Cue "Nothing to do. Waiting for Windows to let the test window come to the front.`nStay off the mouse and keyboard: any input makes this wait longer."
        }
        Start-Sleep -Seconds 1
    }
    if ($waited) { Clear-Cue }
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
    $findMain = { $AE::RootElement.FindFirst($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id)) }
    $script:main = Wait-For $findMain 40 'the confirm window'
    $first = $script:main
    # The element is found again each time: one found while the window was being made can stay empty.
    $script:approve = Wait-For {
        $script:main = & $findMain
        if ($script:main) { Find-Button $button }
    } 30 $button
    if (-not $first.FindFirst($TS::Descendants, (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Button)))) {
        Write-Host 'measured: the window element found first has no buttons, a fresh one does: UI Automation kept an early view'
    }
    $script:deny = Wait-For { Find-Deny } 10 'Deny'
    [void](Wait-For { if ($script:deny.Current.IsEnabled) { $true } } 30 'the buttons to turn on')
    Show-App
    Move-ClearOfPopUp
}
function Find-Deny {
    $found = Find-Named 'Deny'
    if ($found) { return $found }
    return (Find-Named 'Cancel')
}
# The pop-up is click-through, but it mustn't hide the window, whose title bar (j) needs: it sits at the
# top of the primary screen, and takes up to 284 of the system's scaled pixels when it says what to do.
# The whole window stays on the screen, or its clicks wouldn't count.
function Move-ClearOfPopUp {
    $work = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $w = Get-Rect $script:main
    $clear = $work.Top + [int](300 * [LilConfirmCheck.Win]::GetDpiForSystem() / 96)
    if ($w.Top -ge $clear -or $w.Left -ge $work.Right -or $w.Right -le $work.Left) { return }
    $newTop = [math]::Min($clear, $work.Bottom - $w.Height)
    if ($newTop -le $w.Top) { return }
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
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
# Asks for a real click on Approve until the core has an answer or the time is up: the answer. A refused
# try's words go on the pop-up. UI Automation only after a refusal: its calls keep the app's GUI thread
# busy, and that mustn't delay the read of a real press.
function Wait-RealClick([string]$cue) {
    $end = (Get-Date).AddSeconds($HandSec)
    $answer = 'pending'
    $said = ''
    $refusedSoFar = 0
    while ((Get-Date) -lt $end) {
        $answer = Get-Answer $script:id
        if ($answer -ne 'pending') { break }
        $left = [int]($end - (Get-Date)).TotalSeconds
        $refusedNow = @(Get-Refusals).Count
        if ($refusedNow -ne $refusedSoFar) { $refusedSoFar = $refusedNow; $said = Get-WindowReason }
        $text = "$cue`n($left s left)"
        if ($said) { $text += "`nLast try: $said" }
        Set-Cue $text
        Start-Sleep -Milliseconds 300
    }
    return $answer
}
# The refusals' names in this step's log.
function Get-RefusalNames { @(Get-Refusals | ForEach-Object { (($_ | ConvertFrom-Json).fields).refusal }) }
# The shell's main window, found afresh: one found while it was being made can stay empty.
function Find-Shell {
    $all = Retry-Walk { $AE::RootElement.FindAll($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id)) }
    $found = @($all | Where-Object { $_.Current.Name -like 'LILITH*' }) | Select-Object -First 1
    if ($found) { $script:main = $found }
    return $found
}
function Find-InShell([string]$name) {
    if (-not (Find-Shell)) { return $null }
    return (Find-Named $name)
}
# Invokes the first element of that name that can be invoked, if any.
function Invoke-AnyNamed([string]$name) {
    $all = Retry-Walk { $script:main.FindAll($TS::Descendants, (Prop $AE::NameProperty $name)) }
    foreach ($e in $all) {
        $pattern = $null
        if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
            $pattern.Invoke()
            return
        }
    }
}
# UI Automation shows a floating dock as a window under the main one, or at the top.
function Find-FloatingWindow([string]$name) {
    $window = [System.Windows.Automation.AndCondition]::new(
        (Prop $AE::ProcessIdProperty $script:app.Id), (Prop $AE::NameProperty $name),
        (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Window)))
    $found = Retry-Walk { $AE::RootElement.FindFirst($TS::Children, $window) }
    if ($found) { return $found }
    return (Retry-Walk { $script:main.FindFirst($TS::Descendants, $window) })
}
# Whether the foreground window is, or isn't, the app's, within the person's time for one stage.
# Held for 300 ms, so a moment with no foreground window at all, between two others, doesn't count.
function Wait-Front([bool]$ours) {
    $end = (Get-Date).AddSeconds($HandSec)
    $since = $null
    while ((Get-Date) -lt $end) {
        $front = Get-WindowProcessId ([LilConfirmCheck.Win]::GetForegroundWindow())
        if (($front -eq $script:app.Id) -eq $ours) {
            if (-not $since) { $since = Get-Date }
            if (((Get-Date) - $since).TotalMilliseconds -ge 300) { return $true }
        } else {
            $since = $null
        }
        Start-Sleep -Milliseconds 30
    }
    return $false
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

        # (s) How often the button-state read would refuse a click of each length. Injected input
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
                for ($i = 0; $i -lt 5; ++$i) {
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
                    Start-Sleep -Milliseconds 1100
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

        # (m) Touch and pen never count. Only at Approve's centre, in front, checked right before each.
        Step '(m)' {
            $script:id = New-Confirmation
            Open-Window $script:id
            foreach ($way in 'touch', 'pen') {
                Start-Step
                $r = Get-Rect $script:approve
                $x = [int]($r.X + $r.Width / 2)
                $y = [int]($r.Y + $r.Height / 2)
                Assert-AppInFront
                Assert-AppAt $x $y
                $failed = 0
                try {
                    $failed = if ($way -eq 'touch') { [LilConfirmCheck.Win]::TouchTap($x, $y, 60) } else { [LilConfirmCheck.Win]::PenTap($x, $y, 60) }
                } catch [System.EntryPointNotFoundException] {
                    $failed = -1
                }
                if ($failed -ne 0) {
                    $note = "(m) $way not checked: $(if ($failed -lt 0) { 'Windows has no such API here' } else { "the injection failed, error $failed" })"
                    Write-Host $note
                    $results.Add($note)
                    continue
                }
                Start-Sleep -Milliseconds 700
                Check ((Get-Answer $script:id) -eq 'pending') "(m) $way on Approve answered nothing"
                $names = (Get-RefusalNames) -join ', '
                $origins = (@(Get-PointerLines) | ForEach-Object { "$($_.kind) origin=$($_.origin) device=$($_.device)" }) -join '; '
                $measured = "(m) $way measured: refusals: [$names]; presses and releases the filter saw: [$origins]; the window: $(Get-WindowReason)"
                Write-Host $measured
                $results.Add($measured)
            }
        }

        # (p) A floated panel's window: if it is layered and not fully opaque, not_exposed would refuse
        # every click on a dialog in it. Read only.
        Step '(p)' {
            Close-App
            $script:app = Start-Process -FilePath $image -WorkingDirectory $root -PassThru
            [void](Wait-For { Find-Shell } 40 'the shell')
            $float = $null
            try { $float = Wait-For { Find-InShell 'Float Confirmations' } 15 'its Float button' } catch { }
            if (-not $float) {
                # Tabbed with Jobs: its tab first.
                Invoke-AnyNamed 'Confirmations'
                try { $float = Wait-For { Find-InShell 'Float Confirmations' } 5 'its Float button' } catch { }
            }
            $window = $null
            if ($float) {
                Invoke-Uia $float
                try { $window = Wait-For { Find-FloatingWindow 'Confirmations' } 10 'the floating window' } catch { }
            }
            if (-not $window) {
                $note = "(p) not checked: the panel didn't float (Float button found: $([bool]$float))"
                Write-Host $note
                $results.Add($note)
            } else {
                $hwnd = [System.IntPtr]$window.Current.NativeWindowHandle
                $style = [LilConfirmCheck.Win]::GetWindowLongPtr($hwnd, -20).ToInt64()
                $text = "ex-style 0x$($style.ToString('X')), not layered, so not_exposed wouldn't refuse for that"
                if ($style -band 0x80000) {
                    [uint32]$key = 0; [byte]$alpha = 0; [uint32]$flags = 0
                    if ([LilConfirmCheck.Win]::GetLayeredWindowAttributes($hwnd, [ref]$key, [ref]$alpha, [ref]$flags)) {
                        $seen = (-not ($flags -band 2) -or $alpha -eq 255) -and -not ($flags -band 1)
                        $text = "ex-style 0x$($style.ToString('X')), layered, alpha $alpha, flags $flags, so not_exposed $(if ($seen) { "wouldn't refuse" } else { 'WOULD REFUSE every click there' })"
                    } else {
                        $text = "ex-style 0x$($style.ToString('X')), layered, its attributes can't be read, so not_exposed WOULD REFUSE every click there"
                    }
                }
                $measured = "(p) measured: the floating Confirmations window: $text"
                Write-Host $measured
                $results.Add($measured)
            }
            Close-App
        }

        # The gesture: before (d), so the suite file's edit and the core's restart can't touch it.
        if ($NoHand) {
            Fail 'the gesture (f, g, h, i, j) was NOT checked: -NoHand'
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
                            # This step's own refusal, in this step's log: (f) left the same words on screen.
                            Check (@(Get-RefusalNames) -contains 'not_pointer') "(h) refused as not_pointer in this step's log: [$((Get-RefusalNames) -join ', ')]"
                            Check ((Get-WindowReason) -like 'That click didn''t come from*') "(h) and the window says why: $(Get-WindowReason)"
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
                $answer = Wait-RealClick 'Now: move off Approve and back, wait a moment, click once'
                if ($answer -eq 'pending') {
                    Set-Cue 'Timed out: no click on Approve counted. The evidence is saved.'
                } else {
                    Set-Cue 'Thanks. Stay off the mouse: one more short step comes soon, and this turns blue for it.'
                }
                $lines = @(Get-PointerLines)
                Write-Host "(i) measured: $(($lines | ForEach-Object { "$($_.kind) origin=$($_.origin) device=$($_.device) physically_down=$($_.physically_down) by_messages_down=$($_.by_messages_down)" }) -join '; ')"
                $press = $lines | Where-Object { $_.kind -eq 'Press' } | Select-Object -Last 1
                $release = $lines | Where-Object { $_.kind -eq 'Release' } | Select-Object -Last 1
                $refused = @(Get-Refusals).Count
                Get-Refusals | ForEach-Object { Write-Host "(i) measured: a refused click: $((($_ | ConvertFrom-Json).fields) | ConvertTo-Json -Compress)" }
                Check ($answer -eq 'approved') "(i) a real click on Approve approved it (the core says: $answer; refused clicks before it: $refused; the window: $(Get-WindowReason))"
                Check ($press -and $press.origin -eq 'Hardware' -and $press.physically_down -eq $true) '(i) the press reads as hardware, the button physically down'
                Check ($release -and $release.origin -eq 'Hardware' -and $release.physically_down -eq $false) '(i) the release reads as hardware, the button physically up'
                Start-Sleep -Seconds 3
                Clear-Cue
            }
            Clear-Cue

            # (l) A measurement, no click: can a warp merge into the hand's next move and read as real?
            $script:readyForJ = $false
            Step '(l)' {
                $script:id = New-Confirmation
                # It may wait here for Windows to let the window come to the front, and says so.
                Open-Window $script:id
                $script:readyForJ = $true
                Start-Step
                $r = Get-Rect $script:approve
                $onX = [int]($r.X + $r.Width / 2)
                $onY = [int]($r.Y + $r.Height / 2)
                # Just above Approve, inside the window: no button there is ever clicked.
                $offY = [int]($r.Y - 2 * $r.Height)
                $cue = 'Now: keep moving the mouse around near Approve for 10 seconds. Do not click. The pointer will jump: that is the test.'
                Set-Cue $cue
                Try-Beep 1047 400
                Write-Host "NOW: $cue"
                # A moment for the hand to start.
                Start-Sleep -Seconds 2
                $warps = [System.Collections.Generic.List[datetime]]::new()
                $stopped = ''
                $end = (Get-Date).AddSeconds(10)
                try {
                    while ((Get-Date) -lt $end) {
                        Assert-AppInFront
                        Assert-AppAt $onX $offY
                        [void][LilConfirmCheck.Win]::SetCursorPos($onX, $offY)
                        Start-Sleep -Milliseconds 250
                        Assert-AppInFront
                        Assert-AppAt $onX $onY
                        $warps.Add([datetime]::UtcNow)
                        [void][LilConfirmCheck.Win]::SetCursorPos($onX, $onY)
                        Start-Sleep -Milliseconds 250
                        Set-Cue "$cue`n($([int]($end - (Get-Date)).TotalSeconds) s left)"
                    }
                } catch { $stopped = "$_" }
                # The last warp left the cursor on Approve. Nothing moves it from here but the hand.
                Set-Cue 'Stop moving the mouse, and do not click. The next step comes in a moment.'
                Start-Sleep -Milliseconds 500
                $moves = @(Get-Moves)
                # Each arrival's origin, and whether the message's own point was where the cursor then was.
                $origins = @($moves | ForEach-Object { "$($_.origin)$(if ($_.on_time -eq $false) { '/late' })" })
                $hardware = @($moves | Where-Object { $_.origin -eq 'Hardware' }).Count
                # A Hardware arrival within 50 ms after a warp onto Approve.
                $merged = 0
                $timed = $true
                foreach ($line in @(Get-LogSinceStep)) {
                    if ($line -notlike "*a move onto a confirmation's button*") { continue }
                    $o = $line | ConvertFrom-Json
                    if ($o.fields.origin -ne 'Hardware') { continue }
                    try {
                        $at = ([datetimeoffset]$o.timestamp).UtcDateTime
                        if (@($warps | Where-Object { ($at - $_).TotalMilliseconds -ge 0 -and ($at - $_).TotalMilliseconds -lt 50 }).Count -gt 0) { ++$merged }
                    } catch { $timed = $false }
                }
                $stoppedNote = if ($stopped) { " It stopped early: $stopped" } else { '' }
                $timedNote = if ($timed) { '' } else { " (some lines' times couldn't be read)" }
                $measured = "(l) measured: $($warps.Count) warps onto Approve; arrival lines by origin: [$($origins -join ', ')]; Hardware: $hardware, of which right after a warp (within 50 ms): $merged$timedNote.$stoppedNote"
                Write-Host $measured
                $results.Add($measured)
                Start-Sleep -Seconds 2
            }

            # (j) A person's normal way back to the window. Coming to the front makes Approve a new
            # button to the gate, and the hand arrives on it by moving, so the first click counts.
            Step '(j)' {
                # (l)'s window and confirmation if they are still good: a new window could wait for
                # minutes on Windows again. A stray click on Approve during (l) would have answered it,
                # and an answer here would pass this with nothing tested.
                if (-not $script:readyForJ -or (Get-Answer $script:id) -ne 'pending') {
                    $script:id = New-Confirmation
                    Open-Window $script:id
                }
                Start-Step
                Set-Cue 'Now: click once outside the LILITH Confirm window, on the empty desktop'
                Try-Beep 1047 400
                Write-Host "NOW: click once outside the window. ($HandSec s)"
                if (-not (Wait-Front $false)) {
                    Set-Cue 'Timed out: the window stayed in front. The check goes on.'
                    Fail "(j) timed out after $HandSec s: the window stayed in front, so it never came back to it"
                    Start-Sleep -Seconds 3
                } else {
                    Set-Cue 'Now: click the title bar of the LILITH Confirm window, once'
                    Try-Beep 880 400
                    Write-Host "NOW: click the window's title bar once. ($HandSec s)"
                    if (-not (Wait-Front $true)) {
                        Set-Cue 'Timed out: the window never came back to the front. The check goes on.'
                        Fail "(j) timed out after $HandSec s: the window never came back to the front"
                        Start-Sleep -Seconds 3
                    } else {
                        $cue = 'Now: move onto Approve, wait a moment, click once'
                        Try-Beep 659 250
                        Write-Host "NOW: $cue. ($HandSec s)"
                        $answer = Wait-RealClick $cue
                        Set-Cue $(if ($answer -eq 'pending') { 'Timed out: no click on Approve counted. The evidence is saved.' } else { 'Thanks. Stay off the mouse: one more short step comes soon, and this turns blue for it.' })
                        $names = @(Get-RefusalNames)
                        $measured = "(j) measured: refusals before it counted: [$($names -join ', ')]; what the gate was told: [$(Get-FrontStory)]"
                        Write-Host $measured
                        $results.Add($measured)
                        Check ($answer -eq 'approved') "(j) after a title-bar click, a real click on Approve approved it (the core says: $answer)"
                        Check (@($names | Where-Object { $_ -in 'brought_forward', 'not_pointer' }).Count -eq 0) '(j) and no refusal said it brought the window forward or was not a pointer'
                        Start-Sleep -Seconds 3
                    }
                }
                Clear-Cue
            }
            Clear-Cue

            # (q) A click straight on Approve in a window that isn't in front: refused, and its words say
            # what works, since coming to the front made a new button.
            Step '(q)' {
                $expected = 'That click brought this window to the front. Move the pointer off Approve, then come back and click it.'
                $script:id = New-Confirmation
                # It may wait here for Windows to let the window come to the front, and says so.
                Open-Window $script:id
                Start-Step
                Set-Cue 'Now: click once outside the LILITH Confirm window, on the empty desktop'
                Try-Beep 1047 400
                Write-Host "NOW: click once outside the window. ($HandSec s)"
                if (-not (Wait-Front $false)) {
                    Set-Cue 'Timed out: the window stayed in front. The check goes on.'
                    Fail "(q) timed out after $HandSec s: the window stayed in front, so no click could bring it forward"
                    Start-Sleep -Seconds 3
                } else {
                    Set-Cue 'Now: click Approve once, in the LILITH Confirm window, while it is not in front'
                    Try-Beep 880 400
                    Write-Host "NOW: click Approve once. ($HandSec s)"
                    # No UI Automation while waiting: it would keep the app's GUI thread busy for the click.
                    # The front is watched all the while, on GetTickCount's clock: if the window is back
                    # in front before the press happened, nothing here tests a click that brings it.
                    $end = (Get-Date).AddSeconds($HandSec)
                    $frontBackTick = $null
                    $press = $null
                    $nextLook = Get-Date
                    while ((Get-Date) -lt $end -and -not $press) {
                        if ($null -eq $frontBackTick -and (Get-WindowProcessId ([LilConfirmCheck.Win]::GetForegroundWindow())) -eq $script:app.Id) {
                            $frontBackTick = [Environment]::TickCount
                        }
                        if ((Get-Date) -ge $nextLook) {
                            $press = @(Get-PointerLines | Where-Object { $_.kind -eq 'Press' }) | Select-Object -First 1
                            $nextLook = (Get-Date).AddMilliseconds(300)
                        }
                        Start-Sleep -Milliseconds 15
                    }
                    Start-Sleep -Milliseconds 500
                    $answer = Get-Answer $script:id
                    $names = @(Get-RefusalNames)
                    $said = Get-WindowReason
                    # Both on GetTickCount's clock, which wraps: compared as a signed difference. The
                    # window comes to the front as the activating click happens, so it counts as back
                    # first only when that was clearly before the press's own time.
                    $frontFirst = $null
                    if ($press -and $null -ne $frontBackTick -and $null -ne $press.tick) {
                        $gap = [int64]$press.tick - ([int64]$frontBackTick -band 0xffffffffL)
                        if ($gap -gt 2147483647L) { $gap -= 4294967296L } elseif ($gap -lt -2147483648L) { $gap += 4294967296L }
                        $frontFirst = $gap -gt 50
                    }
                    $measured = "(q) measured: the first click: the core says $answer; refusals: [$($names -join ', ')]; the window said: $said; the window was back in front before the press: $frontFirst; what the gate was told: [$(Get-FrontStory)]"
                    Write-Host $measured
                    $results.Add($measured)
                    if ($frontFirst) {
                        $note = '(q) NOT TESTED: the window was back in front before the click on Approve, so that click could not bring it forward'
                        Write-Host $note
                        $results.Add($note)
                        Set-Cue 'That one could not be tested: the window was already in front. The check goes on.'
                        Start-Sleep -Seconds 3
                    } elseif ($names.Count -eq 0) {
                        Set-Cue $(if ($answer -eq 'pending') { 'Timed out: no click on Approve came. The evidence is saved.' } else { 'That click answered it, and it should not have. The evidence is saved.' })
                        Fail "(q) the click on Approve in a window not in front wasn't refused (the core says: $answer)"
                        Start-Sleep -Seconds 3
                    } else {
                        Check ($names[0] -eq 'brought_forward') "(q) it was refused as brought_forward: $($names[0])"
                        Check ($answer -eq 'pending') '(q) and it answered nothing'
                        Check ($said -eq $expected) "(q) and the window says what works: $said"
                        Start-Step
                        Try-Beep 659 250
                        $answer = Wait-RealClick 'Now: move the pointer off Approve, then come back and click it'
                        Set-Cue $(if ($answer -eq 'pending') { 'Timed out: no click on Approve counted. The evidence is saved.' } else { 'That was the last thing needed from you. Stay off the mouse until this says the PC is free.' })
                        $measured = "(q) measured: refused again before it counted: [$((Get-RefusalNames) -join ', ')]"
                        Write-Host $measured
                        $results.Add($measured)
                        Check ($answer -eq 'approved') "(q) and doing that approved it (the core says: $answer)"
                        Start-Sleep -Seconds 3
                    }
                }
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
            # The window has settled, so a too-soon now comes from the change alone.
            Start-Sleep -Milliseconds 700
            Start-Step
            $vp.SetValue('25')
            Invoke-Uia $script:deny
            Check (@(Get-RefusalNames) -contains 'too_soon') "(d) right after the change the gate had re-armed: Cancel was too soon: [$((Get-RefusalNames) -join ', ')]"
            Check ((Get-Answer $script:id) -eq 'pending') '(d) and nothing was answered'
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
