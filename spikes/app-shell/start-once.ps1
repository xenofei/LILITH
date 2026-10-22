<#
.SYNOPSIS
  One start of the deployed spike, for the owner's checks (OWNER-STEPS.md): S0.5 (b) after a
  reboot, S0.5 (c) after a Defender scan, and S0.7 (Narrator, -Interactive).

.DESCRIPTION
  Checks that the folder holds the Qt, KDDockWidgets, shim and VC++ DLLs first (it never launches
  blind), sets SetErrorMode so a missing DLL is an exit code rather than a dialog, then starts
  the app with a clean environment (no Qt or MSVC on PATH). Without -Interactive the window is
  shown without taking focus and quits after 2 s; results go to results/s05-<Label>.json
  (first_frame and interactive in ms from process start; PASS = interactive < 2,000 ms). If the
  GUI thread stalls for 5 s, the app writes every thread's stack beside its log
  (<log>.stall-stacks.txt), which the result names.

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/start-once.ps1 -Label after-reboot
#>
param(
    [Parameter(Mandatory)] [string]$Label,
    [string]$Folder = (Join-Path $PSScriptRoot 'build\deploy'),
    [switch]$Interactive,
    [int]$TimeoutSec = 300
)
$ErrorActionPreference = 'Stop'
$Results = Join-Path $PSScriptRoot 'results'
$Runs = Join-Path $PSScriptRoot 'build\runs'
New-Item -ItemType Directory -Force $Results, $Runs | Out-Null
Add-Type -Namespace AppShellOnce -Name Win -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'
[void][AppShellOnce.Win]::SetErrorMode(0x0001 -bor 0x8000)

$exe = Join-Path $Folder 'app_shell_spike.exe'
$need = 'app_shell_spike.exe', 'Qt6Core.dll', 'Qt6Gui.dll', 'Qt6Qml.dll', 'Qt6Quick.dll', 'kddockwidgets-qt62.dll', 'spike_shell_shim.dll', 'vcruntime140.dll', 'msvcp140.dll', 'platforms\qwindows.dll'
$missing = @($need | Where-Object { -not (Test-Path (Join-Path $Folder $_)) })
if ($missing.Count) { throw "not launched: $Folder is missing $($missing -join ', ')" }

$log = Join-Path $Runs "s05-$Label.jsonl"
Remove-Item "$log*" -ErrorAction SilentlyContinue
$appArgs = @('--log', $log)
if (-not $Interactive) { $appArgs += @('--no-activate', '--quit-after', '2000') }
$psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $Folder
$strip = '\\Qt\\|Microsoft Visual Studio|Windows Kits|\\MSVC\\'
$psi.Environment['PATH'] = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch $strip }) -join ';'
foreach ($k in 'QTDIR', 'QT_PLUGIN_PATH', 'QML_IMPORT_PATH', 'QML2_IMPORT_PATH', 'QT_QPA_PLATFORM') { [void]$psi.Environment.Remove($k) }
$p = [System.Diagnostics.Process]::Start($psi)
$limit = if ($Interactive) { 3600 } else { $TimeoutSec }
if (-not $p.WaitForExit($limit * 1000)) { $p.Kill($true); $timedOut = $true } else { $timedOut = $false }

$ev = @(Get-Content $log -ErrorAction SilentlyContinue | ForEach-Object { try { $_ | ConvertFrom-Json } catch {} } | Where-Object { $_ })
$it = ($ev | Where-Object event -eq 'interactive' | Select-Object -First 1).ms
$ff = ($ev | Where-Object event -eq 'first_frame' | Select-Object -First 1).ms
$res = [ordered]@{ label = $Label; when = (Get-Date).ToString('o'); exit_code = $(if ($timedOut) { $null } else { $p.ExitCode })
    timed_out = $timedOut; first_frame_ms = $ff; interactive_ms = $it
    verdict = $(if ($Interactive) { 'n/a' } elseif ($null -ne $it -and $it -lt 2000) { 'PASS' } else { 'FAIL' })
    stall_stacks = $(if (Test-Path "$log.stall-stacks.txt") { "$log.stall-stacks.txt" } else { $null })
    log = $log }
$res | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Results "s05-$Label.json")
Write-Host "$Label : first_frame $ff ms, interactive $it ms -> $($res.verdict)"
if ($res.stall_stacks) { Write-Host "the GUI thread stalled; stacks: $($res.stall_stacks) (send it to the agent)" }
