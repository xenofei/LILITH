<#
.SYNOPSIS
  The owner's hands-off notice (owner direction, 2026-10-23): a small topmost window that never
  takes focus or input. It warns -LeadSeconds ahead that the keyboard and mouse must not be used,
  then counts down the hands-off stretch, and says clearly when the PC is free again.

.DESCRIPTION
  Runs under Windows PowerShell 5.1 (WinForms), started by comfy-load.ps1 (S0.6's measured run) and
  run-spike.ps1 (S0.7's click). The caller waits -LeadSeconds before the stretch starts, and
  writes -DoneFile when it ends; the window then says "done" for 6 s and closes. It also closes if
  -ParentPid exits, or after its own deadline.

  It can't take focus or input: WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, and
  WS_EX_TRANSPARENT on a layered window (clicks go through it), shown without activation, and
  WM_MOUSEACTIVATE answered with MA_NOACTIVATE. It goes on a screen that isn't the primary one if
  there is one (the spike runs on the primary), else in the primary screen's bottom-right corner,
  clear of the spike's windows. It redraws a short text once a second. -Log gets a JSON line
  with the screen kind and whether the window was ever the foreground window.
#>
param([int]$LeadSeconds = 10, [int]$Seconds = 60, [string]$What = 'the measurement', [string]$DoneFile,
      [int]$ParentPid = 0, [string]$Log)
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
$other = @($screens | Where-Object { -not $_.Primary }) | Select-Object -First 1
$screen = if ($other) { $other } else { [System.Windows.Forms.Screen]::PrimaryScreen }
$area = $screen.WorkingArea
$form = [HandsOffForm]::new()
$form.FormBorderStyle = 'None'; $form.TopMost = $true; $form.ShowInTaskbar = $false
$form.StartPosition = 'Manual'; $form.Size = [System.Drawing.Size]::new(420, 110)
$form.Location = [System.Drawing.Point]::new($area.Right - 440, $area.Bottom - 130)
$form.BackColor = [System.Drawing.Color]::FromArgb(40, 40, 48); $form.Opacity = 0.96
$label = [System.Windows.Forms.Label]::new()
$label.Dock = 'Fill'; $label.ForeColor = [System.Drawing.Color]::White; $label.TextAlign = 'MiddleCenter'
$label.Font = [System.Drawing.Font]::new('Segoe UI', 13)
$form.Controls.Add($label)

$t0 = Get-Date
$handsOffAt = $t0.AddSeconds($LeadSeconds)
$deadline = $handsOffAt.AddSeconds($Seconds + 180)
$parent = if ($ParentPid) { try { $p = [System.Diagnostics.Process]::GetProcessById($ParentPid); [void]$p.Handle; $p } catch { $null } } else { $null }
$script:doneAt = $null; $script:endReason = $null
$timer = [System.Windows.Forms.Timer]::new(); $timer.Interval = 1000
$tick = {
    $now = Get-Date
    $form.CheckForeground()
    if (-not $script:doneAt) {
        if ($DoneFile -and (Test-Path $DoneFile)) { $script:doneAt = $now; $script:endReason = 'done' }
        elseif ($parent -and $parent.HasExited) { $script:doneAt = $now; $script:endReason = 'stopped' }
        elseif ($now -gt $deadline) { $script:doneAt = $now; $script:endReason = 'deadline' }
    }
    if ($script:doneAt) {
        $form.BackColor = [System.Drawing.Color]::FromArgb(24, 110, 60)
        $label.Text = if ($script:endReason -eq 'done') { "Done: you can use the keyboard and mouse again." }
                      else { "The run stopped: you can use the keyboard and mouse again." }
        if (($now - $script:doneAt).TotalSeconds -ge 6) { $timer.Stop(); $form.Close() }
    } elseif ($now -lt $handsOffAt) {
        $form.BackColor = [System.Drawing.Color]::FromArgb(150, 90, 0)
        $label.Text = "In $([math]::Ceiling(($handsOffAt - $now).TotalSeconds)) s: please don't use the keyboard or mouse`nfor about $Seconds s ($What)."
    } else {
        $form.BackColor = [System.Drawing.Color]::FromArgb(150, 30, 30)
        $left = [math]::Ceiling(($handsOffAt.AddSeconds($Seconds) - $now).TotalSeconds)
        $label.Text = if ($left -gt 0) { "Hands off: about $left s left ($What)." } else { "Hands off: finishing, a few more seconds ($What)." }
    }
}
$timer.Add_Tick($tick)
& $tick
$timer.Start()
[System.Windows.Forms.Application]::Run($form)   # Show(), which honours ShowWithoutActivation (ShowDialog activates)
if ($Log) {
    [ordered]@{ screen = $(if ($other) { 'not primary' } else { 'primary, bottom-right corner' }); screens = $screens.Count
                ever_foreground = $form.EverForeground; end = $script:endReason; seconds_shown = [math]::Round(((Get-Date) - $t0).TotalSeconds, 1) } |
        ConvertTo-Json -Compress | Set-Content -Encoding UTF8 $Log
}
