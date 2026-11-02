<#
.SYNOPSIS
  CTest's bench_cold_start (label bench; APP-136): the deploy folder's warm starts, each
  `interactive` under the budget.

.DESCRIPTION
  As spike 10's S0.5 (a): one untimed start, then -Starts timed starts of the deployed
  lil-image.exe with `--smoke-exit --no-activate` (it quits once it is interactive, and under
  --smoke-exit makes no core session and reads no app state). Each takes the stripped environment
  of check-deploy.ps1 (no Qt, Visual Studio, MSVC or Windows Kits folder on PATH, the developer
  variables unset), on a test channel of its own under a new temporary root (LIL_CHANNEL,
  LIL_TEST_ROOT), so its log and Qt's cache folder (%LOCALAPPDATA%\LILITH-<that channel>) are the
  test's. `interactive` and `first_frame` come from each start's JSON lines on stderr. It prints
  each start and the p50 and max, and passes when every timed start exits 0 with an `interactive`
  under the budget, never takes the foreground, and the test channel's folders are removed after.
  Before each start it waits, at most 60 s, for the machine's CPU load to fall under 35 % (the
  spike's measurement condition, recorded beside each start; it never changes the verdict).

  -First times the first start alone, with no untimed start before it: Task 29 Step 4's first start
  of a fresh copy of the deploy folder after a Defender scan of it (APP-136, spike 10's S0.5 (c)). The
  test channel is new each run, so Qt's caches for it start empty.

  The budget is -BudgetMs, else LIL_BUDGET_COLD_START_MS (test-params.toml's [budgets]
  cold_start_ms, which CMake passes), else it fails.

  Off a GitHub runner the starts run inside the hands-off pop-up's window
  (tools/sandbox/hands-off.ps1: 10 s of warning, then a countdown): a window shows each time.
  -NoHandsOff skips it.
#>
param(
    [Parameter(Mandatory)] [string]$Deploy,
    [double]$BudgetMs = 0,
    [int]$Starts = 10,
    [int]$TimeoutSec = 60,
    [switch]$First,
    [switch]$NoHandsOff
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$exe = Join-Path $Deploy 'lil-image.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    Write-Host "bench_cold_start: no $exe (build the deploy target first)"
    exit 1
}
if ($BudgetMs -le 0 -and $env:LIL_BUDGET_COLD_START_MS) { $BudgetMs = [double]$env:LIL_BUDGET_COLD_START_MS }
if ($BudgetMs -le 0) {
    Write-Host 'bench_cold_start: no budget (-BudgetMs, or LIL_BUDGET_COLD_START_MS from CMake)'
    exit 1
}
$failures = [System.Collections.Generic.List[string]]::new()
if ($First) { $Starts = 1 }

# The test channel: its own root, removed at the end with Qt's cache folder of that channel only.
$channel = 'test-coldstart'
$root = Join-Path ([IO.Path]::GetTempPath()) ("lil-coldstart-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
foreach ($s in 'roaming', 'local', 'profile') { New-Item -ItemType Directory -Force (Join-Path $root $s) | Out-Null }

# The stripped environment (check-deploy.ps1's).
$stripRe = '\\Qt\\|Microsoft Visual Studio|Windows Kits|\\MSVC\\|\\Qt6?$'
$cleanPath = (($env:PATH -split ';') | Where-Object { $_ -and $_ -notmatch $stripRe }) -join ';'
$unset = 'QTDIR', 'QT_PLUGIN_PATH', 'QML_IMPORT_PATH', 'QML2_IMPORT_PATH', 'QT_QPA_PLATFORM', 'QT_QPA_PLATFORM_PLUGIN_PATH',
         'CMAKE_PREFIX_PATH', 'LIL_QT_DIR', 'QMAKE', 'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsRedistDir', 'INCLUDE',
         'LIB', 'LIBPATH'

if (-not ('LilColdStart.Win' -as [type])) {
    Add-Type -Namespace LilColdStart -Name Win -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
[DllImport("user32.dll")] static extern System.IntPtr GetForegroundWindow();
[DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(System.IntPtr hwnd, out uint pid);
public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
'@
}
# A missing DLL or a crash is an exit code, never a dialog.
[void][LilColdStart.Win]::SetErrorMode(0x0001 -bor 0x0002)

function Get-CpuLoad {
    try { return [math]::Round((Get-CimInstance Win32_Processor | Measure-Object LoadPercentage -Average).Average, 1) } catch { return -1 }
}

# One start: {code, interactive, first, foreground, load}.
function Start-Once {
    $deadline = (Get-Date).AddSeconds(60)
    $load = Get-CpuLoad
    while ($load -ge 35 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 500; $load = Get-CpuLoad }
    $psi = [System.Diagnostics.ProcessStartInfo]::new($exe)
    foreach ($a in '--smoke-exit', '--no-activate') { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $root
    $psi.RedirectStandardError = $true
    $psi.Environment['PATH'] = $cleanPath
    foreach ($k in $unset) { [void]$psi.Environment.Remove($k) }
    $psi.Environment['LIL_CHANNEL'] = $channel
    $psi.Environment['LIL_TEST_ROOT'] = $root
    $p = [System.Diagnostics.Process]::Start($psi)
    $stderr = $p.StandardError.ReadToEndAsync()
    $foreground = 0
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $code = $null
    try {
        while (-not $p.WaitForExit(50)) {
            if ([LilColdStart.Win]::ForegroundPid() -eq $p.Id) { $foreground++ }
            if ($sw.Elapsed.TotalSeconds -gt $TimeoutSec) { break }
        }
        if ($p.HasExited) { $p.WaitForExit(); $code = $p.ExitCode }
    } finally {
        if (-not $p.HasExited) { try { $p.Kill($true) } catch { } }
    }
    $events = @($stderr.GetAwaiter().GetResult() -split "`r?`n" | Where-Object { $_.StartsWith('{') } |
                ForEach-Object { try { $_ | ConvertFrom-Json } catch { } })
    $inter = $events | Where-Object { $_.event -eq 'interactive' } | Select-Object -First 1
    $first = $events | Where-Object { $_.event -eq 'first_frame' } | Select-Object -First 1
    [pscustomobject]@{ code = $code; interactive = $inter.ms; first = $first.ms; foreground = $foreground; load = $load }
}

# The hands-off pop-up, off CI.
$popup = $null
$state = $null
if (-not $NoHandsOff -and $env:GITHUB_ACTIONS -ne 'true') {
    $state = Join-Path ([IO.Path]::GetTempPath()) ("lil-cold-start-{0}.state" -f $PID)
    $psi = [System.Diagnostics.ProcessStartInfo]::new((Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'))
    foreach ($a in '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Repo 'tools\sandbox\hands-off.ps1'),
                   '-What', "starts the app $($Starts + 1) times (a window opens each time)", '-WarnSec', '10',
                   '-RunSec', ([string](($Starts + 1) * 4)), '-CapSec', ([string](($Starts + 1) * ($TimeoutSec + 60))),
                   '-StateFile', $state, '-ParentPid', ([string]$PID)) {
        $psi.ArgumentList.Add($a)
    }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $popup = [System.Diagnostics.Process]::Start($psi)
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path -LiteralPath "$state.shown")) {
        if (Test-Path -LiteralPath "$state.error") { Write-Host "bench_cold_start: the hands-off pop-up failed: $(Get-Content -Raw "$state.error")"; exit 1 }
        if ((Get-Date) -gt $deadline) { Write-Host 'bench_cold_start: the hands-off pop-up was not on screen within 30 s, so nothing ran'; exit 1 }
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Seconds 10
}

$runs = @()
try {
    if (-not $First) {
        $warm = Start-Once
        Write-Host ("untimed start: exit {0}, interactive {1} ms" -f $warm.code, $warm.interactive)
    }
    for ($i = 1; $i -le $Starts; ++$i) {
        $r = Start-Once
        $runs += $r
        Write-Host ("start {0}: exit {1}, first_frame {2} ms, interactive {3} ms, CPU {4} % before it, foreground samples {5}" -f `
            $i, $r.code, $r.first, $r.interactive, $r.load, $r.foreground)
        if ($r.code -ne 0) { $failures.Add("start $i exited $($r.code)") }
        if ($null -eq $r.interactive) { $failures.Add("start $i logged no interactive") }
        elseif ([double]$r.interactive -ge $BudgetMs) { $failures.Add("start $i was interactive at $($r.interactive) ms, not under $BudgetMs ms") }
        if ($r.foreground -gt 0) { $failures.Add("start $i took the foreground with --no-activate") }
    }
} finally {
    if ($state) { Set-Content -LiteralPath $state -Value 'done' }
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $root); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $root } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path -LiteralPath $root) { $failures.Add("cleanup: $root is still there") }
    # Exactly this test channel's cache folder (the organisation name is the channel's).
    $cache = Join-Path $env:LOCALAPPDATA "LILITH-$channel"
    for ($i = 0; $i -lt 20 -and (Test-Path -LiteralPath $cache); ++$i) {
        try { Remove-Item -Recurse -Force -LiteralPath $cache } catch { Start-Sleep -Milliseconds 500 }
    }
    if (Test-Path -LiteralPath $cache) { $failures.Add("cleanup: $cache is still there") }
}

$times = @($runs | Where-Object { $null -ne $_.interactive } | ForEach-Object { [double]$_.interactive } | Sort-Object)
if ($times.Count) {
    Write-Host ("bench: cold start, {0} {1}: interactive p50 {2} ms, max {3} ms (budget {4} ms)" -f `
        $times.Count, $(if ($First) { 'first start' } else { 'warm starts' }), $times[[math]::Floor(($times.Count - 1) / 2)], $times[-1], $BudgetMs)
}
if ($failures.Count) {
    Write-Host 'bench_cold_start: FAIL'
    $failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host 'bench_cold_start: PASS'
exit 0
