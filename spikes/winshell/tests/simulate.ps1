#requires -Version 7
<#
Scope cut (coordinator's ruling, 2026-10-23): run-all.ps1's default run, S1.9 alone, end to end
with every Windows call faked (tests/sim-fakes.ps1), under the scripts' own Set-StrictMode
-Version Latest and $ErrorActionPreference = 'Stop'. Each stage runs in its own pwsh, and every
record and result file is written and read back.

What it checks:
- the default run is S1.9 only: no Part B stage, no host checks file;
- no prompt anywhere (the fakes' Read-Host throws);
- 3 Sandbox runs, one of which doesn't close by itself and is ended through its pinned object;
- the Qt item is produced by the real Invoke-QtItem over the deploy copy the run mapped, which,
  like Task 0's trimmed deploy, has platforms\qwindows.dll only: the on-screen grab is judged, and
  the missing offscreen plugin is recorded as "not tested";
- nothing is ever copied into the deploy copy (no qoffscreen.dll from elsewhere);
- no Process object or environment block in any record (fix round 4, N3-C2).

Nothing on this PC is touched: no Sandbox starts, no input is injected, no power request is held,
no pop-up is shown. The only real processes are the fakes' windowless pings. Everything goes to a
temp folder removed at the end (-Keep keeps it), and the spike's own results\ folder is checked to
be unchanged.

Run by tests/simulate.rs (cargo test). Exit 0 = every case passed.
#>
param([switch]$Keep)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Spike = Split-Path $PSScriptRoot
$failures = [System.Collections.Generic.List[string]]::new()
function Expect([string]$Case, [bool]$Ok, [string]$What) {
    if ($Ok) { Write-Host "ok   $Case : $What" } else { Write-Host "FAIL $Case : $What"; $failures.Add("$Case : $What") }
}
function P($Obj, [string]$Name) {
    if ($null -eq $Obj) { return $null }
    if ($Obj -is [System.Collections.IDictionary]) { return $Obj[$Name] }
    $p = $Obj.PSObject.Properties[$Name]
    if ($p) { $p.Value } else { $null }
}
function Read-Doc([string]$Path) { if (Test-Path -LiteralPath $Path) { Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json } else { $null } }

# ---------------------------------------------------------------- the folder the fakes use

$sim = Join-Path ([IO.Path]::GetTempPath()) ('winshell-sim-' + [guid]::NewGuid().ToString('N').Substring(0, 12))
foreach ($d in 'bin', 'deploy', 'deploy\platforms', 'results', 'localappdata', 'programs', 'temp', 'vcredist', 'rusttest', 'build') { New-Item -ItemType Directory -Force -Path (Join-Path $sim $d) | Out-Null }
Set-Content -LiteralPath (Join-Path $sim 'sim.marker') -Value 'spike 11 simulation folder'
# Empty files where the scripts expect the built exes and the deploy: copied and checked, never
# run. The deploy is shaped like Task 0's trimmed one: platforms\qwindows.dll only.
foreach ($f in 'bin\winshell-core.exe', 'bin\winshell-app.exe', 'bin\winshell-hello.exe', 'bin\winshell-tool.exe', 'deploy\app_shell_spike.exe',
    'deploy\platforms\qwindows.dll', 'vcredist\vcruntime140.dll', 'vcredist\vcruntime140_1.dll', 'vcredist\msvcp140.dll', 'rusttest\winshell-0000.exe') {
    New-Item -ItemType File -Force -Path (Join-Path $sim $f) | Out-Null
}

function Get-RealResultsState {
    $r = Join-Path $Spike 'results'
    if (-not (Test-Path -LiteralPath $r)) { return '' }
    (@(Get-ChildItem -LiteralPath $r -Recurse -Force | ForEach-Object { "$($_.FullName)|$(if ($_.PSIsContainer) { '' } else { $_.Length })|$($_.LastWriteTimeUtc.Ticks)" }) | Sort-Object) -join "`n"
}
$realBefore = Get-RealResultsState

$saved = @{ WINSHELL_SIM_DIR = $env:WINSHELL_SIM_DIR; TEMP = $env:TEMP; TMP = $env:TMP }
$env:WINSHELL_SIM_DIR = $sim
$env:TEMP = Join-Path $sim 'temp'; $env:TMP = $env:TEMP

try {
    $t = [Diagnostics.Stopwatch]::StartNew()
    $out = & pwsh -NoProfile -NonInteractive -File (Join-Path $Spike 'run-all.ps1') -Simulate -DeployDir (Join-Path $sim 'deploy') 2>&1 | Out-String
    $code = $LASTEXITCODE
    Set-Content -LiteralPath (Join-Path $sim 'console.txt') -Value $out
    Write-Host "---- run-all: exit $code after $([int]$t.Elapsed.TotalSeconds) s"
    $res = Join-Path $sim 'results'
    $runs = Read-Doc (Join-Path $res 'sandbox-checks.json')

    # ---- the default run: S1.9 only, no prompt, no error
    Expect 'run-all' ($code -eq 0) "exit code $code"
    Expect 'run-all' ($out -notmatch 'ended with exit code') 'every stage exited 0'
    Expect 'run-all' ($out -notmatch 'simulation: ') 'no fake was asked for something unexpected, and no prompt was reached'
    Expect 'run-all' ($out -notmatch 'YOUR TURN|YOUR CLICK|\[y/n\]') 'no owner turn was asked for'
    Expect 'default is S1.9 only' ($out -notmatch 'Part B') 'no Part B stage ran'
    Expect 'default is S1.9 only' (-not (Test-Path -LiteralPath (Join-Path $res 'checks.json'))) 'no host checks file was written'

    # ---- the 3 Sandbox runs
    Expect 'sandbox-checks.json' ($null -ne $runs) 'written and read back'
    if ($null -ne $runs) {
        Expect 'Sandbox runs' ((P $runs 'verdict') -eq 'PASS') "verdict '$(P $runs 'verdict')'"
        $rr = @(P $runs 'runs')
        Expect 'Sandbox runs' ($rr.Count -eq 3) "3 runs ($($rr.Count))"
        Expect 'Qt bar recorded' ("$(P $runs 'qt_bar')" -match 'on-screen grab only' -and "$(P $runs 'qt_bar')" -match 'no qoffscreen\.dll') "qt_bar: '$(P $runs 'qt_bar')'"
        if ($rr.Count -ge 1) {
            $qt = P (P (P $rr[0] 'items') 'qt') 'file'
            Expect 'Sandbox Qt: real decision' ((P (P $qt 'onscreen') 'launched') -eq $true -and (P (P $qt 'onscreen') 'nonflat') -eq $true) 'the on-screen grab was launched (qwindows.dll present) and judged'
            Expect 'Sandbox Qt: offscreen not tested' ((P (P $qt 'offscreen') 'tested') -eq $false -and "$(P (P $qt 'offscreen') 'reason')" -match 'ships no qoffscreen\.dll') "offscreen: '$(P (P $qt 'offscreen') 'reason')'"
            Expect 'Sandbox Qt: plugins as deployed' (@(P $qt 'platform_plugins') -join ',' -eq 'qwindows.dll') "platform plugins: $(@(P $qt 'platform_plugins') -join ',')"
        }
        if ($rr.Count -eq 3) {
            $stop2 = "$(P (P (P (P (P $rr[1] 'items') 'lifecycle') 'file') 'stop') 'method')"
            Expect 'Sandbox run 2 ended through its pinned object' ($stop2.StartsWith('Kill')) "stop: '$stop2'"
            $own = @(P (P (P (P (P $rr[0] 'items') 'lifecycle') 'file') 'started') 'own_procs')
            Expect 'lifecycle record (N3-C2)' ($own.Count -ge 1 -and @($own | Where-Object { @($_.PSObject.Properties.Name | Where-Object { $_ -notin 'pid', 'name', 'start' }).Count -gt 0 }).Count -eq 0) "own_procs hold pid, name and start only ($($own.Count))"
        }
        Expect 'host keep-awake' ((P $runs 'host_kept_awake') -eq $true) 'held for the runs'
        foreach ($n in 1..3) { Expect "lifecycle.json run $n" (Test-Path -LiteralPath (Join-Path $res "sandbox\run-$n\lifecycle.json")) 'kept' }
    }
    Expect 'deploy copy untouched' (-not (Test-Path -LiteralPath (Join-Path $sim 'build\sandbox\deploy-copy\platforms\qoffscreen.dll'))) 'no qoffscreen.dll was added to the deploy copy'
    Expect 'no watched run' (-not (Test-Path -LiteralPath (Join-Path $res 'sandbox-observe.json'))) 'no sandbox-observe.json'

    # ---- every record serialised and read back; no Process object or environment block in any
    $files = @(Get-ChildItem -LiteralPath $res -Recurse -File -Filter *.json)
    $bad = @(); $leaked = @()
    foreach ($f in $files) {
        $text = Get-Content -Raw -LiteralPath $f.FullName
        try { [void]($text | ConvertFrom-Json) } catch { $bad += $f.Name }
        if ($text -match '"(StartInfo|EnvironmentVariables|SafeHandle|MainModule)"') { $leaked += $f.Name }
    }
    Expect 'result files' ($files.Count -ge 20 -and $bad.Count -eq 0) "$($files.Count) JSON files, all parse ($($bad -join ', '))"
    Expect 'result files' ($leaked.Count -eq 0) "no Process object or environment block in a record ($($leaked -join ', '))"
    Expect "the spike's own results" ((Get-RealResultsState) -eq $realBefore) 'spikes\winshell\results\ is unchanged'
}
finally {
    foreach ($k in $saved.Keys) {
        if ($null -eq $saved[$k]) { Remove-Item -Path "Env:\$k" -ErrorAction SilentlyContinue } else { Set-Item -Path "Env:\$k" -Value $saved[$k] }
    }
    if ($Keep -or $failures.Count -gt 0) { Write-Host "Simulation folder kept: $sim" }
    else { Remove-Item -LiteralPath $sim -Recurse -Force -ErrorAction SilentlyContinue }
}

if ($failures.Count -gt 0) { Write-Host "$($failures.Count) failed"; exit 1 }
Write-Host 'all cases passed'
exit 0
