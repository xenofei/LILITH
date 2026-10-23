<#
Spike 11 (plan 4a Task 1): detection helpers, dot-sourced by run-spike.ps1, run-sandbox.ps1 and
inside.ps1. Written for Windows PowerShell 5.1 and PowerShell 7. Loading this file defines
functions and does nothing else.

What they detect, and how (automation round, owner direction 2026-10-23):
- A toast popup or a notification-centre entry: UI Automation (System.Windows.Automation).
  Microsoft does not document which process or window hosts toasts, so nothing here names one.
  A toast's text is unique per post (a nonce), and the search looks only in top-level windows
  that appeared after the toast was posted, plus the hosts learned that way earlier in the run
  (process name and window class, recorded with every result). A text element's UIA Name is its
  text, so the search matches Names that contain the nonce.
- The foreground window: GetForegroundWindow.
- The session's lock state: WTSQuerySessionInformation(WTSSessionInfoEx), SessionFlags.

What they do to the desktop, and only when a caller asks: a synthesized left click at a point
(SendInput; the cursor is put back afterwards), key presses (Win+N, Win+R, Esc, Enter), moving
one of the spike's own windows (SetWindowPos), and UIA InvokePattern / ValuePattern calls.
#>

$script:SpikeUiSource = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace SpikeUi {
    public static class Win {
        [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
        [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
        [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
        [StructLayout(LayoutKind.Explicit)] struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
        [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public INPUTUNION u; }

        [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
        [DllImport("user32.dll")] static extern bool GetPhysicalCursorPos(out POINT p);
        [DllImport("user32.dll")] static extern bool SetPhysicalCursorPos(int x, int y);
        [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
        [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
        [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
        [DllImport("wtsapi32.dll", SetLastError = true)] static extern bool WTSQuerySessionInformationW(IntPtr server, int session, int infoClass, out IntPtr buffer, out int bytes);
        [DllImport("wtsapi32.dll")] static extern void WTSFreeMemory(IntPtr p);

        // Physical pixels for this thread, as UI Automation reports rectangles.
        public static void PerMonitorAware() {
            try { SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch (EntryPointNotFoundException) { }
        }

        // {hwnd, pid} of the foreground window.
        public static long[] Foreground() {
            IntPtr h = GetForegroundWindow();
            uint pid; GetWindowThreadProcessId(h, out pid);
            return new long[] { h.ToInt64(), pid };
        }

        public static string ClassOf(long hwnd) {
            StringBuilder s = new StringBuilder(256);
            GetClassNameW(new IntPtr(hwnd), s, s.Capacity);
            return s.ToString();
        }

        // A left click at (x, y) in physical pixels; the cursor goes back where it was.
        public static uint ClickAt(int x, int y) {
            PerMonitorAware();
            POINT old; GetPhysicalCursorPos(out old);
            SetPhysicalCursorPos(x, y);
            Thread.Sleep(80);
            INPUT[] i = new INPUT[2];
            i[0].type = 0; i[0].u.mi.dwFlags = 0x0002;   // MOUSEEVENTF_LEFTDOWN
            i[1].type = 0; i[1].u.mi.dwFlags = 0x0004;   // MOUSEEVENTF_LEFTUP
            uint n = SendInput(2, i, Marshal.SizeOf(typeof(INPUT)));
            Thread.Sleep(150);
            SetPhysicalCursorPos(old.X, old.Y);
            return n;
        }

        // The cursor to (x, y) without a click (a hover); returns where it was.
        public static int[] MoveTo(int x, int y) {
            PerMonitorAware();
            POINT old; GetPhysicalCursorPos(out old);
            SetPhysicalCursorPos(x, y);
            return new int[] { old.X, old.Y };
        }

        // Presses the keys in order, then releases them in reverse (a chord such as Win+N).
        public static uint Keys(ushort[] vks) {
            INPUT[] i = new INPUT[vks.Length * 2];
            for (int k = 0; k < vks.Length; k++) {
                i[k].type = 1; i[k].u.ki.wVk = vks[k];
                i[k].u.ki.dwFlags = vks[k] == 0x5B ? 0x0001u : 0u;   // VK_LWIN is an extended key
            }
            for (int k = 0; k < vks.Length; k++) {
                int j = vks.Length + k; ushort vk = vks[vks.Length - 1 - k];
                i[j].type = 1; i[j].u.ki.wVk = vk;
                i[j].u.ki.dwFlags = 0x0002u | (vk == 0x5B ? 0x0001u : 0u);   // KEYEVENTF_KEYUP
            }
            return SendInput((uint)i.Length, i, Marshal.SizeOf(typeof(INPUT)));
        }

        // Moves and sizes a window (physical pixels) and puts it at the top of the z-order.
        public static bool Place(long hwnd, int x, int y, int w, int h) {
            PerMonitorAware();
            return SetWindowPos(new IntPtr(hwnd), IntPtr.Zero, x, y, w, h, 0x0040);   // SWP_SHOWWINDOW
        }

        [StructLayout(LayoutKind.Sequential)] struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
        [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LASTINPUTINFO i);

        // Milliseconds since the session's last keyboard or mouse input (GetLastInputInfo).
        public static long IdleMs() {
            LASTINPUTINFO i = new LASTINPUTINFO(); i.cbSize = (uint)Marshal.SizeOf(typeof(LASTINPUTINFO));
            if (!GetLastInputInfo(ref i)) return -1;
            return (long)unchecked((uint)Environment.TickCount - i.dwTime);
        }

        // WTSINFOEXW.Data.WTSInfoExLevel1.SessionFlags: 0 = locked, 1 = unlocked; -1 = unknown.
        public static int SessionFlags() {
            IntPtr b; int n;
            if (!WTSQuerySessionInformationW(IntPtr.Zero, -1, 25, out b, out n)) return -1;   // WTSSessionInfoEx
            try {
                if (Marshal.ReadInt32(b, 0) != 1) return -1;   // Level
                return Marshal.ReadInt32(b, 16);               // Data (8-aligned) + SessionId + SessionState
            } finally { WTSFreeMemory(b); }
        }
    }
}
'@

# ready: the assemblies and the helper type loaded; proven: a toast was found this run, so a
# toast not found is a toast that did not show; hosts: where toasts were found.
$script:Uia = [ordered]@{ ready = $false; error = $null; proven = $false; hosts = @() }
$script:ProcNames = @{}

function Initialize-SpikeUi {
    if ($script:Uia.ready) { return $true }
    if ($null -ne $script:Uia.error) { return $false }
    try {
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase
        if (-not ('SpikeUi.Win' -as [type])) { Add-Type -TypeDefinition $script:SpikeUiSource }
        [SpikeUi.Win]::PerMonitorAware()
        $script:Uia.ready = $true
    } catch {
        $script:Uia.error = $_.Exception.Message
    }
    $script:Uia.ready
}

function Get-ProcName([int]$ProcessId) {
    if (-not $script:ProcNames.ContainsKey($ProcessId)) {
        $n = $null
        try { $n = (Get-Process -Id $ProcessId -ErrorAction Stop).ProcessName } catch { }
        $script:ProcNames[$ProcessId] = $n
    }
    $script:ProcNames[$ProcessId]
}

# The visible top-level windows, in z-order.
function Get-UiaTop {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    @($root.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition))
}

function Get-TopInfo($Top) {
    $c = $Top.Current
    [ordered]@{ hwnd = [int64]$c.NativeWindowHandle; pid = $c.ProcessId; process = (Get-ProcName $c.ProcessId); class = $c.ClassName; name = $c.Name }
}

function Get-ElementInfo($Element) {
    if ($null -eq $Element) { return $null }
    try {
        $c = $Element.Current
        [ordered]@{ control = $c.ControlType.ProgrammaticName; name = $c.Name; automation_id = $c.AutomationId; class = $c.ClassName; rect = "$($c.BoundingRectangle)" }
    } catch { [ordered]@{ error = $_.Exception.Message } }
}

# The top-level windows' handles now: what a later search treats as "already there".
function Get-UiaSnapshot {
    $s = @{}
    if (-not (Initialize-SpikeUi)) { return $s }
    foreach ($t in Get-UiaTop) { try { $s[[int64]$t.Current.NativeWindowHandle] = $true } catch { } }
    $s
}

function Add-UiaHost($Info) {
    foreach ($h in $script:Uia.hosts) { if ($h.process -eq $Info.process -and $h.class -eq $Info.class) { return } }
    $script:Uia.hosts = @($script:Uia.hosts) + @([ordered]@{ process = $Info.process; class = $Info.class; name = $Info.name })
}

function Test-KnownHost($Info) {
    foreach ($h in $script:Uia.hosts) { if ($h.process -eq $Info.process -and $h.class -eq $Info.class) { return $true } }
    $false
}

function Find-NeedleUnder($Top, [string]$Needle) {
    if ("$($Top.Current.Name)".Contains($Needle)) { return $Top }
    foreach ($e in @($Top.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition))) {
        try { if ("$($e.Current.Name)".Contains($Needle)) { return $e } } catch { }
    }
    $null
}

# The element whose Name contains $Needle, in a top-level window that is new since $Before or
# is a host learned earlier; $null when there is none.
function Find-Needle([string]$Needle, $Before) {
    foreach ($t in Get-UiaTop) {
        try {
            $hwnd = [int64]$t.Current.NativeWindowHandle
            $info = $null
            if ($Before.ContainsKey($hwnd)) {
                $info = Get-TopInfo $t
                if (-not (Test-KnownHost $info)) { continue }
            }
            $hit = Find-NeedleUnder $t $Needle
            if ($null -ne $hit) {
                if ($null -eq $info) { $info = Get-TopInfo $t }
                return [pscustomobject]@{ Element = $hit; Top = $t; TopInfo = $info }
            }
        } catch { }
    }
    $null
}

# Appearances from timed samples {t, present}. An appearance ends only when the toast was seen
# absent for more than $GapSec (a single missed poll is not a second pop, and a slow poll with
# no absence in between is not one either); unknown samples (present = $null) are skipped.
# Pure; tests/strict-results.ps1 covers it.
function Get-Appearances($Samples, [double]$GapSec = 1.0) {
    $apps = New-Object System.Collections.ArrayList
    $cur = $null
    $absentSince = $null
    foreach ($s in @($Samples)) {
        $t = [double]$s['t']
        if ($s['present'] -eq $true) {
            if ($null -ne $cur -and ($null -eq $absentSince -or ($t - $absentSince) -le $GapSec)) { $cur['to_s'] = $t }
            else {
                $cur = [ordered]@{ from_s = $t; to_s = $t }
                [void]$apps.Add($cur)
            }
            $absentSince = $null
        } elseif ($s['present'] -eq $false -and $null -eq $absentSince) {
            $absentSince = $t
        }
    }
    $apps.ToArray()
}

# Polls for $Needle every $IntervalMs for $Seconds (or until found with -UntilFound). Hit is the
# last element found (for a click); Record is what goes into the results.
function Watch-Needle([string]$Needle, $Before, [double]$Seconds, [switch]$UntilFound, [int]$IntervalMs = 250) {
    if (-not (Initialize-SpikeUi)) {
        return [pscustomobject]@{ Hit = $null; Record = [ordered]@{ method = 'none'; why = "UI Automation not available: $($script:Uia.error)" } }
    }
    $samples = New-Object System.Collections.ArrayList
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $last = $null
    $firstS = $null
    while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        $hit = $null; $ok = $true
        try { $hit = Find-Needle $Needle $Before } catch { $ok = $false }
        $t = [math]::Round($sw.Elapsed.TotalSeconds, 2)
        $present = $null
        if ($ok) { $present = ($null -ne $hit) }
        [void]$samples.Add([ordered]@{ t = $t; present = $present })
        if ($null -ne $hit) {
            $last = $hit
            if ($null -eq $firstS) { $firstS = $t }
            Add-UiaHost $hit.TopInfo
            $script:Uia.proven = $true
            if ($UntilFound) { break }
        }
        Start-Sleep -Milliseconds $IntervalMs
    }
    $apps = @(Get-Appearances $samples.ToArray())
    $hostInfo = $null; $el = $null
    if ($null -ne $last) { $hostInfo = $last.TopInfo; $el = Get-ElementInfo $last.Element }
    [pscustomobject]@{
        Hit = $last
        Record = [ordered]@{
            method = 'uia'; needle = $Needle; seconds = $Seconds; polls = $samples.Count
            first_seen_s = $firstS; appearances = $apps; host = $hostInfo; element = $el
        }
    }
}

function Get-TopAtPoint([int]$X, [int]$Y) {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $e = [System.Windows.Automation.AutomationElement]::FromPoint((New-Object System.Windows.Point($X, $Y)))
    for ($i = 0; $i -lt 60 -and $null -ne $e; $i++) {
        $p = $walker.GetParent($e)
        if ($null -eq $p -or [System.Windows.Automation.Automation]::Compare($p, $root)) { return [int64]$e.Current.NativeWindowHandle }
        $e = $p
    }
    [int64]0
}

# InvokePattern on the element or its nearest ancestor that has one, inside the window $TopHwnd.
function Invoke-UiaElement($Element, [int64]$TopHwnd) {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $e = $Element
    try {
        for ($i = 0; $i -lt 12 -and $null -ne $e; $i++) {
            $pat = $null
            if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pat)) {
                $info = Get-ElementInfo $e
                $pat.Invoke()
                return [ordered]@{ mode = 'uia-invoke'; ok = $true; invoked = $info; levels_up = $i }
            }
            if ([int64]$e.Current.NativeWindowHandle -eq $TopHwnd) { break }
            $e = $walker.GetParent($e)
        }
    } catch {
        return [ordered]@{ mode = 'uia-invoke'; ok = $false; why = $_.Exception.Message }
    }
    [ordered]@{ mode = 'uia-invoke'; ok = $false; why = 'no InvokePattern on the element or its ancestors in its window' }
}

# A synthesized left click on the centre of the element, only when that point is on the window
# $TopHwnd (else nothing is clicked).
function Click-ElementCentre($Element, [int64]$TopHwnd) {
    try {
        [SpikeUi.Win]::PerMonitorAware()
        $r = $Element.Current.BoundingRectangle
        if ($r.IsEmpty -or $r.Width -le 0 -or $r.Height -le 0) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'no bounding rectangle' } }
        $x = [int]($r.X + $r.Width / 2); $y = [int]($r.Y + $r.Height / 2)
        $at = Get-TopAtPoint $x $y
        if ($at -ne $TopHwnd) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = "the point ($x, $y) is on another window ($at), not on $TopHwnd"; point = "$x,$y" } }
        $sent = [SpikeUi.Win]::ClickAt($x, $y)
        [ordered]@{ mode = 'synthetic-mouse'; ok = ($sent -eq 2); point = "$x,$y"; inputs_sent = $sent }
    } catch {
        [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = $_.Exception.Message }
    }
}

# A synthesized click on a visible part of the window $Hwnd's lower area (never its title bar,
# so never its close button): the first point of a grid that UIA says is on that window.
function Click-WindowVisible([int64]$Hwnd) {
    try {
        [SpikeUi.Win]::PerMonitorAware()
        $r = ([System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Hwnd)).Current.BoundingRectangle
        if ($r.IsEmpty) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'no window rectangle' } }
        foreach ($fy in 0.5, 0.65, 0.8, 0.92, 0.4) {
            foreach ($fx in 0.5, 0.3, 0.7, 0.1, 0.9) {
                $x = [int]($r.X + $r.Width * $fx); $y = [int]($r.Y + $r.Height * $fy)
                if ((Get-TopAtPoint $x $y) -eq $Hwnd) {
                    $sent = [SpikeUi.Win]::ClickAt($x, $y)
                    return [ordered]@{ mode = 'synthetic-mouse'; ok = ($sent -eq 2); point = "$x,$y" }
                }
            }
        }
        [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'no visible point of the window' }
    } catch {
        [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = $_.Exception.Message }
    }
}

function Get-IdleSeconds {
    if (-not (Initialize-SpikeUi)) { return $null }
    $ms = [SpikeUi.Win]::IdleMs()
    if ($ms -lt 0) { return $null }
    [math]::Round($ms / 1000.0, 1)
}

# The button named $Name inside the window $Top (the toast's own action content, e.g. "Open").
function Find-ButtonNamed($Top, [string]$Name) {
    $isButton = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    $named = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $cond = [System.Windows.Automation.AndCondition]::new([System.Windows.Automation.Condition[]]@($isButton, $named))
    $Top.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Get-ForegroundNow {
    if (-not (Initialize-SpikeUi)) { return $null }
    $f = [SpikeUi.Win]::Foreground()
    [ordered]@{ hwnd = $f[0]; pid = [int]$f[1]; process = (Get-ProcName ([int]$f[1])); class = [SpikeUi.Win]::ClassOf($f[0]) }
}

# $true locked, $false unlocked, $null unknown.
function Get-SessionLocked {
    if (-not (Initialize-SpikeUi)) { return $null }
    $f = [SpikeUi.Win]::SessionFlags()
    if ($f -eq 0) { return $true }
    if ($f -eq 1) { return $false }
    $null
}

function Send-Keys([int[]]$Vk) {
    [void][SpikeUi.Win]::Keys([uint16[]]$Vk)
}

# Win+N, then whether an entry holding $Needle is listed; then Esc closes the centre again.
function Test-InNotificationCentre([string]$Needle) {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ method = 'none'; why = "UI Automation not available: $($script:Uia.error)" } }
    $before = Get-UiaSnapshot
    Send-Keys @(0x5B, 0x4E)   # Win+N
    $w = Watch-Needle -Needle $Needle -Before $before -Seconds 5 -UntilFound
    $opened = @(Get-UiaTop | Where-Object { -not $before.ContainsKey([int64]$_.Current.NativeWindowHandle) } |
        ForEach-Object { $i = Get-TopInfo $_; [ordered]@{ process = $i.process; class = $i.class } })
    $closed = Close-NotificationCentre $before
    [ordered]@{
        method = 'uia (Win+N and Esc sent by the script)'; listed = ($null -ne $w.Hit)
        centre_opened = ($opened.Count -gt 0); new_windows = $opened; host = $w.Record.host; closed_after = $closed
    }
}

function Close-NotificationCentre($Before) {
    for ($k = 0; $k -lt 2; $k++) {
        Send-Keys @(0x1B)   # Esc
        Start-Sleep -Milliseconds 800
        $still = @(Get-UiaTop | Where-Object { -not $Before.ContainsKey([int64]$_.Current.NativeWindowHandle) })
        if ($still.Count -eq 0) { return $true }
    }
    $false
}

# Dismisses the notification-centre entry holding $Needle with its own dismiss button: Win+N,
# find the entry, find a button in it whose AutomationId or Name says dismiss, close or clear
# (hovering over the entry first if none is exposed), invoke it (or click it), Esc. The buttons
# seen are recorded either way, so a miss shows what the entry offers.
function Dismiss-InNotificationCentre([string]$Needle) {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ method = 'none'; ok = $false; why = "UI Automation not available: $($script:Uia.error)" } }
    $before = Get-UiaSnapshot
    Send-Keys @(0x5B, 0x4E)   # Win+N
    $w = Watch-Needle -Needle $Needle -Before $before -Seconds 5 -UntilFound
    $rec = [ordered]@{ method = 'uia (Win+N, the entry''s dismiss button, Esc)'; found_entry = ($null -ne $w.Hit); ok = $false; buttons = @(); used = $null; hovered = $false }
    $restore = $null
    try {
        if ($null -ne $w.Hit) {
            $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
            $btnCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
            $topHwnd = [int64]$w.Hit.TopInfo.hwnd
            $entry = $w.Hit.Element
            $candidate = $null
            for ($pass = 0; $pass -lt 2 -and $null -eq $candidate; $pass++) {
                $e = $w.Hit.Element
                for ($i = 0; $i -lt 8 -and $null -ne $e -and $null -eq $candidate; $i++) {
                    $buttons = @($e.FindAll([System.Windows.Automation.TreeScope]::Descendants, $btnCond))
                    if ($buttons.Count -gt 0) {
                        $entry = $e
                        $rec.buttons = @($buttons | ForEach-Object { [ordered]@{ name = $_.Current.Name; automation_id = $_.Current.AutomationId } })
                        foreach ($b in $buttons) {
                            if ("$($b.Current.AutomationId) $($b.Current.Name)" -match '(?i)dismiss|close|clear') { $candidate = $b; break }
                        }
                        break
                    }
                    if ([int64]$e.Current.NativeWindowHandle -eq $topHwnd) { break }
                    $e = $walker.GetParent($e)
                }
                if ($null -eq $candidate -and $pass -eq 0) {
                    # The dismiss button may show only while the pointer is over the entry.
                    $r = $entry.Current.BoundingRectangle
                    if (-not $r.IsEmpty) {
                        $restore = [SpikeUi.Win]::MoveTo([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
                        $rec.hovered = $true
                        Start-Sleep -Milliseconds 700
                    }
                }
            }
            if ($null -ne $candidate) {
                $rec.used = Get-ElementInfo $candidate
                $how = Invoke-UiaElement $candidate ([int64]$candidate.Current.NativeWindowHandle)
                if (-not $how.ok) { $how = Click-ElementCentre $candidate $topHwnd }
                $rec.how = $how
                $rec.ok = [bool]$how.ok
            }
        }
    } catch {
        $rec.error = $_.Exception.Message
    } finally {
        if ($null -ne $restore) { [void][SpikeUi.Win]::MoveTo($restore[0], $restore[1]) }
        $rec.closed_after = Close-NotificationCentre $before
    }
    $rec
}
