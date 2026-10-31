<#
.SYNOPSIS
  The hands-off check of the Diagnostics panel's bundle export and Backups in the deployed
  lil-image (ARCH-121, ARCH-122, APP-133, APP-134), in one stretch:
    pwsh -NoProfile -File tools/sandbox/diagnostics-check.ps1 [-DryRun]

.DESCRIPTION
  1. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown).
  2. A test channel under a new temporary root (LIL_CHANNEL=test-diagcheck, LIL_TEST_ROOT): the
     deployed lil-image starts on it, with its own test-channel core, and is driven through UI
     Automation. Its dock tab (else Ctrl+6) opens Diagnostics, and then:
       (a) "Export diagnostics bundle" opens the save dialog: the ZIP filter is there and no file
           name is filled in; Cancel makes no bundle, checked on disk in the core's folder and in
           the folder the file would go to;
       (b) a save to <temp>\out\bundle.zip: the file is a valid ZIP with the bundle's entries, and
           nothing is left beside it;
       (c) "Show in Explorer": an Explorer window opens on that temporary folder with the file
           selected, found through the shell's window list by its folder alone, and only that
           window is closed. If it can't be told apart safely, nothing is closed and the run says
           so;
       (d) "Open Backups" on the real windows platform: the Backups panel's empty state shows.
     Then the app's log is read for QML errors from these panels.
  On a failure, the app's UIA tree and the test channel's logs go to -DiagDir first. The tree holds
  no name from a dialog's file list, navigation pane, address bar or breadcrumb.
  3. lil-image is closed, the test core stopped (only the processes this run started, by recorded
     PID), the test channel's registry keys and the temporary root removed, and the pop-up told it
     is done. Exit 0 only when every check passed, and the cleanup too.

  It never touches the real stable or dev channel or their registrations: the cleanup does nothing
  unless LIL_CHANNEL is this script's test channel. No key or press is sent unless the foreground
  window, and the window under the target's point, are lil-image's (the run fails instead).

  -DryRun does the disk checks' self-test, and a bundle from the test core through `lil call`, with
  the same cleanup: no pop-up, no lil-image, no window.
#>
param(
    # The repository whose build/dev/deploy is checked: by default this script's own.
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    # Where a failed run leaves the app's UIA tree and the logs: by default build\dev\diagnostics-check.
    [string]$DiagDir = '',
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $Deploy) { $Deploy = Join-Path $Repo 'build\dev\deploy' }
if (-not $DiagDir) { $DiagDir = Join-Path $Repo 'build\dev\diagnostics-check' }
$lil = Join-Path $Deploy 'lil.exe'
$image = Join-Path $Deploy 'lil-image.exe'
$coreExe = Join-Path $Deploy 'lil-core.exe'
$results = [System.Collections.Generic.List[string]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
function Pass([string]$what) { $results.Add("PASS $what"); Write-Host "PASS $what" }
function Fail([string]$what) { $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what" }
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }
Add-Type -AssemblyName System.IO.Compression.FileSystem

# --- The disk checks -------------------------------------------------------------------------
function Get-Listing([string]$dir) {
    if (-not (Test-Path -LiteralPath $dir)) { return @() }
    @(Get-ChildItem -LiteralPath $dir -Force -File | Sort-Object Name | ForEach-Object { $_.Name })
}
# Failures of a diagnostics bundle at `file`: a ZIP that opens, with the entries every bundle has
# and a manifest of a bundle.
function Test-Bundle([string]$file) {
    $out = [System.Collections.Generic.List[string]]::new()
    if (-not (Test-Path -LiteralPath $file)) { $out.Add('no file'); return $out }
    try {
        $zip = [IO.Compression.ZipFile]::OpenRead($file)
        try {
            $names = @($zip.Entries | ForEach-Object { $_.FullName })
            foreach ($n in 'manifest.json', 'versions.json', 'errors.json', 'hardware.json', 'engines.json') {
                if ($names -notcontains $n) { $out.Add("no $n") }
            }
            $entry = $zip.GetEntry('manifest.json')
            if ($entry) {
                $reader = [IO.StreamReader]::new($entry.Open())
                try { $manifest = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
                if ($manifest.bundle -ne 1) { $out.Add("the manifest's bundle is $($manifest.bundle)") }
            }
        } finally { $zip.Dispose() }
    } catch { $out.Add('not a valid ZIP') }
    return $out
}
# Whether a shell window's LocationURL is `folder`.
function Test-SameFolder([string]$locationUrl, [string]$folder) {
    if (-not $locationUrl) { return $false }
    try { $local = ([Uri]$locationUrl).LocalPath } catch { return $false }
    return [string]::Equals($local.TrimEnd('\'), $folder.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}

function Self-Test {
    $d = Join-Path ([IO.Path]::GetTempPath()) ("lil-diagcheck-selftest-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory $d | Out-Null
    try {
        $src = Join-Path $d 'src'
        New-Item -ItemType Directory $src | Out-Null
        foreach ($n in 'versions.json', 'errors.json', 'hardware.json', 'engines.json') { Set-Content -LiteralPath (Join-Path $src $n) -Value '{}' }
        Set-Content -LiteralPath (Join-Path $src 'manifest.json') -Value '{"bundle": 1}'
        $good = Join-Path $d 'good.zip'
        [IO.Compression.ZipFile]::CreateFromDirectory($src, $good)
        $r = @(Test-Bundle $good)
        Check ($r.Count -eq 0) "self-test: a good bundle passes ($($r -join '; '))"
        Remove-Item -LiteralPath (Join-Path $src 'engines.json')
        $partial = Join-Path $d 'partial.zip'
        [IO.Compression.ZipFile]::CreateFromDirectory($src, $partial)
        $r = @(Test-Bundle $partial)
        Check ($r.Count -eq 1 -and $r[0] -eq 'no engines.json') 'self-test: a missing entry fails'
        $text = Join-Path $d 'text.zip'
        Set-Content -LiteralPath $text -Value 'not a zip'
        $r = @(Test-Bundle $text)
        Check ($r.Count -eq 1 -and $r[0] -eq 'not a valid ZIP') 'self-test: a file that is no ZIP fails'
        $r = @(Test-Bundle (Join-Path $d 'missing.zip'))
        Check ($r.Count -eq 1 -and $r[0] -eq 'no file') 'self-test: no file fails'
        $folderUrl = ([Uri]$d).AbsoluteUri
        Check (Test-SameFolder $folderUrl $d) 'self-test: a window on the folder is told by its URL'
        Check (-not (Test-SameFolder $folderUrl (Join-Path $d 'src'))) 'self-test: a window on another folder is not'
        Check (-not (Test-SameFolder '' $d)) 'self-test: a window with no location is not'
    } finally { Remove-Item -Recurse -Force -LiteralPath $d }
}

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-diagcheck'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-diagcheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$outDir = Join-Path $root 'out'
$coreBundles = Join-Path $root "local\LILITH-$channel\diagnostics"
# The lil-core.exe processes of this deploy folder that this run started, by PID.
$script:corePidsBefore = @()
$script:corePids = @()
function Get-DeployCorePids {
    @(Get-CimInstance Win32_Process -Filter "Name = 'lil-core.exe'" |
        Where-Object { $_.ExecutablePath -and [string]::Equals($_.ExecutablePath, $coreExe, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { [int]$_.ProcessId })
}
function Enter-TestChannel {
    $env:LIL_CHANNEL = $channel
    $env:LIL_TEST_ROOT = $root
    New-Item -ItemType Directory $root | Out-Null
    foreach ($s in 'roaming', 'local', 'profile', 'out') { New-Item -ItemType Directory (Join-Path $root $s) | Out-Null }
    $script:corePidsBefore = Get-DeployCorePids
    # The core finds no workspace from this folder.
    Set-Location -LiteralPath $root
}
function Invoke-Lil([string[]]$a) {
    $o = & $lil @a 2>&1
    if ($LASTEXITCODE -ne 0) { throw "lil $($a -join ' ') exited $LASTEXITCODE`: $($o -join ' ')" }
    return ($o -join "`n")
}
function Note-CorePids {
    $script:corePids = @(Get-DeployCorePids | Where-Object { $script:corePidsBefore -notcontains $_ })
}
# Each step's failure counts in the exit code. Unless this is the test channel it does nothing: a
# setup that failed early must not run `lil core stop` against another channel's core.
function Exit-TestChannel {
    if ($env:LIL_CHANNEL -ne $channel) { return }
    Note-CorePids
    try { [void](& $lil core stop 2>&1) } catch { }
    foreach ($id in $script:corePids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p -and -not $p.WaitForExit(10000)) {
            Write-Host "stopping the test core this run started (pid $id)"
            Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
            [void]$p.WaitForExit(5000)
        }
    }
    $alive = @($script:corePids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
    Check ($alive.Count -eq 0) "cleanup: none of the $($script:corePids.Count) core process(es) this run recorded still runs"
    Set-Location -LiteralPath $Repo
    Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
    $keysLeft = 0
    foreach ($k in "HKCU:\Software\LILITH-$channel", "HKCU:\Software\Classes\lil-$channel") {
        if (Test-Path $k) {
            Write-Host "removing the test channel's $k"
            try { Remove-Item -Recurse -Force $k } catch { Write-Host "couldn't remove it: $_" }
        }
        if (Test-Path $k) { ++$keysLeft }
    }
    Check ($keysLeft -eq 0) "cleanup: neither of the test channel's two registry keys is left ($keysLeft)"
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $root); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $root } catch { Start-Sleep -Milliseconds 500 }
    }
    Check (-not (Test-Path -LiteralPath $root)) 'cleanup: the temporary root is gone'
}

if ($DryRun) {
    Self-Test
    try {
        Enter-TestChannel
        $b = (Invoke-Lil @('call', 'diagnostics.bundle', '{}')) | ConvertFrom-Json
        $path = $b.structured.path
        $r = @(Test-Bundle $path)
        Check ($r.Count -eq 0) "dry run: the test core's bundle passes the disk check ($($r -join '; '))"
        Check ((Split-Path -Parent $path) -eq $coreBundles) 'dry run: the bundle is in the folder the cancel check watches'
        Note-CorePids
        Check ($script:corePids.Count -ge 1) "dry run: a lil-core.exe of this deploy folder that wasn't there before is recorded ($($script:corePids.Count))"
        Check (Test-Path -LiteralPath $image) 'dry run: lil-image.exe is deployed'
        Check (Test-Path -LiteralPath (Join-Path $Deploy 'qml\QtQuick\Dialogs')) 'dry run: QtQuick\Dialogs is deployed'
        Check (Test-Path -LiteralPath (Join-Path $Repo 'tools\sandbox\hands-off.ps1')) 'dry run: the hands-off pop-up is there'
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        Pass 'dry run: UI Automation loads'
        $count = (New-Object -ComObject Shell.Application).Windows().Count
        Check ($count -ge 0) 'dry run: the shell window list can be read'
    } catch { Fail "dry run: $_" } finally { Exit-TestChannel }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-diagcheck-{0}.state" -f $PID)
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', 'checks the diagnostics bundle (a window, a save dialog and an Explorer window open)', '-WarnSec', '10', '-RunSec', '60',
               '-CapSec', '240', '-StateFile', $state, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
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
        Start-Sleep -Milliseconds 250
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
if (-not ('LilDiagCheck.Win' -as [type])) {
    Add-Type -Namespace LilDiagCheck -Name Win -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);
[DllImport("user32.dll")] public static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern System.IntPtr WindowFromPoint(POINT point);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint processId);
'@
}
function Get-WindowProcess([System.IntPtr]$hwnd) {
    $owner = [uint32]0
    [void][LilDiagCheck.Win]::GetWindowThreadProcessId($hwnd, [ref]$owner)
    return [int]$owner
}
# Throws unless the foreground window, and the window under the target's point, are lil-image's: no
# key or press goes to whatever else is in front. The target is `$e`, or the middle of its window
# `$win` when `$e` has no point inside that window (scrolled away). Another program's window is
# never named.
function Assert-InputGoesToApp($e, $win, [string]$what) {
    $end = (Get-Date).AddSeconds(3)
    while ($true) {
        $front = [LilDiagCheck.Win]::GetForegroundWindow()
        $frontOk = $front -ne [System.IntPtr]::Zero -and (Get-WindowProcess $front) -eq $script:app.Id
        $area = $win.Current.BoundingRectangle
        $r = $e.Current.BoundingRectangle
        $x = $r.X + $r.Width / 2
        $y = $r.Y + $r.Height / 2
        if ($r.IsEmpty -or $x -lt $area.Left -or $x -ge $area.Right -or $y -lt $area.Top -or $y -ge $area.Bottom) {
            $x = $area.X + $area.Width / 2
            $y = $area.Y + $area.Height / 2
        }
        $point = [LilDiagCheck.Win+POINT]::new()
        $point.X = [int]$x
        $point.Y = [int]$y
        $under = [LilDiagCheck.Win]::WindowFromPoint($point)
        $underOk = $under -ne [System.IntPtr]::Zero -and (Get-WindowProcess $under) -eq $script:app.Id
        if ($frontOk -and $underOk) { return }
        if ((Get-Date) -gt $end) {
            throw "no input was sent ($what): $(if (-not $frontOk) { "the foreground window isn't lil-image's" } else { "the window under the target isn't lil-image's" })"
        }
        Start-Sleep -Milliseconds 200
    }
}
# Keys, once that holds.
function Send-Keys([string]$keys, $e, $win, [string]$what) {
    Assert-InputGoesToApp $e $win $what
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}
# A press of a control of the app's main window or a floating dock, once that holds: its Invoke
# pattern, else Space with the focus on it.
function Invoke-Element($e, [string]$what) {
    Show-App
    Assert-InputGoesToApp $e $script:main $what
    $p = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return }
    try { $e.SetFocus() } catch { }
    Send-Keys ' ' $e $script:main $what
}
# The app's window in front, through Win32 (UIA's SetFocus is refused on a top-level window).
function Show-App {
    try { [void][LilDiagCheck.Win]::SetForegroundWindow([System.IntPtr]$script:main.Current.NativeWindowHandle) } catch { }
    Start-Sleep -Milliseconds 300
}
# The app's top-level windows (the main window, a floating dock, a dialog).
function App-Windows {
    @($AE::RootElement.FindAll($TS::Children, (Prop $AE::ProcessIdProperty $script:app.Id)))
}
# The first element named `name`, under `in` or else under any window of the app.
function Find-Named($in, [string]$name) {
    $c = Prop $AE::NameProperty $name
    $e = $in.FindFirst($TS::Descendants, $c)
    if ($e) { return $e }
    foreach ($w in App-Windows) { $e = $w.FindFirst($TS::Descendants, $c); if ($e) { return $e } }
    return $null
}
# How many elements of the app's windows are named `name`.
function Count-Named([string]$name) {
    $n = 0
    foreach ($w in App-Windows) { $n += $w.FindAll($TS::Descendants, (Prop $AE::NameProperty $name)).Count }
    return $n
}
# The first element whose name starts with `prefix`, under any window of the app.
function Find-NameStarting([string]$prefix) {
    foreach ($root2 in App-Windows) {
        $all = $root2.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        foreach ($e in $all) { try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { } }
    }
    return $null
}
# Diagnostics in front: its dock tab through UIA (select, else invoke), else Ctrl+6; then what it
# reads only once it is shown must be there: its keep-awake line, from `desktop.get`.
function Open-DiagnosticsPanel {
    $how = ''
    $tab = $script:main.FindFirst($TS::Descendants, (And (Prop $AE::NameProperty 'Diagnostics') (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::TabItem))))
    if ($tab) {
        $p = $null
        if ($tab.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $p.Select(); $how = 'its dock tab (select)' }
        elseif ($tab.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); $how = 'its dock tab (invoke)' }
    }
    if (-not $how) { Show-App; Send-Keys '^6' $script:main $script:main 'Ctrl+6'; $how = 'Ctrl+6' }
    $found = {
        $e = Find-NameStarting 'Keep-awake is '
        if ($e) { $e } else { Find-NameStarting "This PC doesn't let LILITH" }
    }
    try {
        [void](Wait-For $found 15 'the keep-awake line')
    } catch {
        if ($how -eq 'Ctrl+6') { throw }
        Show-App; Send-Keys '^6' $script:main $script:main 'Ctrl+6'; $how = "Ctrl+6 (after $how did nothing)"
        [void](Wait-For $found 15 'the keep-awake line')
    }
    Pass "$how opened Diagnostics: its keep-awake line, read from desktop.get, shows"
}

# On a failure: every window of the app as UIA shows it (depth, control type, name, automation
# id, offscreen, enabled), into <DiagDir>\uia-tree.txt, at most 4000 elements.
function Save-UiaTree([string]$path) {
    $lines = [System.Collections.Generic.List[string]]::new()
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $walk = {
        param($e, [int]$depth)
        if ($lines.Count -ge 4000) { return }
        try {
            $c = $e.Current
            # Names only for the app's own Qt elements (their ids start "QGuiApplication"):
            # nothing of the system dialogs, whose lists show the PC's own folders and files.
            $ours = $c.AutomationId.StartsWith('QGuiApplication') -or ($depth -eq 0 -and $c.Name -like 'LILITH*')
            $name = if (-not $ours) { '' } elseif ($c.Name.Length -gt 100) { $c.Name.Substring(0, 100) + '…' } else { $c.Name }
            # The panel's own banner can name the temporary folder, which names the account.
            $name = $name.Replace($root, '<root>').Replace($env:USERPROFILE, '%USERPROFILE%')
            $lines.Add(('{0}{1} "{2}" id={3} offscreen={4} enabled={5}' -f ('  ' * $depth), $c.ControlType.ProgrammaticName, $name, $c.AutomationId, $c.IsOffscreen, $c.IsEnabled))
            # The save dialog's file list, navigation pane and address bar or breadcrumb: the
            # node alone, nothing under it.
            if ($c.AutomationId -in 'listview', 'ProperTreeHost', '40965', '41477' -or $c.ClassName -in 'Breadcrumb Parent', 'Address Band Root') { return }
        } catch { $lines.Add(('{0}(gone)' -f ('  ' * $depth))); return }
        $child = $walker.GetFirstChild($e)
        while ($child) { & $walk $child ($depth + 1); $child = $walker.GetNextSibling($child) }
    }
    foreach ($w in App-Windows) { & $walk $w 0 }
    Set-Content -LiteralPath $path -Value $lines -Encoding utf8
}

$dialogTitle = 'Export the diagnostics bundle'
function Open-SaveDialog {
    $btn = Wait-For { $b = Find-Named $script:main 'Export diagnostics bundle'; if ($b -and $b.Current.IsEnabled) { $b } } 30 'an enabled export button'
    Invoke-Element $btn 'the export button'
    return Wait-For { Find-Window $script:app.Id $dialogTitle } 20 'the save dialog'
}
# `e`'s window in front (Win32).
function Show-Element($e) {
    try { [void][LilDiagCheck.Win]::SetForegroundWindow([System.IntPtr]$e.Current.NativeWindowHandle) } catch { }
    Start-Sleep -Milliseconds 300
}
# SendKeys' own characters, escaped.
function Escape-Keys([string]$s) { [regex]::Replace($s, '[+^%~(){}\[\]]', '{$0}') }
# A child of the dialog with automation id `id`, any control type, invoked; false when it has
# no Invoke pattern.
function Invoke-DialogId($dlg, [string]$id) {
    $e = $dlg.FindFirst($TS::Children, (Prop $AE::AutomationIdProperty $id))
    if (-not $e) { $e = $dlg.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty $id)) }
    $p = $null
    if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) {
        Show-Element $dlg
        Assert-InputGoesToApp $e $dlg "the dialog's button $id"
        $p.Invoke()
        return $true
    }
    return $false
}
# The dialog's file name box: id 1001 under FileNameControlHost, any control type.
function Get-NameBox($dlg) {
    $hostPane = Wait-For { $dlg.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty 'FileNameControlHost')) } 10 'the file name box''s host'
    $box = $hostPane.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty '1001'))
    if ($box) { return $box }
    return $hostPane
}
# What the file name box holds; $null when it can't be read. The name of a file the app filled in,
# never a name from the dialog's list.
function Get-NameBoxValue($dlg) {
    $box = Get-NameBox $dlg
    $vp = $null
    if ($box.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) { return $vp.Current.Value }
    return $null
}
# The filter the dialog shows: FileTypeControlHost's value, else its selected item; $null when it
# can't be read. It holds only the app's own filter text.
function Get-FilterText($dlg) {
    $combo = Wait-For { $dlg.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty 'FileTypeControlHost')) } 10 'the file type box'
    $p = $null
    if ($combo.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$p)) {
        $v = $p.Current.Value
        if ($v) { return $v }
    }
    if ($combo.TryGetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern, [ref]$p)) {
        $s = @($p.Current.GetSelection())
        if ($s.Count -gt 0) { return $s[0].Current.Name }
    }
    return $null
}
# The full path into the file name box (the dialog goes to its folder), then Save (id 1, else Enter).
function Save-As($dlg, [string]$path) {
    $box = Get-NameBox $dlg
    $vp = $null
    $typed = $false
    if ($box.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) {
        Show-Element $dlg
        Assert-InputGoesToApp $box $dlg 'the file name box'
        try { $vp.SetValue($path); $typed = $true; Write-Host 'file name: set through its value pattern' } catch { }
    }
    if (-not $typed) {
        # The file name box has the keyboard focus when the dialog opens.
        Show-Element $dlg
        try { $box.SetFocus() } catch { }
        Send-Keys '^a' $box $dlg 'the file name box'
        Send-Keys (Escape-Keys $path) $box $dlg 'the file name box'
        Write-Host 'file name: typed'
        Start-Sleep -Milliseconds 300
        Send-Keys '{ENTER}' $box $dlg 'the file name box'
        return
    }
    if (-not (Invoke-DialogId $dlg '1')) {
        Show-Element $dlg; try { $box.SetFocus() } catch { }
        Send-Keys '{ENTER}' $box $dlg 'the file name box'
    }
}
# Cancel: id 2, else Escape.
function Cancel-Dialog($dlg) {
    if (-not (Invoke-DialogId $dlg '2')) { Show-Element $dlg; Send-Keys '{ESC}' $dlg $dlg 'the dialog' }
}
# The shell's Explorer window on `folder`, or $null. Only the location of each window is read:
# no other window is named, logged or touched.
function Find-ExplorerWindow([string]$folder) {
    $shell = New-Object -ComObject Shell.Application
    foreach ($w in @($shell.Windows())) {
        try { if (Test-SameFolder $w.LocationURL $folder) { return $w } } catch { }
    }
    return $null
}

$script:app = $null
$script:main = $null
try {
    Enter-TestChannel
    $script:app = Start-Process -FilePath $image -WorkingDirectory $root -PassThru
    [void](Wait-For { Find-Window $script:app.Id '' } 40 'the lil-image window')
    # The shell's window is titled "LILITH" (or "LILITH — <project>"); else the first one.
    Start-Sleep -Seconds 1
    $wins = App-Windows
    $script:main = ($wins | Where-Object { $_.Current.Name -like 'LILITH*' } | Select-Object -First 1)
    if (-not $script:main) { $script:main = $wins[0] }
    Pass 'the lil-image started on the test channel has a window'
    Open-DiagnosticsPanel
    $file = Join-Path $outDir 'bundle.zip'

    # (a) The dialog, then Cancel.
    $dlg = Open-SaveDialog
    Pass '(a) "Export diagnostics bundle" opened a window with the dialog''s title'
    $filter = Get-FilterText $dlg
    Check ($filter -and $filter -match 'ZIP files' -and $filter -match '\*\.zip') "(a) the ZIP filter is there ($filter)"
    $name = Get-NameBoxValue $dlg
    # Its value isn't printed unless it is one the app filled in by mistake.
    Check ($null -ne $name -and $name -eq '') $(if ($null -eq $name) { "(a) the file name box couldn't be read" } elseif ($name -eq '') { '(a) no file name is filled in' } else { "(a) a file name is filled in: $name" })
    Cancel-Dialog $dlg
    [void](Wait-For { if (-not (Find-Window $script:app.Id $dialogTitle)) { $true } } 10 'the dialog to close')
    Start-Sleep -Seconds 2
    Check ((Get-Listing $coreBundles).Count -eq 0) "(a) cancelled: no file in the core's bundle folder 2 s later"
    Check ((Get-Listing $outDir).Count -eq 0) '(a) cancelled: no file in the folder the file would go to 2 s later'

    # (b) Save.
    $dlg = Open-SaveDialog
    Save-As $dlg $file
    [void](Wait-For { if (Test-Path -LiteralPath $file) { $true } } 30 'bundle.zip')
    [void](Wait-For { Find-NameStarting 'Saved the diagnostics bundle as ' } 15 'the saved banner')
    Pass '(b) a banner starting "Saved the diagnostics bundle as" shows'
    $r = @(Test-Bundle $file)
    Check ($r.Count -eq 0) "(b) the file is a valid ZIP with the bundle's entries ($($r -join '; '))"
    $left = @(Get-Listing $outDir | Where-Object { $_ -ne 'bundle.zip' })
    Check ($left.Count -eq 0) "(b) nothing left beside it ($($left -join ', '))"
    Check ((Get-Listing $coreBundles).Count -eq 1) "(b) one file in the core's bundle folder"

    # (c) Show in Explorer.
    $show = Wait-For { Find-Named $script:main 'Show in Explorer' } 10 'Show in Explorer'
    Invoke-Element $show 'Show in Explorer'
    $window = $null
    try { $window = Wait-For { Find-ExplorerWindow $outDir } 20 'an Explorer window on the temporary folder' } catch { }
    if (-not $window) {
        Fail '(c) no Explorer window on the temporary folder could be told apart: nothing was closed. Look for one, and close it by hand'
    } else {
        Pass '(c) an Explorer window is on the temporary folder'
        $selected = $null
        for ($i = 0; $i -lt 20 -and -not $selected; ++$i) {
            try {
                $items = $window.Document.SelectedItems()
                if ($items.Count -ge 1) { $selected = $items.Item(0).Name }
            } catch { }
            if (-not $selected) { Start-Sleep -Milliseconds 250 }
        }
        Check ($selected -eq 'bundle.zip' -or $selected -eq 'bundle') "(c) its first selected item is the file ($selected)"
        try {
            $window.Quit()
            [void](Wait-For { if (-not (Find-ExplorerWindow $outDir)) { $true } } 10 'that Explorer window to close')
            Pass '(c) Quit went to that window alone, and no window is on the folder now'
        } catch { Fail "(c) that Explorer window didn't close: close it by hand ($_)" }
    }

    # (d) Open Backups: the status line's words, and the panel's own beside them.
    $line = 'No backups yet: backups arrive in a later version of LILITH'
    Check ((Count-Named $line) -ge 1) '(d) the backup status line is in the UIA tree'
    $windowsBefore = (App-Windows).Count
    $open = Wait-For { Find-Named $script:main 'Open Backups' } 10 'Open Backups'
    Invoke-Element $open 'Open Backups'
    [void](Wait-For { if ((Count-Named $line) -ge 2) { $true } } 15 'the Backups panel''s empty state')
    Pass "(d) after Open Backups the empty-state sentence shows a second time (the app's windows: $windowsBefore, then $((App-Windows).Count))"
} catch {
    Fail "the check stopped: $_"
    # What the app showed, and its and the core's logs, for the fix (the test root goes below).
    try {
        New-Item -ItemType Directory -Force $DiagDir | Out-Null
        if ($script:app -and -not $script:app.HasExited) { Save-UiaTree (Join-Path $DiagDir 'uia-tree.txt') }
        $logs = Join-Path $root "local\LILITH-$channel\logs"
        if (Test-Path -LiteralPath $logs) { Copy-Item -LiteralPath $logs -Destination (Join-Path $DiagDir 'logs') -Recurse -Force }
        Write-Host "diagnostics in $DiagDir"
    } catch { Write-Host "the diagnostics weren't saved: $_" }
} finally {
    if ($script:app -and -not $script:app.HasExited) {
        try {
            $wp = $null
            if ($script:main -and $script:main.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$wp)) { $wp.Close() }
            if (-not $script:app.WaitForExit(10000)) { Stop-Process -Id $script:app.Id -Force }
        } catch { Stop-Process -Id $script:app.Id -Force -ErrorAction SilentlyContinue }
        [void]$script:app.WaitForExit(5000)
        Check $script:app.HasExited 'cleanup: the lil-image this run started has exited'
    }
    # The app's log: a QML error from these panels. Counted, and the first few shown without the
    # temporary folder's name.
    try {
        $log = Get-ChildItem -LiteralPath (Join-Path $root "local\LILITH-$channel\logs") -Filter 'lil-image.*.log' -ErrorAction SilentlyContinue
        if ($log) {
            $bad = @($log | Get-Content | Where-Object { $_ -match 'panels/(diagnostics|backups)' -and $_ -match 'TypeError|ReferenceError|Cannot |is not|Binding loop|Unable to' })
            Check ($bad.Count -eq 0) "no line of the app's log names these panels with an error word ($($bad.Count))"
            $bad | Select-Object -First 3 | ForEach-Object { Write-Host (($_.Replace($root, '<root>').Replace($root.Replace('\', '\\'), '<root>')) -replace '^(.{300}).*$', '$1…') }
        } else { Fail "the app's log wasn't found" }
    } catch { Fail "the app's log couldn't be read: $_" }
    Exit-TestChannel
    Set-Content -LiteralPath $state -Value 'done'
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
exit ([int]($failures.Count -gt 0))
