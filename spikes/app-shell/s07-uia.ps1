<#
.SYNOPSIS
  S0.7's UI Automation client (APP-156). Runs under Windows PowerShell 5.1, whose .NET Framework
  has the managed UIA client (System.Windows.Automation, with NotificationEvent); run-spike.ps1
  starts it beside a spike started with --no-activate. It needs no focus: UIA reads properties,
  receives events and invokes a button in a window that was never activated (Qt then activates
  that window itself; see step 5).

  Order, each step only once the one before it is done:
    1. read the app's windows' elements (Name, ControlType, FrameworkId, ProcessId, ...) every
       500 ms until every item S0.7 looks for is present, at most -TreeTimeoutSeconds;
    2. subscribe to UIA notification events (what Accessible.announce() raises);
    3. click "Increment the counter" through its InvokePattern (no mouse, no focus);
    4. wait at most -AnnounceTimeoutSeconds for "Count is now 1";
    5. close the app's windows (WindowPattern): Qt activates the window whose button was invoked,
       so the app is ended at once rather than keeping the focus; then write <Out> (JSON).
  Times are recorded in ms since the app's start (read_app_ms, subscribed_app_ms,
  invoked_app_ms). It judges nothing: Get-S07Items in lib/SpikeMeasure.psm1 does.
#>
param([int]$AppPid, [string]$Out, [int]$TreeTimeoutSeconds = 15, [int]$AnnounceTimeoutSeconds = 2, [int]$MinWindows = 2)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase
Add-Type -ReferencedAssemblies UIAutomationClient, UIAutomationTypes, WindowsBase -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Windows.Automation;
public static class S07Uia {
    public static readonly Stopwatch Clock = Stopwatch.StartNew();
    static readonly object Gate = new object();
    static readonly List<Dictionary<string, object>> Seen = new List<Dictionary<string, object>>();
    // UIA calls this on its own thread, so it only copies the event into a locked list.
    static void OnNotification(object sender, AutomationEventArgs e) {
        var n = e as NotificationEventArgs;
        var row = new Dictionary<string, object>();
        row["ms"] = Clock.ElapsedMilliseconds;
        if (n != null) {
            row["display_string"] = n.DisplayString; row["activity_id"] = n.ActivityId;
            row["kind"] = n.NotificationKind.ToString(); row["processing"] = n.NotificationProcessing.ToString();
        }
        try {
            var el = sender as AutomationElement;
            if (el != null) { row["process_id"] = el.Current.ProcessId; row["source_name"] = el.Current.Name;
                              row["source_control_type"] = el.Current.ControlType.ProgrammaticName; }
        } catch (Exception x) { row["source_error"] = x.GetType().Name; }
        lock (Gate) Seen.Add(row);
    }
    public static void Subscribe(int pid) {
        Automation.AddAutomationEventHandler(AutomationElement.NotificationEvent, AutomationElement.RootElement,
                                             TreeScope.Subtree, new AutomationEventHandler(OnNotification));
    }
    public static List<Dictionary<string, object>> Notifications() { lock (Gate) return new List<Dictionary<string, object>>(Seen); }
    public static AutomationElementCollection Windows(int pid) {
        return AutomationElement.RootElement.FindAll(TreeScope.Children,
            new PropertyCondition(AutomationElement.ProcessIdProperty, pid));
    }
    // Every element under a window, raw view, depth-first (the spike's trees are small).
    public static List<Dictionary<string, object>> Elements(AutomationElement window, string windowName) {
        var list = new List<Dictionary<string, object>>();
        Walk(TreeWalker.RawViewWalker, window, windowName, 0, list);
        return list;
    }
    public static int Errors;
    static AutomationElement Next(TreeWalker w, AutomationElement e, bool first) {
        try { return first ? w.GetFirstChild(e) : w.GetNextSibling(e); } catch (Exception) { Errors++; return null; }
    }
    static void Walk(TreeWalker w, AutomationElement e, string windowName, int depth, List<Dictionary<string, object>> list) {
        if (depth > 40 || list.Count > 5000) return;
        var c = Next(w, e, true);
        while (c != null) {
            try {
                var cur = c.Current;
                var row = new Dictionary<string, object>();
                row["window"] = windowName; row["depth"] = depth + 1; row["process_id"] = cur.ProcessId;
                row["name"] = cur.Name; row["control_type"] = cur.ControlType.ProgrammaticName;
                row["localized_control_type"] = cur.LocalizedControlType; row["framework_id"] = cur.FrameworkId;
                row["class_name"] = cur.ClassName; row["automation_id"] = cur.AutomationId;
                row["is_offscreen"] = cur.IsOffscreen; row["is_keyboard_focusable"] = cur.IsKeyboardFocusable;
                list.Add(row);
            } catch (Exception) { Errors++; }   // an element that went away, or a provider error
            Walk(w, c, windowName, depth + 1, list);
            c = Next(w, c, false);
        }
    }
}
'@

$res = [ordered]@{ app_pid = $AppPid; started = (Get-Date).ToString('o'); windows_found = $false; items_ready = $false
                   invoked = $false; windows = @(); elements = @(); notifications = @() }
# The items S0.7 looks for (Get-S07Items judges them): present means the layout is built.
function Get-Missing($rows) {
    $qt = @($rows | Where-Object { $_.framework_id -eq 'Qt' -and [int]$_.process_id -eq $AppPid })
    $m = @()
    if (-not @($qt | Where-Object { $_.control_type -eq 'ControlType.TitleBar' -and $_.name -in 'Jobs', 'Grid' }).Count) { $m += 'title bar' }
    foreach ($t in 'Grid', 'Web') { if (-not @($qt | Where-Object { $_.control_type -eq 'ControlType.TabItem' -and $_.name -eq $t }).Count) { $m += "tab $t" } }
    if (-not @($qt | Where-Object { $_.control_type -eq 'ControlType.Button' -and $_.name -eq 'Increment the counter' }).Count) { $m += 'counter button' }
    if (-not @($qt | Where-Object { $_.name -match '^GPU load chart, latest ' }).Count) { $m += 'chart tile' }
    $m
}
try {
    $app = [System.Diagnostics.Process]::GetProcessById($AppPid); [void]$app.Handle   # pinned
    $appMs = { [math]::Round(((Get-Date) - $app.StartTime).TotalMilliseconds) }
    $wins = @(); $rows = @(); $missing = @('everything'); $polls = 0
    $deadline = (Get-Date).AddSeconds($TreeTimeoutSeconds)
    while ((Get-Date) -lt $deadline -and -not $app.HasExited) {
        $polls++
        $wins = @([S07Uia]::Windows($AppPid))
        if ($wins.Count -ge $MinWindows) {
            $res.windows_found = $true
            $rows = @(); foreach ($w in $wins) { $rows += @([S07Uia]::Elements($w, $w.Current.Name) | ForEach-Object { [pscustomobject]$_ }) }
            $missing = @(Get-Missing $rows)
            if (-not $missing.Count) { break }
        }
        Start-Sleep -Milliseconds 500
    }
    $res.read_app_ms = & $appMs
    $res.tree_polls = $polls
    $res.items_ready = $res.windows_found -and -not $missing.Count
    $res.missing_at_timeout = @($missing | Where-Object { -not $res.items_ready })
    foreach ($w in $wins) {   # the floating Counter window is owned by main-b in the UIA tree
        $res.windows += [ordered]@{ name = $w.Current.Name; control_type = $w.Current.ControlType.ProgrammaticName
                                    framework_id = $w.Current.FrameworkId; class_name = $w.Current.ClassName }
    }
    $res.elements = $rows
    $res.element_errors = [S07Uia]::Errors

    [S07Uia]::Subscribe($AppPid)
    $res.subscribed_app_ms = & $appMs
    $cond = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $AppPid),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'Increment the counter'),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button))
    $btn = $null
    foreach ($w in $wins) { if (-not $btn) { $btn = $w.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond) } }
    if (-not $btn) { $res.invoke_error = 'no button named "Increment the counter"' }
    else {
        try {
            $btn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            $res.invoked = $true
            $res.invoked_app_ms = & $appMs
        } catch { $res.invoke_error = "$($_.Exception.GetType().Name): $($_.Exception.Message)" }
    }
    if ($res.invoked) {
        $until = (Get-Date).AddSeconds($AnnounceTimeoutSeconds)
        while ((Get-Date) -lt $until -and -not @([S07Uia]::Notifications() | Where-Object { $_['display_string'] -eq 'Count is now 1' }).Count) {
            Start-Sleep -Milliseconds 100
        }
        $res.announce_wait_ended_app_ms = & $appMs
    }
    # Qt activates the window whose button UIA invoked, although S0.7 itself needs no focus, and
    # UIA can't hand the focus back. So the client closes the app's windows (WindowPattern) as soon
    # as it is done: the app quits, and the focus goes back, within a fraction of a second.
    # This runs whether or not the announcement came (the wait is 2 s). If WindowPattern.Close
    # fails, the app's main window is asked to close through its pinned Process (WM_CLOSE to the
    # same pid); either way the app quits normally. Only if both fail does it keep running, and
    # with it the focus, until its --quit-after.
    if ($res.invoked) {
        foreach ($w in $wins) {
            try { $w.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close() } catch { $res.close_error = "$($_.Exception.GetType().Name)" }
        }
        if ($res.close_error -and -not $app.WaitForExit(1000)) { $res.close_fallback = $app.CloseMainWindow() }
        $res.closed_app_ms = & $appMs
    }
} catch {
    $res.error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
} finally {
    $res.notifications = @([S07Uia]::Notifications() | ForEach-Object { [pscustomobject]$_ })
    try { [System.Windows.Automation.Automation]::RemoveAllEventHandlers() } catch {}
    $res | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 $Out
}
