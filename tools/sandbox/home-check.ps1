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
           floats is listed there, and the time moves with the app in front (Qt calls a floating window
           active while any of the app's windows has the focus). The main window minimized with Home
           floating: Windows hides the floating window with its owner; the step prints what UI
           Automation says of it, a job finishes, and the time stays, as no window of the app is
           active, whatever Qt's visibility says. Docked again: the same lines.
       (d) Home behind a tab: a real drag of its title bar onto the drop indicator at the centre of
           the Jobs group puts it in the Jobs tabs, and the Jobs tab is chosen. A job finishes: the
           time stays. Home's tab chosen: the job is listed, and the time moves 3 s or more later. If
           the drag doesn't take, this fails by name and prints what it aimed at. The tab's effect on
           Home is QML's own whatever the platform, and tst_home's
           test_last_seen_waits_until_really_on_screen covers it offscreen.
       (f) another program's window in front (a small window of this script's), with Home on screen
           behind it: a job finishes, the line shows, and the time stays. The app back in front: the
           time moves.
       (e) a real second launch, with Home closed: it hands over and exits 0, Home opens, and the
           Opened card shows the target; a refused link says so there.
  Each step that expects lines prints the count, each line as the app's UI tree names it, the kept
  time, each job's end time and which jobs the core's `since` still lists at that time. A time check
  only passes beside the line count it depends on. When the lines are missing, the check is instead
  that the time hasn't moved past those jobs, which is what would lose them for good.
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
# A job that finishes, from another session: its id once it has. Its end time is kept to be printed.
$jobEnds = [ordered]@{}
function Invoke-FinishedJob {
    $id = (Invoke-Lil @('call', 'test.job', '{"lane":"cpu"}') | ConvertFrom-Json -DateKind String).structured.job.job_id
    $done = Invoke-Lil @('call', 'job.wait', ('{"job_id":"' + $id + '","timeout_s":30}')) | ConvertFrom-Json -DateKind String
    if ($done.structured.job.status -ne 'succeeded') { throw "the test job ended $($done.structured.job.status)" }
    $jobEnds[$id] = "$($done.structured.job.updated)"
    return $id
}
# The ids the core's `jobs.list {since}` gives at time `at`: a job not among them is lost to the card.
function Get-SinceIds([string]$at) {
    $json = Invoke-Lil @('call', 'jobs.list', ('{"since":"' + $at + '","limit":500}')) | ConvertFrom-Json -DateKind String
    return @($json.structured.jobs | ForEach-Object { $_.job_id })
}
# Asks `count` until it gives `want`, for at most `sec` seconds: the last count, -1 if it threw. The
# asking is in this function, so its answer can't be left behind in a script block's own scope.
function Wait-Count([scriptblock]$count, [int]$want, [int]$sec) {
    $end = (Get-Date).AddSeconds($sec)
    while ($true) {
        $n = -1
        try { $n = [int](& $count) } catch { Write-Host "the count threw: $_" }
        if ($n -eq $want -or (Get-Date) -ge $end) { return $n }
        Start-Sleep -Milliseconds 200
    }
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
# The count's rules, with no app: what it asks reaches the caller, and a throw isn't a count.
function Test-WaitCount {
    Check ((Wait-Count { 2 } 2 1) -eq 2) 'self-test: a count reached is returned'
    Check ((Wait-Count { 0 } 2 1) -eq 0) 'self-test: a count not reached is returned as it stands'
    Check ((Wait-Count { throw 'no' } 2 1) -eq -1) 'self-test: a count that throws reads as unknown, not as none'
}

if (-not ('LilHomeCheck.Win' -as [type])) {
    Add-Type -Namespace LilHomeCheck -Name Win -MemberDefinition @'
public struct POINT { public int X; public int Y; }
[DllImport("user32.dll")] public static extern System.IntPtr WindowFromPoint(POINT point);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint processId);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT point);
[DllImport("user32.dll")] public static extern uint GetDpiForSystem();
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
    Test-WaitCount
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
        Check ($jobEnds[$job] -match '^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d') "dry run: its end time reads as the core wrote it ($($jobEnds[$job]))"
        Check ((Get-SinceIds '2026-01-01T00:00:00Z') -contains $job) 'dry run: the core lists it from a time before it'
        Check ((Get-SinceIds '2099-01-01T00:00:00Z') -notcontains $job) 'dry run: and not from a time after it'
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
               '-What', 'checks Home (windows that minimize and float, and a real drag)', '-WarnSec', '10', '-RunSec', '150',
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
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
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
# The card's lines of finished jobs now, a line once (a window under another can list it twice), as
# the app's UI tree has them: `name`, its `rect`, and whether UI Automation calls it off screen.
function Get-FinishedLines {
    $seen = @{}
    foreach ($w in App-Windows) {
        $all = Retry-Walk { $w.FindAll($TS::Descendants, (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Text))) }
        foreach ($e in $all) {
            try {
                $c = $e.Current
                if ($c.Name.StartsWith('Finished · ')) {
                    $seen["$($c.Name)$($c.BoundingRectangle)"] = "$($c.Name) | at $($c.BoundingRectangle) | offscreen=$($c.IsOffscreen)"
                }
            } catch { }
        }
    }
    return @($seen.Values)
}
# What one run needs in order to decide: the count, the lines, the kept time, the job's end times, and
# which of them the core still lists at the kept time. Only the app's own names and ids.
function Write-LinesReport([string]$step, [int]$count) {
    Write-Host "$step line count: $count"
    try { Get-FinishedLines | ForEach-Object { Write-Host "$step line: $_" } } catch { Write-Host "$step the lines couldn't be read: $_" }
    $kept = Get-Kept
    Write-Host "$step kept time: $kept"
    foreach ($id in $jobEnds.Keys) { Write-Host "$step job $id ended $($jobEnds[$id])" }
    try { Write-Host "$step the core lists from the kept time: $((Get-SinceIds $kept) -join ', ')" } catch { Write-Host "$step the core's list couldn't be read: $_" }
}
# The pair that must never come apart: the card shows `want` lines and the time then moves past the
# `jobs` on them; or, if the lines aren't there, the time has not moved past those jobs, so they are
# only late, not lost. `shownAt`, when given, is when Home came back: the move must be 3 s or more on.
function Check-LinesThenTime([string]$step, [int]$want, [string[]]$jobs, [string]$keptBefore, $shownAt) {
    $n = Wait-Count { @(Get-FinishedLines).Count } $want 15
    Write-LinesReport $step $n
    if ($n -eq $want) {
        Pass "$step the card lists $want line(s)"
        $took = Wait-KeptMoved $keptBefore $(if ($shownAt) { $shownAt } else { Get-Date }) 'the kept time to move'
        if ($shownAt) { Check ($took -ge 2.5) "$step the time moved 3 s or more after Home showed ($took s)" } else { Pass "$step the time moves" }
        $at = Get-Kept
        $still = @(Get-SinceIds $at | Where-Object { $jobs -contains $_ })
        Check ($still.Count -eq 0) "$step the time ($at) moved past the jobs shown, so they don't come back (still listed: $($still -join ', '))"
    } else {
        Fail "$step the card shows $n line(s), not $want (-1: the tree couldn't be read); whether the time may move isn't judged"
        # Long enough for a write that shouldn't happen.
        Start-Sleep -Seconds 5
        Write-LinesReport $step $n
        $at = Get-Kept
        $listed = @(Get-SinceIds $at)
        $lost = @($jobs | Where-Object { $listed -notcontains $_ })
        Check ($lost.Count -eq 0) "$step with the lines missing, the time ($at) hasn't moved past those jobs (past it: $($lost -join ', '))"
    }
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
# A window of this script's, in front: what another program would be. It waits for the front as
# Show-App does. Its messages are pumped only as far as it needs to stay a window.
function Show-Cover {
    $form = [System.Windows.Forms.Form]::new()
    $form.Text = 'home-check: another program'
    $form.Width = 320
    $form.Height = 120
    $form.ShowInTaskbar = $true
    $form.Show()
    $end = (Get-Date).AddSeconds(240)
    while ($true) {
        [System.Windows.Forms.Application]::DoEvents()
        $form.Activate()
        [void][LilHomeCheck.Win]::SetForegroundWindow($form.Handle)
        Start-Sleep -Milliseconds 500
        [System.Windows.Forms.Application]::DoEvents()
        if ([LilHomeCheck.Win]::GetForegroundWindow() -eq $form.Handle) { return $form }
        if ((Get-Date) -gt $end) { $form.Close(); throw 'the check''s own window didn''t get the front in 240 s' }
    }
}
# What UI Automation says of a window of the app: only its state.
function Write-WindowState([string]$step, $window) {
    try {
        $c = $window.Current
        $p = $null
        $visual = if ($window.TryGetCurrentPattern($WP::Pattern, [ref]$p)) { $p.Current.WindowVisualState } else { 'no WindowPattern' }
        Write-Host "$step the floating window: state $visual, offscreen=$($c.IsOffscreen), at $($c.BoundingRectangle)"
    } catch { Write-Host "$step the floating window can't be read: $_" }
}
# A real drag, with the window in front and under both ends. The cursor moves in steps, as the docking
# code needs to see it move, and the end is gone over again: the drop indicators are made as it hovers,
# and need a move over them. It says where the cursor really got to, as a scaled desktop would differ.
function Drag-From([double]$fromX, [double]$fromY, [double]$toX, [double]$toY) {
    Assert-AppInFront
    Assert-AppAt ([int]$fromX) ([int]$fromY)
    Assert-AppAt ([int]$toX) ([int]$toY)
    [void][LilHomeCheck.Win]::SetCursorPos([int]$fromX, [int]$fromY)
    Start-Sleep -Milliseconds 100
    Assert-AppAt ([int]$fromX) ([int]$fromY)
    [LilHomeCheck.Win]::mouse_event(2, 0, 0, 0, [System.UIntPtr]::Zero)
    try {
        foreach ($i in 1..30) {
            [void][LilHomeCheck.Win]::SetCursorPos([int]($fromX + ($toX - $fromX) * $i / 30), [int]($fromY + ($toY - $fromY) * $i / 30))
            Start-Sleep -Milliseconds 40
        }
        Start-Sleep -Milliseconds 500
        foreach ($d in 6, 3, 0) {
            [void][LilHomeCheck.Win]::SetCursorPos([int]$toX + $d, [int]$toY)
            Start-Sleep -Milliseconds 150
        }
        Start-Sleep -Milliseconds 300
        $at = [LilHomeCheck.Win+POINT]::new()
        [void][LilHomeCheck.Win]::GetCursorPos([ref]$at)
        Write-Host "(d) dropping at $($at.X),$($at.Y), aimed at $([int]$toX),$([int]$toY); system DPI $([LilHomeCheck.Win]::GetDpiForSystem())"
    } finally {
        [LilHomeCheck.Win]::mouse_event(4, 0, 0, 0, [System.UIntPtr]::Zero)
    }
    Start-Sleep -Milliseconds 500
}
function Find-Pane([string]$name) {
    $c = And (Prop $AE::NameProperty $name) (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::Pane))
    foreach ($w in App-Windows) { $e = Retry-Walk { $w.FindFirst($TS::Descendants, $c) }; if ($e) { return $e } }
    return $null
}
# The tabs the app's windows have, by name.
function Get-TabNames {
    $names = foreach ($w in App-Windows) {
        $all = Retry-Walk { $w.FindAll($TS::Descendants, (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::TabItem))) }
        foreach ($e in $all) { try { $e.Current.Name } catch { } }
    }
    return ($names -join ', ')
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
    $first = Invoke-FinishedJob
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
    # The time moves only in the active window.
    Show-App
    [void](Wait-For { Find-NameStarting 'Core connection: Connected' } 40 'the connection')
    Check-LinesThenTime '(a)' 1 @($first) $seed $restored
    Check ((@(Get-FinishedLines).Count) -eq 1) '(a) the line stays as the time moves'

    # (b) Minimized for longer, with two more jobs finishing.
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Minimized)
    Start-Sleep -Seconds 1
    $kept = Get-Kept
    $jobB = @((Invoke-FinishedJob), (Invoke-FinishedJob))
    Start-Sleep -Seconds 4
    Check ((Get-Kept) -eq $kept) '(b) minimized, with jobs finishing: the kept time stays'
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Normal)
    $restored = Get-Date
    Show-App
    # The two since, not the first: the card now counts from the kept time.
    Check-LinesThenTime '(b)' 2 $jobB $kept $restored

    # (c) A floating Home.
    $kept = Get-Kept
    Invoke-Element (Wait-For { Find-Named 'Float Home' } 10 'Float Home')
    $floating = Wait-For { Find-Window 'Home' } 10 'the floating Home'
    Pass '(c) Home floats in a window of its own'
    Start-Sleep -Seconds 1
    $n = @(Get-FinishedLines).Count
    Check ($n -eq 2) "(c) a floating Home keeps its lines ($n)"
    Check ((Get-Kept) -eq $kept) '(c) the float doesn''t move the kept time'
    # The main window minimized: Windows hides the floating window with its owner. Whatever Qt's
    # visibility says, no window of the app is active, so the time stays. It is read before the job, so
    # that a job can't be the reason.
    $keptC = Get-Kept
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Minimized)
    Start-Sleep -Seconds 1
    Write-WindowState '(c) main minimized:' $floating
    $jobC = Invoke-FinishedJob
    Start-Sleep -Seconds 5
    Check ((Get-Kept) -eq $keptC) '(c) the main window minimized, Home floating, with a job finishing: the kept time stays'
    Set-WindowState ([System.Windows.Automation.WindowVisualState]::Normal)
    Show-App
    Write-WindowState '(c) main restored:' $floating
    # Floating, with the app in front: the job is listed there, and the time moves past it. How many
    # lines says what Qt made of the owner's minimizing: 3 if the floating window stayed "visible" to
    # it, so the card kept its time; 1 if it went hidden for 3 s or more, so the card moved to the kept
    # time, as after any time away. Either is right; the job's line is the point.
    $wantC = Wait-Count { @(Get-FinishedLines).Count } 3 8
    if ($wantC -ne 3) { $wantC = 1 }
    Write-Host "(c) with the main window minimized, Qt's visibility of the floating window $(if ($wantC -eq 3) { 'didn''t change (3 lines expected)' } else { 'went hidden (1 line expected)' })"
    Check-LinesThenTime '(c)' $wantC @($jobC) $kept $null
    Invoke-Element (Wait-For { Find-Named 'Float Home' } 10 'Float Home, to dock it')
    [void](Wait-For { if (-not (Find-Window 'Home')) { $true } } 10 'Home to dock back')
    Start-Sleep -Seconds 1
    $n = @(Get-FinishedLines).Count
    Check ($n -eq $wantC) "(c) docked again: the same lines ($n)"

    # (d) Behind a tab: a real drag of Home's title bar onto the Jobs group's centre, where KDDockWidgets
    # puts the indicator that makes a tab (ClassicIndicatorsOverlay: the hovered group's centre). The
    # group runs from its title bar to the bottom of its panel. Home's title bar is pressed left of its
    # Float button: the rest of the bar is the drag area.
    Show-App
    $floatHome = (Wait-For { Find-Named 'Float Home' } 10 'Float Home').Current.BoundingRectangle
    $closeJobs = (Wait-For { Find-Named 'Close Jobs' } 10 'Close Jobs').Current.BoundingRectangle
    $jobsPane = (Wait-For { Find-Pane 'Jobs' } 10 'the Jobs panel').Current.BoundingRectangle
    $fromX = $floatHome.X - 60
    $fromY = $floatHome.Y + $floatHome.Height / 2
    $groupTop = $closeJobs.Y - 4
    $toX = $jobsPane.X + $jobsPane.Width / 2
    $toY = ($groupTop + $jobsPane.Bottom) / 2
    Write-Host "(d) Float Home at $floatHome, Close Jobs at $closeJobs, the Jobs panel at $jobsPane; tabs before: $(Get-TabNames)"
    Write-Host "(d) dragging from $([int]$fromX),$([int]$fromY) to $([int]$toX),$([int]$toY)"
    Drag-From $fromX $fromY $toX $toY
    $homeTab = $null
    try { $homeTab = Wait-For { Find-TabItem 'Home' } 10 'the Home tab' } catch { }
    Write-Host "(d) tabs after: $(Get-TabNames); a window of Home's own: $([bool](Find-Window 'Home'))"
    if (-not $homeTab) {
        Fail '(d) the drag didn''t put Home in the Jobs tabs, so the tab case did not run here (tst_home''s test_last_seen_waits_until_really_on_screen covers it offscreen)'
    } else {
        Pass '(d) Home is in the Jobs tabs'
        Invoke-Element (Wait-For { Find-TabItem 'Jobs' } 10 'the Jobs tab')
        Start-Sleep -Seconds 4
        $kept = Get-Kept
        $jobD = Invoke-FinishedJob
        Start-Sleep -Seconds 5
        Check ((Get-Kept) -eq $kept) '(d) behind a tab, with a job finishing: the kept time stays'
        Invoke-Element (Wait-For { Find-TabItem 'Home' } 10 'the Home tab')
        $shown = Get-Date
        # One line: Home was behind the tab long enough for the card to count from the kept time, and
        # that is past the second of the jobs it listed before, since it was written from a read made
        # 3 s after they showed.
        Check-LinesThenTime '(d)' 1 @($jobD) $kept $shown
    }

    # (f) Another program in front, with Home on screen behind it. The main window, and Home in it if
    # (d) took, or else its own window, stays open.
    if (Find-TabItem 'Home') { Invoke-Element (Find-TabItem 'Home') }
    Start-Sleep -Seconds 4
    $kept = Get-Kept
    $wantF = @(Get-FinishedLines).Count + 1
    $cover = Show-Cover
    try {
        $jobF = Invoke-FinishedJob
        $n = Wait-Count { [System.Windows.Forms.Application]::DoEvents(); @(Get-FinishedLines).Count } $wantF 15
        Write-LinesReport '(f)' $n
        Check ($n -eq $wantF) "(f) another program in front: the line still shows ($n)"
        $until = (Get-Date).AddSeconds(5)
        while ((Get-Date) -lt $until) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 200 }
        Check ((Get-Kept) -eq $kept) '(f) another program in front, with a job finishing: the kept time stays'
    } finally {
        # Closing it hands the front back at once, before Show-App's own wait: the time counts from here.
        $backAt = Get-Date
        $cover.Close()
        $cover.Dispose()
    }
    Show-App
    $took = Wait-KeptMoved $kept $backAt 'the kept time to move'
    Check ($took -ge 2.5) "(f) the app back in front: the time moves 3 s or more later ($took s)"
    $still = @(Get-SinceIds (Get-Kept) | Where-Object { $_ -eq $jobF })
    Check ($still.Count -eq 0) '(f) and past that job'

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
