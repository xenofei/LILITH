<#
.SYNOPSIS
  The hands-off check of the Settings panel and the first-start card in the deployed lil-image, on a
  test channel (APP-022, APP-146; P4a-R26):
    pwsh -NoProfile -File tools/sandbox/settings-check.ps1 [-DryRun]

.DESCRIPTION
  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown).
  2. A test channel under a new temporary root (LIL_CHANNEL=test-settingscheck, LIL_TEST_ROOT), with
     the deployed lil-image on it, driven through UI Automation. Its core's desktop is FakeDesktop,
     which test.desktop sets, so what the core says is the test's; what Set up writes is real, under
     the test channel's own registry root and the temporary root's Programs folder, which the shell
     never reads. A channel already taken (a pipe of that channel, whichever deploy's core or app
     serves it, or its registry root left by a crashed run) is refused: nothing is started, and
     neither is touched. Buttons and switches are pressed through
     UI Automation's Invoke or Toggle, Qt's accessible press and not mouse or key input; only the
     panel keys (Ctrl+, and Ctrl+4) are real input. It checks, on disk, in the registry or through UIA:
       (a) the first start: the card offers Set up and Not now; state.json is on disk; Not now puts it
           away, and the next start doesn't show it;
       (b) state.json removed, so a first start again: the card's Set up, pressed through Invoke, writes
           the shortcut, exactly to the deployed lil-image.exe, the channel's AUMID and CLSID registrations
           (what the shortcut itself carries of them is desktop_set_up_writes_the_shortcut_and_handlers'
           to check), and the own scheme's command, exactly `"<lil-image.exe>" "%1"`, and no `.lilref` under the test
           channel's classes root. Under the real classes root, read only, before and after: `.lilref`'s
           value, the real channels' schemes, their AUMID registrations and the CLSIDs those name are
           unchanged, and neither the test channel's scheme, AUMID nor CLSID is there;
       (c) the core then told it is set up: Settings says nothing it can check stands in the way and has no Set up;
       (d) a switch in Settings, pressed through Toggle or Invoke, reaches the core and suite.toml;
       (e) Windows' reason: "Open Windows notification settings", pressed through Invoke, opens Windows'
           Settings. Only when no Settings process is running, so the one it starts is the one this
           check closes; if one is, this part counts as NOT CHECKED, neither a pass nor a failure.
  On a failure, the app's UIA tree and the test channel's logs go to -DiagDir first. The tree holds
  no names from a dialog's file list, navigation pane, address bar or breadcrumb. Real keys go only
  while the app's window is in front; otherwise the check fails and sends nothing.
  3. lil-image is closed, the test core stopped (only the PIDs recorded here), and what the run wrote
     removed: the test channel's registry root (all that Set up writes is under it), temporary root
     and Qt cache folder (%LOCALAPPDATA%\LILITH-test-settingscheck). A key of the channel's name
     outside its root is a failure of the check, removed only if it wasn't there when the run began.
     The pop-up is told it is done. Anything left behind fails the run. Exit 0 only when every check
     passed and the cleanup was complete.
  Every `lil` call is limited to 30 s (sandbox-common.ps1's Invoke-LilTimed), and no input or wait goes
  on past the pop-up's cap less 30 s: the run stops with "out of the stretch's time" and cleans up. A
  card or button that "went" is proved over three whole walks 300 ms apart, each of which still shows
  the connection indicator.
  -DryRun does step 2's channel set-up, the real registrations' read and the guards' self-test only,
  with no pop-up, no window and no input.
#>
param(
    # The repository whose build/dev/deploy is checked: by default this script's own.
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    # Where a failed run leaves the app's UIA tree and the logs: by default build\dev\settings-check.
    [string]$DiagDir = '',
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $Deploy) { $Deploy = Join-Path $Repo 'build\dev\deploy' }
if (-not $DiagDir) { $DiagDir = Join-Path $Repo 'build\dev\settings-check' }
$lil = Join-Path $Deploy 'lil.exe'
$image = Join-Path $Deploy 'lil-image.exe'
$results = [System.Collections.Generic.List[string]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
function Pass([string]$what) { $what = Hide-Paths $what; $results.Add("PASS $what"); Write-Host "PASS $what" }
function Fail([string]$what) { $what = Hide-Paths $what; $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what" }
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }
# Invoke-LilTimed, the stretch's deadline, Retry-Walk, Assert-Gone and the masked log copy.
. (Join-Path $PSScriptRoot 'sandbox-common.ps1')

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-settingscheck'
$scheme = "lil-$channel"
$capitalised = $channel.Substring(0, 1).ToUpperInvariant() + $channel.Substring(1)
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-settingscheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$classes = "HKCU:\Software\LILITH-$channel\Classes"
$realClasses = 'HKCU:\Software\Classes'
# Keys of the channel's name outside its registry root: nothing should make them.
$strayKeys = "$realClasses\$scheme", "$realClasses\AppUserModelId\LILITH.Suite.$capitalised",
             "HKCU:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\LILITH.Suite.$capitalised"
# Every core PID seen on this test channel: the only core processes this script may stop.
$corePids = [System.Collections.Generic.HashSet[int]]::new()
$script:channelTaken = $false
$script:strayAtStart = @()
function Enter-TestChannel {
    # First, before anything that can throw: every `lil` from here on is on the test channel.
    $env:LIL_CHANNEL = $channel
    $env:LIL_TEST_ROOT = $root
    New-Item -ItemType Directory $root | Out-Null
    foreach ($s in 'roaming', 'local', 'profile') { New-Item -ItemType Directory (Join-Path $root $s) | Out-Null }
    # The pipe's name comes from the channel alone, so a core left by a crashed run would serve this
    # run from its old root; and its registry root would pass for what this run wrote. The pipes are
    # listed, not asked: another deploy's core makes `lil --no-start status` fail as if there were none,
    # and cleanup's `lil core stop` could then drain it.
    if ((Get-ChannelPipes $channel).Count -gt 0) {
        $script:channelTaken = $true
        throw "a core or an app already serves $channel, from this deploy or another. It isn't this run's, so nothing was started and it wasn't stopped: end it, then run this again"
    }
    if (Test-Path "HKCU:\Software\LILITH-$channel") {
        $script:channelTaken = $true
        throw "HKCU:\Software\LILITH-$channel is there already, so it isn't this run's and it wasn't touched: a crashed run left it. Remove it, then run this again"
    }
    $script:strayAtStart = @($strayKeys | Where-Object { Test-Path $_ })
    Set-Location -LiteralPath $root
}
# What the real channels have registered, read only. `name = value` lines, so a run can compare before
# and after; printed by name alone, since a value holds a path.
function Get-RealRegistrations {
    $default = { param($key) try { (Get-ItemProperty -LiteralPath $key -ErrorAction Stop).'(default)' } catch { $null } }
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add(".lilref = $(& $default "$realClasses\.lilref")")
    foreach ($s in 'lil', 'lil-dev') { $lines.Add("$s = $(& $default "$realClasses\$s\shell\open\command")") }
    foreach ($type in 'stable', 'dev') {
        $lines.Add("LILITH.lilref.$type = $(& $default "$realClasses\LILITH.lilref.$type\shell\open\command")")
    }
    foreach ($aumid in @(Get-ChildItem -LiteralPath "$realClasses\AppUserModelId" -ErrorAction SilentlyContinue |
                         Where-Object { $_.PSChildName -like 'LILITH.Suite.*' } | Sort-Object PSChildName)) {
        $p = Get-ItemProperty -LiteralPath $aumid.PSPath
        $lines.Add("$($aumid.PSChildName) = $($p.DisplayName) | $($p.CustomActivator)")
        if ($p.CustomActivator) {
            $lines.Add("CLSID $($p.CustomActivator) = $(& $default "$realClasses\CLSID\$($p.CustomActivator)\LocalServer32")")
        }
    }
    return @($lines)
}
function Compare-RealRegistrations([string[]]$before, [string[]]$after) {
    $changed = @(Compare-Object $before $after | ForEach-Object { ($_.InputObject -split ' = ', 2)[0] } | Sort-Object -Unique)
    Check ($changed.Count -eq 0) "(b) the real registrations are as they were, $($before.Count) read before and after$(if ($changed.Count) { ': changed: ' + ($changed -join ', ') })"
}
function Invoke-Lil([string[]]$a) {
    $r = Invoke-LilTimed $a
    if ($r.ExitCode -ne 0) { throw "lil $($a -join ' ') exited $($r.ExitCode): $($r.Output -join ' ')" }
    return ($r.Output -join "`n")
}
# The running test core's PID, from its own hello (recorded), or 0 with none.
function Get-CorePid {
    $r = Invoke-LilTimed @('--no-start', 'status')
    if ($r.ExitCode -ne 0) { return 0 }
    $m = [regex]::Match(($r.Output -join "`n"), 'pid (\d+), channel (\S+?)\)')
    if (-not $m.Success -or $m.Groups[2].Value -cne $channel) { return 0 }
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
    if ($script:channelTaken) {
        Write-Host 'the test channel is not this run''s: its core, its keys and its cache stay'
        try { Set-Location -LiteralPath $Repo } catch { }
        Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
        for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $root); ++$i) {
            try { Remove-Item -Recurse -Force -LiteralPath $root } catch { Start-Sleep -Milliseconds 500 }
        }
        return
    }
    try { [void](Get-CorePid) } catch { }
    try { [void](Invoke-LilTimed @('core', 'stop', '--timeout-secs', '20')) } catch { }
    Start-Sleep -Milliseconds 500
    try { Stop-RecordedCores } catch { Fail "cleanup: stopping the test core: $_" }
    try { Set-Location -LiteralPath $Repo } catch { }
    Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
    # What this run wrote: its registry root, with all that Set up wrote under it. A key of the
    # channel's name elsewhere is this run's only if it wasn't there when the run began, and then it
    # is a failure as well: FakeDesktop posts no toast, and Set up writes under the root.
    $keys = @("HKCU:\Software\LILITH-$channel")
    foreach ($k in $strayKeys) {
        if ((Test-Path $k) -and $script:strayAtStart -notcontains $k) {
            Fail "cleanup: $k was written by this run, outside the channel's root"
            $keys += $k
        }
    }
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
    # A core that hung before any status answered was never recorded: its pipe is the last trace.
    Assert-PipeFree $channel
}
# The fake desktop's state, from another session: the core reads it at once.
function Set-FakeDesktop([string]$json) { [void](Invoke-Lil @('call', 'test.desktop', $json)) }
function Get-DesktopGet { return ((Invoke-Lil @('call', 'desktop.get', '{"fresh":true}')) | ConvertFrom-Json).structured }
function Get-StateFile {
    $found = Get-ChildItem -LiteralPath (Join-Path $root 'local') -Recurse -Force -Filter 'state.json' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -like '*\apps\image\state.json' } | Select-Object -First 1
    return $found
}
function Get-SuiteToml {
    return Get-ChildItem -LiteralPath (Join-Path $root 'roaming') -Recurse -Force -Filter 'suite.toml' -ErrorAction SilentlyContinue | Select-Object -First 1
}

if (-not ('LilSettingsCheck.Win' -as [type])) {
    Add-Type -Namespace LilSettingsCheck -Name Win -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint processId);
'@
}
# Real input only ever goes to this app: anything else in front, and the check fails and sends nothing.
function Get-WindowProcessId([System.IntPtr]$hwnd) {
    [uint32]$owner = 0
    [void][LilSettingsCheck.Win]::GetWindowThreadProcessId($hwnd, [ref]$owner)
    return [int]$owner
}
function Assert-AppInFront {
    Assert-InStretch
    for ($i = 0; $i -lt 5; ++$i) {
        if ((Get-WindowProcessId ([LilSettingsCheck.Win]::GetForegroundWindow())) -eq $script:app.Id) { return }
        Start-Sleep -Milliseconds 200
    }
    throw 'no keys sent: another program''s window is in front'
}
function Send-Checked([string]$keys) {
    Assert-AppInFront
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}
# The guard, without any input: a process that owns no window is never in front.
function Self-Test {
    $script:app = [pscustomobject]@{ Id = -1 }
    $refused = 0
    try { Assert-AppInFront } catch { if ("$_" -like 'no keys sent*') { ++$refused } }
    # Past the stretch's deadline, the guard stops before it looks at any window.
    $script:stopAt = [DateTime]::UtcNow.AddSeconds(-1)
    $late = $false
    try { Assert-AppInFront } catch { $late = "$_" -like "out of the stretch's time*" }
    $script:stopAt = $null
    $script:app = $null
    Check ($refused -eq 1) 'self-test: the guard refuses a window that isn''t the app''s'
    Check $late 'self-test: past the stretch''s deadline the guard stops'
}

if ($DryRun) {
    Add-Type -AssemblyName System.Windows.Forms
    Self-Test
    try {
        Enter-TestChannel
        [void](Invoke-Lil @('status'))
        $id = Get-CorePid
        Check ($id -gt 0) "dry run: the test core runs, pid $id"
        $real = Get-RealRegistrations
        Check ($real.Count -ge 5) "dry run: the real registrations read, $($real.Count) lines, read only"
        Set-FakeDesktop '{"handlers":false,"shortcut":false}'
        $d = Get-DesktopGet
        Check ($d.notification_path.handlers -eq 'missing' -and $d.notification_path.shortcut -eq 'missing') 'dry run: test.desktop sets what the core says'
        Set-FakeDesktop '{"handlers":true,"shortcut":true,"toasts":"disabled_for_app"}'
        $d = Get-DesktopGet
        Check ($d.notification_path.toasts_reason -eq 'disabled_for_app') 'dry run: and Windows'' reason'
        Check (Test-Path -LiteralPath $image) "dry run: $image is deployed"
        Check (-not (Test-Path "$realClasses\$scheme")) 'dry run: nothing under the real classes root'
    } catch { Fail "dry run: $_" } finally {
        try { Exit-TestChannel } catch { Fail "cleanup: $_" }
    }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-settingscheck-{0}.state" -f $PID)
$capSec = 300
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', 'checks the Settings panel (windows, real key presses, and Windows'' Settings once)', '-WarnSec', '10', '-RunSec', '120',
               '-CapSec', [string]$capSec, '-StateFile', $state, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
[void][System.Diagnostics.Process]::Start($psi)
$deadline = (Get-Date).AddSeconds(30)
while (-not (Test-Path -LiteralPath "$state.shown")) {
    if (Test-Path -LiteralPath "$state.error") { throw "the hands-off pop-up failed: $(Get-Content -Raw "$state.error")" }
    if ((Get-Date) -gt $deadline) { throw 'the hands-off pop-up was not on screen within 30 s, so nothing ran' }
    Start-Sleep -Milliseconds 250
}
# From here no input goes after the pop-up's cap less 30 s.
Start-Stretch $capSec
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
        Assert-InStretch
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
    return $AE::RootElement.FindFirst($TS::Children, $c)
}
function Show-Element($e) {
    Assert-InStretch
    try { [void][LilSettingsCheck.Win]::SetForegroundWindow([System.IntPtr]$e.Current.NativeWindowHandle) } catch { }
    Start-Sleep -Milliseconds 300
}
function Show-App { Show-Element $script:main }
function Send-Keys([string]$keys) { Show-App; Send-Checked $keys; Start-Sleep -Milliseconds 200 }
function Invoke-Element($e) {
    $p = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return }
    if ($e.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$p)) { $p.Toggle(); return }
    Assert-InStretch
    try { $e.SetFocus() } catch { Show-App }
    Send-Checked ' '
}
function App-Windows {
    @($AE::RootElement.FindAll($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id)))
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
function Is-Enabled($e) { try { return $e.Current.IsEnabled } catch { return $false } }
function Wait-Enabled([string]$name, [int]$sec) {
    return Wait-For { $b = Find-Named $name; if ($b -and (Is-Enabled $b)) { $b } } $sec "'$name', enabled"
}
function Is-Toggled($e) {
    $p = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$p)) { return $p.Current.ToggleState -eq 'On' }
    return $null
}

# On a failure: every window of the app as UIA shows it, at most 4000 elements. Names only for the
# app's own Qt elements (their ids start "QGuiApplication"): nothing of a system dialog.
function Save-UiaTree([string]$path) {
    $lines = [System.Collections.Generic.List[string]]::new()
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $walk = {
        param($e, [int]$depth)
        if ($lines.Count -ge 4000) { return }
        try {
            $c = $e.Current
            $ours = $c.AutomationId.StartsWith('QGuiApplication') -or ($depth -eq 0 -and $c.Name -like 'LILITH*')
            $shown = Hide-Paths $c.Name
            $name = if (-not $ours) { '' } elseif ($shown.Length -gt 100) { $shown.Substring(0, 100) + '…' } else { $shown }
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
function Start-App {
    $script:app = Start-Process -FilePath $image -WorkingDirectory $root -PassThru
    $script:main = $null
    [void](Wait-For { Find-Window $script:app.Id '' } 40 'the lil-image window')
    Start-Sleep -Seconds 1
    $wins = App-Windows
    $script:main = ($wins | Where-Object { $_.Current.Name -like 'LILITH*' } | Select-Object -First 1)
    if (-not $script:main) { $script:main = $wins[0] }
    [void](Wait-For { Find-NameStarting 'Core connection: Connected' } 40 'the connection')
    [void](Get-CorePid)
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

    # (a) The first start, and Not now. A test-channel core starts with `handlers: missing`.
    Start-App
    Pass 'lil-image is up on the test channel'
    $notNow = Wait-Enabled 'Not now' 20
    Pass '(a) the first start shows the card'
    Check ($null -ne (Find-Named 'Set up')) '(a) with Set up'
    $stateFile = Get-StateFile
    Check ($null -ne $stateFile) '(a) state.json is on disk'
    Invoke-Element $notNow
    [void](Wait-For { if (-not (Find-Named 'Not now')) { $true } } 5 'Not now to put it away')
    Pass '(a) Not now puts it away'
    Close-App
    Start-App
    Start-Sleep -Seconds 3
    Assert-Gone 'Not now' '(a) the next start doesn''t show it' -Exact
    Close-App

    # (b) A first start again, and Set up, pressed through Invoke: what it writes is real.
    Remove-Item -LiteralPath $stateFile.FullName
    foreach ($f in Get-ChildItem -LiteralPath $stateFile.DirectoryName -Force -Filter 'state.json*') { Remove-Item -LiteralPath $f.FullName }
    Start-App
    $setUp = Wait-Enabled 'Set up' 20
    $realBefore = Get-RealRegistrations
    Invoke-Element $setUp
    $said = Wait-For { Find-NameStarting 'The Start-menu shortcut and this LILITH' } 20 'the card to say what it wrote'
    Pass "(b) the card says: $($said.Current.Name)"
    $lnk = Join-Path $root "profile\Programs\LILITH ($capitalised).lnk"
    Check (Test-Path -LiteralPath $lnk) "(b) the shortcut is at the test channel's path"
    $shell = New-Object -ComObject WScript.Shell
    $target = $shell.CreateShortcut($lnk).TargetPath
    Check ($target -ieq $image) "(b) it targets the deployed lil-image.exe, the path the core checks ($target)"
    $command = (Get-ItemProperty -LiteralPath "$classes\$scheme\shell\open\command" -ErrorAction SilentlyContinue).'(default)'
    Check ($command -ieq "`"$image`" `"%1`"") "(b) the own scheme's command ($command)"
    Check (Test-Path -LiteralPath "$classes\AppUserModelId\LILITH.Suite.$capitalised") '(b) the AUMID is registered'
    $clsids = @(Get-ChildItem -LiteralPath "$classes\CLSID" -ErrorAction SilentlyContinue)
    Check ($clsids.Count -eq 1) '(b) the CLSID is registered'
    Check (-not (Test-Path -LiteralPath "$classes\.lilref")) '(b) no .lilref association'
    Check (@(Get-ChildItem -LiteralPath $classes -ErrorAction SilentlyContinue | Where-Object { $_.PSChildName -like 'LILITH.lilref.*' }).Count -eq 0) '(b) no .lilref file type'
    Compare-RealRegistrations $realBefore (Get-RealRegistrations)
    Check (-not (Test-Path -LiteralPath "$realClasses\$scheme")) '(b) the test channel''s scheme isn''t under the real classes root'
    Check (-not (Test-Path -LiteralPath "$realClasses\AppUserModelId\LILITH.Suite.$capitalised")) '(b) nor its AUMID'
    if ($clsids.Count -eq 1) {
        Check (-not (Test-Path -LiteralPath "$realClasses\CLSID\$($clsids[0].PSChildName)")) '(b) nor its CLSID'
    }

    # (c) The core told so: Settings has no Set up.
    Invoke-Element (Wait-Enabled 'Not now' 5)
    Set-FakeDesktop '{"handlers":true,"shortcut":true,"toasts":"ready"}'
    Send-Keys '^,'
    [void](Wait-For { Find-NameStarting 'Nothing LILITH can check stands in the way' } 15 'Settings to say so')
    Pass '(c) Settings says nothing it can check stands in the way'
    Assert-Gone 'Set up' '(c) and has no Set up' -Exact

    # (d) A switch, pressed through Toggle or Invoke.
    $switch = Wait-Enabled 'Job failed' 15
    $was = Is-Toggled $switch
    Invoke-Element $switch
    $core = Wait-For { $d = Get-DesktopGet; if ($d.notifications.job_failed -eq $false) { $d } } 10 'the core to say Job failed is off'
    Pass '(d) Job failed, pressed through UI Automation, reached the core'
    $toml = Get-SuiteToml
    Check ($toml -and ((Get-Content -Raw -LiteralPath $toml.FullName) -match 'job_failed\s*=\s*false')) '(d) and suite.toml'
    $now = Wait-For { $s = Find-Named 'Job failed'; if ($s -and (Is-Toggled $s) -ne $was) { $s } } 10 'the switch to show the answer'
    Pass '(d) the switch shows the core''s answer'

    # (e) Windows' reason, and its page.
    Set-FakeDesktop '{"toasts":"disabled_for_app"}'
    Send-Keys '^4'
    Start-Sleep -Seconds 3
    Send-Keys '^,'
    $link = Wait-Enabled 'Open Windows notification settings' 15
    Pass '(e) Windows'' reason offers its page'
    if (Get-Process -Name SystemSettings -ErrorAction SilentlyContinue) {
        Write-Host 'NOT CHECKED (e): Windows'' Settings already runs, so its opening can''t be told apart and nothing is closed'
    } else {
        Invoke-Element $link
        try {
            $opened = Wait-For { Get-Process -Name SystemSettings -ErrorAction SilentlyContinue | Select-Object -First 1 } 15 'Windows'' Settings'
            Pass "(e) the button, pressed through Invoke, opened Windows' Settings"
            Stop-Process -Id $opened.Id -Force
        } catch { Fail "(e) $_" }
    }
} catch {
    Fail "the settings check stopped: $_"
    try {
        New-Item -ItemType Directory -Force $DiagDir | Out-Null
        if ($script:app -and -not $script:app.HasExited) { Save-UiaTree (Join-Path $DiagDir 'uia-tree.txt') }
        $logs = Join-Path $root "local\LILITH-$channel\logs"
        if (Test-Path -LiteralPath $logs) { Copy-MaskedLogs $logs (Join-Path $DiagDir "failure-$script:runStamp-logs") }
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
