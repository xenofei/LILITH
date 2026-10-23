#requires -Version 7
<#
Owner run 3 (full automation): run-all.ps1's one unattended command end to end, with every Windows
call faked (tests/sim-fakes.ps1), under the scripts' own Set-StrictMode -Version Latest and
$ErrorActionPreference = 'Stop'. It runs Part B (the host checks) then Part C (the Sandbox runs),
each stage in its own pwsh, and every check's record and result file is written and read back.

It exercises the owner-run-3 redesign:
- the toast checks (S1.5) decide from each toast's own events, not UI Automation;
- S1.3 and S1.4 are settled from the owner's recorded runs and not re-run;
- S1.7 is judged from the execution-state self-check, with no real sleep;
- S1.8's Hello prompt placement is probed across a normal, a topmost and a fullscreen window and
  the prompt is cancelled (no gesture); the route comes from the settled crypto evidence;
- the Sandbox Qt step records both grabs with their platform plugins;
- no owner input anywhere.

It also still guards the fix-round-4 runtime traps: no strict-mode read of a removed variable, no
ConvertTo-Json of a live Process object, the pop-up log kept for the stage records.

Nothing on this PC is touched: no toast, shortcut, registry key or Hello credential is written, no
Sandbox starts, no input is injected, no power request is held, no pop-up is shown. The only real
processes are the fakes' windowless pings. Everything goes to a temp folder removed at the end
(-Keep keeps it), and the spike's own results\ folder is checked to be unchanged.

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
# Empty files where the scripts expect the built exes and the Qt plugins: copied and checked,
# never run.
foreach ($f in 'bin\winshell-core.exe', 'bin\winshell-app.exe', 'bin\winshell-hello.exe', 'bin\winshell-tool.exe', 'deploy\app_shell_spike.exe',
    'deploy\platforms\qwindows.dll', 'vcredist\vcruntime140.dll', 'vcredist\vcruntime140_1.dll', 'vcredist\msvcp140.dll', 'rusttest\winshell-0000.exe') {
    New-Item -ItemType File -Force -Path (Join-Path $sim $f) | Out-Null
}

# The owner's earlier runs, as Find-SettledVerdict / Find-PriorCheckDetails / Find-EarlierListing
# read them: S1.3 and S1.4 settled PASS (with details), S1.6 PASS, S1.7's elevated listing and
# sleep-and-wake record, and S1.8's crypto evidence (create, 10 identical signatures, delete),
# recorded by the owner's run of 2026-10-23 even though its S1.8 verdict was FAIL for placement.
$ownerRun = Join-Path $sim 'results\run-20261023T204648Z'
New-Item -ItemType Directory -Force -Path $ownerRun | Out-Null
$ownerChecks = @{
    checks = @{
        'S1.1' = @{ verdict = 'PASS'; details = @{} }
        'S1.2' = @{ verdict = 'PASS'; details = @{} }
        'S1.3' = @{ verdict = 'PASS'; details = @{ expected = 'x'; trials = @(1..6 | ForEach-Object { @{ trial = $_; exact = $true } }) } }
        'S1.4' = @{ verdict = 'PASS'; details = @{
                with_a = @(1..5 | ForEach-Object { @{ trial = $_; pass = $true; click_method = 'owner' } })
                no_a = @(1..5 | ForEach-Object { @{ trial = $_; pass = $true; click_method = 'owner' } })
                with_a_synthetic = @{ recorded_only = $true; passes = 5; owner_clicked = 0 } } }
        'S1.6' = @{ verdict = 'PASS'; details = @{} }
        'S1.7' = @{ verdict = 'IN PROGRESS'; details = @{
                listing = @{ api = 'request'; listed_while_held = $true; gone_after_clear = $true }
                sleep_wake = @{ pass = $true; broadcasts = @('PBT_APMSUSPEND', 'PBT_APMRESUMEAUTOMATIC') } } }
        'S1.8' = @{ verdict = 'FAIL (passphrase only: report to the owner)'; details = @{
                b_create = @{ status = 'Success' }; c_identical = $true; c_count = 10
                f_delete = @{ deleted = $true }; f_after = @{ status = 'NotFound' } } }
    }
}
$ownerChecks | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $ownerRun 'checks.json')

function Get-RealResultsState {
    $r = Join-Path $Spike 'results'
    if (-not (Test-Path -LiteralPath $r)) { return '' }
    (@(Get-ChildItem -LiteralPath $r -Recurse -Force | ForEach-Object { "$($_.FullName)|$(if ($_.PSIsContainer) { '' } else { $_.Length })|$($_.LastWriteTimeUtc.Ticks)" }) | Sort-Object) -join "`n"
}
$realBefore = Get-RealResultsState

$saved = @{ WINSHELL_SIM_DIR = $env:WINSHELL_SIM_DIR; TEMP = $env:TEMP; TMP = $env:TMP }
$env:WINSHELL_SIM_DIR = $sim
$env:TEMP = Join-Path $sim 'temp'; $env:TMP = $env:TEMP   # the pop-ups' state files go there too

try {
    $t = [Diagnostics.Stopwatch]::StartNew()
    $out = & pwsh -NoProfile -NonInteractive -File (Join-Path $Spike 'run-all.ps1') -Simulate -DeployDir (Join-Path $sim 'deploy') 2>&1 | Out-String
    $code = $LASTEXITCODE
    Set-Content -LiteralPath (Join-Path $sim 'console.txt') -Value $out
    Write-Host "---- run-all: exit $code after $([int]$t.Elapsed.TotalSeconds) s"
    $res = Join-Path $sim 'results'
    $checks = Read-Doc (Join-Path $res 'checks.json')
    $runs = Read-Doc (Join-Path $res 'sandbox-checks.json')

    # ---- the one command ran without an error, no owner input
    Expect 'run-all' ($code -eq 0) "exit code $code"
    Expect 'run-all' ($out -notmatch 'ended with exit code') 'every stage exited 0'
    Expect 'run-all' ($out -notmatch 'stopped with an error') 'no check stopped with an error'
    Expect 'run-all' ($out -notmatch 'simulation: ') 'no fake was asked for something unexpected'
    Expect 'run-all' ($out -notmatch 'YOUR TURN|YOUR CLICK|\[y/n\]') 'no owner turn was asked for'
    Expect 'run-all' ($out -match 'Cleanup -> PASS') 'the cleanup passed'

    $c = P $checks 'checks'
    Expect 'checks.json' ($null -ne $c) 'written and read back'

    # ---- the host checks, all automatic
    Expect 'S1.2p' ((P (P $c 'S1.2p') 'verdict') -eq 'RECORDED') "verdict '$(P (P $c 'S1.2p') 'verdict')'"

    $v13 = "$(P (P $c 'S1.3') 'verdict')"
    Expect 'S1.3 settled' ($v13.StartsWith('PASS (settled by')) "verdict '$v13'"
    Expect 'S1.3 grammar' ((P (P (P $c 'S1.3') 'details') 'grammar_exact') -eq $true) 'the activation grammar matched exactly (ShellExecute, no click)'

    $v14 = "$(P (P $c 'S1.4') 'verdict')"
    Expect 'S1.4 settled' ($v14.StartsWith('PASS (settled by')) "verdict '$v14'"
    Expect 'S1.4 not re-run' ($out -notmatch 'winshell blocker.*click') 'no owner click was asked for'

    $v15 = "$(P (P $c 'S1.5') 'verdict')"
    $d15 = P (P $c 'S1.5') 'details'
    Expect 'S1.5 events' ($v15 -eq 'PASS') "verdict '$v15'"
    Expect 'S1.5 method' ("$(P $d15 'method')" -match 'events') 'decided by the toasts'' own events'
    Expect 'S1.5 no re-pop' ((P $d15 'no_second_pop') -eq $true) 'the progress toast did not re-pop'
    Expect 'S1.5 quiet' ((P (P $d15 'quiet') 'no_pop') -eq $true -and (P (P $d15 'quiet') 'in_centre') -eq $true) 'the quiet toast is in the centre and did not pop'
    Expect 'S1.5 control' ((P (P $d15 'quiet') 'control_toast_popped') -eq $true) 'the control toast popped (the pop signal fired this run)'

    $s17 = P (P $c 'S1.7') 'details'
    Expect 'S1.7' ((P (P $c 'S1.7') 'verdict') -eq 'PASS') "verdict '$(P (P $c 'S1.7') 'verdict')'"
    Expect 'S1.7 self-check' ((P $s17 'self_check') -eq $true -and (P $s17 'api_set_ok') -eq $true -and (P $s17 'api_cleared_ok') -eq $true) 'the execution-state self-check held'
    Expect 'S1.7 no real sleep' ("$(P $s17 'sleep_wake_note')" -match 'no real sleep') 'no real sleep was done; the broadcast record is from the owner run'

    $s18 = P (P $c 'S1.8') 'details'
    Expect 'S1.8' ((P (P $c 'S1.8') 'verdict') -eq 'PASS') "verdict '$(P (P $c 'S1.8') 'verdict')'"
    Expect 'S1.8 route' ((P $s18 'route') -eq 'KEK') "route '$(P $s18 'route')'"
    $pl = P $s18 'placement'
    $scn = @(P $pl 'scenarios')
    Expect 'S1.8 placement' ((P $pl 'ok') -eq $true -and $scn.Count -eq 3) "placement over $($scn.Count) scenarios, ok=$(P $pl 'ok')"
    Expect 'S1.8 scenarios' (@($scn | ForEach-Object { $_.scenario }) -join ',' -eq 'normal,topmost,fullscreen') "scenarios: $(@($scn | ForEach-Object { $_.scenario }) -join ',')"
    Expect 'S1.8 cancelled' (@($scn | Where-Object { "$($_.result_after_cancel)" -ne 'Canceled' }).Count -eq 0) 'each prompt was cancelled (no gesture)'
    Expect 'S1.8 crypto settled' ((P (P $s18 'crypto_settled') 'create_ok') -eq $true -and (P (P $s18 'crypto_settled') 'sign_identical') -eq $true) 'the KEK crypto is settled from the owner run'

    Expect 'cleanup' ((P (P $checks 'cleanup') 'pass') -eq $true) 'cleanup pass'

    # ---- the pop-up records survive to the stage record (N3-I1), and the Hello placement probe
    # recorded its pop-ups (shown, when, not checked).
    $stage = @(P $checks 'stages')[0]
    $ho = @(P $stage 'hands_off')
    Expect 'pop-up records (N3-I1)' ($ho.Count -ge 3) "the stage keeps its pop-up records ($($ho.Count))"
    Expect 'pop-up records (m2)' (@($ho | Where-Object { (P $_ 'shown') -eq $true -and $null -ne (P $_ 'shown_after_ms') }).Count -gt 0) 'shown pop-ups record when they showed'

    # ---- the 3 Sandbox runs, unattended (no watched run, no lock)
    Expect 'sandbox-checks.json' ($null -ne $runs) 'written and read back'
    Expect 'no watched run' (-not (Test-Path -LiteralPath (Join-Path $res 'sandbox-observe.json'))) 'no sandbox-observe.json (no watched run)'
    if ($null -ne $runs) {
        Expect 'Sandbox runs' ((P $runs 'verdict') -eq 'PASS') "verdict '$(P $runs 'verdict')'"
        $rr = @(P $runs 'runs')
        Expect 'Sandbox runs' ($rr.Count -eq 3) "3 runs ($($rr.Count))"
        $qt = P (P (P $rr[0] 'items') 'qt') 'file'
        Expect 'Sandbox Qt' ((P (P $qt 'onscreen') 'available') -eq $true -and (P (P $qt 'offscreen') 'available') -eq $true) 'both Qt grabs recorded with their platform plugins'
        if ($rr.Count -eq 3) {
            $stop2 = "$(P (P (P (P (P $rr[1] 'items') 'lifecycle') 'file') 'stop') 'method')"
            Expect 'Sandbox run 2 ended through its pinned object' ($stop2.StartsWith('Kill')) "stop: '$stop2'"
        }
        Expect 'host keep-awake' ((P $runs 'host_kept_awake') -eq $true) 'held for the runs'
        foreach ($n in 1..3) { Expect "lifecycle.json run $n" (Test-Path -LiteralPath (Join-Path $res "sandbox\run-$n\lifecycle.json")) 'kept' }
    }

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
