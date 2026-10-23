<#
.SYNOPSIS
  S0.7's UI Automation client (APP-156). Runs under Windows PowerShell 5.1, whose .NET Framework
  has the managed UIA client (System.Windows.Automation, with NotificationEvent); run-spike.ps1
  starts it beside a spike started with --no-activate --auto-counter. It needs no focus: UIA
  reads properties and receives events from a window that was never activated.

  It waits for the app's top-level windows, reads every element under them (Name, ControlType,
  FrameworkId, ...), subscribes to UIA notification events (what Accessible.announce() raises),
  writes <Out>.ready, waits until the app exits or -WaitSeconds, and writes <Out> (JSON). It
  judges nothing: Get-S07Items in lib/SpikeMeasure.psm1 does.
#>
param([int]$AppPid, [string]$Out, [int]$WaitSeconds = 40, [int]$MinWindows = 2, [int]$SettleMs = 1500)
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
                row["window"] = windowName; row["depth"] = depth + 1;
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

$res = [ordered]@{ app_pid = $AppPid; started = (Get-Date).ToString('o'); windows = @(); window_polls = @(); elements = @(); notifications = @() }
try {
    $app = [System.Diagnostics.Process]::GetProcessById($AppPid); [void]$app.Handle   # pinned
    # The app's top-level windows (main-a, main-b and the floating Counter window), at most 20 s.
    $wins = $null
    while ([S07Uia]::Clock.ElapsedMilliseconds -lt 20000 -and -not $app.HasExited) {
        $wins = [S07Uia]::Windows($AppPid)
        $res.window_polls += [ordered]@{ ms = [S07Uia]::Clock.ElapsedMilliseconds; n = $wins.Count }
        if ($wins.Count -ge $MinWindows) { break }
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Milliseconds $SettleMs   # the layout settles (the floating Counter window is owned by main-a)
    $res.windows_found_ms = [S07Uia]::Clock.ElapsedMilliseconds
    [S07Uia]::Subscribe($AppPid)
    $res.subscribed_ms = [S07Uia]::Clock.ElapsedMilliseconds
    $res.subscribed_utc = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    Set-Content -Path "$Out.ready" -Value $res.subscribed_utc
    # Read the trees now (the counter is clicked a few seconds later, so its label still says 0).
    $els = @()
    foreach ($w in @($wins)) {
        $wn = $w.Current.Name
        $res.windows += [ordered]@{ name = $wn; control_type = $w.Current.ControlType.ProgrammaticName; framework_id = $w.Current.FrameworkId
                                    class_name = $w.Current.ClassName }
        $els += @([S07Uia]::Elements($w, $wn))
    }
    $res.elements = @($els | ForEach-Object { [pscustomobject]$_ })
    $res.elements_read_ms = [S07Uia]::Clock.ElapsedMilliseconds
    $res.element_errors = [S07Uia]::Errors
    $until = [S07Uia]::Clock.ElapsedMilliseconds + $WaitSeconds * 1000
    while (-not $app.HasExited -and [S07Uia]::Clock.ElapsedMilliseconds -lt $until) { Start-Sleep -Milliseconds 200 }
    Start-Sleep -Milliseconds 300   # events raised just before the exit
} catch {
    $res.error = "$($_.Exception.GetType().Name): $($_.Exception.Message)"
} finally {
    $res.notifications = @([S07Uia]::Notifications() | ForEach-Object { [pscustomobject]$_ })
    try { [System.Windows.Automation.Automation]::RemoveAllEventHandlers() } catch {}
    $res | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 $Out
}
