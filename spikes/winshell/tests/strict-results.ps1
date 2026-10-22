#requires -Version 7
<#
Rerun review I1 and M1: the verdict logic of S1.3, S1.4 and S1.7 under run-spike.ps1's own
Set-StrictMode -Version Latest (dot-sourced with -LibraryOnly; nothing else runs). In run 1's
code, S1.7 read `$stay.result` from a dictionary that has `result` only when the stay-awake
wait was skipped, which strict mode turns into an error at the end of the longest unattended
wait. These cases build the result dictionaries exactly as the checks do.

Run by tests/strict_results.rs (cargo test). Exit 0 = every case passed.
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\run-spike.ps1') -LibraryOnly

$failures = [System.Collections.Generic.List[string]]::new()
function Expect([string]$Case, $Got, $Want) {
    if ("$Got" -ceq "$Want") { Write-Host "ok   $Case : $Got" }
    else { Write-Host "FAIL $Case : got '$Got', want '$Want'"; $failures.Add($Case) }
}
function Try-Case([string]$Case, [scriptblock]$Body, $Want) {
    try { Expect $Case (& $Body) $Want }
    catch { Write-Host "FAIL $Case : threw $($_.Exception.Message)"; $failures.Add($Case) }
}

# ---- S1.7: the dictionaries Power-Listing and Power-StayAwake return
$listed = [ordered]@{ api = 'request'; listed_while_held = $true; gone_after_clear = $true }
$notListed = [ordered]@{ api = 'request'; listed_while_held = $false; gone_after_clear = $true }
$stayRan = [ordered]@{ api = 'request'; timeout_s = 600; waited_s = 930; slept_events = @(); pass = $true }
$staySlept = [ordered]@{ api = 'request'; timeout_s = 600; waited_s = 930; slept_events = @(1); pass = $false }
$staySkipped = [ordered]@{ api = 'request'; result = 'skipped'; why = 'sleep timeout is never' }

Try-Case 'S1.7 stay-awake ran and passed' { (Get-S17Verdict $listed $stayRan $null $true).verdict } 'PASS'
Try-Case 'S1.7 stay-awake skipped' { (Get-S17Verdict $listed $staySkipped $null $true).verdict } 'PASS'
Try-Case 'S1.7 no sleep broadcasts' { (Get-S17Verdict $listed $stayRan $null $false).verdict } 'FAIL'
Try-Case 'S1.7 slept while held, no ste' { (Get-S17Verdict $listed $staySlept $null $true).verdict } 'FAIL'
Try-Case 'S1.7 request failed, ste passed' {
    (Get-S17Verdict $notListed $stayRan ([ordered]@{ listing = $listed; stay_awake = $stayRan }) $true).verdict
} 'PASS (SetThreadExecutionState)'
Try-Case 'S1.7 request slept, ste skipped' {
    (Get-S17Verdict $listed $staySlept ([ordered]@{ listing = $listed; stay_awake = $staySkipped }) $true).api
} 'SetThreadExecutionState'
Try-Case 'S1.7 both failed' {
    (Get-S17Verdict $notListed $staySlept ([ordered]@{ listing = $notListed; stay_awake = $staySlept }) $true).verdict
} 'FAIL'
Try-Case 'S1.7 ste listing only (interrupted)' {
    (Get-S17Verdict $notListed $stayRan ([ordered]@{ listing = $listed }) $true).verdict
} 'FAIL'
Try-Case 'Test-PowerApiOk on a run stay-awake' { Test-PowerApiOk $listed $stayRan } 'True'
Try-Case 'Test-PowerApiOk on a skipped stay-awake' { Test-PowerApiOk $listed $staySkipped } 'True'
Try-Case 'Test-PowerApiOk on nothing' { Test-PowerApiOk $null $null } 'False'

# ---- S1.4: trial dictionaries as Trial-WithA and Trial-NoA build them
function T([bool]$Valid, [bool]$Pass) { [ordered]@{ trial = 0; valid = $Valid; pass = $Pass } }
$five = { param($v, $p) @(1..5 | ForEach-Object { T $v $p }) }
$allOk = & $five $true $true
$oneValidFail = @((T $true $false)) + @(1..4 | ForEach-Object { T $true $true })
$oneInvalid = @((T $false $false)) + @(1..4 | ForEach-Object { T $true $true })

Try-Case 'S1.4 all pass' { Get-S14Verdict $allOk @() $allOk } 'PASS'
Try-Case 'S1.4 basic failed, fallback passed' { Get-S14Verdict $oneValidFail $allOk $allOk } 'PASS (fallback recipe)'
Try-Case 'S1.4 basic and fallback failed' { Get-S14Verdict $oneValidFail $oneValidFail $allOk } 'FAIL'
Try-Case 'S1.4 basic and fallback failed, a no-A trial invalid' { Get-S14Verdict $oneValidFail $oneValidFail $oneInvalid } 'FAIL'
Try-Case 'S1.4 a valid no-A failure with invalid with-A trials' { Get-S14Verdict $oneInvalid @() $oneValidFail } 'FAIL'
Try-Case 'S1.4 one invalid with-A trial' { Get-S14Verdict $oneInvalid @() $allOk } 'INCOMPLETE (invalid trials: rerun S1.4)'
Try-Case 'S1.4 basic failed, fallback had an invalid trial' { Get-S14Verdict $oneValidFail $oneInvalid $allOk } 'INCOMPLETE (invalid trials: rerun S1.4)'
Try-Case 'S1.4 one invalid no-A trial' { Get-S14Verdict $allOk @() $oneInvalid } 'INCOMPLETE (invalid trials: rerun S1.4)'
Try-Case 'S1.4 a single trial each (no unrolling trouble)' { Get-S14Verdict @((T $true $true)) @() @((T $true $true)) } 'INCOMPLETE (invalid trials: rerun S1.4)'

# ---- S1.3: trial dictionaries as Check-S13 builds them
function C([bool]$Got, [bool]$Exact) { [ordered]@{ trial = 0; got_click = $Got; cmdline = $(if ($Got) { 'x' } else { $null }); exact = $Exact } }
Try-Case 'S1.3 six exact' { Get-S13Verdict @(1..6 | ForEach-Object { C $true $true }) } 'PASS'
Try-Case 'S1.3 one inexact' { Get-S13Verdict (@((C $true $false)) + @(1..5 | ForEach-Object { C $true $true })) } 'FAIL'
Try-Case 'S1.3 one never clicked' { Get-S13Verdict (@((C $false $false)) + @(1..5 | ForEach-Object { C $true $true })) } 'INCOMPLETE (1 trial(s) never got a click: rerun S1.3)'
Try-Case 'S1.3 never clicked and inexact' { Get-S13Verdict (@((C $false $false), (C $true $false)) + @(1..4 | ForEach-Object { C $true $true })) } 'FAIL'

if ($failures.Count -gt 0) { Write-Host "$($failures.Count) failed"; exit 1 }
Write-Host 'all cases passed'
exit 0
