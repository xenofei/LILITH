#requires -Version 7
<#
Fix round 4 (coordinator's ruling): run-all.ps1's attended and unattended blocks end to end, with
every Windows call faked (tests/sim-fakes.ps1), under the scripts' own Set-StrictMode -Version
Latest and $ErrorActionPreference = 'Stop'.

Past rounds shipped errors that showed only when the code ran: a strict-mode read of a removed
variable (S1.8's HelloWatched, review N3-C1), ConvertTo-Json of a live Process object (the
Sandbox lifecycle record, N3-C2), and the pop-up log reset before the stage record read it
(N3-I1). Here the owner's two commands run as the owner runs them, through run-all.ps1, each
stage in its own pwsh, and every stage record and result file is written and read back:
- the attended block: Part B (S1.2p, S1.3 with every refusal path of its owner fallback, S1.4,
  S1.5, S1.7's sleep and wake, S1.8 with its foreground loop), its cleanup, and the watched
  Sandbox run with the cover window and the lock;
- the unattended block: S1.7's stay-awake wait (with one touch), and 3 Sandbox runs, one of which
  doesn't close by itself and is ended through its pinned object.
Nothing on this PC is touched: no toast, shortcut, registry key or Hello credential is written,
no Sandbox starts, no input is injected, no power request is held, and no pop-up is shown. The
only real processes are the fakes' windowless pings (sim-fakes.ps1 says which). Everything is
written to a temp folder that is removed at the end (-Keep keeps it), and the spike's own
results\ folder is checked to be unchanged.

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
foreach ($d in 'bin', 'deploy', 'results', 'localappdata', 'programs', 'temp', 'vcredist', 'rusttest', 'build') { New-Item -ItemType Directory -Force -Path (Join-Path $sim $d) | Out-Null }
Set-Content -LiteralPath (Join-Path $sim 'sim.marker') -Value 'spike 11 simulation folder'
# Empty files where the scripts expect the built exes: they are copied and checked, never run.
foreach ($f in 'bin\winshell-core.exe', 'bin\winshell-app.exe', 'bin\winshell-hello.exe', 'bin\winshell-tool.exe', 'deploy\app_shell_spike.exe',
    'vcredist\vcruntime140.dll', 'vcredist\vcruntime140_1.dll', 'vcredist\msvcp140.dll', 'rusttest\winshell-0000.exe') {
    New-Item -ItemType File -Force -Path (Join-Path $sim $f) | Out-Null
}
# The owner's elevated listing from an earlier run, in the shape the run of 2026-10-23 left it.
$earlier = Join-Path $sim 'results\run-20261023T032541Z'
New-Item -ItemType Directory -Force -Path $earlier | Out-Null
@{ checks = @{ 'S1.7' = @{ verdict = 'IN PROGRESS'; details = @{ listing = @{ api = 'request'; listed_while_held = $true; gone_after_clear = $true } } } } } |
    ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $earlier 'checks.json')

function Get-RealResultsState {
    $r = Join-Path $Spike 'results'
    if (-not (Test-Path -LiteralPath $r)) { return '' }
    (@(Get-ChildItem -LiteralPath $r -Recurse -Force | ForEach-Object { "$($_.FullName)|$(if ($_.PSIsContainer) { '' } else { $_.Length })|$($_.LastWriteTimeUtc.Ticks)" }) | Sort-Object) -join "`n"
}
$realBefore = Get-RealResultsState

$saved = @{ WINSHELL_SIM_DIR = $env:WINSHELL_SIM_DIR; TEMP = $env:TEMP; TMP = $env:TMP }
$env:WINSHELL_SIM_DIR = $sim
$env:TEMP = Join-Path $sim 'temp'; $env:TMP = $env:TEMP   # the pop-ups' state files go there too

function Invoke-Block([string]$Block) {
    $t = [Diagnostics.Stopwatch]::StartNew()
    $out = & pwsh -NoProfile -NonInteractive -File (Join-Path $Spike 'run-all.ps1') -Block $Block -Simulate -DeployDir (Join-Path $sim 'deploy') 2>&1 | Out-String
    $code = $LASTEXITCODE
    Set-Content -LiteralPath (Join-Path $sim "console-$Block.txt") -Value $out
    Write-Host "---- $Block block: exit $code after $([int]$t.Elapsed.TotalSeconds) s"
    [pscustomobject]@{ Out = $out; Exit = $code }
}

try {
    $a = Invoke-Block 'Attended'
    $res = Join-Path $sim 'results'
    $checksA = Read-Doc (Join-Path $res 'checks.json')
    $observe = Read-Doc (Join-Path $res 'sandbox-observe.json')
    $u = Invoke-Block 'Unattended'
    $checksU = Read-Doc (Join-Path $res 'checks.json')
    $runs = Read-Doc (Join-Path $res 'sandbox-checks.json')

    # ---- the blocks ran without an error
    foreach ($b in @(@('Attended', $a), @('Unattended', $u))) {
        $o = $b[1].Out
        Expect "$($b[0]) block" ($b[1].Exit -eq 0) "run-all exit code $($b[1].Exit)"
        Expect "$($b[0]) block" ($o -notmatch 'ended with exit code') 'every stage exited 0'
        Expect "$($b[0]) block" ($o -notmatch 'stopped with an error') 'no check stopped with an error'
        Expect "$($b[0]) block" ($o -notmatch 'simulation: ') 'no fake was asked for something unexpected'
        Expect "$($b[0]) block" ($o -match 'Cleanup -> PASS') 'the cleanup passed'
    }

    # ---- Part B (the attended block's checks.json)
    $c = P $checksA 'checks'
    Expect 'checks.json (attended)' ($null -ne $c) 'written and read back'
    $verdicts = [ordered]@{ 'S1.2p' = 'RECORDED'; 'S1.3' = 'PASS'; 'S1.4' = 'PASS'; 'S1.5' = 'PASS (UIA saw 1 popup; the owner saw no second pop)'; 'S1.7' = 'PENDING (the stay-awake wait runs last)'; 'S1.8' = 'PASS' }
    foreach ($id in $verdicts.Keys) { $v = P (P $c $id) 'verdict'; Expect $id ($v -eq $verdicts[$id]) "verdict '$v' (want '$($verdicts[$id])')" }
    Expect 'S1.8 (N3-C1)' ((P (P (P $c 'S1.8') 'details') 'b_prompt_in_front') -eq $true) 'the foreground loop ran: the create prompt was seen in front'
    Expect 'S1.8 (N3-C1)' ((P (P (P $c 'S1.8') 'details') 'e_prompt_in_front') -eq $true) 'the verification prompt was seen in front'

    $t13 = @(P (P (P $c 'S1.3') 'details') 'trials')
    $att = { param($i) @(P $t13[$i] 'attempts') }
    Expect 'S1.3 fallback' ($t13.Count -eq 6) "6 trials ($($t13.Count))"
    if ($t13.Count -eq 6) {
        Expect 'S1.3 fallback: invoke' ((P $t13[0] 'click_method') -eq 'uia-invoke') "trial 1 clicked by UIA invoke ($(P $t13[0] 'click_method'))"
        $f2 = "$(P (& $att 1)[0] 'fallback')"
        Expect 'S1.3 fallback: every way refused' ($f2.StartsWith('the script refused to act') -and (P $t13[1] 'click_method') -eq 'owner') "trial 2: '$f2', clicked by $(P $t13[1] 'click_method')"
        $f3 = "$(P (& $att 2)[0] 'fallback')"
        Expect 'S1.3 fallback: owner active' ($f3.StartsWith('the owner was using the PC') -and (P $t13[2] 'click_method') -eq 'owner') "trial 3: '$f3'"
        $a4 = (& $att 3)[0]
        Expect 'S1.3 fallback: button refused' ((P $a4 'button_missing') -eq $true -and "$(P $a4 'fallback')".StartsWith('the script refused to act') -and (P $t13[3] 'click_method') -eq 'owner') "trial 4: '$(P $a4 'fallback')'"
        $uc = @(P $a4 'unknown_controls')
        Expect 'S1.3 unknown controls' ($uc.Count -ge 1 -and $null -ne (P $uc[0] 'automation_id') -and $null -eq (P $uc[0] 'name')) 'recorded by AutomationId, type and class, with no Name'
        Expect 'S1.3 fallback: button invoke' ((P $t13[4] 'click_method') -eq 'uia-invoke') "trial 5 ($(P $t13[4] 'click_method'))"
        Expect 'S1.3 fallback: a click without a launch is reposted' ((P $t13[5] 'posts') -eq 2) "trial 6 posts: $(P $t13[5] 'posts')"
    }
    $d13 = P (P $c 'S1.3') 'details'
    Expect 'S1.3 Run box' ($null -ne (P (P (P $d13 'recorded') 'run') 'cmdline')) 'the Run box launch was recorded'
    $s14 = P (P $c 'S1.4') 'details'
    Expect 'S1.4' ((P (P $s14 'with_a_synthetic') 'passes') -eq 5) "synthetic passes $(P (P $s14 'with_a_synthetic') 'passes')"
    Expect 'S1.4 blocker clicked to the front' (@(@(P $s14 'with_a') | Where-Object { (P (P $_ 'precondition') 'how') -eq 'synthetic click on the blocker' }).Count -eq 1) 'one blocker was clicked to the front by the script'
    Expect 'S1.5 dismissal (m7)' ((P (P (P (P $c 'S1.5') 'details') 'dismissal') 'by') -eq 'uia') "dismissed by '$(P (P (P (P $c 'S1.5') 'details') 'dismissal') 'by')' (Windows' own chrome is known in the scope)"
    Expect 'cleanup (attended)' ((P (P $checksA 'cleanup') 'pass') -eq $true) 'cleanup pass'

    $stA = @(P $checksA 'stages')
    $hoA = @(P $stA[0] 'hands_off')
    Expect 'pop-up records (N3-I1)' ($hoA.Count -ge 10) "the attended stage keeps its pop-up records ($($hoA.Count))"
    Expect 'pop-up records (m2)' (@($hoA | Where-Object { (P $_ 'shown') -eq $true -and $null -ne (P $_ 'shown_after_ms') }).Count -gt 0) 'shown pop-ups record when they showed'
    Expect 'pop-up records (m2)' (@($hoA | Where-Object { (P $_ 'popup_started') -eq $true -and (P $_ 'shown') -eq $false }).Count -gt 0) 'a pop-up that never showed is recorded as not shown'

    # ---- the watched Sandbox run
    Expect 'sandbox-observe.json (N3-C2)' ($null -ne $observe) 'written and read back'
    if ($null -ne $observe) {
        Expect 'watched run' ((P $observe 'verdict') -eq 'PASS') "verdict '$(P $observe 'verdict')'"
        $r1 = @(P $observe 'runs')[0]
        $own = @(P (P (P (P (P $r1 'items') 'lifecycle') 'file') 'started') 'own_procs')
        Expect 'lifecycle record (N3-C2)' ($own.Count -ge 1 -and @($own | Where-Object { @($_.PSObject.Properties.Name | Where-Object { $_ -notin 'pid', 'name', 'start' }).Count -gt 0 }).Count -eq 0) "own_procs hold pid, name and start only ($($own.Count))"
        Expect 'watched run: cover' ((P (P (P $observe 'observation') 'round2_cover') 'covered') -eq $true) 'the Sandbox window was covered'
        Expect 'watched run: lock' ($null -ne (P (P (P $observe 'observation') 'round3_lock') 'unlocked_after_s')) 'the lock and the sign-in were seen'
        Expect 'watched run: pop-up record (N3-I1)' (@(P $observe 'hands_off').Count -ge 1) 'the cover pop-up is recorded'
        Expect 'lifecycle.json' (Test-Path -LiteralPath (Join-Path $res 'sandbox\observe-1\lifecycle.json')) 'kept in results\sandbox\observe-1'
    }

    # ---- S1.7's stay-awake wait (the unattended block)
    $s17 = P (P $checksU 'checks') 'S1.7'
    Expect 'S1.7 (unattended)' ((P $s17 'verdict') -eq 'PASS') "verdict '$(P $s17 'verdict')'"
    $stay = P (P $s17 'details') 'stay_awake'
    Expect 'S1.7 one touch' (@(P $stay 'restarts').Count -eq 1) "restarts $(@(P $stay 'restarts').Count)"
    Expect 'S1.7 idle reset (N3-I2)' ($null -ne (P (P $stay 'idle_reset') 'at')) 'one idle-timer reset after the after-reads is recorded'
    $stU = @(P $checksU 'stages')
    Expect 'stages' ($stU.Count -eq 2) "both stages are on record ($($stU.Count))"
    if ($stU.Count -eq 2) {
        $hoU = @(P $stU[1] 'hands_off')
        Expect 'pop-up records (N3-I1)' ($hoU.Count -ge 2) "the stay-awake stage keeps its pop-up records ($($hoU.Count))"
        Expect 'pop-up records (m2)' (@($hoU | Where-Object { (P $_ 'popup_started') -eq $true -and $null -eq (P $_ 'shown') }).Count -ge 1) 'the restarted pop-up (no warning wait) is recorded as not checked'
    }
    Expect 'cleanup (unattended)' ((P (P $checksU 'cleanup') 'pass') -eq $true) 'cleanup pass'

    # ---- the 3 Sandbox runs
    Expect 'sandbox-checks.json (N3-C2)' ($null -ne $runs) 'written and read back'
    if ($null -ne $runs) {
        Expect 'Sandbox runs' ((P $runs 'verdict') -eq 'PASS') "verdict '$(P $runs 'verdict')'"
        $rr = @(P $runs 'runs')
        Expect 'Sandbox runs' ($rr.Count -eq 3) "3 runs ($($rr.Count))"
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
