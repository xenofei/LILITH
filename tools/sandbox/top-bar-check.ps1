<#
.SYNOPSIS
  The hands-off check of the top bar in the deployed lil-image, against a test-channel core:
    pwsh -NoProfile -File tools/sandbox/top-bar-check.ps1 [-DryRun]

.DESCRIPTION
  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown).
  2. A test channel under a new temporary root (LIL_CHANNEL=test-topbarcheck, LIL_TEST_ROOT), with
     the deployed lil-image on it, driven through UI Automation. It checks, on disk or through UIA:
       (a) with no workspace: "Create or choose a workspace" opens the native folder dialog (window
           class #32770). Cancel it: suite.toml and the root's folders are as they were, and the
           banner stays.
       Then a workspace is made, and lil-image starts again on it:
       (b) a real Ctrl+Shift+P pauses GPU work: the core says so, and the bar's button and badge
           show it; a second press resumes.
       (c) a switcher's list opens over the docks and takes the focus; a real press on its unavailable
           row leaves it open, which only the list itself would; Escape closes it; a press on a docked
           panel closes it; a press on a floating dock window closes it.
       (d) `lil workspace use` while the app stays connected: the banner names it, and "Use the
           default workspace" gets out.
       (e) the test core is killed (by its recorded PID): "Reconnecting…" shows and the pause control
           is off in each sample. The app's session starts a new core: the connection and the counts
           come back, and the control then offers Pause, never the old Resume enabled (a restart ended
           the pause). A press after that works. Sampling can't see the moment between `ready` and the
           fresh read, and a press can't be aimed at an outage under a second:
           test_the_lanes_are_not_trusted_from_a_loss_until_the_fresh_read proves the ordering.
  On a failure, the app's UIA tree and the test channel's logs go to -DiagDir first. The tree holds
  no names from a dialog's file list, navigation pane, address bar or breadcrumb. Real keys go only
  while the app's window is in front, and a real press only where its window is under the point;
  otherwise the check fails and sends nothing.
  3. lil-image is closed, the test core stopped (only the PIDs recorded here), the test channel's
     registry keys, temporary root and Qt cache folder (%LOCALAPPDATA%\LILITH-test-topbarcheck)
     removed, and the pop-up told it is done. Anything left behind fails the run. Exit 0 only when
     every check passed and the cleanup was complete.
  -DryRun does step 2's channel set-up and the guards' self-test only, with no pop-up, no window and
  no input.
#>
param(
    # The repository whose build/dev/deploy is checked: by default this script's own.
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    # Where a failed run leaves the app's UIA tree and the logs: by default build\dev\top-bar-check.
    [string]$DiagDir = '',
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $Deploy) { $Deploy = Join-Path $Repo 'build\dev\deploy' }
if (-not $DiagDir) { $DiagDir = Join-Path $Repo 'build\dev\top-bar-check' }
$lil = Join-Path $Deploy 'lil.exe'
$image = Join-Path $Deploy 'lil-image.exe'
$results = [System.Collections.Generic.List[string]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
function Pass([string]$what) { $results.Add("PASS $what"); Write-Host "PASS $what" }
function Fail([string]$what) { $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what" }
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-topbarcheck'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-topbarcheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
# Every core PID seen on this test channel: the only core processes this script may stop.
$corePids = [System.Collections.Generic.HashSet[int]]::new()
function Enter-TestChannel {
    # First, before anything that can throw: every `lil` from here on is on the test channel.
    $env:LIL_CHANNEL = $channel
    $env:LIL_TEST_ROOT = $root
    New-Item -ItemType Directory $root | Out-Null
    foreach ($s in 'roaming', 'local', 'profile', 'places') { New-Item -ItemType Directory (Join-Path $root $s) | Out-Null }
    Set-Location -LiteralPath $root
}
function Invoke-Lil([string[]]$a) {
    $o = & $lil @a 2>&1
    if ($LASTEXITCODE -ne 0) { throw "lil $($a -join ' ') exited $LASTEXITCODE`: $($o -join ' ')" }
    return ($o -join "`n")
}
# The running test core's PID, from its own hello (recorded), or 0 with none.
function Get-CorePid {
    $o = & $lil --no-start status 2>&1
    if ($LASTEXITCODE -ne 0) { return 0 }
    $m = [regex]::Match(($o -join "`n"), 'pid (\d+), channel (\S+?)\)')
    if (-not $m.Success -or $m.Groups[2].Value -notlike '*topbarcheck*') { return 0 }
    $id = [int]$m.Groups[1].Value
    [void]$corePids.Add($id)
    return $id
}
function Stop-RecordedCores {
    foreach ($id in $corePids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        # Only a core run from this deploy: a PID can be reused.
        if ($p -and $p.ProcessName -eq 'lil-core' -and $p.Path -and $p.Path.StartsWith($Deploy, [StringComparison]::OrdinalIgnoreCase)) {
            Write-Host "stopping the test core, pid $id"
            Stop-Process -Id $id -Force
        }
    }
}
function Exit-TestChannel {
    # Never off the test channel: `lil core stop` would reach the channel this shell defaults to.
    if ($env:LIL_CHANNEL -ne $channel) { Write-Host 'the test channel was never entered: nothing to clean up'; return }
    try { [void](Get-CorePid) } catch { }
    try { [void](& $lil core stop 2>&1) } catch { }
    Start-Sleep -Milliseconds 500
    try { Stop-RecordedCores } catch { Fail "cleanup: stopping the test core: $_" }
    try { Set-Location -LiteralPath $Repo } catch { }
    Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
    # Each exactly the test channel's: its registry root (its classes are under that), and Windows'
    # notification settings entry, which a toast would make.
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
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $root); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $root } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path -LiteralPath $root) { Fail "cleanup: $root is still there" }
    # The organisation name is the channel's (apps/lil-image/main.cpp), so Qt's qmlcache goes to the
    # real local data folder, outside the test root. Exactly this channel's folder: on any other
    # channel this function has returned above.
    $cache = Join-Path $env:LOCALAPPDATA "LILITH-$channel"
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $cache); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $cache } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path -LiteralPath $cache) { Fail "cleanup: $cache is still there" }
    foreach ($id in $corePids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p -and $p.ProcessName -eq 'lil-core' -and $p.Path -and $p.Path.StartsWith($Deploy, [StringComparison]::OrdinalIgnoreCase)) {
            if (-not $p.WaitForExit(5000)) { Fail "cleanup: the test core, pid $id, is still running" }
        }
    }
}
# What the core says of the GPU lane, from another session (the test channel's `lil`).
function Get-GpuLane {
    $json = Invoke-Lil @('call', 'jobs.list', '{"limit":1}')
    $lanes = ($json | ConvertFrom-Json).structured.lanes
    return ($lanes | Where-Object { $_.lane -eq 'gpu-heavy' })
}
# What on disk says this check changed nothing: suite.toml, and the folders under the root's places.
function Get-DiskState {
    $suite = Get-ChildItem -LiteralPath (Join-Path $root 'roaming') -Recurse -Force -Filter 'suite.toml' -ErrorAction SilentlyContinue | Select-Object -First 1
    $hash = if ($suite) { (Get-FileHash -LiteralPath $suite.FullName).Hash } else { 'none' }
    $names = @(Get-ChildItem -LiteralPath (Join-Path $root 'places') -Force -Recurse | ForEach-Object { $_.FullName }) -join '|'
    return "$hash;$names"
}

if (-not ('LilTopBarCheck.Win' -as [type])) {
    Add-Type -Namespace LilTopBarCheck -Name Win -MemberDefinition @'
public struct POINT { public int X; public int Y; }
[DllImport("user32.dll")] public static extern System.IntPtr WindowFromPoint(POINT point);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint processId);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, System.UIntPtr extra);
'@
}
# Real input only ever goes to this app: anything else in front, and the check fails and sends nothing.
function Get-WindowProcessId([System.IntPtr]$hwnd) {
    [uint32]$owner = 0
    [void][LilTopBarCheck.Win]::GetWindowThreadProcessId($hwnd, [ref]$owner)
    return [int]$owner
}
function Assert-AppInFront {
    for ($i = 0; $i -lt 5; ++$i) {
        if ((Get-WindowProcessId ([LilTopBarCheck.Win]::GetForegroundWindow())) -eq $script:app.Id) { return }
        Start-Sleep -Milliseconds 200
    }
    throw 'no keys sent: another program''s window is in front'
}
function Send-Checked([string]$keys) {
    Assert-AppInFront
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}
function Assert-AppAt([int]$x, [int]$y) {
    $point = [LilTopBarCheck.Win+POINT]::new()
    $point.X = $x
    $point.Y = $y
    if ((Get-WindowProcessId ([LilTopBarCheck.Win]::WindowFromPoint($point))) -ne $script:app.Id) {
        throw "no press sent: another program's window is at $x,$y"
    }
}
# The guards, without any input: a process that owns no window is never in front or under a point.
function Self-Test {
    $script:app = [pscustomobject]@{ Id = -1 }
    $refused = 0
    try { Assert-AppInFront } catch { if ("$_" -like 'no keys sent*') { ++$refused } }
    try { Assert-AppAt 0 0 } catch { if ("$_" -like 'no press sent*') { ++$refused } }
    $script:app = $null
    Check ($refused -eq 2) "self-test: the guards refuse a window that isn't the app's ($refused of 2)"
    $own = Get-WindowProcessId ([LilTopBarCheck.Win]::GetForegroundWindow())
    Check ($own -ge 0) 'self-test: the foreground window''s owner can be read'
}

if ($DryRun) {
    Self-Test
    Enter-TestChannel
    try {
        $ws = Join-Path $root 'places\harbor'
        [void](Invoke-Lil @('workspace', 'init', $ws, '--default'))
        $id = Get-CorePid
        Check ($id -gt 0) "dry run: the test core runs, pid $id"
        $lane = Get-GpuLane
        Check ($null -ne $lane -and $lane.paused -eq $false) 'dry run: the core answers for the GPU lane'
        Check (Test-Path -LiteralPath $image) "dry run: $image is deployed"
        Check (Test-Path -LiteralPath (Join-Path $Deploy 'qml\QtQuick\Dialogs')) 'dry run: QtQuick\Dialogs is deployed'
        $s = Get-DiskState
        Check ($s -notlike 'none;*') 'dry run: suite.toml is read from the test root'
    } catch { Fail "dry run: $_" } finally { Exit-TestChannel }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-topbarcheck-{0}.state" -f $PID)
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', 'checks the top bar (windows, a folder dialog, and real key presses)', '-WarnSec', '10', '-RunSec', '150',
               '-CapSec', '420', '-StateFile', $state, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
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
$AE = [System.Windows.Automation.AutomationElement]
$TS = [System.Windows.Automation.TreeScope]
function Prop($p, $v) { [System.Windows.Automation.PropertyCondition]::new($p, $v) }
function And($a, $b) { [System.Windows.Automation.AndCondition]::new($a, $b) }
function Wait-For([scriptblock]$find, [int]$sec, [string]$what) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) {
        $e = & $find
        if ($e) { return $e }
        Start-Sleep -Milliseconds 200
    }
    throw "timed out after $sec s waiting for $what"
}
function Find-Window([int]$procId, [string]$name) {
    $c = Prop $AE::ProcessIdProperty $procId
    if ($name) { $c = And $c (Prop $AE::NameProperty $name) }
    $c = And $c (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Window))
    $e = $AE::RootElement.FindFirst($TS::Children, $c)
    if (-not $e -and $script:main) { $e = $script:main.FindFirst($TS::Descendants, $c) }
    return $e
}
function Show-Element($e) {
    try { [void][LilTopBarCheck.Win]::SetForegroundWindow([System.IntPtr]$e.Current.NativeWindowHandle) } catch { }
    Start-Sleep -Milliseconds 300
}
function Show-App { Show-Element $script:main }
function Send-Keys([string]$keys) { Show-App; Send-Checked $keys; Start-Sleep -Milliseconds 200 }
# A real press, at screen coordinates, only if the app's window is there.
function Press-At([double]$x, [double]$y) {
    [void][LilTopBarCheck.Win]::SetCursorPos([int]$x, [int]$y)
    Start-Sleep -Milliseconds 100
    Assert-AppAt ([int]$x) ([int]$y)
    [LilTopBarCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
    Start-Sleep -Milliseconds 50
    [LilTopBarCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
    Start-Sleep -Milliseconds 200
}
function Press-Center($e) {
    $r = $e.Current.BoundingRectangle
    Press-At ($r.X + $r.Width / 2) ($r.Y + $r.Height / 2)
}
function Invoke-Element($e) {
    $p = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return }
    try { $e.SetFocus() } catch { Show-App }
    Send-Checked ' '
}
function App-Windows {
    @($AE::RootElement.FindAll($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id)))
}
function Find-Named($name) {
    $c = Prop $AE::NameProperty $name
    foreach ($w in App-Windows) { $e = $w.FindFirst($TS::Descendants, $c); if ($e) { return $e } }
    return $null
}
# The first element whose name starts with `prefix`, in a window of the app. Only the main window
# and floating docks are searched this way, never a dialog (the check closes those by id).
function Find-NameStarting([string]$prefix) {
    foreach ($w in App-Windows) {
        $all = $w.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        foreach ($e in $all) { try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { } }
    }
    return $null
}
function Find-List([string]$name) {
    $c = And (Prop $AE::NameProperty $name) (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::List))
    return $script:main.FindFirst($TS::Descendants, $c)
}
function Is-Enabled($e) { try { return $e.Current.IsEnabled } catch { return $false } }

# On a failure: every window of the app as UIA shows it, at most 4000 elements. Names only for the
# app's own Qt elements (their ids start "QGuiApplication"): nothing of the system dialogs.
function Save-UiaTree([string]$path) {
    $lines = [System.Collections.Generic.List[string]]::new()
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $walk = {
        param($e, [int]$depth)
        if ($lines.Count -ge 4000) { return }
        try {
            $c = $e.Current
            $ours = $c.AutomationId.StartsWith('QGuiApplication') -or ($depth -eq 0 -and $c.Name -like 'LILITH*')
            $name = if (-not $ours) { '' } elseif ($c.Name.Length -gt 100) { $c.Name.Substring(0, 100) + '…' } else { $c.Name }
            $lines.Add(('{0}{1} "{2}" id={3} offscreen={4} enabled={5}' -f ('  ' * $depth), $c.ControlType.ProgrammaticName, $name, $c.AutomationId, $c.IsOffscreen, $c.IsEnabled))
            # A dialog's file list, navigation pane and address bar or breadcrumb: the node alone.
            if ($c.AutomationId -in 'listview', 'ProperTreeHost', '40965', '41477' -or $c.ClassName -in 'Breadcrumb Parent', 'Address Band Root') { return }
        } catch { $lines.Add(('{0}(gone)' -f ('  ' * $depth))); return }
        $child = $walker.GetFirstChild($e)
        while ($child) { & $walk $child ($depth + 1); $child = $walker.GetNextSibling($child) }
    }
    foreach ($w in App-Windows) { & $walk $w 0 }
    Set-Content -LiteralPath $path -Value $lines -Encoding utf8
}
function Invoke-DialogId($dlg, [string]$id) {
    $e = $dlg.FindFirst($TS::Children, (Prop $AE::AutomationIdProperty $id))
    if (-not $e) { $e = $dlg.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty $id)) }
    $p = $null
    if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return $true }
    return $false
}
function Start-App {
    $script:app = Start-Process -FilePath $image -WorkingDirectory $root -PassThru
    $script:main = $null
    [void](Wait-For { Find-Window $script:app.Id '' } 40 'the lil-image window')
    Start-Sleep -Seconds 1
    $wins = App-Windows
    $script:main = ($wins | Where-Object { $_.Current.Name -like 'LILITH*' } | Select-Object -First 1)
    if (-not $script:main) { $script:main = $wins[0] }
    [void](Wait-For { Find-NameStarting 'Core connection: Connected' } 40 'the connection')
}
function Close-App {
    if ($script:app -and -not $script:app.HasExited) {
        try {
            $wp = $null
            if ($script:main -and $script:main.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$wp)) { $wp.Close() }
            if (-not $script:app.WaitForExit(10000)) { Stop-Process -Id $script:app.Id -Force }
        } catch { Stop-Process -Id $script:app.Id -Force -ErrorAction SilentlyContinue }
    }
}

$script:app = $null
$script:main = $null
try {
    Enter-TestChannel

    # (a) No workspace: the folder dialog, cancelled.
    Start-App
    Pass 'lil-image is up on the test channel, with no workspace'
    [void](Get-CorePid)
    $create = Wait-For { $b = Find-Named 'Create or choose a workspace'; if ($b -and (Is-Enabled $b)) { $b } } 20 'the banner''s button'
    $before = Get-DiskState
    Invoke-Element $create
    $dlg = Wait-For { Find-Window $script:app.Id 'Create or choose a workspace' } 20 'the folder dialog'
    Pass '(a) "Create or choose a workspace" opened a folder dialog'
    Check ($dlg.Current.ClassName -eq '#32770') "(a) it is the native one (window class: $($dlg.Current.ClassName))"
    if (-not (Invoke-DialogId $dlg '2')) { Show-Element $dlg; Send-Checked '{ESC}' }
    [void](Wait-For { if (-not (Find-Window $script:app.Id 'Create or choose a workspace')) { $true } } 10 'the dialog to close')
    Start-Sleep -Seconds 1
    Check ((Get-DiskState) -eq $before) '(a) cancelled: suite.toml and the folders are as they were'
    Check ($null -ne (Find-Named 'Create or choose a workspace')) '(a) cancelled: the banner stays'
    Close-App

    # A workspace, and the app on it.
    $harbor = Join-Path $root 'places\harbor'
    $quay = Join-Path $root 'places\quay'
    [void](Invoke-Lil @('workspace', 'init', $harbor, '--default'))
    [void](Invoke-Lil @('workspace', 'init', $quay))
    Start-App
    [void](Get-CorePid)
    $jobs = Wait-For { $e = Find-NameStarting 'Jobs: 0 running'; if ($e) { $e } } 30 'the counts'
    Pass "the bar reads: $($jobs.Current.Name)"

    # (b) A real Ctrl+Shift+P.
    $pause = Wait-For { $b = Find-Named 'Pause GPU work'; if ($b -and (Is-Enabled $b)) { $b } } 20 'the pause control'
    Send-Keys '^+p'
    $badge = Wait-For { Find-NameStarting 'GPU work paused by you' } 10 'the badge'
    Pass "(b) Ctrl+Shift+P: the bar's badge says: $($badge.Current.Name)"
    Check ($null -ne (Find-Named 'Resume GPU work')) '(b) the bar offers Resume GPU work'
    $lane = Get-GpuLane
    Check ($lane.paused -eq $true -and ($lane.reasons -contains 'manual')) '(b) the core says the GPU lane is paused by you'
    Send-Keys '^+p'
    [void](Wait-For { Find-Named 'Pause GPU work' } 10 'Pause GPU work again')
    $lane = Get-GpuLane
    Check ($lane.paused -eq $false) '(b) a second press: the core says it is not paused'
    Check ($null -eq (Find-NameStarting 'GPU work paused')) '(b) a second press: the badge is gone'

    # (c) A switcher's list.
    function Open-List {
        $button = Wait-For { Find-NameStarting 'Workspace: ' } 10 'the workspace switcher'
        Show-App
        Invoke-Element $button
        return Wait-For { Find-List 'Workspaces' } 10 'the workspace list'
    }
    $list = Open-List
    Pass '(c) the workspace switcher''s list opened'
    $home_ = Wait-For { $script:main.FindFirst($TS::Descendants, (Prop $AE::NameProperty 'Home')) } 10 'the Home panel'
    $r = $list.Current.BoundingRectangle
    $h = $home_.Current.BoundingRectangle
    Check ($r.IntersectsWith($h)) "(c) the list lies over a dock (list $r, Home $h)"
    $focusName = ''
    $focused = Wait-For {
        $f = $AE::FocusedElement
        try { $focusName = $f.Current.Name } catch { $focusName = '' }
        if ($focusName -eq 'Workspaces' -or $focusName -like 'harbor*' -or $focusName -like 'quay*') { $f }
    } 5 'the list to take the focus'
    Pass "(c) the list took the focus ($($focused.Current.Name))"
    # Only the list keeps itself open: a press that reached anything under it would close it.
    $row = Wait-For {
        foreach ($e in $list.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
            try { if ($e.Current.Name -like '*unavailable*') { return $e } } catch { }
        }
    } 5 'an unavailable row'
    $b = $row.Current.BoundingRectangle
    Press-At ($b.X + 40) ($b.Y + $b.Height / 2)
    Start-Sleep -Milliseconds 600
    Check ($null -ne (Find-List 'Workspaces')) '(c) the list is on top over the docks: a real press on its unavailable row left it open'
    Check ($null -ne (Find-NameStarting 'Workspace: harbor')) '(c) that press chose nothing'
    Send-Keys '{ESC}'
    [void](Wait-For { if (-not (Find-List 'Workspaces')) { $true } } 5 'Escape to close the list')
    Pass '(c) Escape closed the list'

    $list = Open-List
    $home_ = $script:main.FindFirst($TS::Descendants, (Prop $AE::NameProperty 'Home'))
    $h = $home_.Current.BoundingRectangle
    # Below the list and inside Home: a docked panel, nothing of the list.
    $r = $list.Current.BoundingRectangle
    Press-At ($h.X + $h.Width / 2) ([Math]::Max($r.Bottom + 40, $h.Y + $h.Height - 30))
    [void](Wait-For { if (-not (Find-List 'Workspaces')) { $true } } 5 'a press on a docked panel to close the list')
    Pass '(c) a press on a docked panel closed the list'

    # A floating dock: another window of the app.
    $floatBtn = Find-Named 'Float Hardware'
    if (-not $floatBtn) {
        Fail '(c) no Float Hardware button, so the floating case did not run'
    } else {
        Invoke-Element $floatBtn
        # UIA shows a floating dock as a Window under the main one ("Floating QWindow with parent").
        $floating = Wait-For { Find-Window $script:app.Id 'Hardware' } 10 'the floating dock'
        Start-Sleep -Milliseconds 500
        Pass "(c) Hardware floats in a window of its own"
        Show-App
        $list = Open-List
        $f = $floating.Current.BoundingRectangle
        $l = $list.Current.BoundingRectangle
        Write-Host "(c) floating dock at $f, list at $l"
        # A point of the floating window outside the list, its title bar's middle.
        $px = $f.X + $f.Width / 2
        $py = $f.Y + 12
        if ($l.Contains([System.Windows.Point]::new($px, $py))) { $py = $f.Bottom - 12 }
        Press-At $px $py
        try {
            [void](Wait-For { if (-not (Find-List 'Workspaces')) { $true } } 5 'a press on the floating dock to close the list')
            Pass '(c) a press on a floating dock window closed the list'
        } catch { Fail "(c) $_" }
        if (Find-List 'Workspaces') { Show-App; Send-Keys '{ESC}' }
    }

    # (d) Another client moves the default while the app stays connected.
    [void](Invoke-Lil @('workspace', 'use', $quay))
    $moved = Wait-For { Find-NameStarting 'The core refused this workspace: ' } 15 'the banner naming the move'
    Pass "(d) the banner says: $($moved.Current.Name)"
    Check ($null -ne (Find-NameStarting 'Core connection: Workspace refused')) '(d) the connection indicator says the workspace is refused'
    $useDefault = Wait-For { Find-Named 'Use the default workspace' } 5 'Use the default workspace'
    Invoke-Element $useDefault
    [void](Wait-For { Find-NameStarting 'Core connection: Connected' } 20 'the connection, on the new default')
    $label = Wait-For { Find-NameStarting 'Workspace: quay' } 10 'the switcher on the new default'
    Pass "(d) Use the default workspace: $($label.Current.Name)"
    Check ($null -eq (Find-NameStarting 'The core refused this workspace: ')) '(d) the banner went'

    # (e) The core goes away. Your pause is set first: a core restart ends it.
    $jobs = Wait-For { $e = Find-NameStarting 'Jobs: 0 running'; if ($e) { $e } } 30 'the counts on the new workspace'
    [void](Wait-For { $b = Find-Named 'Pause GPU work'; if ($b -and (Is-Enabled $b)) { $b } } 20 'the pause control')
    Send-Keys '^+p'
    [void](Wait-For { Find-Named 'Resume GPU work' } 10 'Resume GPU work')
    $victim = Get-CorePid
    if ($victim -le 0) { throw 'no test core to kill' }
    Stop-Process -Id $victim -Force
    Write-Host "(e) killed the test core, pid $victim"
    $seen = [System.Collections.Generic.List[string]]::new()
    $sawReconnecting = $false
    $sawBanner = $false
    $earlyEnabled = $false
    $end = (Get-Date).AddSeconds(60)
    $done = $false
    while (-not $done -and (Get-Date) -lt $end) {
        $conn = Find-NameStarting 'Core connection: '
        $connName = if ($conn) { try { $conn.Current.Name } catch { '' } } else { '' }
        $control = Find-NameStarting 'Resume GPU work'
        if (-not $control) { $control = Find-NameStarting 'Pause GPU work' }
        $controlName = if ($control) { try { $control.Current.Name } catch { '' } } else { '' }
        $enabled = if ($control) { Is-Enabled $control } else { $false }
        $jobsNow = Find-NameStarting 'Jobs: '
        $jobsName = if ($jobsNow) { try { $jobsNow.Current.Name } catch { '' } } else { '' }
        $line = "$connName | $controlName enabled=$enabled | $jobsName"
        if ($seen.Count -eq 0 -or $seen[$seen.Count - 1] -ne $line) { $seen.Add($line) }
        if ($connName -notlike '*Connected*') {
            if ($connName -like '*Reconnecting*') { $sawReconnecting = $true }
            if (Find-NameStarting 'Reconnecting… ') { $sawBanner = $true }
            if ($enabled) { $earlyEnabled = $true }
        } else {
            # The fresh read landed when counts show; before it, the control must be off.
            if ($jobsName -eq 'Jobs: —' -and $enabled) { $earlyEnabled = $true }
            if ($jobsName -like 'Jobs: * running*' -and $controlName -eq 'Pause GPU work' -and $enabled) { $done = $true }
            if ($controlName -eq 'Resume GPU work' -and $enabled) { $earlyEnabled = $true }
        }
        if (-not $done) { Start-Sleep -Milliseconds 100 }
    }
    $seen | ForEach-Object { Write-Host "(e) saw: $_" }
    Check $sawReconnecting '(e) the connection indicator said Reconnecting'
    Check $sawBanner '(e) the "Reconnecting…" banner showed'
    Check (-not $earlyEnabled) '(e) in each sample the pause control was off until the counts came back, and the old Resume never showed enabled'
    Check $done '(e) the connection and the counts came back, and the control offers Pause: the restart ended the pause'
    $newCore = Get-CorePid
    Check ($newCore -gt 0 -and $newCore -ne $victim) "(e) a new core runs, pid $newCore"
    $lane = Get-GpuLane
    Check ($lane.paused -eq $false) '(e) the core says the GPU lane is not paused: a restart ended it'
    Check ($null -eq (Find-NameStarting 'Reconnecting… ')) '(e) the banner went'
    Send-Keys '^+p'
    [void](Wait-For { Find-Named 'Resume GPU work' } 10 'Resume GPU work after the restart')
    $lane = Get-GpuLane
    Check ($lane.paused -eq $true -and ($lane.reasons -contains 'manual')) '(e) a press after the fresh read pauses the new core'
} catch {
    Fail "the top bar check stopped: $_"
    try {
        New-Item -ItemType Directory -Force $DiagDir | Out-Null
        if ($script:app -and -not $script:app.HasExited) { Save-UiaTree (Join-Path $DiagDir 'uia-tree.txt') }
        $logs = Join-Path $root "local\LILITH-$channel\logs"
        if (Test-Path -LiteralPath $logs) { Copy-Item -LiteralPath $logs -Destination (Join-Path $DiagDir 'logs') -Recurse -Force }
        Write-Host "diagnostics in $DiagDir"
    } catch { Write-Host "the diagnostics weren't saved: $_" }
} finally {
    try { Close-App } catch { Fail "cleanup: closing lil-image: $_" }
    try { Exit-TestChannel } catch { Fail "cleanup: $_" }
    try { Set-Content -LiteralPath $state -Value 'done' } catch { Write-Host "note: the pop-up wasn't told: $_" }
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
exit ([int]($failures.Count -gt 0))
