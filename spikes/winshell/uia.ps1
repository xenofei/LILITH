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
- The owner's own input: GetLastInputInfo, told apart from the script's own injections.
- Power evidence: CallNtPowerInformation (SystemExecutionState, SystemPowerInformation), read only.

What they do to the desktop, and only when a caller asks: a synthesized left click (one SendInput
call carrying the position, MOUSEEVENTF_MOVE | ABSOLUTE | VIRTUALDESK, with the button down and up;
the cursor is put back afterwards), key presses (Win+N, Win+R, Esc, Enter), a hover (the cursor
moved, no click), moving one of the spike's own windows (SetWindowPos), and UIA InvokePattern /
ValuePattern calls. Every synthesized input first checks that the owner gave no input in the last
2 s (else it waits and retries, then gives up to the owner), and a click also checks, right before
it is injected, that the window under the point belongs to the expected process and class.
Every UIA action on a toast is scoped to that toast: the nearest ancestor of its nonce text that
holds no other spike toast's nonce. Nothing whose Name or AutomationId says "clear" or "all" is
ever pressed (Test-DeniedButton).
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
        [StructLayout(LayoutKind.Sequential)] struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
        [StructLayout(LayoutKind.Sequential)] struct SPI { public uint MaxIdlenessAllowed; public uint Idleness; public uint TimeRemaining; public byte CoolingMode; }

        [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
        [DllImport("user32.dll")] static extern bool GetPhysicalCursorPos(out POINT p);
        [DllImport("user32.dll")] static extern bool SetPhysicalCursorPos(int x, int y);
        [DllImport("user32.dll")] static extern IntPtr WindowFromPhysicalPoint(POINT p);
        [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr h, uint flags);
        [DllImport("user32.dll")] static extern int GetSystemMetrics(int i);
        [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
        [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
        [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
        [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LASTINPUTINFO i);
        [DllImport("wtsapi32.dll", SetLastError = true)] static extern bool WTSQuerySessionInformationW(IntPtr server, int session, int infoClass, out IntPtr buffer, out int bytes);
        [DllImport("wtsapi32.dll")] static extern void WTSFreeMemory(IntPtr p);
        [DllImport("powrprof.dll")] static extern uint CallNtPowerInformation(int level, IntPtr inBuf, uint inLen, out uint outBuf, uint outLen);
        [DllImport("powrprof.dll")] static extern uint CallNtPowerInformation(int level, IntPtr inBuf, uint inLen, out SPI outBuf, uint outLen);

        // The tick of the script's own last injection, so its own input is not taken for the owner's.
        static uint lastInject;
        static bool injected;

        // Physical pixels for this thread, as UI Automation reports rectangles.
        public static void PerMonitorAware() {
            try { SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch (EntryPointNotFoundException) { }
        }

        public static uint LastInputTick() {
            LASTINPUTINFO i = new LASTINPUTINFO(); i.cbSize = (uint)Marshal.SizeOf(typeof(LASTINPUTINFO));
            return GetLastInputInfo(ref i) ? i.dwTime : 0;
        }

        public static uint Now() { return unchecked((uint)Environment.TickCount); }

        // Milliseconds since the session's last keyboard or mouse input (GetLastInputInfo).
        public static long IdleMs() {
            LASTINPUTINFO i = new LASTINPUTINFO(); i.cbSize = (uint)Marshal.SizeOf(typeof(LASTINPUTINFO));
            if (!GetLastInputInfo(ref i)) return -1;
            return (long)unchecked(Now() - i.dwTime);
        }

        // Milliseconds since the owner's last input; long.MaxValue when the last input was the
        // script's own injection.
        public static long OwnerIdleMs() {
            uint last = LastInputTick();
            if (injected) {
                int d = unchecked((int)(last - lastInject));
                if (d >= -50 && d <= 400) return long.MaxValue;
            }
            return (long)unchecked(Now() - last);
        }

        static void MarkInjected() { lastInject = Now(); injected = true; }

        // {hwnd, pid} of the foreground window (hwnd 0 while activation is changing).
        public static long[] Foreground() {
            IntPtr h = GetForegroundWindow();
            uint pid = 0; if (h != IntPtr.Zero) GetWindowThreadProcessId(h, out pid);
            return new long[] { h.ToInt64(), pid };
        }

        public static string ClassOf(long hwnd) {
            StringBuilder s = new StringBuilder(256);
            GetClassNameW(new IntPtr(hwnd), s, s.Capacity);
            return s.ToString();
        }

        public static long PidOf(long hwnd) { uint pid; GetWindowThreadProcessId(new IntPtr(hwnd), out pid); return pid; }

        // The top-level window under (x, y), physical pixels: {hwnd, pid}.
        public static long[] RootAt(int x, int y) {
            PerMonitorAware();
            POINT p; p.X = x; p.Y = y;
            IntPtr root = GetAncestor(WindowFromPhysicalPoint(p), 2);   // GA_ROOT
            uint pid = 0; if (root != IntPtr.Zero) GetWindowThreadProcessId(root, out pid);
            return new long[] { root.ToInt64(), pid };
        }

        // A left click at (x, y), physical pixels, carried by the input itself (MOVE | ABSOLUTE |
        // VIRTUALDESK on both the down and the up), in one SendInput call. Returns 2 when both were
        // injected; -2 when the owner gave input in the last 2 s; -1 when the window under the
        // point is not the expected process and class; -3 when SendInput injected fewer than 2.
        public static int ClickAt(int x, int y, long wantPid, string wantClass) {
            PerMonitorAware();
            if (OwnerIdleMs() < 2000) return -2;
            long[] r = RootAt(x, y);
            if (r[0] == 0 || r[1] != wantPid || ClassOf(r[0]) != wantClass) return -1;
            int vx = GetSystemMetrics(76), vy = GetSystemMetrics(77), vw = GetSystemMetrics(78), vh = GetSystemMetrics(79);
            int nx = (int)Math.Round((x - vx) * 65535.0 / Math.Max(1, vw - 1));
            int ny = (int)Math.Round((y - vy) * 65535.0 / Math.Max(1, vh - 1));
            POINT old; GetPhysicalCursorPos(out old);
            INPUT[] i = new INPUT[2];
            uint at = 0x0001 | 0x8000 | 0x4000;   // MOVE | ABSOLUTE | VIRTUALDESK
            i[0].type = 0; i[0].u.mi.dx = nx; i[0].u.mi.dy = ny; i[0].u.mi.dwFlags = at | 0x0002;   // LEFTDOWN
            i[1].type = 0; i[1].u.mi.dx = nx; i[1].u.mi.dy = ny; i[1].u.mi.dwFlags = at | 0x0004;   // LEFTUP
            uint n = SendInput(2, i, Marshal.SizeOf(typeof(INPUT)));
            MarkInjected();
            Thread.Sleep(150);
            SetPhysicalCursorPos(old.X, old.Y);
            return n == 2 ? 2 : -3;
        }

        // The cursor to (x, y) without a click (a hover). Returns where it was, or null when the
        // owner gave input in the last 2 s.
        public static int[] MoveTo(int x, int y) {
            PerMonitorAware();
            if (OwnerIdleMs() < 2000) return null;
            POINT old; GetPhysicalCursorPos(out old);
            SetPhysicalCursorPos(x, y);
            MarkInjected();
            return new int[] { old.X, old.Y };
        }

        public static void MoveBack(int x, int y) { PerMonitorAware(); SetPhysicalCursorPos(x, y); }

        // Presses the keys in order, then releases them in reverse (a chord such as Win+N), in one
        // SendInput call. Returns the number injected, or -2 when the owner gave input in the
        // last 2 s.
        public static int Keys(ushort[] vks) {
            if (OwnerIdleMs() < 2000) return -2;
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
            uint n = SendInput((uint)i.Length, i, Marshal.SizeOf(typeof(INPUT)));
            MarkInjected();
            return (int)n;
        }

        // Moves and sizes a window (physical pixels) and puts it at the top of the z-order.
        public static bool Place(long hwnd, int x, int y, int w, int h) {
            PerMonitorAware();
            return SetWindowPos(new IntPtr(hwnd), IntPtr.Zero, x, y, w, h, 0x0040);   // SWP_SHOWWINDOW
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

        // SystemExecutionState (16): the EXECUTION_STATE flags in force; negative = the NTSTATUS.
        public static long ExecutionState() {
            uint s; uint r = CallNtPowerInformation(16, IntPtr.Zero, 0, out s, 4);
            return r == 0 ? (long)s : -(long)r;
        }

        // SystemPowerInformation (12): {status, MaxIdlenessAllowed, Idleness, TimeRemaining}.
        public static long[] PowerInfo() {
            SPI p; uint r = CallNtPowerInformation(12, IntPtr.Zero, 0, out p, (uint)Marshal.SizeOf(typeof(SPI)));
            return new long[] { r, p.MaxIdlenessAllowed, p.Idleness, p.TimeRemaining };
        }
    }
}
'@

# ready: the assemblies and the helper type loaded; proven: a toast was found this run, so a
# toast not found in clean readings is a toast that did not show; hosts: where toasts were found.
$script:Uia = [ordered]@{ ready = $false; error = $null; proven = $false; hosts = @() }
$script:ProcNames = @{}
$script:UiaSearchFailed = $false

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

# The one-line warning before each stretch of synthesized input.
function Show-HandsOff([string]$What) {
    Write-Host "HANDS OFF: the script $What now. Ctrl+C in this terminal stops it." -ForegroundColor Yellow
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
        [ordered]@{ control = $c.ControlType.ProgrammaticName; name = $c.Name; automation_id = $c.AutomationId; class = $c.ClassName; rect = "$($c.BoundingRectangle)"; offscreen = $c.IsOffscreen }
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
# is a host learned earlier; $null when there is none. A window whose search threw sets
# $script:UiaSearchFailed, so that a poll without a hit counts as unknown, not as absent.
function Find-Needle([string]$Needle, $Before) {
    $script:UiaSearchFailed = $false
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
        } catch { $script:UiaSearchFailed = $true }
    }
    $null
}

# Appearances from timed samples {t, present}. An appearance ends only after at least 2
# consecutive clean absent samples spanning more than $GapSec: a single missed poll is not a
# second pop, and neither is a slow poll. Unknown samples (present = $null) are skipped. Pure;
# tests/strict-results.ps1 covers it.
function Get-Appearances($Samples, [double]$GapSec = 1.0) {
    $apps = New-Object System.Collections.ArrayList
    $cur = $null
    $absentSince = $null
    $absentCount = 0
    foreach ($s in @($Samples)) {
        $t = [double]$s['t']
        if ($s['present'] -eq $true) {
            $ended = $absentCount -ge 2 -and ($t - $absentSince) -gt $GapSec
            if ($null -ne $cur -and -not $ended) { $cur['to_s'] = $t }
            else {
                $cur = [ordered]@{ from_s = $t; to_s = $t }
                [void]$apps.Add($cur)
            }
            $absentSince = $null; $absentCount = 0
        } elseif ($s['present'] -eq $false) {
            if ($null -eq $absentSince) { $absentSince = $t }
            $absentCount++
        }
    }
    $apps.ToArray()
}

# Polls for $Needle every $IntervalMs for $Seconds (or until found with -UntilFound). Hit is the
# last element found (for a click); Record is what goes into the results. Present samples carry
# the element's rectangle and IsOffscreen (a slide-in shows as a moving rectangle).
function Watch-Needle([string]$Needle, $Before, [double]$Seconds, [switch]$UntilFound, [int]$IntervalMs = 250) {
    if (-not (Initialize-SpikeUi)) {
        return [pscustomobject]@{ Hit = $null; Record = [ordered]@{ method = 'none'; why = "UI Automation not available: $($script:Uia.error)"; clean_polls = 0 } }
    }
    $samples = New-Object System.Collections.ArrayList
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $last = $null
    $firstS = $null
    while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        $hit = $null; $ok = $true
        try { $hit = Find-Needle $Needle $Before; if ($null -eq $hit -and $script:UiaSearchFailed) { $ok = $false } } catch { $ok = $false }
        $t = [math]::Round($sw.Elapsed.TotalSeconds, 2)
        $present = $null
        if ($null -ne $hit) { $present = $true } elseif ($ok) { $present = $false }
        $sample = [ordered]@{ t = $t; present = $present }
        if ($null -ne $hit) {
            try { $sample.rect = "$($hit.Element.Current.BoundingRectangle)"; $sample.offscreen = $hit.Element.Current.IsOffscreen } catch { }
            $last = $hit
            if ($null -eq $firstS) { $firstS = $t }
            Add-UiaHost $hit.TopInfo
            $script:Uia.proven = $true
        }
        [void]$samples.Add($sample)
        if ($null -ne $hit -and $UntilFound) { break }
        Start-Sleep -Milliseconds $IntervalMs
    }
    $all = $samples.ToArray()
    $apps = @(Get-Appearances $all)
    $hostInfo = $null; $el = $null
    if ($null -ne $last) { $hostInfo = $last.TopInfo; $el = Get-ElementInfo $last.Element }
    [pscustomobject]@{
        Hit = $last
        Record = [ordered]@{
            method = 'uia'; needle = $Needle; seconds = $Seconds; polls = $all.Count
            clean_polls = @($all | Where-Object { $null -ne $_['present'] }).Count
            first_seen_s = $firstS; appearances = $apps; host = $hostInfo; element = $el
            samples = @($all | Where-Object { $_['present'] -eq $true })
        }
    }
}

# ---------------------------------------------------------------- scoping to one toast (pure parts)

# Whether a subtree holds this toast only: the nonce's needle is among its Names, no other spike
# nonce (#xxxxxx) is, and at most one "Open" button. Pure; tests/strict-results.ps1 covers it.
function Test-OnlyThisToast([string[]]$Names, [string]$Needle, [int]$OpenButtons = 0) {
    $all = @($Names | Where-Object { $_ })
    $ours = @($all | Where-Object { $_.Contains($Needle) }).Count -gt 0
    $others = @($all | Where-Object { $_ -match '#[0-9a-f]{6}' -and -not $_.Contains($Needle) }).Count
    [bool]($ours -and $others -eq 0 -and $OpenButtons -le 1)
}

# Never pressed, whatever else it says: anything about "clear" or "all" (the notification
# centre's "Clear all" and "Clear all <app> notifications"). Pure; tested.
function Test-DeniedButton([string]$Name, [string]$AutomationId) {
    "$Name $AutomationId" -match '(?i)clear|\ball\b'
}

# A toast's own dismiss button: says dismiss or close, and is not denied. Pure; tested.
function Test-DismissButton([string]$Name, [string]$AutomationId) {
    ("$Name $AutomationId" -match '(?i)dismiss|close') -and -not (Test-DeniedButton $Name $AutomationId)
}

# Whether rectangle $Inner lies inside $Outer (x, y, width, height). Pure; tested.
function Test-RectInside($Inner, $Outer) {
    [bool]($Inner.X -ge $Outer.X - 1 -and $Inner.Y -ge $Outer.Y - 1 -and
        ($Inner.X + $Inner.Width) -le ($Outer.X + $Outer.Width + 1) -and ($Inner.Y + $Inner.Height) -le ($Outer.Y + $Outer.Height + 1))
}

function Get-SubtreeNames($Element) {
    @($Element.FindAll([System.Windows.Automation.TreeScope]::Subtree, [System.Windows.Automation.Condition]::TrueCondition) |
        ForEach-Object { try { $_.Current.Name } catch { } })
}

function Get-Buttons($Element) {
    $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    @($Element.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond))
}

# The toast that carries $Needle: the nearest ancestor of the hit (up to 8 levels, not beyond its
# top-level window) whose subtree holds a button that $Accept takes, as long as that subtree holds
# this toast only. Returns {Root, Button, Buttons} or $null (then nothing is pressed).
function Find-ToastButton($Hit, [string]$Needle, [scriptblock]$Accept) {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $topHwnd = [int64]$Hit.TopInfo.hwnd
    $e = $Hit.Element
    for ($i = 0; $i -lt 8 -and $null -ne $e; $i++) {
        $buttons = @(Get-Buttons $e)
        $pick = @($buttons | Where-Object { & $Accept $_.Current.Name $_.Current.AutomationId }) | Select-Object -First 1
        if ($null -ne $pick) {
            $open = @($buttons | Where-Object { $_.Current.Name -eq 'Open' }).Count
            if (-not (Test-OnlyThisToast (Get-SubtreeNames $e) $Needle $open)) { return $null }
            if (-not (Test-RectInside $pick.Current.BoundingRectangle $e.Current.BoundingRectangle)) { return $null }
            return [pscustomobject]@{
                Root = $e; Button = $pick
                Buttons = @($buttons | ForEach-Object { [ordered]@{ name = $_.Current.Name; automation_id = $_.Current.AutomationId } })
            }
        }
        if ([int64]$e.Current.NativeWindowHandle -eq $topHwnd) { break }
        $e = $walker.GetParent($e)
    }
    $null
}

# ---------------------------------------------------------------- actions

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

# InvokePattern on the element or its nearest ancestor that has one, inside the window $TopHwnd;
# with $Needle, an ancestor is used only while its subtree holds this toast only.
function Invoke-UiaElement($Element, [int64]$TopHwnd, [string]$Needle = '') {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $e = $Element
    try {
        for ($i = 0; $i -lt 12 -and $null -ne $e; $i++) {
            if (Test-DeniedButton $e.Current.Name $e.Current.AutomationId) {
                return [ordered]@{ mode = 'uia-invoke'; ok = $false; why = 'reached an element that says clear or all: nothing invoked' }
            }
            if ($Needle -and $i -gt 0 -and -not (Test-OnlyThisToast (Get-SubtreeNames $e) $Needle @(Get-Buttons $e | Where-Object { $_.Current.Name -eq 'Open' }).Count)) {
                return [ordered]@{ mode = 'uia-invoke'; ok = $false; why = 'the nearest invokable element is shared with another toast: nothing invoked' }
            }
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

# A synthesized click at (x, y) on the window $Top ({hwnd, pid, class}): UIA must put the point
# on that window, and ClickAt re-checks the window under the point right before injecting. While
# the owner is using the PC it waits 2 s and tries again, up to 5 times, then gives up.
function Invoke-SafeClick([int]$X, [int]$Y, $Top) {
    for ($try = 1; $try -le 5; $try++) {
        $at = Get-TopAtPoint $X $Y
        if ($at -ne [int64]$Top.hwnd) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = "the point ($X, $Y) is on another window ($at), not on $($Top.hwnd)"; point = "$X,$Y" } }
        $code = [SpikeUi.Win]::ClickAt($X, $Y, [int64]$Top.pid, [string]$Top.class)
        if ($code -eq 2) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $true; point = "$X,$Y"; tries = $try } }
        if ($code -eq -1) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'the window under the point changed before the click: nothing clicked'; point = "$X,$Y" } }
        if ($code -eq -3) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'SendInput injected fewer than 2 events'; point = "$X,$Y" } }
        Start-Sleep -Seconds 2   # -2: the owner is using the PC
    }
    [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'owner active: the owner gave input within 2 s of each of 5 tries'; owner_active = $true }
}

# A synthesized click on the centre of the element, which must be on the window $Top.
function Click-ElementCentre($Element, $Top) {
    try {
        [SpikeUi.Win]::PerMonitorAware()
        $r = $Element.Current.BoundingRectangle
        if ($r.IsEmpty -or $r.Width -le 0 -or $r.Height -le 0) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'no bounding rectangle' } }
        Invoke-SafeClick ([int]($r.X + $r.Width / 2)) ([int]($r.Y + $r.Height / 2)) $Top
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
        $top = [ordered]@{ hwnd = $Hwnd; pid = [SpikeUi.Win]::PidOf($Hwnd); class = [SpikeUi.Win]::ClassOf($Hwnd) }
        foreach ($fy in 0.5, 0.65, 0.8, 0.92, 0.4) {
            foreach ($fx in 0.5, 0.3, 0.7, 0.1, 0.9) {
                $x = [int]($r.X + $r.Width * $fx); $y = [int]($r.Y + $r.Height * $fy)
                if ((Get-TopAtPoint $x $y) -eq $Hwnd) { return Invoke-SafeClick $x $y $top }
            }
        }
        [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'no visible point of the window' }
    } catch {
        [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = $_.Exception.Message }
    }
}

# Key presses, held back while the owner is using the PC (5 tries, 2 s apart). $true when sent.
function Send-Keys([int[]]$Vk) {
    for ($try = 1; $try -le 5; $try++) {
        if ([SpikeUi.Win]::Keys([uint16[]]$Vk) -gt 0) { return $true }
        Start-Sleep -Seconds 2
    }
    $false
}

function Get-ForegroundNow {
    if (-not (Initialize-SpikeUi)) { return $null }
    $f = [SpikeUi.Win]::Foreground()
    [ordered]@{ hwnd = $f[0]; pid = [int]$f[1]; process = $(if ($f[1] -ne 0) { Get-ProcName ([int]$f[1]) } else { $null }); class = $(if ($f[0] -ne 0) { [SpikeUi.Win]::ClassOf($f[0]) } else { $null }) }
}

# $true locked, $false unlocked, $null unknown.
function Get-SessionLocked {
    if (-not (Initialize-SpikeUi)) { return $null }
    $f = [SpikeUi.Win]::SessionFlags()
    if ($f -eq 0) { return $true }
    if ($f -eq 1) { return $false }
    $null
}

function Get-IdleSeconds {
    if (-not (Initialize-SpikeUi)) { return $null }
    $ms = [SpikeUi.Win]::IdleMs()
    if ($ms -lt 0) { return $null }
    [math]::Round($ms / 1000.0, 1)
}

# Power evidence, read only: the execution state in force (ES_SYSTEM_REQUIRED 0x1, ES_DISPLAY_
# REQUIRED 0x2, ES_AWAYMODE_REQUIRED 0x40) and the idle timer (SystemPowerInformation).
function Get-PowerEvidence {
    if (-not (Initialize-SpikeUi)) { return $null }
    $es = [SpikeUi.Win]::ExecutionState()
    $pi = [SpikeUi.Win]::PowerInfo()
    [ordered]@{
        at = (Get-Date).ToUniversalTime().ToString('o')
        execution_state = $(if ($es -ge 0) { '0x{0:x}' -f $es } else { "error 0x{0:x}" -f (-$es) })
        system_required = $(if ($es -ge 0) { [bool]($es -band 1) } else { $null })
        idle_timer = [ordered]@{ status = $pi[0]; max_idleness_allowed_s = $pi[1]; idleness = $pi[2]; time_remaining_s = $pi[3] }
        idle_s = Get-IdleSeconds
    }
}

# The notification centre the script opened: the window holding the needle, else the new window
# that took the foreground after Win+N; 0 when neither (then nothing is sent to close it).
function Get-CentreHwnd($Watch, $Before) {
    if ($null -ne $Watch.Hit) { return [int64]$Watch.Hit.TopInfo.hwnd }
    $fg = Get-ForegroundNow
    if ($null -ne $fg -and $fg.hwnd -ne 0 -and -not $Before.ContainsKey([int64]$fg.hwnd)) { return [int64]$fg.hwnd }
    [int64]0
}

function Test-TopPresent([int64]$Hwnd) {
    @(Get-UiaTop | Where-Object { [int64]$_.Current.NativeWindowHandle -eq $Hwnd }).Count -gt 0
}

# Closes the centre the script opened: Esc only while that window is the foreground, else Win+N
# (which toggles it) only while it is still shown. Nothing is sent for a centre not identified.
function Close-NotificationCentre([int64]$CentreHwnd) {
    if ($CentreHwnd -eq 0) { return 'not identified: nothing sent' }
    for ($k = 0; $k -lt 2; $k++) {
        if (-not (Test-TopPresent $CentreHwnd)) { return 'closed' }
        $fg = Get-ForegroundNow
        if ($null -ne $fg -and [int64]$fg.hwnd -eq $CentreHwnd) { [void](Send-Keys @(0x1B)) }   # Esc
        else { [void](Send-Keys @(0x5B, 0x4E)) }                                                 # Win+N
        Start-Sleep -Milliseconds 800
    }
    $(if (Test-TopPresent $CentreHwnd) { 'still open' } else { 'closed' })
}

# Win+N, then whether an entry holding $Needle is listed; then the centre is closed again.
function Test-InNotificationCentre([string]$Needle) {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ method = 'none'; why = "UI Automation not available: $($script:Uia.error)" } }
    $before = Get-UiaSnapshot
    if (-not (Send-Keys @(0x5B, 0x4E))) { return [ordered]@{ method = 'none'; why = 'owner active: Win+N not sent' } }
    $w = Watch-Needle -Needle $Needle -Before $before -Seconds 5 -UntilFound
    $centre = Get-CentreHwnd $w $before
    [ordered]@{
        method = 'uia (Win+N sent by the script; closed with Esc or Win+N)'; listed = ($null -ne $w.Hit)
        centre_identified = ($centre -ne 0); host = $w.Record.host; closed_after = (Close-NotificationCentre $centre)
    }
}

# Dismisses the notification-centre entry holding $Needle with that entry's own dismiss button:
# Win+N, find the entry, hover over it (the button may show only then), find a dismiss or close
# button scoped to this toast (Find-ToastButton; never "clear" or "all"), invoke it (or click
# it). When no such button is found, nothing is pressed and the caller asks the owner.
function Dismiss-InNotificationCentre([string]$Needle) {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ method = 'none'; ok = $false; why = "UI Automation not available: $($script:Uia.error)" } }
    $before = Get-UiaSnapshot
    if (-not (Send-Keys @(0x5B, 0x4E))) { return [ordered]@{ method = 'none'; ok = $false; why = 'owner active: Win+N not sent' } }
    $w = Watch-Needle -Needle $Needle -Before $before -Seconds 5 -UntilFound
    $centre = Get-CentreHwnd $w $before
    $rec = [ordered]@{ method = 'uia (Win+N, hover, this entry''s own dismiss button)'; found_entry = ($null -ne $w.Hit); ok = $false; buttons = @(); used = $null; hovered = $false }
    $restore = $null
    try {
        if ($null -ne $w.Hit) {
            $r = $w.Hit.Element.Current.BoundingRectangle
            if (-not $r.IsEmpty) {
                $restore = [SpikeUi.Win]::MoveTo([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
                $rec.hovered = $null -ne $restore
                Start-Sleep -Milliseconds 700
            }
            $found = Find-ToastButton $w.Hit $Needle { param($n, $a) Test-DismissButton $n $a }
            if ($null -ne $found) {
                $rec.buttons = $found.Buttons
                $rec.used = Get-ElementInfo $found.Button
                $how = Invoke-UiaElement $found.Button ([int64]$found.Button.Current.NativeWindowHandle)
                if (-not $how.ok) { $how = Click-ElementCentre $found.Button $w.Hit.TopInfo }
                $rec.how = $how
                $rec.ok = [bool]$how.ok
            } else {
                $rec.why = 'no dismiss or close button scoped to this toast: nothing pressed'
            }
        }
    } catch {
        $rec.error = $_.Exception.Message
    } finally {
        if ($null -ne $restore) { [SpikeUi.Win]::MoveBack($restore[0], $restore[1]) }
        $rec.closed_after = Close-NotificationCentre $centre
    }
    $rec
}
