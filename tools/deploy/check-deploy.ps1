<#
.SYNOPSIS
  CTest's deploy_runs_off_path (label deploy): the deploy folder runs on its own, within its size
  budget.

.DESCRIPTION
  1. Measures the folder (every file, in MiB) against the budget: test-params.toml's
     [budgets] deploy_folder_mib once Task 29 adds it; until then 365.3 MiB, Task 0's trimmed
     332.1 MiB plus 10 % (spike 10 §8.2).
  2. Starts the deployed lil-image.exe --smoke-exit --no-activate with spike 02's stripped
     environment: every PATH folder of Qt, Visual Studio, MSVC or the Windows Kits removed, and
     the Qt, QML and developer-environment variables unset. It also sets QT_QPA_PLATFORM to
     offscreen, which lil-image must clear before Qt starts (Checkpoint A I-3), so the start
     event must name the windows platform. Missing-DLL and crash dialogs are turned off for it.
  3. Passes when it exits 0 within -TimeoutSec, its log (JSON lines on stderr) has `first_frame`
     and `interactive` (with WebEngine not loaded, APP-137), it never took the foreground, and
     the folder is within budget.
  Off a GitHub runner the window shows on this PC's screen, so the hands-off pop-up
  (tools/sandbox/hands-off.ps1) warns 10 s ahead and counts down; -NoHandsOff skips it.
#>
param(
    [Parameter(Mandatory)] [string]$Deploy,
    [double]$BudgetMiB = 0,
    [int]$TimeoutSec = 90,
    [switch]$NoHandsOff
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$exe = Join-Path $Deploy 'lil-image.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    Write-Host "deploy_runs_off_path: no $exe (build the deploy target first)"
    exit 1
}
$failures = [System.Collections.Generic.List[string]]::new()

# 1. Size.
if ($BudgetMiB -le 0) {
    $BudgetMiB = 365.3
    $params = Join-Path $Repo 'test-params.toml'
    if (Test-Path -LiteralPath $params) {
        $line = Select-String -LiteralPath $params -Pattern '^\s*deploy_folder_mib\s*=\s*([0-9.]+)' | Select-Object -First 1
        if ($line) { $BudgetMiB = [double]$line.Matches[0].Groups[1].Value }
    }
}
$all = @(Get-ChildItem -LiteralPath $Deploy -Recurse -File)
$mib = [math]::Round((($all | Measure-Object Length -Sum).Sum) / 1MB, 1)
Write-Host ("deploy folder: {0} files, {1} MiB (budget {2} MiB)" -f $all.Count, $mib, $BudgetMiB)
$all | Sort-Object Length -Descending | Select-Object -First 5 | ForEach-Object {
    Write-Host ("  {0} {1:N1} MiB" -f $_.FullName.Substring($Deploy.Length).TrimStart('\', '/'), ($_.Length / 1MB))
}
if ($mib -gt $BudgetMiB) { $failures.Add("the folder is $mib MiB, over its $BudgetMiB MiB budget") }

# 2. The stripped environment.
$stripRe = '\\Qt\\|Microsoft Visual Studio|Windows Kits|\\MSVC\\|\\Qt6?$'
$cleanPath = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch $stripRe }) -join ';'
$leaks = @(($cleanPath -split ';') | Where-Object { $_ -and ((Test-Path (Join-Path $_ 'Qt6Core.dll')) -or (Test-Path (Join-Path $_ 'cl.exe'))) })
if ($leaks.Count) { $failures.Add("the stripped PATH still has Qt or MSVC in: $($leaks -join '; ')") }
$unset = 'QTDIR', 'QT_PLUGIN_PATH', 'QML_IMPORT_PATH', 'QML2_IMPORT_PATH', 'QT_QPA_PLATFORM_PLUGIN_PATH', 'CMAKE_PREFIX_PATH',
         'LIL_QT_DIR', 'QMAKE', 'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsRedistDir', 'INCLUDE', 'LIB', 'LIBPATH'

if (-not ('LilDeployCheck.Win' -as [type])) {
    Add-Type -Namespace LilDeployCheck -Name Win -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
[DllImport("user32.dll")] static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint pid);
public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
'@
}
# SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX: inherited, so a missing DLL or a crash is an
# exit code, never a dialog.
[void][LilDeployCheck.Win]::SetErrorMode(0x0001 -bor 0x0002)

# The hands-off pop-up, off CI.
$popup = $null
$state = $null
if (-not $NoHandsOff -and $env:GITHUB_ACTIONS -ne 'true') {
    $state = Join-Path ([IO.Path]::GetTempPath()) ("lil-check-deploy-{0}.state" -f $PID)
    $psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
    foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
                   '-What', 'checks the deploy folder (a window opens briefly)', '-WarnSec', '10', '-RunSec', '20',
                   '-CapSec', ([string]($TimeoutSec + 30)), '-StateFile', $state, '-ParentPid', ([string]$PID)) {
        $psi.ArgumentList.Add($a)
    }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $popup = [System.Diagnostics.Process]::Start($psi)
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path -LiteralPath "$state.shown")) {
        if (Test-Path -LiteralPath "$state.error") { Write-Host "deploy_runs_off_path: the hands-off pop-up failed: $(Get-Content -Raw "$state.error")"; exit 1 }
        if ((Get-Date) -gt $deadline) { Write-Host 'deploy_runs_off_path: the hands-off pop-up was not on screen within 30 s, so nothing ran'; exit 1 }
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Seconds 10
}

# 3. The run.
$err = Join-Path ([IO.Path]::GetTempPath()) ("lil-check-deploy-{0}.log" -f $PID)
$psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
$psi.ArgumentList.Add('--smoke-exit')
$psi.ArgumentList.Add('--no-activate')
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $Deploy
$psi.RedirectStandardError = $true
$psi.Environment['PATH'] = $cleanPath
foreach ($k in $unset) { [void]$psi.Environment.Remove($k) }
$psi.Environment['QT_QPA_PLATFORM'] = 'offscreen'
$code = $null
$foreground = 0
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = [System.Diagnostics.Process]::Start($psi)
$stderr = $p.StandardError.ReadToEndAsync()
try {
    while (-not $p.WaitForExit(100)) {
        if ([LilDeployCheck.Win]::ForegroundPid() -eq $p.Id) { $foreground++ }
        if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) { break }
    }
    if ($p.HasExited) { $p.WaitForExit(); $code = $p.ExitCode }
} finally {
    if (-not $p.HasExited) { try { $p.Kill($true) } catch { } }
    if ($state) { Set-Content -LiteralPath $state -Value 'done' }
}
$text = $stderr.GetAwaiter().GetResult()
Set-Content -LiteralPath $err -Value $text -Encoding utf8
$events = @($text -split "`r?`n" | Where-Object { $_.StartsWith('{') } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } })
$start = $events | Where-Object { $_.event -eq 'start' } | Select-Object -First 1
$first = $events | Where-Object { $_.event -eq 'first_frame' } | Select-Object -First 1
$inter = $events | Where-Object { $_.event -eq 'interactive' } | Select-Object -First 1
$qtWarnings = @($events | Where-Object { $_.event -eq 'qt_msg' -and $_.type -ne 'debug' -and $_.type -ne 'info' } | ForEach-Object { "[$($_.type)] $($_.msg)" })

Write-Host ("run: exit {0} in {1:N1} s; platform {2}; first_frame {3} ms; interactive {4} ms; foreground samples {5}" -f `
    $(if ($null -eq $code) { 'none (timed out)' } else { $code }), $sw.Elapsed.TotalSeconds, $start.platform, $first.ms, $inter.ms, $foreground)
$qtWarnings | Select-Object -First 10 | ForEach-Object { Write-Host "  qt: $_" }
if ($null -eq $code) { $failures.Add("lil-image.exe didn't exit within $TimeoutSec s") }
elseif ($code -ne 0) { $failures.Add("lil-image.exe exited $code") }
if (-not $start) { $failures.Add('no start event') }
elseif ($start.platform -ne 'windows') { $failures.Add("the platform was $($start.platform): QT_QPA_PLATFORM wasn't cleared before Qt started") }
if (-not $first) { $failures.Add('no first_frame event') }
if (-not $inter) { $failures.Add('no interactive event') }
elseif ($inter.webengine_loaded -ne $false) { $failures.Add('Qt6WebEngineCore.dll was loaded at interactive (APP-137)') }
if ($foreground -gt 0) { $failures.Add("lil-image.exe took the foreground with --no-activate ($foreground samples)") }

if ($failures.Count) {
    Write-Host 'deploy_runs_off_path: FAIL'
    $failures | ForEach-Object { Write-Host "  $_" }
    Write-Host "  log: $err"
    exit 1
}
Remove-Item -LiteralPath $err -ErrorAction SilentlyContinue
Write-Host 'deploy_runs_off_path: PASS'
exit 0
