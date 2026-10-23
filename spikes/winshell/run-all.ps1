#requires -Version 7
<#
.SYNOPSIS
Spike 11 (plan 4a Task 1): the owner's commands for a session, in two blocks. Each prints the time
it will take before it starts.

.DESCRIPTION
-Block Attended (the default; about 50 minutes, with the owner):
  1. Part B (run-spike.ps1 -Only <the Part B checks> [-PowerPart sleep-wake]). Detected, not
     asked; the owner clicks S1.4's toasts, watches S1.5's progress toast, and does the body
     steps (one sleep and wake, the Hello prompts).
  2. Part C's watched run (run-sandbox.ps1 -Observe): the owner locks the PC once.
-Block Unattended (nobody at the PC; its length follows "Sleep after": about 46 min at 5 min,
about 2 h 40 min at 2 hours):
  1. S1.7's stay-awake wait (run-spike.ps1 -Only S1.7 -Append -PowerPart stay-awake), first, so
     that no Sandbox is running during it and it starts right after the owner's last input.
  2. Part C's 3 runs (run-sandbox.ps1 -Runs 3 -KeepAwake: the PC is kept awake meanwhile).
Each stage registers what it needs and removes it again (run-spike.ps1 and run-sandbox.ps1 say
what); one stage failing doesn't stop the next. Part B and the stay-awake wait write
results\checks.json; Part C writes results\sandbox-observe.json and results\sandbox-checks.json.

.PARAMETER Block
Attended or Unattended.
.PARAMETER Only
The checks to run, for reruns: S1.1 to S1.8 are Part B, S1.9 is Part C. The default is what is
still open after the runs of 2026-10-22 and 2026-10-23: S1.2p,S1.3,S1.4,S1.5,S1.7,S1.8,S1.9.
.PARAMETER Task0Repo
The folder of Task 0's worktree, where its deploy folder was built (or set LILITH_TASK0_REPO).
.PARAMETER DeployDir
Task 0's deploy folder itself, instead of -Task0Repo.
.PARAMETER Runs
Part C's unattended runs (the bar is 3 of 3).
.PARAMETER DryRun
Prints the plan as JSON and stops; nothing runs (tests/run_all.rs).
#>
param(
    [ValidateSet('Attended', 'Unattended')]
    [string]$Block = 'Attended',
    [string[]]$Only = @(),
    [string]$Task0Repo = '',
    [string]$DeployDir = '',
    [int]$Runs = 3,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Spike = $PSScriptRoot
$PartB = @('S1.1', 'S1.2', 'S1.2p', 'S1.3', 'S1.4', 'S1.5', 'S1.6', 'S1.7', 'S1.8')
$Known = $PartB + @('S1.9')
$Default = @('S1.2p', 'S1.3', 'S1.4', 'S1.5', 'S1.7', 'S1.8', 'S1.9')

# `pwsh -File` hands a comma list over as one string (as in run-spike.ps1).
$Only = @($Only | ForEach-Object { "$_" -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ } |
    ForEach-Object { $n = $_; $k = @($Known | Where-Object { $_ -eq $n }); if ($k.Count -gt 0) { $k[0] } else { $n } })
$unknown = @($Only | Where-Object { $_ -notin $Known })
if ($unknown.Count -gt 0) { throw "Unknown check name in -Only: $($unknown -join ', '). The names are: $($Known -join ', '). Nothing was done." }
$givenOnly = $Only.Count -gt 0
if (-not $givenOnly) { $Only = $Default }
$b = @($PartB | Where-Object { $_ -in $Only })
$withSandbox = 'S1.9' -in $Only
$withS17 = 'S1.7' -in $b

# Minutes per check (Part B), from the earlier runs and the trial counts; S1.4 is the worst case
# (with the fallback recipe: 15 clicks and 5 recorded trials).
$Minutes = [ordered]@{ 'S1.1' = 3; 'S1.2' = 2; 'S1.2p' = 2; 'S1.3' = 4; 'S1.4' = 16; 'S1.5' = 4; 'S1.6' = 5; 'S1.7' = 5; 'S1.8' = 4 }

$sandboxArgs = @()
if ($Task0Repo) { $sandboxArgs += @('-Task0Repo', $Task0Repo) }
if ($DeployDir) { $sandboxArgs += @('-DeployDir', $DeployDir) }
$stages = @()
if ($Block -eq 'Attended') {
    if ($b.Count -gt 0) {
        $a = @('-Only', ($b -join ','))
        if ($withS17) { $a += @('-PowerPart', 'sleep-wake') }
        $stages += [ordered]@{ name = 'Part B'; script = 'run-spike.ps1'; args = $a }
    }
    if ($withSandbox) { $stages += [ordered]@{ name = 'Part C, the watched run'; script = 'run-sandbox.ps1'; args = @('-Observe') + $sandboxArgs } }
} else {
    if ($withS17) { $stages += [ordered]@{ name = 'S1.7, the stay-awake wait'; script = 'run-spike.ps1'; args = @('-Only', 'S1.7', '-Append', '-PowerPart', 'stay-awake', '-SkipBuild') } }
    # -KeepAwake: the wait before has left the PC past its idle timeout (review N-I1).
    if ($withSandbox) { $stages += [ordered]@{ name = 'Part C, the unattended runs'; script = 'run-sandbox.ps1'; args = @('-Runs', "$Runs", '-KeepAwake') + $sandboxArgs } }
}

if ($DryRun) {
    [ordered]@{ block = $Block; only = $Only; part_b = $b; sandbox = $withSandbox; stages = $stages } | ConvertTo-Json -Depth 5 -Compress
    return
}

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }
. (Join-Path $Spike 'uia.ps1')   # Get-PowerEvidence, Get-KeepAwakeAdvice (defines functions only)

# The sleep timeout in use (read only: powercfg /query; the scripts never change it).
function Get-SleepTimeout {
    $q = (& powercfg /query SCHEME_CURRENT SUB_SLEEP STANDBYIDLE) -join "`n"
    $onBattery = @(Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Where-Object { $_.BatteryStatus -eq 1 }).Count -gt 0
    $ac = [regex]::Match($q, 'Current AC Power Setting Index:\s*0x([0-9a-fA-F]+)')
    $dc = [regex]::Match($q, 'Current DC Power Setting Index:\s*0x([0-9a-fA-F]+)')
    $use = $(if ($onBattery) { $dc } else { $ac })
    [ordered]@{
        ac_s = $(if ($ac.Success) { [Convert]::ToInt64($ac.Groups[1].Value, 16) } else { $null })
        dc_s = $(if ($dc.Success) { [Convert]::ToInt64($dc.Groups[1].Value, 16) } else { $null })
        on_battery = $onBattery
        used_s = $(if ($use.Success) { [Convert]::ToInt64($use.Groups[1].Value, 16) } else { $null })
        at = (Get-Date).ToUniversalTime().ToString('o')
    }
}

function Fmt-Timeout($Sec) { if ($null -eq $Sec) { 'unknown' } elseif ($Sec -eq 0) { 'never' } else { Fmt ($Sec / 60) } }
$TimeoutFile = Join-Path $Spike 'results\sleep-timeout-at-attended.json'
function Fmt([double]$Min) { $m = [int][math]::Ceiling($Min); if ($m -ge 60) { '{0} h {1:00} min' -f [math]::Floor($m / 60), ($m % 60) } else { "$m min" } }

$identity = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from a normal (not elevated) terminal: an elevated sender''s toasts do not show.'
}

# The sleep timeout in use decides the stay-awake wait. It is only ever read (the same query as
# S1.7); the wait follows whatever value it finds.
$stayMin = 0.0
$timeoutNow = Get-SleepTimeout
$timeoutText = Fmt-Timeout $timeoutNow.used_s
$original = $null
if ($Block -eq 'Attended') {
    # Kept so that the unattended block can remind the owner of the value to set back (ruling
    # of 2026-10-23: the owner may lower "Sleep after" for the stay-awake test; the scripts only
    # read). Every value seen is kept, never overwritten, so a rerun after lowering it can't hide
    # the original (fix round 3, m6); the reminder names the largest.
    New-Item -ItemType Directory -Force -Path (Split-Path $TimeoutFile) | Out-Null
    $seen = @()
    if (Test-Path -LiteralPath $TimeoutFile) { try { $seen = @((Get-Content -Raw -LiteralPath $TimeoutFile | ConvertFrom-Json).values) } catch { } }
    if (@($seen | Where-Object { $_.used_s -eq $timeoutNow.used_s }).Count -eq 0) { $seen += [pscustomobject]$timeoutNow }
    [ordered]@{ values = @($seen) } | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $TimeoutFile
} elseif (Test-Path -LiteralPath $TimeoutFile) {
    try {
        $vals = @((Get-Content -Raw -LiteralPath $TimeoutFile | ConvertFrom-Json).values | Where-Object { $null -ne $_.used_s })
        $original = @($vals | Sort-Object { if ($_.used_s -eq 0) { [long]::MaxValue } else { [long]$_.used_s } } -Descending) | Select-Object -First 1
    } catch { }
}
if ($withS17 -and $Block -eq 'Unattended' -and $timeoutNow.used_s -gt 0) { $stayMin = ($timeoutNow.used_s + 330) / 60 }

# Part C can start only with the Sandbox feature on and Task 0's deploy folder found.
$sandboxReady = $false
if ($withSandbox) {
    $pf = (& pwsh -NoProfile -File (Join-Path $Spike 'run-sandbox.ps1') -Preflight @sandboxArgs) | Out-String | ConvertFrom-Json
    $sandboxReady = [bool]$pf.ready
    if (-not $sandboxReady) {
        Say 'Part C (Windows Sandbox) cannot start, so this block leaves it out:' 'Red'
        foreach ($w in @($pf.why)) { Say "  - $w" 'Red' }
        $wt = @($pf.task0_worktrees_with_a_deploy)
        $rest = $(if ($givenOnly) { " -Only $(@($Only) -join ',')" } else { '' })
        foreach ($t in $wt) { Say "  To include it, stop now (Ctrl+C) and type:  pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Block $Block$rest -Task0Repo `"$t`"" 'Yellow' }
        $stages = @($stages | Where-Object { $_.script -ne 'run-sandbox.ps1' })
    } else {
        Say "Task 0's app: $($pf.deploy.path)\$($pf.qt_exe) (from $($pf.deploy.from))"
    }
}

Write-Host ''
if ($Block -eq 'Attended') {
    $bMin = ($b | ForEach-Object { $Minutes[$_] } | Measure-Object -Sum).Sum
    $cMin = $(if ($withSandbox -and $sandboxReady) { 15 } else { 0 })
    Say ("Spike 11, the attended block: about {0}, you at the PC." -f (Fmt ($bMin + $cMin))) 'Yellow'
    Say ("  1. Part B (about {0}): {1}." -f (Fmt $bMin), ($b -join ', '))
    Say  '     The script watches the screen itself (UI Automation) and at times moves the mouse or presses'
    Say  '     keys. Before each such stretch a pop-up at the top of the screen warns you 10 s ahead and then'
    Say  '     counts down until you can use the PC again; it holds back while you are using the PC.'
    Say  '     Your turns, each marked YOUR TURN: click 10 toasts in S1.4 (15 if its first method fails;'
    Say  '     a real click is what S1.4 measures); watch the progress toast in S1.5; then, together, one'
    Say  '     sleep and wake (the script carries on only once the PC is awake again) and the Hello prompts.'
    if ($cMin -gt 0) {
        Say  '  2. Part C, the watched run (about 15 min): a Windows Sandbox window opens. Your turn once: lock'
        Say  '     the PC (Win+L) when asked, and sign in again when you hear two beeps (about 4 minutes).'
    }
} else {
    # The runs, plus about 5 minutes for the build, the copy and the Sandbox start.
    $cMin = $(if ($withSandbox -and $sandboxReady) { 10 * $Runs + 5 } else { 0 })
    # At worst (review m9): per power API, the 10-minute recheck and the wait plus 3 restarts,
    # and the second API after a failed first one.
    $worstStay = $(if ($stayMin -gt 0) { 2 * (10 + 4 * $stayMin) } else { 0 })
    Say ("Spike 11, the unattended block: about {0} (at worst {1}); nobody at the PC." -f (Fmt ($stayMin + $cMin)), (Fmt ($worstStay + $cMin))) 'Yellow'
    if ($withS17) {
        Say ("  Sleep after (in use now): {0}. The stay-awake wait is that plus about 5.5 minutes." -f $timeoutText) 'Yellow'
        Say ("  1. The stay-awake wait (about {0}). Nobody touches the PC; the" -f (Fmt $stayMin))
        Say  '     screen may turn off, and the PC must NOT go to sleep. A touch restarts the wait (up to 3 times).'
        Say  '     If the first method fails, the wait runs once more with the second one; touches and a'
        Say  '     program keeping the PC awake add to that (the "at worst" time).'
        Say  '     It does not start while a Windows Sandbox runs or while something else keeps the PC awake.'
        Say  '     The wait follows "Sleep after" (Settings, System, Power), which the scripts only read; at 5'
        Say  '     minutes, as planned, it is about 11 minutes. The end of this block reminds you of the value to'
        Say  '     set back.' 'Gray'
    }
    if ($cMin -gt 0) { Say ("  2. Part C, {0} runs (about {1}): a Windows Sandbox window opens and closes by itself; the PC is kept" -f $Runs, (Fmt $cMin)); Say '     awake meanwhile (a parent-watched request, released at the end).' }
}
Say  '  Everything the scripts add is removed at the end of each part. Ctrl+C stops at any time and still cleans up.'
# Nothing else may hold the PC awake during the stay-awake wait (review N-I2): checked now, while
# the owner is still here, and again until it clears or the owner types skip.
if ($Block -eq 'Unattended' -and $withS17) {
    $ev = Get-PowerEvidence
    while ((P2o $ev 'system_required') -eq $true) {
        Write-Host ''
        Say (Get-KeepAwakeAdvice (P2o $ev 'execution_state')) 'Yellow'
        $a = Read-Host 'Press Enter to check again, or type skip to run only the Sandbox part'
        if ($a -match '^\s*skip\s*$') {
            $stages = @($stages | Where-Object { $_.script -ne 'run-spike.ps1' })
            Say 'Skipped: the stay-awake wait is left out of this block.' 'Yellow'
            break
        }
        $ev = Get-PowerEvidence
    }
    if ((P2o $ev 'system_required') -ne $true) { Say ("Nothing else keeps the PC awake now (execution state {0})." -f (P2o $ev 'execution_state')) 'Green' }
}
[void](Read-Host 'Press Enter to start')
if ($Block -eq 'Unattended') {
    for ($k = 60; $k -gt 0; $k -= 10) { Say "Starting in $k s: leave the PC now (Ctrl+C stops)." 'Yellow'; Start-Sleep -Seconds 10 }
}

$started = Get-Date
$outcomes = @()
foreach ($s in $stages) {
    Write-Host ''
    Say ("######## {0} ({1:HH:mm})" -f $s.name, (Get-Date)) 'Yellow'
    & pwsh -NoProfile -File (Join-Path $Spike $s.script) @($s.args)
    $outcomes += [ordered]@{ stage = $s.name; exit = $LASTEXITCODE }
    if ($LASTEXITCODE -ne 0) { Say "$($s.name) ended with exit code $LASTEXITCODE; going on with the next part." 'Red' }
}

Write-Host ''
Say ("{0} block done in {1}." -f $Block, (Fmt ((Get-Date) - $started).TotalMinutes)) 'Yellow'
$checks = Join-Path $Spike 'results\checks.json'
if (Test-Path $checks) {
    $doc = Get-Content -Raw $checks | ConvertFrom-Json
    foreach ($p in @($doc.checks.PSObject.Properties)) { Say ("  {0,-6} {1}" -f $p.Name, $p.Value.verdict) }
    if ($doc.PSObject.Properties['cleanup'] -and $doc.cleanup) { Say ("  cleanup {0}" -f $(if ($doc.cleanup.pass) { 'PASS' } else { 'FAIL' })) }
}
foreach ($f in 'sandbox-checks.json', 'sandbox-observe.json') {
    $p = Join-Path $Spike "results\$f"
    if ((Test-Path $p) -and (Get-Item $p).LastWriteTime -gt $started) {
        $j = Get-Content -Raw $p | ConvertFrom-Json
        $v = $(if ($j.PSObject.Properties['verdict']) { $j.verdict } elseif ($j.pass) { 'PASS' } else { 'FAIL' })
        Say ("  S1.9 {0}: {1}" -f $(if ($f -like '*observe*') { 'watched run' } else { 'runs' }), $v)
    }
}
foreach ($o in $outcomes) { if ($o.exit -ne 0) { Say "  $($o.stage): exit code $($o.exit)" 'Red' } }
if ($Block -eq 'Attended' -and ($withS17 -or $withSandbox)) {
    $rest = $(if ($givenOnly) { " -Only $(@($Only) -join ',')" } else { '' })
    $t0 = $(if ($Task0Repo) { " -Task0Repo `"$Task0Repo`"" } elseif ($DeployDir) { " -DeployDir `"$DeployDir`"" } else { '' })
    Say 'Next, when you can leave the PC for a few hours (that block prints its own time first), type:' 'Yellow'
    Say "  pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Block Unattended$rest$t0" 'Yellow'
}
if ($Block -eq 'Unattended' -and $withS17) {
    $nowT = Get-SleepTimeout
    if ($null -ne $original -and $null -ne $original.used_s) {
        if ($original.used_s -ne $nowT.used_s) {
            Say ("REMINDER: 'Sleep after' is {0} now. Before the test it was {1}: set it back to {1} in Settings, System, Power." -f (Fmt-Timeout $nowT.used_s), (Fmt-Timeout $original.used_s)) 'Yellow'
        } else {
            Say ("'Sleep after' is {0}, as before the test." -f (Fmt-Timeout $nowT.used_s))
        }
    } else {
        Say ("'Sleep after' is {0} now. If you changed it for this test, set it back to what it was." -f (Fmt-Timeout $nowT.used_s)) 'Yellow'
    }
}
Say "Results: spikes\winshell\results\ (tell the agent the block is done)."
