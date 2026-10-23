#requires -Version 7
<#
.SYNOPSIS
Spike 11 (plan 4a Task 1): the owner's one command for a session. Runs Part B's checks on this
PC (run-spike.ps1), then Part C's Windows Sandbox runs (run-sandbox.ps1), then the long
stay-awake wait, and prints the time it will take before it starts.

.DESCRIPTION
Stages, in order:
  1. Part B (run-spike.ps1 -Only <the Part B checks> [-PowerPart sleep-wake]). Detected, not
     asked; the owner clicks S1.4's toasts and does the body steps (Hello, sleep and wake).
  2. Part C (S1.9): run-sandbox.ps1 -Observe (the owner locks the PC once), then -Runs 3.
  3. S1.7's stay-awake wait (run-spike.ps1 -Only S1.7 -Append -PowerPart stay-awake): nobody
     touches the PC for its sleep timeout plus about 5.5 minutes.
Each stage registers what it needs and removes it again (run-spike.ps1 and run-sandbox.ps1 say
what); one stage failing doesn't stop the next.

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
if ($Only.Count -eq 0) { $Only = $Default }
$b = @($PartB | Where-Object { $_ -in $Only })
$withSandbox = 'S1.9' -in $Only
$withS17 = 'S1.7' -in $b

# Minutes per check (Part B), from the earlier runs and the trial counts.
$Minutes = [ordered]@{ 'S1.1' = 3; 'S1.2' = 2; 'S1.2p' = 2; 'S1.3' = 4; 'S1.4' = 14; 'S1.5' = 3; 'S1.6' = 5; 'S1.7' = 5; 'S1.8' = 4 }

$sandboxArgs = @()
if ($Task0Repo) { $sandboxArgs += @('-Task0Repo', $Task0Repo) }
if ($DeployDir) { $sandboxArgs += @('-DeployDir', $DeployDir) }
$stages = @()
if ($b.Count -gt 0) {
    $a = @('-Only', ($b -join ','))
    if ($withS17) { $a += @('-PowerPart', 'sleep-wake') }
    $stages += [ordered]@{ name = 'Part B'; script = 'run-spike.ps1'; args = $a }
}
if ($withSandbox) {
    $stages += [ordered]@{ name = 'Part C, the watched run'; script = 'run-sandbox.ps1'; args = @('-Observe') + $sandboxArgs }
    $stages += [ordered]@{ name = 'Part C, the unattended runs'; script = 'run-sandbox.ps1'; args = @('-Runs', "$Runs") + $sandboxArgs }
}
if ($withS17) {
    $stages += [ordered]@{ name = 'S1.7, the stay-awake wait'; script = 'run-spike.ps1'; args = @('-Only', 'S1.7', '-Append', '-PowerPart', 'stay-awake', '-SkipBuild') }
}

if ($DryRun) {
    [ordered]@{ only = $Only; part_b = $b; sandbox = $withSandbox; stages = $stages } | ConvertTo-Json -Depth 5 -Compress
    return
}

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }
function Fmt([double]$Min) { $m = [int][math]::Ceiling($Min); if ($m -ge 60) { '{0} h {1:00} min' -f [math]::Floor($m / 60), ($m % 60) } else { "$m min" } }

$identity = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from a normal (not elevated) terminal: an elevated sender''s toasts do not show.'
}

# The sleep timeout in use decides the stay-awake wait (read only; the same query as S1.7).
$stayMin = 0.0
$timeoutText = 'unknown'
if ($withS17) {
    $q = (& powercfg /query SCHEME_CURRENT SUB_SLEEP STANDBYIDLE) -join "`n"
    $onBattery = @(Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Where-Object { $_.BatteryStatus -eq 1 }).Count -gt 0
    $m = [regex]::Match($q, "Current $(if ($onBattery) { 'DC' } else { 'AC' }) Power Setting Index:\s*0x([0-9a-fA-F]+)")
    if ($m.Success) {
        $sec = [Convert]::ToInt64($m.Groups[1].Value, 16)
        $timeoutText = $(if ($sec -eq 0) { 'never' } else { Fmt ($sec / 60) })
        if ($sec -gt 0) { $stayMin = ($sec + 330) / 60 }
    }
}

# Part C can start only with the Sandbox feature on and Task 0's deploy folder found.
$sandboxReady = $false
if ($withSandbox) {
    $pf = (& pwsh -NoProfile -File (Join-Path $Spike 'run-sandbox.ps1') -Preflight @sandboxArgs) | Out-String | ConvertFrom-Json
    $sandboxReady = [bool]$pf.ready
    if (-not $sandboxReady) {
        Say 'Part C (Windows Sandbox) cannot start, so this session leaves it out:' 'Red'
        foreach ($w in @($pf.why)) { Say "  - $w" 'Red' }
        $wt = @($pf.task0_worktrees_with_a_deploy)
        $rest = (@($Only) -join ',')
        if ($wt.Count -gt 0) { Say "  To include it, stop now (Ctrl+C) and type:  pwsh -NoProfile -File spikes/winshell/run-all.ps1 -Only $rest -Task0Repo `"$($wt[0])`"" 'Yellow' }
        $stages = @($stages | Where-Object { $_.script -ne 'run-sandbox.ps1' })
    } else {
        Say "Task 0's app: $($pf.deploy.path)\$($pf.qt_exe) (from $($pf.deploy.from))"
    }
}

$bMin = ($b | ForEach-Object { $Minutes[$_] } | Measure-Object -Sum).Sum
$cMin = $(if ($withSandbox -and $sandboxReady) { 15 + 10 * $Runs } else { 0 })
$total = $bMin + $cMin + $stayMin

Write-Host ''
Say ("Spike 11, one session: about {0} in all." -f (Fmt $total)) 'Yellow'
Say ("  1. Part B, you present (about {0}): {1}." -f (Fmt $bMin), ($b -join ', '))
Say  '     The script watches the screen itself (UI Automation) and moves the mouse at times: keep your'
Say  '     hands off unless it says YOUR TURN. Your turns: click 15 toasts in S1.4 (a real click is what'
Say  '     S1.4 measures); then, together near the end, Windows Hello prompts and one sleep and wake.'
if ($cMin -gt 0) {
    Say ("  2. Part C (about {0}): a Windows Sandbox window opens 4 times. Your turn once: lock the PC" -f (Fmt $cMin))
    Say  '     (Win+L) when asked, about 4 minutes, then sign in. The rest runs by itself.'
}
if ($withS17) {
    Say ("  3. The stay-awake wait (about {0}; your sleep timeout is {1}): nobody touches the PC. You can" -f (Fmt $stayMin), $timeoutText)
    Say  '     walk away once it starts; the screen may turn off. The PC must NOT go to sleep.'
}
Say  '  Everything the scripts add (a Start-menu shortcut, the lilspike: link type, a Hello key, a toast'
Say  '  activator key) is removed at the end of each part. Ctrl+C stops at any time and still cleans up.'
[void](Read-Host 'Press Enter to start')

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
Say ("Session done in {0}." -f (Fmt ((Get-Date) - $started).TotalMinutes)) 'Yellow'
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
        Say ("  S1.9 {0}: {1}" -f $(if ($f -like '*observe*') { 'watched run' } else { 'runs' }), $(if ($j.pass) { 'PASS' } else { 'FAIL' }))
    }
}
foreach ($o in $outcomes) { if ($o.exit -ne 0) { Say "  $($o.stage): exit code $($o.exit)" 'Red' } }
Say "Results: spikes\winshell\results\ (tell the agent the session is done)."
