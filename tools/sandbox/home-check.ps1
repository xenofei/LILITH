<#
.SYNOPSIS
  The hands-off check of Home in the deployed lil-image, against a test-channel core:
    pwsh -NoProfile -File tools/sandbox/home-check.ps1 [-DryRun]

.DESCRIPTION
  What only a real window can prove (Task 26). The offscreen tests (tst_home) prove the rules; this
  proves Windows tells Home what those rules read: when it is minimized, floating or behind a tab.
  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown).
  2. A test channel under a new temporary root (LIL_CHANNEL=test-homecheck, LIL_TEST_ROOT), with the
     deployed lil-image on it, driven through UI Automation. A job has finished since the kept time
     (state.json, written before the start). It checks, on disk or through UIA:
       (a) minimized as soon as the window is there: the kept time stays for 6 s. Restored: the away
           card lists the job, and the time moves 3 s or more after the restore.
       (b) minimized for longer, with two more jobs finishing: the time stays. Restored: the card
           counts from the kept time, so it lists those two and not the first.
       (c) a floating Home: the same lines, the time not moved by the float; a job finishing while it
           floats is listed there, and the time moves. Docked again: the same lines.
       (d) Home behind a tab: a real drag of its title bar puts it in the Jobs tabs, and the Jobs tab
           is chosen. A job finishes: the time stays. Home's tab chosen: the job is listed, and the
           time moves 3 s or more later.
       (e) a real second launch, with Home closed: it hands over and exits 0, Home opens, and the
           Opened card shows the target; a refused link says so there.
  On a failure, the app's UIA tree and the test channel's logs go to -DiagDir first. The tree holds no
  names from a dialog's file list, navigation pane, address bar or breadcrumb. A real press goes only
  while the app's window is in front and under the point; otherwise the check fails and sends nothing.
  If Windows keeps the front, it waits up to 240 s for it, sending no input.
  3. lil-image is closed, the test core stopped (only the PIDs recorded here), the test channel's
     registry keys, temporary root and Qt cache folder (%LOCALAPPDATA%\LILITH-test-homecheck) removed,
     and the pop-up told it is done. Anything left behind fails the run. Exit 0 only when every check
     passed and the cleanup was complete.
  -DryRun does step 2's channel set-up, a job, and the guards' self-test only, with no pop-up, no
  window and no input.
#>
param(
    # The repository whose build/dev/deploy is checked: by default this script's own.
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    # Where a failed run leaves the app's UIA tree and the logs: by default build\dev\home-check.
    [string]$DiagDir = '',
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $Deploy) { $Deploy = Join-Path $Repo 'build\dev\deploy' }
if (-not $DiagDir) { $DiagDir = Join-Path $Repo 'build\dev\home-check' }
$lil = Join-Path $Deploy 'lil.exe'
$image = Join-Path $Deploy 'lil-image.exe'
$results = [System.Collections.Generic.List[string]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
function Pass([string]$what) { $results.Add("PASS $what"); Write-Host "PASS $what" }
function Fail([string]$what) { $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what" }
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-homecheck'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-homecheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$stateFile = Join-Path $root "local\LILITH-$channel\apps\image\state.json"
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
# The running test core's PID, from its own hello (recorded), or 0 with none. It refuses a channel
# that isn't this one.
function Get-CorePid {
    $o = & $lil --no-start status 2>&1
    if ($LASTEXITCODE -ne 0) { return 0 }
    $m = [regex]::Match(($o -join "`n"), 'pid (\d+), channel (\S+?)\)')
    if (-not $m.Success -or $m.Groups[2].Value -notlike '*homecheck*') { return 0 }
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
# The default workspace's id, from the test channel's core.
function Get-WorkspaceId {
    $rows = (Invoke-Lil @('call', 'workspaces.list', '{}') | ConvertFrom-Json).structured.workspaces
    return ($rows | Where-Object { $_.default } | Select-Object -First 1).id
}
# Refuses a channel already taken, before anything can stop or change what runs on it: a core that
# answers on it, or a lil-image from this deploy. It then leaves the channel, so that the cleanup does
# nothing.
function Assert-ChannelFree {
    $taken = ''
    $o = & $lil --no-start status 2>&1
    if ($LASTEXITCODE -eq 0 -and [regex]::IsMatch(($o -join "`n"), 'pid \d+, channel ')) { $taken = 'a core already runs on it' }
    $apps = @(Get-Process -Name 'lil-image' -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($Deploy, [StringComparison]::OrdinalIgnoreCase) })
    if ($apps.Count -gt 0) { $taken = "a lil-image from this deploy runs (pid $($apps[0].Id))" }
    if ($taken) {
        Set-Location -LiteralPath $Repo
        Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
        Remove-Item -Recurse -Force -LiteralPath $root -ErrorAction SilentlyContinue
        throw "refused: the test channel is taken: $taken"
    }
}
# A job that finishes, from another session: its id once it has.
function Invoke-FinishedJob {
    $id = (Invoke-Lil @('call', 'test.job', '{"lane":"cpu"}') | ConvertFrom-Json).structured.job.job_id
    $done = Invoke-Lil @('call', 'job.wait', ('{"job_id":"' + $id + '","timeout_s":30}')) | ConvertFrom-Json
    if ($done.structured.job.status -ne 'succeeded') { throw "the test job ended $($done.structured.job.status)" }
    return $id
}
# The app's kept time as written: the text itself, since ConvertFrom-Json would turn it into a date.
function Get-Kept {
    if (-not (Test-Path -LiteralPath $stateFile)) { return '' }
    $m = [regex]::Match((Get-Content -Raw -LiteralPath $stateFile), '"last_seen_at"\s*:\s*"([^"]+)"')
    return $(if ($m.Success) { $m.Groups[1].Value } else { '' })
}
function Set-Kept([string]$workspace, [string]$at) {
    New-Item -ItemType Directory -Force (Split-Path $stateFile) | Out-Null
    $text = '{"state": 1, "workspaces": {"' + $workspace + '": {"last_seen_at": "' + $at + '"}}}'
    [IO.File]::WriteAllText($stateFile, $text)
}

if (-not ('LilHomeCheck.Win' -as [type])) {
    Add-Type -Namespace LilHomeCheck -Name Win -MemberDefinition @'
public struct POINT { public int X; public int Y; }
[DllImport("user32.dll")] public static extern System.IntPtr WindowFromPoint(POINT point);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint processId);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, System.UIntPtr extra);
'@
}
# Real input only ever goes to this app: anything else in front or at the point, and the check fails
# and sends nothing.
function Get-WindowProcessId([System.IntPtr]$hwnd) {
    [uint32]$owner = 0
    [void][LilHomeCheck.Win]::GetWindowThreadProcessId($hwnd, [ref]$owner)
    return [int]$owner
}
function Assert-AppInFront {
    for ($i = 0; $i -lt 5; ++$i) {
        if ((Get-WindowProcessId ([LilHomeCheck.Win]::GetForegroundWindow())) -eq $script:app.Id) { return }
        Start-Sleep -Milliseconds 200
    }
    throw 'no press sent: another program''s window is in front'
}
function Assert-AppAt([int]$x, [int]$y) {
    $point = [LilHomeCheck.Win+POINT]::new()
    $point.X = $x
    $point.Y = $y
    if ((Get-WindowProcessId ([LilHomeCheck.Win]::WindowFromPoint($point))) -ne $script:app.Id) {
        throw "no press sent: another program's window is at $x,$y"
    }
}
# The guards, without any input: a process that owns no window is never in front or under a point.
function Self-Test {
    $script:app = [pscustomobject]@{ Id = -1 }
    $refused = 0
    try { Assert-AppInFront } catch { if ("$_" -like 'no press sent*') { ++$refused } }
    try { Assert-AppAt 0 0 } catch { if ("$_" -like 'no press sent*') { ++$refused } }
    $script:app = $null
    Check ($refused -eq 2) "self-test: the guards refuse a window that isn't the app's ($refused of 2)"
    $own = Get-WindowProcessId ([LilHomeCheck.Win]::GetForegroundWindow())
    Check ($own -ge 0) 'self-test: the foreground window''s owner can be read'
}

if ($DryRun) {
    Self-Test
    try {
        Enter-TestChannel
        Assert-ChannelFree
        Pass 'dry run: the test channel isn''t taken'
        $ws = Join-Path $root 'places\harbor'
        [void](Invoke-Lil @('workspace', 'init', $ws, '--default'))
        $id = Get-CorePid
        Check ($id -gt 0) "dry run: the test core runs, pid $id"
        $wsId = Get-WorkspaceId
        Check ([bool]$wsId) 'dry run: the default workspace has an id'
        $job = Invoke-FinishedJob
        Check ([bool]$job) 'dry run: a test job finishes from the command line'
        Check (Test-Path -LiteralPath (Join-Path $root "local\LILITH-$channel")) 'dry run: the local data folder is where the kept time goes'
        Set-Kept $wsId '2026-01-01T00:00:00Z'
        Check ((Get-Kept) -eq '2026-01-01T00:00:00Z') 'dry run: the kept time reads back'
        Check (Test-Path -LiteralPath $image) "dry run: $image is deployed"
    } catch { Fail "dry run: $_" } finally { Exit-TestChannel }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-homecheck-{0}.state" -f $PID)
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', 'checks Home (windows that minimize and float, and a real drag)', '-WarnSec', '10', '-RunSec', '120',
               '-CapSec', '600', '-StateFile', $state, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
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
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$AE = [System.Windows.Automation.AutomationElement]
$TS = [System.Windows.Automation.TreeScope]
$WP = [System.Windows.Automation.WindowPattern]
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
# UI Automation can throw for a whole walk while the tree moves: walk again, a few times.
function Retry-Walk([scriptblock]$walk) {
    for ($i = 0; ; ++$i) {
        try { return (& $walk) } catch { if ($i -ge 4) { throw }; Start-Sleep -Milliseconds 100 }
    }
}
function App-Windows {
    @($AE::RootElement.FindAll($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id)))
}
function Find-Window([string]$name) {
    $c = And (Prop $AE::ProcessIdProperty $script:app.Id) (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Window))
    if ($name) { $c = And $c (Prop $AE::NameProperty $name) }
    $e = $AE::RootElement.FindFirst($TS::Children, $c)
    if (-not $e -and $script:main) { $e = Retry-Walk { $script:main.FindFirst($TS::Descendants, $c) } }
    return $e
}
function Find-Named($name) {
    $c = Prop $AE::NameProperty $name
    foreach ($w in App-Windows) { $e = Retry-Walk { $w.FindFirst($TS::Descendants, $c) }; if ($e) { return $e } }
    return $null
}
function Find-NameStarting([string]$prefix) {
    foreach ($w in App-Windows) {
        $all = Retry-Walk { $w.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition) }
        foreach ($e in $all) { try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { } }
    }
    return $null
}
# The card's lines of finished jobs now, a line once (a window under another can list it twice).
function Count-FinishedLines {
    $names = [System.Collections.Generic.List[string]]::new()
    foreach ($w in App-Windows) {
        $all = Retry-Walk { $w.FindAll($TS::Descendants, (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Text))) }
        foreach ($e in $all) { try { if ($e.Current.Name.StartsWith('Finished · ')) { $names.Add($e.Current.Name + $e.Current.BoundingRectangle) } } catch { } }
    }
    return @($names | Select-Object -Unique).Count
}
function Find-TabItem([string]$name) {
    $c = And (Prop $AE::NameProperty $name) (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::TabItem))
    foreach ($w in App-Windows) { $e = Retry-Walk { $w.FindFirst($TS::Descendants, $c) }; if ($e) { return $e } }
    return $null
}
function Invoke-Element($e) {
    $p = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return }
    if ($e.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $p.Select(); return }
    throw "nothing to invoke on $($e.Current.Name)"
}
function Set-WindowState($state) {
    $p = $null
    if (-not $script:main.TryGetCurrentPattern($WP::Pattern, [ref]$p)) { throw 'the main window has no WindowPattern' }
    $p.SetWindowVisualState($state)
}
# Asks for the front, and waits up to 240 s for Windows to give it. Nothing is sent meanwhile: input
# would only make the wait longer.
function Show-App {
    $hwnd = [System.IntPtr]$script:main.Current.NativeWindowHandle
    $end = (Get-Date).AddSeconds(240)
    $waited = $false
    while ($true) {
        [void][LilHomeCheck.Win]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 800
        $holder = Get-WindowProcessId ([LilHomeCheck.Win]::GetForegroundWindow())
        if ($holder -eq $script:app.Id) { return }
        if ((Get-Date) -gt $end) { throw "the window didn't get the front in 240 s: $((Get-Process -Id $holder -ErrorAction SilentlyContinue).ProcessName) holds it" }
        if (-not $waited) {
            $waited = $true
            Write-Host "Windows keeps the front for now: $((Get-Process -Id $holder -ErrorAction SilentlyContinue).ProcessName) holds it"
        }
        Start-Sleep -Seconds 1
    }
}
# A real drag, with the window in front and under both ends. The cursor moves in steps, as the docking
# code needs to see it move.
function Drag-From([double]$fromX, [double]$fromY, [double]$toX, [double]$toY) {
    Assert-AppInFront
    Assert-AppAt ([int]$fromX) ([int]$fromY)
    Assert-AppAt ([int]$toX) ([int]$toY)
    [void][LilHomeCheck.Win]::SetCursorPos([int]$fromX, [int]$fromY)
    Start-Sleep -Milliseconds 100
    Assert-AppAt ([int]$fromX) ([int]$fromY)
    [LilHomeCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
    try {
        foreach ($i in 1..20) {
            [void][LilHomeCheck.Win]::SetCursorPos([int]($fromX + ($toX - $fromX) * $i / 20), [int]($fromY + ($toY - $fromY) * $i / 20))
            Start-Sleep -Milliseconds 40
        }
        Start-Sleep -Milliseconds 500
    } finally {
        [LilHomeCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
    }
    Start-Sleep -Milliseconds 500
}

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
function Close-App {
    if ($script:app -and -not $script:app.HasExited) {
        try {
            $wp = $null
            if ($script:main -and $script:main.TryGetCurrentPattern($WP::Pattern, [ref]$wp)) { $wp.Close() }
            if (-not $script:app.WaitForExit(10000)) { Stop-Process -Id $script:app.Id -Force }
        } catch { Stop-Process -Id $script:app.Id -Force -ErrorAction SilentlyContinue }
    }
}
# Waits for the kept time to differ from `from`; the seconds that took from `since`.
function Wait-KeptMoved([string]$from, [datetime]$since, [string]$what) {
    [void](Wait-For { if ((Get-Kept) -ne $from) { $true } } 30 $what)
    return ((Get-Date) - $since).TotalSeconds
}

$script:app = $null
$script:main = $null
# The second launches: stopped in the cleanup if they never ended.
$second = $null
$refused = $null
try {
    Enter-TestChannel
    Assert-ChannelFree
    $harbor = Join-Path $root 'places\harbor'
    [void](Invoke-Lil @('workspace', 'init', $harbor, '--default'))
    [void](Get-CorePid)
    $wsId = Get-WorkspaceId
    [void](Invoke-FinishedJob)
    $seed = '2026-01-01T00:00:00Z'
    Set-Kept $wsId $seed

    # (a) Minimized as soon as the window is there.
    $script:app = Start-Process -FilePath $image -WorkingDirectory $root -PassThru
    $script:main = Wait-For { Find-Window '' } 40 'the lil-image window'
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Minimized)
    $kept = Get-Kept
    if ($kept -ne $seed) { throw "the time had moved before the window could be minimized ($kept): nothing can be said" }
    Start-Sleep -Seconds 6
    Check ((Get-Kept) -eq $seed) '(a) minimized for 6 s: the kept time stays'
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Normal)
    $restored = Get-Date
    [void](Wait-For { Find-NameStarting 'Core connection: Connected' } 40 'the connection')
    [void](Wait-For { Find-NameStarting 'Finished · ' } 30 'the card''s line')
    Pass '(a) restored: the card lists the job'
    $took = Wait-KeptMoved $seed $restored 'the kept time to move'
    Check ($took -ge 2.5) "(a) the time moved 3 s or more after the restore ($took s)"
    Check ($null -ne (Find-NameStarting 'Finished · ')) '(a) the line stays as the time moves'

    # (b) Minimized for longer, with two more jobs finishing.
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Minimized)
    Start-Sleep -Seconds 1
    $kept = Get-Kept
    [void](Invoke-FinishedJob)
    [void](Invoke-FinishedJob)
    Start-Sleep -Seconds 4
    Check ((Get-Kept) -eq $kept) '(b) minimized, with jobs finishing: the kept time stays'
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Normal)
    $restored = Get-Date
    $n = 0
    try { [void](Wait-For { $n = Count-FinishedLines; if ($n -eq 2) { $true } } 15 'two lines') } catch { }
    Check ($n -eq 2) "(b) restored: the card lists the two jobs since, not the first ($n lines)"
    $took = Wait-KeptMoved $kept $restored 'the kept time to move'
    Check ($took -ge 2.5) "(b) the time moved 3 s or more after the restore ($took s)"

    # (c) A floating Home.
    $kept = Get-Kept
    Invoke-Element (Wait-For { Find-Named 'Float Home' } 10 'Float Home')
    $floating = Wait-For { Find-Window 'Home' } 10 'the floating Home'
    Pass '(c) Home floats in a window of its own'
    Start-Sleep -Seconds 1
    Check ((Count-FinishedLines) -eq 2) '(c) a floating Home keeps its lines'
    Check ((Get-Kept) -eq $kept) '(c) the float doesn''t move the kept time'
    [void](Invoke-FinishedJob)
    $n = 0
    try { [void](Wait-For { $n = Count-FinishedLines; if ($n -eq 3) { $true } } 15 'three lines') } catch { }
    Check ($n -eq 3) "(c) a job finishing while it floats is listed ($n lines)"
    [void](Wait-KeptMoved $kept (Get-Date) 'the kept time to move')
    Pass '(c) floating, it counts as on screen: the time moves'
    Invoke-Element (Wait-For { Find-Named 'Float Home' } 10 'Float Home, to dock it')
    [void](Wait-For { if (-not (Find-Window 'Home')) { $true } } 10 'Home to dock back')
    Start-Sleep -Seconds 1
    Check ((Count-FinishedLines) -eq 3) '(c) docked again: the same lines'

    # (d) Behind a tab: a real drag of Home's title bar onto the Jobs group.
    Show-App
    $bar = Wait-For { Find-Named 'Home' } 10 'Home''s title bar'
    $barCondition = And (Prop $AE::NameProperty 'Home') (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::TitleBar))
    $found = Retry-Walk { $script:main.FindFirst($TS::Descendants, $barCondition) }
    if ($found) { $bar = $found }
    $b = $bar.Current.BoundingRectangle
    $jobsTab = Wait-For { Find-TabItem 'Jobs' } 10 'the Jobs tab'
    $t = $jobsTab.Current.BoundingRectangle
    Drag-From ($b.X + 60) ($b.Y + $b.Height / 2) ($t.X + 100) ($t.Y + 300)
    $homeTab = $null
    try { $homeTab = Wait-For { Find-TabItem 'Home' } 10 'the Home tab' } catch { }
    if (-not $homeTab) {
        Fail '(d) the drag didn''t put Home in the Jobs tabs, so the tab case did not run'
    } else {
        Pass '(d) Home is in the Jobs tabs'
        Invoke-Element (Wait-For { Find-TabItem 'Jobs' } 10 'the Jobs tab')
        Start-Sleep -Seconds 4
        $kept = Get-Kept
        [void](Invoke-FinishedJob)
        Start-Sleep -Seconds 5
        Check ((Get-Kept) -eq $kept) '(d) behind a tab, with a job finishing: the kept time stays'
        Invoke-Element (Wait-For { Find-TabItem 'Home' } 10 'the Home tab')
        $shown = Get-Date
        $n = 0
        try { [void](Wait-For { $n = Count-FinishedLines; if ($n -eq 1) { $true } } 15 'one line') } catch { }
        # One line: Home was behind the tab long enough for the card to count from the kept time, and
        # that is past the second of the jobs it listed before, since it was written from a read made
        # 3 s after they showed.
        Check ($n -eq 1) "(d) Home's tab chosen: the card lists that job, counting from the kept time ($n lines)"
        $took = Wait-KeptMoved $kept $shown 'the kept time to move'
        Check ($took -ge 2.5) "(d) the time moved 3 s or more after Home showed ($took s)"
    }

    # (e) A real second launch, with Home closed.
    Invoke-Element (Wait-For { Find-Named 'Close Home' } 10 'Close Home')
    [void](Wait-For { if (-not (Find-Named 'Close Home')) { $true } } 10 'Home to close')
    $target = 'C:\art\crane.png'
    $b64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($target)).TrimEnd('=').Replace('+', '-').Replace('/', '_')
    $second = Start-Process -FilePath $image -ArgumentList @('--intent', 'edit', "lil-$channel`:open/$b64") -WorkingDirectory $root -PassThru
    Check ($second.WaitForExit(30000) -and $second.ExitCode -eq 0) '(e) the second launch hands over and exits 0'
    [void](Wait-For { Find-Named $target } 15 'the Opened card''s target')
    Pass '(e) the Opened card shows the target'
    Check ($null -ne (Find-Named 'Close Home')) '(e) the launch opened Home'
    $refused = Start-Process -FilePath $image -ArgumentList @('--no-activate', 'lil-test-nope:open/x') -WorkingDirectory $root -PassThru
    Check ($refused.WaitForExit(30000) -and $refused.ExitCode -eq 0) '(e) a refused link''s launch hands over and exits 0'
    [void](Wait-For { Find-NameStarting "This link isn't one LILITH opens" } 15 'the refusal')
    Pass '(e) the card says the link was refused'
} catch {
    Fail "the Home check stopped: $_"
    try {
        New-Item -ItemType Directory -Force $DiagDir | Out-Null
        if ($script:app -and -not $script:app.HasExited) { Save-UiaTree (Join-Path $DiagDir 'uia-tree.txt') }
        $logs = Join-Path $root "local\LILITH-$channel\logs"
        if (Test-Path -LiteralPath (Join-Path $DiagDir 'logs')) { Remove-Item -Recurse -Force -LiteralPath (Join-Path $DiagDir 'logs') }
        if (Test-Path -LiteralPath $logs) { Copy-Item -LiteralPath $logs -Destination (Join-Path $DiagDir 'logs') -Recurse -Force }
        Write-Host "diagnostics in $DiagDir"
    } catch { Write-Host "the diagnostics weren't saved: $_" }
} finally {
    foreach ($p in $second, $refused) {
        if ($p -and -not $p.HasExited) {
            Write-Host "stopping a second launch that never ended, pid $($p.Id)"
            try { Stop-Process -Id $p.Id -Force } catch { Fail "cleanup: stopping a second launch: $_" }
        }
    }
    try { Close-App } catch { Fail "cleanup: closing lil-image: $_" }
    try { Exit-TestChannel } catch { Fail "cleanup: $_" }
    try { Set-Content -LiteralPath $state -Value 'done' } catch { Write-Host "note: the pop-up wasn't told: $_" }
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
exit ([int]($failures.Count -gt 0))
