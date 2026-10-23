#requires -Version 7
<#
.SYNOPSIS
Spike 11 (plan 4a Task 1): the one command that runs the whole spike, unattended (owner run 3,
owner q1/q2: tests may run on the PC at any time and need no one present).

.DESCRIPTION
One command, no owner input. It runs, in order:
  1. Part B on the host desktop (run-spike.ps1 -Only <the host checks>): the toast, activation,
     foreground, progress-toast, keep-awake and Windows Hello checks. Each check decides itself
     (uia.ps1 / the toasts' own events / the Hello prompt placement probe); none asks a question.
     Toasts may pop and the Hello prompt may appear; a hands-off pop-up still warns before any
     stretch that moves the mouse or presses keys, and nothing acts on the owner's own windows.
  2. Part C in Windows Sandbox (run-sandbox.ps1 -Runs 3): the desktop checks inside the Sandbox,
     3 runs of 3. No watched run and no lock step.

It prints each check's verdict at the end. Ctrl+C stops it and still cleans up. The owner is
involved only if a check is genuinely undecidable, and then only through a short pop-up prompt
(owner q2); as designed, the whole run needs none.

.PARAMETER Only
The checks to run, for reruns: S1.1 to S1.8 are Part B, S1.9 is Part C. The default is what is
still open after the earlier runs: S1.2p,S1.3,S1.4,S1.5,S1.7,S1.8,S1.9.
.PARAMETER Task0Repo
The folder of Task 0's worktree, where its deploy folder was built (or set LILITH_TASK0_REPO).
.PARAMETER DeployDir
Task 0's deploy folder itself, instead of -Task0Repo.
.PARAMETER Runs
Part C's Sandbox runs (the bar is 3 of 3).
.PARAMETER DryRun
Prints the plan as JSON and stops; nothing runs (tests/run_all.rs).
.PARAMETER Simulate
For tests/simulate.ps1 only: runs end to end with every Windows call faked (tests/sim-fakes.ps1).
Nothing on this PC is touched, every path goes to the folder that test made (WINSHELL_SIM_DIR),
and each stage gets -Simulate too.
#>
param(
    [string[]]$Only = @(),
    [string]$Task0Repo = '',
    [string]$DeployDir = '',
    [int]$Runs = 3,
    [switch]$DryRun,
    [switch]$Simulate
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

$sandboxArgs = @()
if ($Task0Repo) { $sandboxArgs += @('-Task0Repo', $Task0Repo) }
if ($DeployDir) { $sandboxArgs += @('-DeployDir', $DeployDir) }

$stages = @()
if ($b.Count -gt 0) { $stages += [ordered]@{ name = 'Part B (host desktop)'; script = 'run-spike.ps1'; args = @('-Only', ($b -join ',')) } }
# -KeepAwake holds a parent-watched power request for the Sandbox runs, so a long run isn't cut
# short by the PC sleeping (any run the PC slept through is recorded interrupted, never a FAIL).
if ($withSandbox) { $stages += [ordered]@{ name = 'Part C (Windows Sandbox)'; script = 'run-sandbox.ps1'; args = @('-Runs', "$Runs", '-KeepAwake') + $sandboxArgs } }

if ($DryRun) {
    [ordered]@{ only = $Only; part_b = $b; sandbox = $withSandbox; stages = $stages } | ConvertTo-Json -Depth 5 -Compress
    return
}

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }
function Fmt([double]$Min) { $m = [int][math]::Ceiling($Min); if ($m -ge 60) { '{0} h {1:00} min' -f [math]::Floor($m / 60), ($m % 60) } else { "$m min" } }
# -Simulate (fix round 4): the fakes replace every Windows call and redirect the paths.
$simArg = @()
if ($Simulate) { . (Join-Path $Spike 'tests\sim-fakes.ps1') -SimFor 'run-all'; $simArg = @('-Simulate') }

$identity = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from a normal (not elevated) terminal: an elevated sender''s toasts do not show.'
}

# Part C can start only with the Sandbox feature on and Task 0's deploy folder found. When it
# can't, the run goes ahead with Part B and leaves Part C out (it prints why).
if ($withSandbox) {
    $pf = (& pwsh -NoProfile -File (Join-Path $Spike 'run-sandbox.ps1') -Preflight @sandboxArgs @simArg) | Out-String | ConvertFrom-Json
    if (-not [bool]$pf.ready) {
        Say 'Part C (Windows Sandbox) cannot start, so this run leaves it out:' 'Yellow'
        foreach ($w in @($pf.why)) { Say "  - $w" 'Yellow' }
        foreach ($t in @($pf.task0_worktrees_with_a_deploy)) { Say "  To include it, add:  -Task0Repo `"$t`"" 'Yellow' }
        $stages = @($stages | Where-Object { $_.script -ne 'run-sandbox.ps1' })
    } else {
        Say "Task 0's app: $($pf.deploy.path)\$($pf.qt_exe) (from $($pf.deploy.from))"
    }
}

Write-Host ''
Say 'Spike 11, the whole run (unattended). Nobody need be at the PC.' 'Yellow'
Say ('  Stages: ' + (@($stages | ForEach-Object { $_.name }) -join ' then '))
Say '  Toasts may pop and a Windows Hello prompt may appear and close by itself. A hands-off pop-up warns before'
Say '  any stretch that moves the mouse or presses keys. Ctrl+C stops at any time and still cleans up.'
Write-Host ''

$started = Get-Date
$outcomes = @()
foreach ($s in $stages) {
    Write-Host ''
    Say ("######## {0} ({1:HH:mm})" -f $s.name, (Get-Date)) 'Yellow'
    & pwsh -NoProfile -File (Join-Path $Spike $s.script) @($s.args) @simArg
    $outcomes += [ordered]@{ stage = $s.name; exit = $LASTEXITCODE }
    if ($LASTEXITCODE -ne 0) { Say "$($s.name) ended with exit code $LASTEXITCODE; going on with the next part." 'Red' }
}

Write-Host ''
Say ("Done in {0}." -f (Fmt ((Get-Date) - $started).TotalMinutes)) 'Yellow'
$checks = Join-Path $Spike 'results\checks.json'
if (Test-Path $checks) {
    $doc = Get-Content -Raw $checks | ConvertFrom-Json
    foreach ($p in @($doc.checks.PSObject.Properties)) { Say ("  {0,-6} {1}" -f $p.Name, $p.Value.verdict) }
    if ($doc.PSObject.Properties['cleanup'] -and $doc.cleanup) { Say ("  cleanup {0}" -f $(if ($doc.cleanup.pass) { 'PASS' } else { 'FAIL' })) }
}
foreach ($f in 'sandbox-checks.json') {
    $p = Join-Path $Spike "results\$f"
    if ((Test-Path $p) -and (Get-Item $p).LastWriteTime -gt $started) {
        $j = Get-Content -Raw $p | ConvertFrom-Json
        $v = $(if ($j.PSObject.Properties['verdict']) { $j.verdict } elseif ($j.pass) { 'PASS' } else { 'FAIL' })
        Say ("  S1.9   {0}" -f $v)
    }
}
foreach ($o in $outcomes) { if ($o.exit -ne 0) { Say "  $($o.stage): exit code $($o.exit)" 'Red' } }
Say "Results: spikes\winshell\results\ (tell the agent the run is done)."
