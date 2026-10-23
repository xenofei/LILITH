<#
.SYNOPSIS
  The owner's hands-off notice (owner direction, 2026-10-23): a small topmost window that never
  takes focus or input. It warns -LeadSeconds ahead that the keyboard and mouse must not be used,
  then counts down the hands-off stretch, and says clearly when the PC is free again.

.DESCRIPTION
  Runs under Windows PowerShell 5.1 (WinForms), started by comfy-load.ps1 (S0.6's measured run) and
  run-spike.ps1 (S0.7's click), through Start-HandsOffNotice and Wait-HandsOffStart.
  The hands-off stretch starts at an absolute time: the later of -HandsOffAtEpochMs and the moment
  the window is first shown plus -LeadSeconds, so the warning is on screen for at least
  -LeadSeconds however long Windows PowerShell and Add-Type take to start. Once shown, it writes
  -ReadyFile (JSON: shown_epoch_ms, hands_off_epoch_ms), and the caller waits until that time.
  The caller writes -DoneFile when the stretch ends; the window then says "done" for 6 s and
  closes. It also closes if -ParentPid exits, or after its own deadline.

  It can't take focus or input: WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, and
  WS_EX_TRANSPARENT on a layered window (clicks go through it), shown without activation, and
  WM_MOUSEACTIVATE answered with MA_NOACTIVATE. It goes in the primary screen's bottom-right
  corner, clear of the spike's windows (another screen may be switched off, and the owner looks
  at the primary one). It redraws a short text once a second, and checks every 50 ms whether it
  is the foreground window. -Log gets a JSON line with the timings and that check.
#>
param([int]$LeadSeconds = 10, [int]$Seconds = 60, [string]$What = 'the measurement', [string]$DoneFile,
      [int]$ParentPid = 0, [string]$Log, [long]$HandsOffAtEpochMs = 0, [string]$ReadyFile)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System;
using System.Windows.Forms;
public class HandsOffForm : Form {
    [System.Runtime.InteropServices.DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    public bool EverForeground;
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams {
        get {
            var cp = base.CreateParams;
            cp.ExStyle |= 0x08000000 | 0x00000080 | 0x00000008 | 0x00080000 | 0x00000020;  // NOACTIVATE, TOOLWINDOW, TOPMOST, LAYERED, TRANSPARENT
            return cp;
        }
    }
    protected override void WndProc(ref Message m) {
        if (m.Msg == 0x0021) { m.Result = (IntPtr)3; return; }   // WM_MOUSEACTIVATE: MA_NOACTIVATE
        base.WndProc(ref m);
    }
    public void CheckForeground() { if (GetForegroundWindow() == Handle) EverForeground = true; }
}
'@

$screens = [System.Windows.Forms.Screen]::AllScreens
$area = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
$form = [HandsOffForm]::new()
$form.FormBorderStyle = 'None'; $form.TopMost = $true; $form.ShowInTaskbar = $false
$form.StartPosition = 'Manual'; $form.Size = [System.Drawing.Size]::new(420, 110)
$form.Location = [System.Drawing.Point]::new($area.Right - 440, $area.Bottom - 130)
$form.BackColor = [System.Drawing.Color]::FromArgb(40, 40, 48); $form.Opacity = 0.96
$label = [System.Windows.Forms.Label]::new()
$label.Dock = 'Fill'; $label.ForeColor = [System.Drawing.Color]::White; $label.TextAlign = 'MiddleCenter'
$label.Font = [System.Drawing.Font]::new('Segoe UI', 13)
$form.Controls.Add($label)
# Where it is, in logical px, so an overlap with the spike's windows (layout_snapshot) can be checked:
# it stays clear of them on a primary working area of at least about 1,380 x 800.
$script:placement = [ordered]@{
    primary_working_area = @($area.X, $area.Y, $area.Width, $area.Height)
    notice = @($form.Location.X, $form.Location.Y, $form.Size.Width, $form.Size.Height) }

$t0 = Get-Date
$script:handsOffAt = $t0.AddSeconds($LeadSeconds)   # replaced once the window is shown
$script:shownEpoch = $null; $script:handsOffEpoch = $null
$deadline = $t0.AddSeconds($LeadSeconds + $Seconds + 240)
$form.Add_Shown({
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $script:shownEpoch = $now
    $script:handsOffEpoch = [math]::Max([long]$HandsOffAtEpochMs, $now + 1000L * $LeadSeconds)
    $script:handsOffAt = [DateTimeOffset]::FromUnixTimeMilliseconds($script:handsOffEpoch).LocalDateTime
    if ($ReadyFile) { [ordered]@{ shown_epoch_ms = $now; hands_off_epoch_ms = $script:handsOffEpoch; placement = $script:placement } | ConvertTo-Json -Compress -Depth 4 | Set-Content -Encoding UTF8 $ReadyFile }
})
$fgTimer = [System.Windows.Forms.Timer]::new(); $fgTimer.Interval = 50
$fgTimer.Add_Tick({ $form.CheckForeground() })
$parent = if ($ParentPid) { try { $p = [System.Diagnostics.Process]::GetProcessById($ParentPid); [void]$p.Handle; $p } catch { $null } } else { $null }
$script:doneAt = $null; $script:endReason = $null
$timer = [System.Windows.Forms.Timer]::new(); $timer.Interval = 1000
$tick = {
    $now = Get-Date
    if (-not $script:doneAt) {
        if ($DoneFile -and (Test-Path $DoneFile)) { $script:doneAt = $now; $script:endReason = 'done' }
        elseif ($parent -and $parent.HasExited) { $script:doneAt = $now; $script:endReason = 'stopped' }
        elseif ($now -gt $deadline) { $script:doneAt = $now; $script:endReason = 'deadline' }
    }
    if ($script:doneAt) {
        $form.BackColor = [System.Drawing.Color]::FromArgb(24, 110, 60)
        $label.Text = if ($script:endReason -eq 'done') { "Done: you can use the keyboard and mouse again." }
                      else { "The run stopped: you can use the keyboard and mouse again." }
        if (($now - $script:doneAt).TotalSeconds -ge 6) { $timer.Stop(); $fgTimer.Stop(); $form.Close() }
    } elseif ($now -lt $script:handsOffAt) {
        $form.BackColor = [System.Drawing.Color]::FromArgb(150, 90, 0)
        $label.Text = "In $([math]::Ceiling(($script:handsOffAt - $now).TotalSeconds)) s: please don't use the keyboard or mouse`nfor about $Seconds s ($What)."
    } else {
        $form.BackColor = [System.Drawing.Color]::FromArgb(150, 30, 30)
        $left = [math]::Ceiling(($script:handsOffAt.AddSeconds($Seconds) - $now).TotalSeconds)
        $label.Text = if ($left -gt 0) { "Hands off: about $left s left ($What)." } else { "Hands off: finishing, a few more seconds ($What)." }
    }
}
$timer.Add_Tick($tick)
& $tick
$timer.Start(); $fgTimer.Start()
[System.Windows.Forms.Application]::Run($form)   # Show(), which honours ShowWithoutActivation (ShowDialog activates)
if ($Log) {
    [ordered]@{ screen = 'primary, bottom-right corner'; screens = $screens.Count; placement = $script:placement
                shown_epoch_ms = $script:shownEpoch; hands_off_epoch_ms = $script:handsOffEpoch
                warning_ms = $(if ($script:shownEpoch) { $script:handsOffEpoch - $script:shownEpoch } else { $null })
                ever_foreground = $form.EverForeground; foreground_checks_ms = 50
                end = $script:endReason; seconds_shown = [math]::Round(((Get-Date) - $t0).TotalSeconds, 1) } |
        ConvertTo-Json -Compress -Depth 4 | Set-Content -Encoding UTF8 $Log
}
