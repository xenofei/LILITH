<#
.SYNOPSIS
  One start of the deployed spike, for the owner's checks (OWNER-STEPS.md): S0.5 (b) after a
  reboot, S0.5 (c) after a Defender scan, and S0.7 (Narrator, -Interactive).

.DESCRIPTION
  Checks that the folder holds the Qt, KDDockWidgets, shim and VC++ DLLs first (it never launches
  blind), sets SetErrorMode so a missing DLL is an exit code rather than a dialog, then starts
  the app with a clean environment (no Qt or MSVC on PATH). Without -Interactive the window is
  shown without taking focus and quits after 2 s; results go to results/s05-<Label>.json
  (first_frame and interactive in ms from process start; PASS = interactive < 2,000 ms), with
  the machine's CPU load just before the start. The run's log stays in its own folder,
  build/runs/<stamp>-s05-<Label>/. If the GUI thread stalls for 5 s, the app writes every
  thread's stack beside its log (<log>.stall-stacks.txt), which the result names.

  -ColdCache moves %LOCALAPPDATA%\LILITH-spike\image\cache aside for this start (so the QML and
  pipeline caches start empty, as on a first install) and puts it back afterwards.

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label after-reboot
#>
param(
    [Parameter(Mandatory)] [string]$Label,
    [string]$Folder = (Join-Path $PSScriptRoot 'build\deploy'),
    [switch]$Interactive,
    [switch]$ColdCache,
    [int]$TimeoutSec = 300
)
$ErrorActionPreference = 'Stop'
$Results = Join-Path $PSScriptRoot 'results'
$Runs = Join-Path $PSScriptRoot ('build\runs\' + (Get-Date -Format 'yyyyMMdd-HHmmss') + "-s05-$Label")
New-Item -ItemType Directory -Force $Results, $Runs | Out-Null
Import-Module (Join-Path $PSScriptRoot 'lib\SpikeMeasure.psm1') -Force
Add-Type -Namespace AppShellOnce -Name Win -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'
[void][AppShellOnce.Win]::SetErrorMode(0x0001 -bor 0x8000)

$exe = Join-Path $Folder 'app_shell_spike.exe'
$need = 'app_shell_spike.exe', 'Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll', 'Qt6Quick.dll', 'kddockwidgets-qt62.dll', 'spike_shell_shim.dll', 'vcruntime140.dll', 'msvcp140.dll', 'platforms\qwindows.dll'
$missing = @($need | Where-Object { -not (Test-Path (Join-Path $Folder $_)) })
if ($missing.Count) { throw "not launched: $Folder is missing $($missing -join ', ')" }
$stale = @(Test-DeployCurrent $Folder (Join-Path $PSScriptRoot 'build\cmake\bin'))
if ($stale.Count) { throw "not launched: $Folder is older than the build ($($stale -join ', ')); redeploy: pwsh -NoProfile -File spikes/app-shell/run-spike.ps1 -Only S0.4" }

$cache = Join-Path $env:LOCALAPPDATA 'LILITH-spike\image\cache'
$cacheAside = "$cache.aside-$(Get-Date -Format 'yyyyMMddHHmmss')"
$movedCache = $false
# The cache is shared by every spike instance: never move it from under one that is running.
if ($ColdCache -and @(Get-SpikeAppProcesses).Count) {
    throw "not launched: a spike app is running (pid $((Get-SpikeAppProcesses | ForEach-Object ProcessId) -join ', ')); close it before a -ColdCache start"
}
if ($ColdCache -and (Test-Path $cache)) { Move-Item $cache $cacheAside; $movedCache = $true }
try {
    $log = Join-Path $Runs "s05-$Label.jsonl"
    $appArgs = @('--log', $log)
    if (-not $Interactive) { $appArgs += @('--no-activate', '--quit-after', '2000') }
    $psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $Folder
    $strip = '\\Qt\\|Microsoft Visual Studio|Windows Kits|\\MSVC\\'
    $psi.Environment['PATH'] = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch $strip }) -join ';'
    foreach ($k in 'QTDIR', 'QT_PLUGIN_PATH', 'QML_IMPORT_PATH', 'QML2_IMPORT_PATH', 'QT_QPA_PLATFORM') { [void]$psi.Environment.Remove($k) }
    $cpu = Get-CpuPercent
    $p = [System.Diagnostics.Process]::Start($psi)
    $limit = if ($Interactive) { 3600 } else { $TimeoutSec }
    if (-not $p.WaitForExit($limit * 1000)) { $p.Kill($true); $timedOut = $true } else { $timedOut = $false }
} finally {
    if ($movedCache) {
        # The run's own (cold-start) cache is kept beside the run's log; the old one goes back.
        if (Test-Path $cache) { Move-Item $cache (Join-Path $Runs 'cold-start-cache') }
        Move-Item $cacheAside $cache
    }
}

$ev = Read-JsonLines $log
$it = ($ev | Where-Object event -eq 'interactive' | Select-Object -First 1).ms
$ff = ($ev | Where-Object event -eq 'first_frame' | Select-Object -First 1).ms
$res = [ordered]@{ label = $Label; when = (Get-Date).ToString('o'); exit_code = $(if ($timedOut) { $null } else { $p.ExitCode })
    timed_out = $timedOut; first_frame_ms = $ff; interactive_ms = $it; cpu_percent_before = $cpu
    qml_cache = $(if ($ColdCache) { 'cold (moved aside for this start)' } else { 'as left by earlier runs' })
    verdict = $(if ($Interactive) { 'n/a' } elseif ($null -ne $it -and $it -lt 2000 -and -not $timedOut -and $p.ExitCode -eq 0) { 'PASS' } else { 'FAIL' })
    stall_stacks = $(if (Test-Path "$log.stall-stacks.txt") { "$log.stall-stacks.txt" } else { $null })
    log = $log }
$res | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Results "s05-$Label.json")
Copy-Item (Join-Path $Results "s05-$Label.json") $Runs
Write-Host "$Label : first_frame $ff ms, interactive $it ms (CPU $cpu % before) -> $($res.verdict)"
if ($res.stall_stacks) { Write-Host "the GUI thread stalled; stacks: $($res.stall_stacks) (send it to the agent)" }
