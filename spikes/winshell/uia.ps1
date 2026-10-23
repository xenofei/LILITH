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

Fix round 2: the scope never goes past the toast's own notification list item, and every Name in
it must be one of that toast's own (Test-ScopeNames, Select-ToastTarget); each click try re-reads
the target's rectangle (stable over 100 ms, on screen), requires UIA's ElementFromPoint at the point
to be the target or inside it, and checks that the per-monitor DPI context applied; Win is always
released after a partial key injection; hands-off pop-ups (popup.ps1) warn 10 s ahead and count
down. Known limit: owner input within 400 ms after one of the script's own injections is taken for
the script's own (GetLastInputInfo keeps only the latest input time).
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

        [DllImport("user32.dll")] static extern IntPtr GetThreadDpiAwarenessContext();
        [DllImport("user32.dll")] static extern bool AreDpiAwarenessContextsEqual(IntPtr a, IntPtr b);
        [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
        [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int value, int size);

        // Physical pixels for this thread, as UI Automation reports rectangles. True only when the
        // thread's context is per-monitor-aware v2 afterwards (the return value is checked).
        public static bool PerMonitorAware() {
            try {
                SetThreadDpiAwarenessContext(new IntPtr(-4));
                return AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), new IntPtr(-4));
            } catch (EntryPointNotFoundException) { return false; }
        }

        // Visible and not cloaked (a closed notification centre may stay listed, cloaked).
        public static bool Shown(long hwnd) {
            IntPtr h = new IntPtr(hwnd);
            if (!IsWindowVisible(h)) return false;
            int cloaked = 0;
            if (DwmGetWindowAttribute(h, 14, out cloaked, 4) != 0) return true;   // DWMWA_CLOAKED
            return cloaked == 0;
        }

        // The virtual desktop in physical pixels: {x, y, width, height}.
        public static int[] VirtualDesk() { PerMonitorAware(); return new int[] { GetSystemMetrics(76), GetSystemMetrics(77), GetSystemMetrics(78), GetSystemMetrics(79) }; }

        // Where the cursor was right after the last click's SendInput, before it was put back.
        public static int LandedX, LandedY;

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
            if (!PerMonitorAware()) return -4;
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
            // Injected input is processed asynchronously: the landing point is read after the wait,
            // before the cursor is put back (review m4 of fix round 2).
            Thread.Sleep(150);
            POINT landed; GetPhysicalCursorPos(out landed); LandedX = landed.X; LandedY = landed.Y;
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
        // SendInput call. Returns the number injected when all were, -2 when the owner gave input
        // in the last 2 s, -3 when only some were: then every key-up is sent again, in a finally,
        // so that Win is never left held down.
        public static int Keys(ushort[] vks) {
            if (OwnerIdleMs() < 2000) return -2;
            uint sent = 0;
            try {
                sent = KeysOnce(vks);
            } finally {
                if (sent != (uint)(vks.Length * 2)) KeyUps(vks);
            }
            return sent == (uint)(vks.Length * 2) ? (int)sent : -3;
        }

        static void KeyUps(ushort[] vks) {
            INPUT[] u = new INPUT[vks.Length];
            for (int k = 0; k < vks.Length; k++) {
                ushort vk = vks[vks.Length - 1 - k];
                u[k].type = 1; u[k].u.ki.wVk = vk;
                u[k].u.ki.dwFlags = 0x0002u | (vk == 0x5B ? 0x0001u : 0u);
            }
            SendInput((uint)u.Length, u, Marshal.SizeOf(typeof(INPUT)));
        }

        static uint KeysOnce(ushort[] vks) {
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
            return n;
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

# The hands-off pop-ups this script started (Start-HandsOff), each with its pinned Process (an
# open handle, so its pid can't be reused under it) and its state files.
$script:Popups = @()
# Every pop-up's record (shown, took the foreground), for the results (review m1 of fix round 2).
$script:HandsOffLog = @()

# ---------------------------------------------------------------- the processes the scripts start

# The process this script has just started, as a Process object that holds an open handle from
# now on (review N2-C1): a Process from Get-Process -Id holds none, so a later HasExited or
# Kill() would open whatever process has that pid by then. It must have started no earlier than
# $NotBefore (2 s slack) and, when given, run $ExpectedPath; otherwise $null. This is the only
# place a pid is turned into a Process the scripts may later check or end
# (tests/kill_scope.rs checks that).
function Get-PinnedProcess([int]$ProcessId, [string]$ExpectedPath, [datetime]$NotBefore) {
    try {
        $p = Get-Process -Id $ProcessId -ErrorAction Stop   # pin-lookup: right after the start
        [void]$p.Handle
        if ($ExpectedPath -and $p.Path -ne $ExpectedPath) { return $null }
        if ($p.StartTime -lt $NotBefore.AddSeconds(-2)) { return $null }
        $p
    } catch { $null }
}

# Ends a pinned process if it is still running (a no-op for one that has exited).
function Stop-PinnedProcess($Process) {
    if ($null -eq $Process) { return }
    try { if (-not $Process.HasExited) { $Process.Kill() } } catch { }
}

# The pids of this script's pop-ups that are still running (a dead pop-up's pid may be reused).
function Get-PopupPids {
    $script:Popups = @($script:Popups | Where-Object { $null -ne $_.Process -and -not $_.Process.HasExited })
    @($script:Popups | ForEach-Object { [int]$_.Process.Id })
}

# Owner direction 2026-10-23: before anything that needs hands off the keyboard and mouse, a
# topmost pop-up warns at least 10 s ahead, then counts down until the PC may be used again, and
# says when it's done. The pop-up (popup.ps1, in Windows PowerShell 5.1 for WinForms) is started
# detached, with no console, through winshell-tool spawn, and pinned at once. It never takes the
# focus (WS_EX_NOACTIVATE, ShowWithoutActivation; topmost through SetWindowPos with
# SWP_NOACTIVATE), is click-through (WS_EX_LAYERED | WS_EX_TRANSPARENT, so WindowFromPoint and
# UIA's FromPoint look through it), sits at the top centre of the primary screen, away from the
# toasts at the bottom right, and its pid is skipped by every window search and foreground check
# while it runs. Without winshell-tool (inside the Sandbox) there is no pop-up.
#
# The handshake (fix round 3): the pop-up touches <state>.shown once it is on screen; this waits
# for that (up to 8 s), then writes <state>.start with the hands-off start (epoch ms, 10 s from
# then), and waits until that moment. The warning therefore lasts at least 10 s on screen, and
# the countdown runs from the real start. With -NoWarnWait (a restarted stay-awake wait) it
# doesn't wait. Whether the pop-up showed and whether it took the foreground is recorded.
function Start-HandsOff([string]$What, [int]$Seconds, [switch]$NoWarnWait) {
    Show-HandsOff $What
    $spawned = Get-Date
    $startAt = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + 10000
    $h = [pscustomobject]@{ Process = $null; StateFile = $null; Done = $false; Record = [ordered]@{ what = $What; shown = $false; took_foreground = $null } }
    $tool = Get-Variable -Name ToolExe -Scope Script -ValueOnly -ErrorAction SilentlyContinue
    $popup = Join-Path $PSScriptRoot 'popup.ps1'
    if ($tool -and (Test-Path -LiteralPath $tool) -and (Test-Path -LiteralPath $popup)) {
        try {
            $h.StateFile = Join-Path ([IO.Path]::GetTempPath()) ('winshell-popup-' + [guid]::NewGuid().ToString('N') + '.done')
            $ps = Join-Path $env:windir 'System32\WindowsPowerShell\v1.0\powershell.exe'
            $r = Invoke-PopupSpawn $tool @($ps, '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $popup, '-What', $What, '-StartAtMs', "$startAt", '-RunSec', "$Seconds", '-StateFile', $h.StateFile, '-ParentPid', "$PID")
            if ($null -ne $r -and $r.PSObject.Properties['pid']) {
                $h.Process = Get-PinnedProcess ([int]$r.pid) $ps $spawned
                if ($null -ne $h.Process) { $script:Popups = @($script:Popups) + @($h) }
            }
        } catch { }
    }
    if ($null -ne $h.Process -and -not $NoWarnWait) {
        $deadline = (Get-Date).AddSeconds(8)
        while ((Get-Date) -lt $deadline -and -not (Test-Path -LiteralPath "$($h.StateFile).shown") -and -not $h.Process.HasExited) { Start-Sleep -Milliseconds 100 }
        $h.Record.shown = Test-Path -LiteralPath "$($h.StateFile).shown"
        if ($h.Record.shown) {
            $startAt = [math]::Max($startAt, [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + 10000)
            try { Set-Content -LiteralPath "$($h.StateFile).start" -Value "$startAt" } catch { }
            $fg = Get-ForegroundNow
            $h.Record.took_foreground = ($null -ne $fg -and $fg.pid -eq $h.Process.Id)
        }
    }
    $h.Record.popup_started = ($null -ne $h.Process)
    $script:HandsOffLog = @($script:HandsOffLog) + @($h.Record)
    if (-not $NoWarnWait) {
        $wait = $startAt - [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        if ($wait -gt 0) { Start-Sleep -Milliseconds $wait }
    }
    $h
}

# winshell-tool spawn of the pop-up: its one JSON line. A seam: under -Simulate,
# tests/sim-fakes.ps1 replaces it, so no pop-up is shown.
function Invoke-PopupSpawn([string]$Tool, [string[]]$SpawnArgs) {
    $out = & $Tool spawn '--' @SpawnArgs
    ($out | Out-String) | ConvertFrom-Json
}

# The stretch is over: the pop-up says "done" for 5 s and closes itself.
function Stop-HandsOff($Handle) {
    if ($null -eq $Handle -or -not $Handle.StateFile) { return }
    $Handle.Done = $true
    try { Set-Content -LiteralPath $Handle.StateFile -Value 'done' } catch { }
}

# Ends the pop-ups this script started that are still open and were never told they were done
# (the others close themselves), through their pinned Process objects only (review N2-C1).
function Stop-AllPopups {
    foreach ($h in @($script:Popups)) {
        if (-not $h.Done) { Stop-PinnedProcess $h.Process }
    }
    $script:Popups = @()
# Every pop-up's record (shown, took the foreground), for the results (review m1 of fix round 2).
$script:HandsOffLog = @()
}

function Initialize-SpikeUi {
    if ($script:Uia.ready) { return $true }
    if ($null -ne $script:Uia.error) { return $false }
    try {
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase
        if (-not ('SpikeUi.Win' -as [type])) { Add-Type -TypeDefinition $script:SpikeUiSource }
        $script:Uia.per_monitor_dpi = [SpikeUi.Win]::PerMonitorAware()
        $script:Uia.ready = $true
    } catch {
        $script:Uia.error = $_.Exception.Message
    }
    $script:Uia.ready
}

# The one-line warning before each stretch of synthesized input (Start-HandsOff adds the pop-up).
function Show-HandsOff([string]$What) {
    Write-Host "HANDS OFF: the script $What now. Ctrl+C in this terminal stops it." -ForegroundColor Yellow
}


function Get-ProcName([int]$ProcessId) {
    if (-not $script:ProcNames.ContainsKey($ProcessId)) {
        $n = $null
        try { $n = (Get-Process -Id $ProcessId -ErrorAction Stop).ProcessName } catch { }   # read-only pid lookup: the name only
        $script:ProcNames[$ProcessId] = $n
    }
    $script:ProcNames[$ProcessId]
}

# The visible top-level windows, in z-order, without this script's own pop-ups.
function Get-UiaTop {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $live = @(Get-PopupPids)
    @($root.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition) |
        Where-Object { $_.Current.ProcessId -notin $live })
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

# The top-level windows now: handle -> whether it was shown (visible and not cloaked). A window
# that was listed but cloaked (a closed notification centre) counts as new once it shows.
function Get-UiaSnapshot {
    $s = @{}
    if (-not (Initialize-SpikeUi)) { return $s }
    foreach ($t in Get-UiaTop) { try { $h = [int64]$t.Current.NativeWindowHandle; $s[$h] = [SpikeUi.Win]::Shown($h) } catch { } }
    $s
}

function Test-WasShown($Before, [int64]$Hwnd) { $Before.ContainsKey($Hwnd) -and $Before[$Hwnd] -eq $true }

function Add-UiaHost($Info) {
    foreach ($h in $script:Uia.hosts) { if ($h.process -eq $Info.process -and $h.class -eq $Info.class) { return } }
    $script:Uia.hosts = @($script:Uia.hosts) + @([ordered]@{ process = $Info.process; class = $Info.class; name = $Info.name })
}

function Test-KnownHost($Info) {
    foreach ($h in $script:Uia.hosts) { if ($h.process -eq $Info.process -and $h.class -eq $Info.class) { return $true } }
    $false
}

# An element whose Name can't be read makes the search unknown, not "absent" (review m13).
function Find-NeedleUnder($Top, [string]$Needle) {
    if ("$($Top.Current.Name)".Contains($Needle)) { return $Top }
    foreach ($e in @($Top.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition))) {
        try { if ("$($e.Current.Name)".Contains($Needle)) { return $e } } catch { $script:UiaSearchFailed = $true }
    }
    $null
}

# The element whose Name contains $Needle, in a shown top-level window that is new since $Before
# (or was cloaked then) or is a host learned earlier; $null when there is none. A window whose
# search threw sets $script:UiaSearchFailed, so that a poll without a hit counts as unknown.
function Find-Needle([string]$Needle, $Before) {
    $script:UiaSearchFailed = $false
    foreach ($t in Get-UiaTop) {
        try {
            $hwnd = [int64]$t.Current.NativeWindowHandle
            if (-not [SpikeUi.Win]::Shown($hwnd)) { continue }
            $info = $null
            if (Test-WasShown $Before $hwnd) {
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

# Whether every non-empty Name in a scope is one of this spike toast's own (review N-I3): one
# that holds its nonce, one of $Allowed (its title, the app name, "Open", ...), or one matching
# $Patterns (its dismiss or close button, its progress strings). Anything else, such as another
# app's notification, makes the scope foreign. Returns {ok, unknown} (unknown is a count only, so
# no other app's text is recorded). Pure; tested.
function Test-ScopeNames([string[]]$Names, [string]$Needle, [string[]]$Allowed, [string[]]$Patterns) {
    $unknown = @()
    $all = @($Names)
    for ($i = 0; $i -lt $all.Count; $i++) {
        $n = "$($all[$i])"
        if (-not $n) { continue }
        if ($n.Contains($Needle)) { if (@([regex]::Matches($n, '#[0-9a-f]{6}') | Where-Object { $_.Value -ne $Needle }).Count -gt 0) { $unknown += $i }; continue }
        if (@($Allowed | Where-Object { $_ -and $n -eq $_ }).Count -gt 0) { continue }
        if (@($Patterns | Where-Object { $_ -and $n -match $_ }).Count -gt 0) { continue }
        $unknown += $i
    }
    # Only the count and the positions come back, never the Names (fix round 3 ruling 5): the
    # caller records the unknown elements' AutomationIds and control types instead.
    [ordered]@{ ok = ($unknown.Count -eq 0); unknown = $unknown.Count; unknown_index = $unknown }
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

# The level to act on, from the hit upward (review N-I3, m11). $Levels are records
# {depth, list_item, names, rect, candidates = [{name, automation_id, rect, ...}]}, nearest first.
# The first level with a candidate that $Accept takes (and that isn't denied) is used, but only
# when its subtree is this toast's alone (Test-OnlyThisToast and Test-ScopeNames) and the
# candidate lies inside it; the climb never goes past the nearest notification list item.
# Returns {ok, depth, candidate} or {ok = $false, why}. Pure; tested with fake levels.
function Select-ToastTarget($Levels, [string]$Needle, [string[]]$Allowed, [string[]]$Patterns, [scriptblock]$Accept) {
    foreach ($lv in @($Levels)) {
        $pick = @(@($lv['candidates']) | Where-Object { $null -ne $_ -and (& $Accept $_['name'] $_['automation_id']) -and -not (Test-DeniedButton $_['name'] $_['automation_id']) }) | Select-Object -First 1
        if ($null -ne $pick) {
            $open = @(@($lv['candidates']) | Where-Object { $null -ne $_ -and $_['name'] -eq 'Open' }).Count
            if (@(@($lv['names']) | Where-Object { $_ -and $_.Contains($Needle) }).Count -eq 0) { return [ordered]@{ ok = $false; depth = $lv['depth']; why = 'the scope does not hold this toast''s text: nothing pressed' } }
            if (-not (Test-OnlyThisToast $lv['names'] $Needle $open)) { return [ordered]@{ ok = $false; depth = $lv['depth']; why = 'the scope holds another spike toast or a second Open: nothing pressed' } }
            $scope = Test-ScopeNames $lv['names'] $Needle $Allowed $Patterns
            if (-not $scope.ok) {
                $els = @($lv['elements'])
                $ctl = @(@($scope.unknown_index) | Select-Object -First 8 | ForEach-Object { if ($_ -lt $els.Count -and $null -ne $els[$_]) { [ordered]@{ automation_id = $els[$_]['automation_id']; control_type = $els[$_]['control']; class = $els[$_]['class'] } } })
                return [ordered]@{ ok = $false; depth = $lv['depth']; why = "the scope holds $($scope.unknown) name(s) that are not this toast's own: nothing pressed"; unknown_controls = $ctl }
            }
            if ($null -ne $pick['rect'] -and $null -ne $lv['rect'] -and -not (Test-RectInside $pick['rect'] $lv['rect'])) { return [ordered]@{ ok = $false; depth = $lv['depth']; why = 'the button lies outside the toast: nothing pressed' } }
            return [ordered]@{ ok = $true; depth = $lv['depth']; candidate = $pick }
        }
        if ($lv['list_item'] -eq $true) { return [ordered]@{ ok = $false; depth = $lv['depth']; why = 'no such button inside this toast''s own list item' } }
    }
    [ordered]@{ ok = $false; why = 'no such button near this toast' }
}

function Get-SubtreeNames($Element) { @(Get-SubtreeElements $Element | ForEach-Object { $_['name'] }) }

# {name, automation_id, control, class} for every element of a subtree; an unreadable one is
# named '<unreadable>', which no scope accepts.
function Get-SubtreeElements($Element) {
    @($Element.FindAll([System.Windows.Automation.TreeScope]::Subtree, [System.Windows.Automation.Condition]::TrueCondition) |
        ForEach-Object {
            try { $c = $_.Current; [ordered]@{ name = $c.Name; automation_id = $c.AutomationId; control = $c.ControlType.ProgrammaticName; class = $c.ClassName } }
            catch { [ordered]@{ name = '<unreadable>'; automation_id = $null; control = $null; class = $null } }
        })
}

function Get-Buttons($Element) {
    $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    @($Element.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond))
}

# The level records for Select-ToastTarget, from the hit upward: up to 8 levels, never past the
# nearest list item or the hit's top-level window. -Invoke makes each level's own element its
# candidate when it supports InvokePattern; otherwise the candidates are its buttons.
function Get-ToastLevels($Hit, [switch]$Invoke) {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $topHwnd = [int64]$Hit.TopInfo.hwnd
    $levels = @()
    $e = $Hit.Element
    for ($i = 0; $i -lt 8 -and $null -ne $e; $i++) {
        $c = $e.Current
        $isItem = $c.ControlType -eq [System.Windows.Automation.ControlType]::ListItem
        $cands = @()
        if ($Invoke) {
            $pat = $null
            if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pat)) {
                $cands = @([ordered]@{ name = $c.Name; automation_id = $c.AutomationId; rect = $null; element = $e; pattern = $pat })
            }
        } else {
            $cands = @(Get-Buttons $e | ForEach-Object { [ordered]@{ name = $_.Current.Name; automation_id = $_.Current.AutomationId; rect = $_.Current.BoundingRectangle; element = $_ } })
        }
        $els = @(Get-SubtreeElements $e)
        $levels += [ordered]@{ depth = $i; list_item = $isItem; names = @($els | ForEach-Object { $_['name'] }); elements = $els; rect = $c.BoundingRectangle; candidates = $cands }
        if ($isItem -or [int64]$c.NativeWindowHandle -eq $topHwnd) { break }
        $e = $walker.GetParent($e)
    }
    $levels
}

# This toast's own button that $Accept takes, or $null (then nothing is pressed); with the why.
function Find-ToastButton($Hit, [string]$Needle, [scriptblock]$Accept, [string[]]$Allowed, [string[]]$Patterns) {
    $sel = Select-ToastTarget (Get-ToastLevels $Hit) $Needle $Allowed $Patterns $Accept
    [pscustomobject]@{ Ok = [bool]$sel.ok; Button = $(if ($sel.ok) { $sel.candidate['element'] } else { $null }); Why = (P2 $sel 'why'); Depth = (P2 $sel 'depth'); UnknownControls = (P2 $sel 'unknown_controls') }
}

function P2($Obj, [string]$Name) { if ($null -ne $Obj -and $Obj.Contains($Name)) { $Obj[$Name] } else { $null } }

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

# Whether the element UIA finds at (x, y) (IUIAutomation::ElementFromPoint) is $Target or one of
# its descendants.
function Test-PointOnElement([int]$X, [int]$Y, $Target) {
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $e = [System.Windows.Automation.AutomationElement]::FromPoint((New-Object System.Windows.Point($X, $Y)))
    for ($i = 0; $i -lt 40 -and $null -ne $e; $i++) {
        if ([System.Windows.Automation.Automation]::Compare($e, $Target)) { return $true }
        $e = $walker.GetParent($e)
    }
    $false
}

# InvokePattern on the hit or its nearest invokable ancestor, scoped as Select-ToastTarget says
# (this toast's own names only, never past its list item).
function Invoke-UiaElement($Hit, [string]$Needle, [string[]]$Allowed, [string[]]$Patterns) {
    try {
        $sel = Select-ToastTarget (Get-ToastLevels $Hit -Invoke) $Needle $Allowed $Patterns { param($n, $a) $true }
        if (-not $sel.ok) { return [ordered]@{ mode = 'uia-invoke'; ok = $false; refused = $true; why = $sel.why; unknown_controls = (P2 $sel 'unknown_controls') } }
        $info = Get-ElementInfo $sel.candidate['element']
        $sel.candidate['pattern'].Invoke()
        [ordered]@{ mode = 'uia-invoke'; ok = $true; invoked = $info; levels_up = $sel.depth }
    } catch {
        [ordered]@{ mode = 'uia-invoke'; ok = $false; why = $_.Exception.Message }
    }
}

# InvokePattern on one element itself (a button already chosen by Find-ToastButton).
function Invoke-UiaButton($Button) {
    try {
        $pat = $null
        if ($Button.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pat)) {
            $info = Get-ElementInfo $Button
            $pat.Invoke()
            return [ordered]@{ mode = 'uia-invoke'; ok = $true; invoked = $info }
        }
        [ordered]@{ mode = 'uia-invoke'; ok = $false; why = 'the button has no InvokePattern' }
    } catch { [ordered]@{ mode = 'uia-invoke'; ok = $false; why = $_.Exception.Message } }
}

function Get-RectNow($Element) {
    try { $c = $Element.Current; [pscustomobject]@{ R = $c.BoundingRectangle; Off = $c.IsOffscreen } } catch { $null }
}

# A synthesized click on $Target, which lies on the window $Top ({hwnd, pid, class}). On every
# try (review N-I4): the per-monitor DPI context must apply; the target's rectangle is read
# twice 100 ms apart and must be unchanged, non-empty and on screen; the point (the centre, or
# the first of $Fractions that UIA puts on the target) must be the target or one of its
# descendants by UIA's ElementFromPoint; then ClickAt re-checks the window under the point and
# holds back while the owner is using the PC (5 tries, 2 s apart, then the owner's turn). The
# point where the cursor landed is recorded (review m7).
function Invoke-SafeClick($Target, $Top, $Fractions = @(, @(0.5, 0.5))) {
    $why = 'no try made'
    for ($try = 1; $try -le 5; $try++) {
        if (-not [SpikeUi.Win]::PerMonitorAware()) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'the per-monitor DPI context did not apply: nothing clicked' } }
        $a = Get-RectNow $Target; Start-Sleep -Milliseconds 100; $b = Get-RectNow $Target
        if ($null -eq $a -or $null -eq $b -or $b.R.IsEmpty -or $b.Off -or $a.R -ne $b.R) {
            $why = 'the target was moving, hidden or gone'
            Start-Sleep -Milliseconds 400
            continue
        }
        $r = $b.R
        $pt = $null
        foreach ($f in @($Fractions)) {
            $x = [int]($r.X + $r.Width * $f[0]); $y = [int]($r.Y + $r.Height * $f[1])
            if (Test-PointOnElement $x $y $Target) { $pt = @($x, $y); break }
        }
        if ($null -eq $pt) { $why = 'UIA puts no candidate point on the target'; Start-Sleep -Milliseconds 400; continue }
        $code = [SpikeUi.Win]::ClickAt($pt[0], $pt[1], [int64]$Top.pid, [string]$Top.class)
        if ($code -eq 2) {
            $lx = [SpikeUi.Win]::LandedX; $ly = [SpikeUi.Win]::LandedY
            $miss = [math]::Max([math]::Abs($lx - $pt[0]), [math]::Abs($ly - $pt[1]))
            return [ordered]@{ mode = 'synthetic-mouse'; ok = $true; point = "$($pt[0]),$($pt[1])"; landed = "$lx,$ly"; missed_by_px = $miss; landed_off = ($miss -gt 1)
                               virtual_desk = (@([SpikeUi.Win]::VirtualDesk()) -join ','); tries = $try }
        }
        if ($code -eq -1) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'the window under the point changed before the click: nothing clicked' } }
        if ($code -eq -3) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'SendInput injected fewer than 2 events' } }
        if ($code -eq -4) { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'the per-monitor DPI context did not apply: nothing clicked' } }
        $why = 'owner active: the owner gave input within 2 s of each try'
        Start-Sleep -Seconds 2   # -2: the owner is using the PC; the next try reads everything again
    }
    [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = $why; owner_active = $why.StartsWith('owner active') }
}

# A synthesized click on the centre of the element, which must be on the window $Top.
function Click-ElementCentre($Element, $Top) {
    try { Invoke-SafeClick $Element $Top } catch { [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = $_.Exception.Message } }
}

# A synthesized click on a visible part of the window $Hwnd's lower area (never its title bar,
# so never its close button): the first point of a grid that UIA puts on that window.
function Click-WindowVisible([int64]$Hwnd) {
    try {
        $el = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$Hwnd)
        $top = [ordered]@{ hwnd = $Hwnd; pid = [SpikeUi.Win]::PidOf($Hwnd); class = [SpikeUi.Win]::ClassOf($Hwnd) }
        $grid = @(foreach ($fy in 0.5, 0.65, 0.8, 0.92, 0.4) { foreach ($fx in 0.5, 0.3, 0.7, 0.1, 0.9) { , @($fx, $fy) } })
        Invoke-SafeClick $el $top $grid
    } catch {
        [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = $_.Exception.Message }
    }
}

# Key presses, held back while the owner is using the PC (5 tries, 2 s apart). $true only when
# every key went down and up; a partial injection is released by Keys itself (review m6).
function Send-Keys([int[]]$Vk) {
    for ($try = 1; $try -le 5; $try++) {
        $n = [SpikeUi.Win]::Keys([uint16[]]$Vk)
        if ($n -eq 2 * $Vk.Count) { return $true }
        if ($n -eq -3) { return $false }
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

# The notification centre the script opened: the window holding the needle, else the window
# that took the foreground after Win+N and wasn't shown before (it may have been listed
# cloaked); 0 when neither (then nothing is sent to close it).
function Get-CentreHwnd($Watch, $Before) {
    if ($null -ne $Watch.Hit) { return [int64]$Watch.Hit.TopInfo.hwnd }
    $fg = Get-ForegroundNow
    if ($null -ne $fg -and $fg.hwnd -ne 0 -and $fg.pid -notin @(Get-PopupPids) -and -not (Test-WasShown $Before ([int64]$fg.hwnd))) { return [int64]$fg.hwnd }
    [int64]0
}

# Shown: visible and not cloaked (review m5), not merely listed by UIA.
function Test-TopPresent([int64]$Hwnd) { [SpikeUi.Win]::Shown($Hwnd) }

# Closes the centre the script opened: Esc only while that window is the foreground, else Win+N
# (which toggles it) only while it is confirmed shown. Nothing is sent for a centre not identified.
function Close-NotificationCentre([int64]$CentreHwnd) {
    if ($CentreHwnd -eq 0) { return 'not identified: nothing sent' }
    for ($k = 0; $k -lt 2; $k++) {
        if (-not (Test-TopPresent $CentreHwnd)) { return 'closed' }
        $fg = Get-ForegroundNow
        if ($null -ne $fg -and [int64]$fg.hwnd -eq $CentreHwnd) { [void](Send-Keys @(0x1B)) }   # Esc
        elseif (Test-TopPresent $CentreHwnd) { [void](Send-Keys @(0x5B, 0x4E)) }                 # Win+N
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
# button inside this toast's own list item whose scope holds this toast's names only
# (Find-ToastButton), invoke it (or click it). When the hover is held back (the owner is using
# the mouse) nothing is searched, and when no such button is found nothing is pressed: the
# caller then asks the owner. $Allowed and $Patterns are this toast's own names.
function Dismiss-InNotificationCentre([string]$Needle, [string[]]$Allowed, [string[]]$Patterns) {
    if (-not (Initialize-SpikeUi)) { return [ordered]@{ method = 'none'; ok = $false; why = "UI Automation not available: $($script:Uia.error)" } }
    $before = Get-UiaSnapshot
    if (-not (Send-Keys @(0x5B, 0x4E))) { return [ordered]@{ method = 'none'; ok = $false; why = 'owner active: Win+N not sent' } }
    $w = Watch-Needle -Needle $Needle -Before $before -Seconds 5 -UntilFound
    $centre = Get-CentreHwnd $w $before
    $rec = [ordered]@{ method = 'uia (Win+N, hover, this entry''s own dismiss button)'; found_entry = ($null -ne $w.Hit); ok = $false; used = $null; hovered = $false }
    $restore = $null
    try {
        if ($null -ne $w.Hit) {
            $r = $w.Hit.Element.Current.BoundingRectangle
            if (-not $r.IsEmpty) {
                $restore = [SpikeUi.Win]::MoveTo([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
                $rec.hovered = $null -ne $restore
            }
            if (-not $rec.hovered) {
                $rec.why = 'the hover was held back (the owner was using the PC): nothing searched, nothing pressed'
            } else {
                Start-Sleep -Milliseconds 700
                $found = Find-ToastButton $w.Hit $Needle { param($n, $a) Test-DismissButton $n $a } $Allowed (@($Patterns) + @('(?i)^(dismiss|close)\b'))
                if ($found.Ok) {
                    $rec.used = Get-ElementInfo $found.Button
                    $how = Invoke-UiaButton $found.Button
                    if (-not $how.ok) { $how = Click-ElementCentre $found.Button $w.Hit.TopInfo }
                    $rec.how = $how
                    $rec.ok = [bool]$how.ok
                } else {
                    $rec.why = "$($found.Why)"
                    $rec.unknown_controls = $found.UnknownControls
                }
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

# What to tell the owner when something already holds ES_SYSTEM_REQUIRED (review N-I2): run-all's
# unattended block shows it before Enter, Power-StayAwake again if it is still set.
function Get-KeepAwakeAdvice([string]$State) {
    ("Something on this PC is keeping it awake right now (execution state $State), so the stay-awake check can't prove anything. " +
     "The likely cause: the Claude app's 'Keep computer awake while Claude works' setting holds the PC awake while any Claude session is working. " +
     "Pause your Claude sessions, or turn that setting off, for this test. " +
     "Other usual causes: a playing video, a download, or another app with a keep-awake option. " +
     "Optional: powercfg /requests in a terminal opened with 'Run as administrator' names the program (it only reads).")
}

# ---------------------------------------------------------------- the Sandbox runs' timeline (pure)

# Whether the PC slept between $FromMs and $ToMs (unix ms), from the host core's events (a
# suspend or resume broadcast, or a clock jump) and the System log's Kernel-Power entries (42
# sleep, 107 resume), each {t_ms, ...}. Returns {slept, events, kernel}. Pure; tested.
function Get-SleepInWindow($Events, $Kernel, [long]$FromMs, [long]$ToMs) {
    $in = { param($e) $null -ne $e -and [long](P2o $e 't_ms') -ge $FromMs -and [long](P2o $e 't_ms') -le $ToMs }
    # 42 sleep, 107 resume, 506 and 507 Modern Standby enter and exit (fix round 3 m8).
    $ev = @(@($Events) | Where-Object { (& $in $_) -and ((P2o $_ 'kind') -eq 'clock_jump' -or ((P2o $_ 'kind') -eq 'power_broadcast' -and (P2o $_ 'pbt') -in 'PBT_APMSUSPEND', 'PBT_APMRESUMEAUTOMATIC', 'PBT_APMRESUMESUSPEND')) })
    $kn = @(@($Kernel) | Where-Object { (& $in $_) -and (P2o $_ 'id') -in 42, 107, 506, 507 })
    [ordered]@{ slept = ($ev.Count + $kn.Count) -gt 0; events = $ev; kernel = $kn }
}

# S1.9's verdict from its runs (review N-I1): a judged run that failed is FAIL; otherwise runs
# interrupted by sleep make it INCOMPLETE; otherwise PASS. Pure; tested.
function Get-SandboxVerdict($Runs) {
    $all = @(@($Runs) | Where-Object { $null -ne $_ })
    $judged = @($all | Where-Object { (P2o $_ 'interrupted_by_sleep') -ne $true })
    $interrupted = $all.Count - $judged.Count
    if (@($judged | Where-Object { (P2o $_ 'pass') -ne $true }).Count -gt 0) { return 'FAIL' }
    if ($interrupted -gt 0) { return "INCOMPLETE ($interrupted run(s) interrupted by sleep: rerun them)" }
    if ($all.Count -eq 0) { return 'INCOMPLETE (no run)' }
    'PASS'
}

# A field of a dictionary or an object, or $null (strict-mode safe; the scripts' own P).
function P2o($Obj, [string]$Name) {
    if ($null -eq $Obj) { return $null }
    if ($Obj -is [System.Collections.IDictionary]) { return $Obj[$Name] }
    $p = $Obj.PSObject.Properties[$Name]
    if ($p) { $p.Value } else { $null }
}
