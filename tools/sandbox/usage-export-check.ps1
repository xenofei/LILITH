<#
.SYNOPSIS
  The hands-off check of the Usage & spend panel's CSV export (APP-117) in the deployed
  lil-image, plus `ctest --preset deploy`, in one stretch:
    pwsh -NoProfile -File tools/sandbox/usage-export-check.ps1 [-DryRun] [-SkipDeployTests]

.DESCRIPTION
  1. Unless -SkipDeployTests: `pwsh -NoProfile -File tools/dev-shell.ps1 ctest --preset deploy`
     from -Repo (deploy_runs_off_path shows its own hands-off pop-up for its launches).
  2. The hands-off pop-up (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown).
  3. A test channel under a new temporary root (LIL_CHANNEL=test-usagecheck, LIL_TEST_ROOT): its
     core is seeded with one usage record through `lil call test.meter_external`, then the
     deployed lil-image starts on it and is driven through UI Automation:
       its dock tab (else Ctrl+5) opens Usage & spend; "Export the rows shown as CSV" opens the native save dialog;
       (a) save to <temp>\out\usage.csv; (b) Export again and Cancel; (c) a second record is
       seeded, then Export again over the same file and confirm the replace.
     After each step the disk is checked: the file, its UTF-8 BOM and header, the record's
     provider, no .lil-tmp or .lil-bak beside it, a pre-placed usage.csv.bak of "yours" left as
     it was; after the cancel the file and the folder are unchanged.
  On a failure, the app's UIA tree and the test channel's logs go to -DiagDir first.
  4. lil-image is closed, the test core stopped, the temporary root removed, and the pop-up told
     it is done. Exit 0 only when every check passed.
  -DryRun does step 3's seeding and the disk checks' self-test only: no pop-up, no window.
#>
param(
    # The repository whose build/dev/deploy is checked: by default this script's own.
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$Deploy = '',
    [switch]$SkipDeployTests,
    # Where a failed run leaves the app's UIA tree and the logs: by default build\dev\usage-export-check.
    [string]$DiagDir = '',
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
if (-not $Deploy) { $Deploy = Join-Path $Repo 'build\dev\deploy' }
if (-not $DiagDir) { $DiagDir = Join-Path $Repo 'build\dev\usage-export-check' }
$lil = Join-Path $Deploy 'lil.exe'
$image = Join-Path $Deploy 'lil-image.exe'
$results = [System.Collections.Generic.List[string]]::new()
$failures = [System.Collections.Generic.List[string]]::new()
function Pass([string]$what) { $results.Add("PASS $what"); Write-Host "PASS $what" }
function Fail([string]$what) { $failures.Add($what); $results.Add("FAIL $what"); Write-Host "FAIL $what" }
function Check([bool]$ok, [string]$what) { if ($ok) { Pass $what } else { Fail $what } }

# --- The disk checks -------------------------------------------------------------------------
function Get-Listing([string]$dir) {
    @(Get-ChildItem -LiteralPath $dir -Force -File | Sort-Object Name | ForEach-Object { $_.Name })
}
# Failures of an export at <dir>\usage.csv that must name each of $providers.
function Test-Export([string]$dir, [string[]]$providers) {
    $out = [System.Collections.Generic.List[string]]::new()
    $file = Join-Path $dir 'usage.csv'
    if (-not (Test-Path -LiteralPath $file)) { $out.Add('no usage.csv'); return $out }
    $bytes = [IO.File]::ReadAllBytes($file)
    if ($bytes.Length -lt 3 -or $bytes[0] -ne 0xEF -or $bytes[1] -ne 0xBB -or $bytes[2] -ne 0xBF) { $out.Add('no UTF-8 BOM') }
    $text = [Text.Encoding]::UTF8.GetString($bytes, [Math]::Min(3, $bytes.Length), [Math]::Max(0, $bytes.Length - 3))
    if (-not $text.StartsWith('period,basis,provider,label,records,estimated,reconciled,currency,amount')) { $out.Add("header: $(($text -split "`r`n")[0])") }
    if (-not $text.EndsWith("`r`n")) { $out.Add('the last line has no CRLF') }
    foreach ($p in $providers) { if ($text -notmatch ",money,$p,") { $out.Add("no money line for $p") } }
    $names = Get-Listing $dir
    foreach ($n in $names) {
        if ($n -like '*.lil-tmp' -or $n -like '*.lil-bak') { $out.Add("left beside it: $n") }
    }
    $mine = Join-Path $dir 'usage.csv.bak'
    if (-not (Test-Path -LiteralPath $mine) -or (Get-Content -Raw -LiteralPath $mine) -ne 'mine') { $out.Add('your usage.csv.bak changed') }
    return $out
}

function Self-Test {
    $d = Join-Path ([IO.Path]::GetTempPath()) ("lil-usagecheck-selftest-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory $d | Out-Null
    try {
        Set-Content -LiteralPath (Join-Path $d 'usage.csv.bak') -Value 'mine' -NoNewline
        $good = "period,basis,provider,label,records,estimated,reconciled,currency,amount,requests`r`n2026-10,money,openrouter,openrouter,1,1,0,USD,1.25,1`r`n"
        [IO.File]::WriteAllBytes((Join-Path $d 'usage.csv'), [byte[]](0xEF, 0xBB, 0xBF) + [Text.Encoding]::UTF8.GetBytes($good))
        $r = @(Test-Export $d @('openrouter'))
        Check ($r.Count -eq 0) "self-test: a good export passes ($($r -join '; '))"
        $r = @(Test-Export $d @('openrouter', 'fal'))
        Check ($r.Count -eq 1 -and $r[0] -eq 'no money line for fal') 'self-test: a missing provider fails'
        Set-Content -LiteralPath (Join-Path $d '.usage.csv.lil-bak') -Value 'x'
        [IO.File]::WriteAllText((Join-Path $d 'usage.csv'), $good)
        $r = @(Test-Export $d @('openrouter'))
        Check (($r -contains 'no UTF-8 BOM') -and ($r -contains 'left beside it: .usage.csv.lil-bak')) 'self-test: no BOM and a left backup fail'
    } finally { Remove-Item -Recurse -Force -LiteralPath $d }
}

# --- The test channel ------------------------------------------------------------------------
$channel = 'test-usagecheck'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-usagecheck-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
function Enter-TestChannel {
    New-Item -ItemType Directory $root | Out-Null
    foreach ($s in 'roaming', 'local', 'profile', 'out') { New-Item -ItemType Directory (Join-Path $root $s) | Out-Null }
    $env:LIL_CHANNEL = $channel
    $env:LIL_TEST_ROOT = $root
    # The core finds no workspace from this folder: usage is loose.
    Set-Location -LiteralPath $root
}
function Invoke-Lil([string[]]$a) {
    $o = & $lil @a 2>&1
    if ($LASTEXITCODE -ne 0) { throw "lil $($a -join ' ') exited $LASTEXITCODE`: $($o -join ' ')" }
    return ($o -join "`n")
}
function Seed([string]$provider, [string]$amount) {
    $json = '{"provider":"' + $provider + '","amount":"' + $amount + '"}'
    [void](Invoke-Lil @('call', 'test.meter_external', $json))
}
function Exit-TestChannel {
    try { [void](& $lil core stop 2>&1) } catch { }
    Set-Location -LiteralPath $Repo
    Remove-Item Env:LIL_CHANNEL, Env:LIL_TEST_ROOT -ErrorAction SilentlyContinue
    foreach ($k in "HKCU:\Software\LILITH-$channel", "HKCU:\Software\Classes\lil-$channel") {
        if (Test-Path $k) { Write-Host "removing the test channel's $k"; Remove-Item -Recurse -Force $k }
    }
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $root); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $root } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path -LiteralPath $root) { Write-Host "note: $root could not be removed yet" }
}

if ($DryRun) {
    Self-Test
    Enter-TestChannel
    try {
        Seed 'openrouter' '1.25'
        Start-Sleep -Milliseconds 500
        $s = Invoke-Lil @('call', 'usage.summary', '{"period":"month","group_by":["provider"]}')
        Check ($s -match '"provider": "openrouter"' -and $s -match '"basis": "money"') 'dry run: the test core has the seeded record'
        Check (Test-Path -LiteralPath $image) "dry run: $image is deployed"
        Check (Test-Path -LiteralPath (Join-Path $Deploy 'qml\QtQuick\Dialogs')) 'dry run: QtQuick\Dialogs is deployed'
    } catch { Fail "dry run: $_" } finally { Exit-TestChannel }
    Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
    exit ([int]($failures.Count -gt 0))
}

# --- 1. The deploy tests (their own pop-up) ----------------------------------------------------
if (-not $SkipDeployTests) {
    Push-Location -LiteralPath $Repo
    try {
        $t0 = Get-Date
        & pwsh -NoProfile -File (Join-Path $Repo 'tools\dev-shell.ps1') ctest --preset deploy --output-on-failure
        Check ($LASTEXITCODE -eq 0) ("ctest --preset deploy ({0:N0} s)" -f ((Get-Date) - $t0).TotalSeconds)
    } finally { Pop-Location }
}

# --- 2. The pop-up -------------------------------------------------------------------------------
$state = Join-Path ([IO.Path]::GetTempPath()) ("lil-usagecheck-{0}.state" -f $PID)
$psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
               '-What', 'checks the usage CSV export (a window and a save dialog open)', '-WarnSec', '10', '-RunSec', '90',
               '-CapSec', '300', '-StateFile', $state, '-ParentPid', ([string]$PID)) { $psi.ArgumentList.Add($a) }
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

# --- 3. The export check -------------------------------------------------------------------------
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
function Invoke-Element($e) {
    $p = $null
    if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return }
    try { $e.SetFocus() } catch { Show-App }
    [System.Windows.Forms.SendKeys]::SendWait(' ')
}
if (-not ('LilUsageCheck.Win' -as [type])) {
    Add-Type -Namespace LilUsageCheck -Name Win -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetForegroundWindow(System.IntPtr hwnd);'
}
# The app's window in front, through Win32 (UIA's SetFocus is refused on a top-level window).
function Show-App {
    try { [void][LilUsageCheck.Win]::SetForegroundWindow([System.IntPtr]$script:main.Current.NativeWindowHandle) } catch { }
    Start-Sleep -Milliseconds 300
}
# Usage & spend in front: its dock tab through UIA (select, else invoke), else Ctrl+5 with the
# app in front; then its month total must show.
function Open-UsagePanel {
    $how = ''
    $tab = $script:main.FindFirst($TS::Descendants, (And (Prop $AE::NameProperty 'Usage & spend') (Prop $AE::ControlTypeProperty ([System.Windows.Automation.ControlType]::TabItem))))
    if ($tab) {
        $p = $null
        if ($tab.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $p.Select(); $how = 'its dock tab (select)' }
        elseif ($tab.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); $how = 'its dock tab (invoke)' }
    }
    Write-Host ("the Usage & spend tab: {0}" -f $(if ($tab) { "found ($how)" } else { 'not found' }))
    if (-not $how) { Show-App; [System.Windows.Forms.SendKeys]::SendWait('^5'); $how = 'Ctrl+5' }
    try {
        [void](Wait-For { Find-NameStarting $script:main 'Spend this month: ' } 15 'the month total in Usage & spend')
    } catch {
        if ($how -eq 'Ctrl+5') { throw }
        Show-App; [System.Windows.Forms.SendKeys]::SendWait('^5'); $how = "Ctrl+5 (after $how did nothing)"
        [void](Wait-For { Find-NameStarting $script:main 'Spend this month: ' } 15 'the month total in Usage & spend')
    }
    Pass "$how opened Usage & spend with the month total"
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
# The first element whose name starts with `prefix`, under `in` or else under any window of
# the app.
function Find-NameStarting($in, [string]$prefix) {
    foreach ($root in @($in) + (App-Windows)) {
        $all = $root.FindAll($TS::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        foreach ($e in $all) { try { if ($e.Current.Name.StartsWith($prefix)) { return $e } } catch { } }
    }
    return $null
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
# The replace prompt's Yes, any control type: under the save dialog, else in a window of the
# app's own.
function Find-Yes($dlg) {
    $yes = Prop $AE::NameProperty 'Yes'
    try { $e = $dlg.FindFirst($TS::Descendants, $yes); if ($e) { return $e } } catch { }
    foreach ($w in App-Windows) { $e = $w.FindFirst($TS::Descendants, $yes); if ($e) { return $e } }
    return $null
}
$dialogTitle = 'Export the usage as CSV'
function Open-SaveDialog {
    $btn = Wait-For { $b = Find-Named $script:main 'Export the rows shown as CSV'; if ($b -and $b.Current.IsEnabled) { $b } } 30 'an enabled Export button'
    Invoke-Element $btn
    return Wait-For { Find-Window $script:app.Id $dialogTitle } 20 'the save dialog'
}
# `e`'s window in front (Win32).
function Show-Element($e) {
    try { [void][LilUsageCheck.Win]::SetForegroundWindow([System.IntPtr]$e.Current.NativeWindowHandle) } catch { }
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
    if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return $true }
    return $false
}
# The full path into the file name box (id 1001 under FileNameControlHost, any control type;
# the dialog goes to its folder), then Save (id 1, else Enter).
function Save-As($dlg, [string]$path) {
    $hostPane = Wait-For { $dlg.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty 'FileNameControlHost')) } 10 'the file name box''s host'
    $box = $hostPane.FindFirst($TS::Descendants, (Prop $AE::AutomationIdProperty '1001'))
    if (-not $box) { $box = $hostPane }
    $vp = $null
    $typed = $false
    if ($box.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$vp)) {
        try { $vp.SetValue($path); $typed = $true; Write-Host 'file name: set through its value pattern' } catch { }
    }
    if (-not $typed) {
        # The file name box has the keyboard focus when the dialog opens.
        Show-Element $dlg
        try { $box.SetFocus() } catch { }
        [System.Windows.Forms.SendKeys]::SendWait('^a')
        [System.Windows.Forms.SendKeys]::SendWait((Escape-Keys $path))
        Write-Host 'file name: typed'
        Start-Sleep -Milliseconds 300
        [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
        return
    }
    if (-not (Invoke-DialogId $dlg '1')) {
        Show-Element $dlg; try { $box.SetFocus() } catch { }
        [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
    }
}
# Cancel: id 2, else Escape.
function Cancel-Dialog($dlg) {
    if (-not (Invoke-DialogId $dlg '2')) { Show-Element $dlg; [System.Windows.Forms.SendKeys]::SendWait('{ESC}') }
}
# The replace prompt: its Yes, any control type, else Alt+Y.
function Confirm-Replace($dlg) {
    $yes = $null
    try { $yes = Wait-For { Find-Yes $dlg } 5 'the replace prompt''s Yes' } catch { }
    if ($yes) { Invoke-Element $yes; Write-Host 'replace prompt: Yes'; return }
    [System.Windows.Forms.SendKeys]::SendWait('%y')
    Write-Host 'replace prompt: Alt+Y'
}

$outDir = $null
$script:app = $null
$script:main = $null
try {
    Enter-TestChannel
    $outDir = Join-Path $root 'out'
    Set-Content -LiteralPath (Join-Path $outDir 'usage.csv.bak') -Value 'mine' -NoNewline
    Seed 'openrouter' '1.25'
    $script:app = Start-Process -FilePath $image -WorkingDirectory $root -PassThru
    [void](Wait-For { Find-Window $script:app.Id '' } 40 'the lil-image window')
    # The shell's window is titled "LILITH" (or "LILITH — <project>"); else the first one.
    Start-Sleep -Seconds 1
    $wins = App-Windows
    $script:main = ($wins | Where-Object { $_.Current.Name -like 'LILITH*' } | Select-Object -First 1)
    if (-not $script:main) { $script:main = $wins[0] }
    $note = ($wins | ForEach-Object { '"' + $_.Current.Name + '"' }) -join ', '
    Pass "lil-image is up on the test channel (its windows: $note)"
    Open-UsagePanel
    $file = Join-Path $outDir 'usage.csv'

    # (a) Save.
    $dlg = Open-SaveDialog
    Pass 'Export CSV… opened the save dialog'
    Save-As $dlg $file
    [void](Wait-For { if (Test-Path -LiteralPath $file) { $true } } 20 'usage.csv')
    Start-Sleep -Milliseconds 500
    $r = @(Test-Export $outDir @('openrouter'))
    Check ($r.Count -eq 0) "(a) saved: BOM, header, the record, nothing left beside it, yours untouched ($($r -join '; '))"
    $banner = Wait-For { Find-NameStarting $script:main 'Saved the CSV as ' } 10 'the saved banner'
    Pass "(a) the banner says: $($banner.Current.Name)"

    # (b) Cancel.
    $before = @{ hash = (Get-FileHash -LiteralPath $file).Hash; time = (Get-Item -LiteralPath $file).LastWriteTimeUtc; names = (Get-Listing $outDir) -join '|' }
    $dlg = Open-SaveDialog
    Cancel-Dialog $dlg
    [void](Wait-For { if (-not (Find-Window $script:app.Id $dialogTitle)) { $true } } 10 'the dialog to close')
    Start-Sleep -Seconds 1
    $after = @{ hash = (Get-FileHash -LiteralPath $file).Hash; time = (Get-Item -LiteralPath $file).LastWriteTimeUtc; names = (Get-Listing $outDir) -join '|' }
    Check ($after.hash -eq $before.hash -and $after.time -eq $before.time -and $after.names -eq $before.names) '(b) cancelled: the file and the folder are as they were'

    # (c) Overwrite, after a second record.
    Seed 'fal' '0.5'
    [void](Wait-For { Find-NameStarting $script:main 'Spend this month: 1.75 USD' } 15 'the panel to read the second record')
    $dlg = Open-SaveDialog
    Save-As $dlg $file
    Start-Sleep -Milliseconds 500
    Confirm-Replace $dlg
    [void](Wait-For { if ((Get-FileHash -LiteralPath $file).Hash -ne $before.hash) { $true } } 20 'usage.csv to change')
    Start-Sleep -Milliseconds 500
    $r = @(Test-Export $outDir @('openrouter', 'fal'))
    Check ($r.Count -eq 0) "(c) overwritten: both records, BOM, no .lil-bak or .lil-tmp left, yours untouched ($($r -join '; '))"
} catch {
    Fail "the export check stopped: $_"
    foreach ($p in 'Reading the usage', "Couldn't read the usage", 'Spend this month') {
        try { $e = Find-NameStarting $script:main $p; if ($e) { Write-Host "the app shows: $($e.Current.Name)" } } catch { }
    }
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
    }
    if ($outDir -and (Test-Path -LiteralPath $outDir)) { Write-Host "the folder at the end: $((Get-Listing $outDir) -join ', ')" }
    Exit-TestChannel
    Set-Content -LiteralPath $state -Value 'done'
}

Write-Host ''
$results | ForEach-Object { Write-Host $_ }
Write-Host ("{0} passed, {1} failed" -f ($results.Count - $failures.Count), $failures.Count)
exit ([int]($failures.Count -gt 0))
