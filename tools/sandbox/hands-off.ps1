<#
.SYNOPSIS
lil dev desktop-tests --host (QA-042's exception, P4a-R39): the hands-off pop-up.

.DESCRIPTION
Started by the runner, detached, without a console and without the focus, in Windows PowerShell
5.1 for WinForms. It shows, at the top centre of the primary screen (away from the toasts at the
bottom right):
  1. from the moment it is on screen, for -WarnSec seconds: "In N s: hands off the mouse and
     keyboard", and what is coming;
  2. then a countdown of -RunSec seconds until the PC may be used again ("almost done" after);
  3. once -StateFile exists: "Done: you can use the PC again", for 5 s, then it closes.
It writes <StateFile>.shown once it is on screen; the runner starts the tests only -WarnSec
seconds after that. It never takes the focus (WS_EX_NOACTIVATE and ShowWithoutActivation; made
topmost with SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE |
SWP_SHOWWINDOW), not with the form's TopMost property, whose show path focuses the form), and it
is click-through (WS_EX_LAYERED | WS_EX_TRANSPARENT). It closes when -ParentPid has gone, and at
the latest -CapSec seconds after the countdown's start (the runner's worst case), removing its state files whichever way it
closes. It changes nothing else.
#>
param(
    [string]$What = 'runs the desktop tests',
    [int]$WarnSec = 10,
    [int]$RunSec = 60,
    # The latest it closes, counted from the countdown's start: the runner sizes it to the
    # worst-case run (every test running until nextest ends it).
    [int]$CapSec = 1200,
    [string]$StateFile = '',
    [int]$ParentPid = 0
)

$ErrorActionPreference = 'Stop'
# It has no console: an error goes to <StateFile>.error, which the runner reports.
trap {
    if ($StateFile) { try { Add-Content -LiteralPath "$StateFile.error" -Value "$_" } catch { } }
    exit 1
}
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
if (-not ('LilHandsOff' -as [type])) {
    Add-Type -WarningAction SilentlyContinue -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Windows.Forms;
public class LilHandsOff : Form {
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams {
        get {
            CreateParams cp = base.CreateParams;
            // WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_LAYERED
            cp.ExStyle |= 0x08000000 | 0x00000080 | 0x00000020 | 0x00080000;
            return cp;
        }
    }
    // HWND_TOPMOST with SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW.
    public bool MakeTopmost() { return SetWindowPos(Handle, new IntPtr(-1), 0, 0, 0, 0, 0x0010 | 0x0002 | 0x0001 | 0x0040); }
}
'@
}

function Remove-StateFiles {
    if ($StateFile) {
        foreach ($f in $StateFile, "$StateFile.shown", "$StateFile.error") { try { Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue } catch { } }
    }
}

$parent = $null
if ($ParentPid -gt 0) {
    # Pinned at start: a later pid reuse can't keep the pop-up open.
    try { $parent = Get-Process -Id $ParentPid; [void]$parent.Handle } catch { exit 0 }
}
$script:shownAt = $null
$script:doneAt = $null

$form = New-Object LilHandsOff
$form.FormBorderStyle = 'None'
$form.ShowInTaskbar = $false
$form.StartPosition = 'Manual'
$form.Opacity = 0.93
$area = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
$form.Width = 620; $form.Height = 130
$form.Left = $area.Left + [int](($area.Width - $form.Width) / 2)
$form.Top = $area.Top + 24
$label = New-Object System.Windows.Forms.Label
$label.Dock = 'Fill'
$label.ForeColor = [System.Drawing.Color]::White
$label.Font = New-Object System.Drawing.Font('Segoe UI', 15)
$label.TextAlign = 'MiddleCenter'
$form.Controls.Add($label)
$amber = [System.Drawing.Color]::FromArgb(150, 90, 0)
$red = [System.Drawing.Color]::FromArgb(140, 30, 30)
$green = [System.Drawing.Color]::FromArgb(20, 110, 40)
# What it shows from its first frame, before the timer's first tick.
$form.BackColor = $amber
$label.Text = ("In {0} s: HANDS OFF the mouse and keyboard.`nLILITH {1}." -f $WarnSec, $What)
$form.Add_Shown({
    [void]$form.MakeTopmost()
    $script:shownAt = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    if ($StateFile) { try { Set-Content -LiteralPath "$StateFile.shown" -Value $script:shownAt } catch { } }
})

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 250
$timer.Add_Tick({
    $nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    if ($null -eq $script:shownAt) { return }
    $startAt = $script:shownAt + $WarnSec * 1000
    $capMs = $startAt + [math]::Max($CapSec, $RunSec) * 1000
    if (($null -ne $parent -and $parent.HasExited) -or $nowMs -gt $capMs) {
        Remove-StateFiles
        $form.Close(); return
    }
    if ($StateFile -and $null -eq $script:doneAt -and (Test-Path -LiteralPath $StateFile)) { $script:doneAt = $nowMs }
    if ($null -ne $script:doneAt) {
        $form.BackColor = $green
        $label.Text = 'Done: you can use the PC again.'
        if ($nowMs - $script:doneAt -gt 5000) { Remove-StateFiles; $form.Close() }
        return
    }
    if ($nowMs -lt $startAt) {
        $form.BackColor = $amber
        $label.Text = ("In {0} s: HANDS OFF the mouse and keyboard.`nLILITH {1}." -f [math]::Ceiling(($startAt - $nowMs) / 1000), $What)
        return
    }
    $left = ($startAt + $RunSec * 1000 - $nowMs) / 1000
    $form.BackColor = $red
    if ($left -gt 0) {
        $label.Text = ("HANDS OFF: LILITH {0}.`nYou can use the PC again in about {1:mm\:ss}." -f $What, [timespan]::FromSeconds([math]::Ceiling($left)))
    } else {
        $label.Text = ("HANDS OFF: LILITH {0}.`nAlmost done..." -f $What)
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)
