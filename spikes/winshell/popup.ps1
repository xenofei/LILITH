<#
Spike 11 (plan 4a Task 1): the hands-off pop-up (owner direction 2026-10-23: "if there is
anything that requires me to not touch my keyboard/mouse during testing, have a pop-up for it
with at least 10 seconds to register it mentally. After that, give a timer for how long it's
going to take to finish before I can use the computer again").

Started by uia.ps1's Start-HandsOff, detached and without a console (winshell-tool spawn), in
Windows PowerShell 5.1 for WinForms. It shows, at the top centre of the primary screen (away
from the toasts at the bottom right):
  1. until the hands-off start (-StartAtMs, epoch ms; replaced by <StateFile>.start once the
     caller has seen this window on screen): "In N s: hands off the mouse and keyboard", and what
     is coming;
  2. then a countdown of -RunSec seconds until the PC may be used again ("almost done" after);
  3. once -StateFile exists: "Done: you can use the PC again", for 5 s, then it closes.
It touches <StateFile>.shown once it is on screen, so that the caller's 10 s warning starts
only then. It never takes the focus: WS_EX_NOACTIVATE and ShowWithoutActivation, and it is made
topmost with SetWindowPos(HWND_TOPMOST, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE |
SWP_SHOWWINDOW), not with the form's TopMost property (whose show path focuses the form). It is
click-through (WS_EX_LAYERED | WS_EX_TRANSPARENT): mouse input and WindowFromPoint / UIA
FromPoint go to the windows under it. It closes when -ParentPid has gone (that process is pinned
by its handle at start), and after the run plus 15 minutes at the latest, removing its state files
whichever way it closes. Nothing ends it from outside: the caller only writes -StateFile. It
changes nothing else.
#>
param(
    [string]$What = 'is working',
    [long]$StartAtMs = 0,
    [int]$RunSec = 60,
    [string]$StateFile = '',
    [int]$ParentPid = 0
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
if (-not ('SpikePopup' -as [type])) {
    Add-Type -WarningAction SilentlyContinue -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Windows.Forms;
public class SpikePopup : Form {
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

$parent = $null
if ($ParentPid -gt 0) {
    try { $parent = Get-Process -Id $ParentPid; [void]$parent.Handle } catch { exit 0 }   # pin-lookup: the live parent, at start
}
if ($StartAtMs -le 0) { $StartAtMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + 10000 }
$script:startAt = $StartAtMs
$script:doneAt = $null

$form = New-Object SpikePopup
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
$form.Add_Shown({
    [void]$form.MakeTopmost()
    if ($StateFile) { try { Set-Content -LiteralPath "$StateFile.shown" -Value ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()) } catch { } }
})

$amber = [System.Drawing.Color]::FromArgb(150, 90, 0)
$red = [System.Drawing.Color]::FromArgb(140, 30, 30)
$green = [System.Drawing.Color]::FromArgb(20, 110, 40)
$capMs = $StartAtMs + ($RunSec + 900) * 1000

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 250
$timer.Add_Tick({
    $nowMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    if (($null -ne $parent -and $parent.HasExited) -or $nowMs -gt $capMs) {
        # Closed by the parent watch or the cap: the state files go too (fix round 4, m8).
        if ($StateFile) { foreach ($f in $StateFile, "$StateFile.shown", "$StateFile.start") { try { Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue } catch { } } }
        $form.Close(); return
    }
    if ($StateFile) {
        $sf = "$StateFile.start"
        if (Test-Path -LiteralPath $sf) { try { $script:startAt = [long]((Get-Content -Raw -LiteralPath $sf).Trim()) } catch { } }
        if ($null -eq $script:doneAt -and (Test-Path -LiteralPath $StateFile)) { $script:doneAt = $nowMs }
    }
    if ($null -ne $script:doneAt) {
        $form.BackColor = $green
        $label.Text = "Done: you can use the PC again."
        if ($nowMs - $script:doneAt -gt 5000) {
            foreach ($f in $StateFile, "$StateFile.shown", "$StateFile.start") { try { Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue } catch { } }
            $form.Close()
        }
        return
    }
    if ($nowMs -lt $script:startAt) {
        $form.BackColor = $amber
        $label.Text = ("In {0} s: HANDS OFF the mouse and keyboard.`nThe script {1}." -f [math]::Ceiling(($script:startAt - $nowMs) / 1000), $What)
        return
    }
    $left = ($script:startAt + $RunSec * 1000 - $nowMs) / 1000
    $form.BackColor = $red
    if ($left -gt 0) {
        $label.Text = ("HANDS OFF: the script {0}.`nYou can use the PC again in about {1:mm\:ss}." -f $What, [timespan]::FromSeconds([math]::Ceiling($left)))
    } else {
        $label.Text = ("HANDS OFF: the script {0}.`nAlmost done..." -f $What)
    }
})
$timer.Start()
[System.Windows.Forms.Application]::Run($form)
