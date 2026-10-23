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

# ---- S1.7 after the automation round: the parts, the dropped listing, input during the wait
$stayInvalid = [ordered]@{ api = 'request'; timeout_s = 600; waited_s = 930; slept_events = @(); pass = $null; result = 'invalid'; why = 'input' }
Try-Case 'S1.7 stay-awake ran, sleep-and-wake not yet' { (Get-S17Verdict $listed $stayRan $null $null).verdict } 'INCOMPLETE (the sleep-and-wake check has not run)'
Try-Case 'S1.7 input during the wait' { (Get-S17Verdict $listed $stayInvalid $null $true).verdict } 'INCOMPLETE (input during the stay-awake wait: rerun it)'
Try-Case 'S1.7 no listing on record' { (Get-S17Verdict $null $stayRan $null $true).verdict } 'INCOMPLETE (no powercfg listing on record)'
Try-Case 'S1.7 ste with no listing (dropped elevated step)' {
    (Get-S17Verdict $listed $staySlept ([ordered]@{ listing = $null; stay_awake = $stayRan }) $true).verdict
} 'PASS (SetThreadExecutionState)'
Try-Case 'Test-NeedSte: slept while held' { Test-NeedSte $listed $staySlept } 'True'
Try-Case 'Test-NeedSte: input during the wait' { Test-NeedSte $listed $stayInvalid } 'False'
Try-Case 'Test-NeedSte: no listing, stay passed' { Test-NeedSte $null $stayRan } 'False'
Try-Case 'Test-NeedSte: listing says not listed' { Test-NeedSte $notListed $stayRan } 'True'

# The owner's listing from an earlier run is found in results\run-*\checks.json, in the shape
# the run of 2026-10-23 left it (S1.7 IN PROGRESS, details.listing), and in an ERROR's partial.
$tmpResults = Join-Path ([IO.Path]::GetTempPath()) ('winshell-results-test-' + [guid]::NewGuid().ToString('N'))
try {
    $savedResults = $Results
    $Results = $tmpResults
    New-Item -ItemType Directory -Force -Path (Join-Path $tmpResults 'run-20261023T032541Z'), (Join-Path $tmpResults 'run-20261024T010000Z') | Out-Null
    @{ checks = @{ 'S1.7' = @{ verdict = 'IN PROGRESS'; details = @{ listing = @{ api = 'request'; listed_while_held = $true; gone_after_clear = $true; other_system_requests = '' } } } } } |
        ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $tmpResults 'run-20261023T032541Z\checks.json')
    @{ checks = @{ 'S1.5' = @{ verdict = 'FAIL' } } } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $tmpResults 'run-20261024T010000Z\checks.json')
    $Report = [ordered]@{ checks = [ordered]@{} }
    Try-Case 'Find-EarlierListing finds the 2026-10-23 listing' { $f = Find-EarlierListing; "$($f.source) $($f.listing.listed_while_held)" } 'results\run-20261023T032541Z True'
    $Report.checks['S1.7'] = @{ verdict = 'ERROR'; details = @{ message = 'x'; partial = @{ listing = @{ api = 'request'; listed_while_held = $true; gone_after_clear = $false }; listing_source = 'results\run-x' } } }
    Try-Case 'Get-ListingFrom reads an ERROR''s partial' { $f = Find-EarlierListing; "$($f.source) $($f.listing.gone_after_clear)" } 'results\run-x False'
    $Report = [ordered]@{ checks = [ordered]@{} }
    Remove-Item -LiteralPath (Join-Path $tmpResults 'run-20261023T032541Z') -Recurse -Force
    Try-Case 'Find-EarlierListing with none on record' { $null -eq (Find-EarlierListing) } 'True'
} finally {
    $Results = $savedResults
    Remove-Item -LiteralPath $tmpResults -Recurse -Force -ErrorAction SilentlyContinue
}

# ---- Detection: popup appearances from UIA samples, and the Hello prompt's foreground
function Smp([double]$T, $Present) { [ordered]@{ t = $T; present = $Present } }
Try-Case 'one appearance' { @(Get-Appearances @((Smp 0 $false), (Smp 0.25 $true), (Smp 0.5 $true), (Smp 5 $true), (Smp 5.25 $false))).Count } '1'
Try-Case 'one missed poll inside a banner is not a second pop' { @(Get-Appearances @((Smp 0.25 $true), (Smp 0.5 $false), (Smp 0.75 $true))).Count } '1'
Try-Case 'an unknown poll is skipped' { @(Get-Appearances @((Smp 0.25 $true), (Smp 0.5 $null), (Smp 0.75 $true))).Count } '1'
Try-Case 'a second pop after the banner went' { @(Get-Appearances @((Smp 0.25 $true), (Smp 5 $true), (Smp 5.25 $false), (Smp 9 $false), (Smp 9.25 $true))).Count } '2'
Try-Case 'a slow poll with no absence between is one appearance' { @(Get-Appearances @((Smp 0.25 $true), (Smp 2.5 $true))).Count } '1'
Try-Case 'a single missed poll then a slow poll is not a second pop' { @(Get-Appearances @((Smp 0.25 $true), (Smp 0.5 $false), (Smp 2.5 $true))).Count } '1'
Try-Case 'never shown' { @(Get-Appearances @((Smp 0.25 $false), (Smp 0.5 $null))).Count } '0'
Try-Case 'appearance times' { $a = @(Get-Appearances @((Smp 1.5 $true), (Smp 6 $true))); "$($a[0]['from_s'])-$($a[0]['to_s'])" } '1.5-6'

# ---- S1.8: the Hello prompt in front (automation review I4)
$term = [ordered]@{ hwnd = 100; pid = 10; process = 'WindowsTerminal' }
function Fg([int64]$H, [int]$P, [string]$Proc, [double]$T, [bool]$Typed) { [ordered]@{ hwnd = $H; pid = $P; process = $Proc; first_s = $T; input_since_start = $Typed } }
Try-Case 'prompt in front: another process took the front, no input' { (Get-PromptInFront $term @($term, (Fg 200 20 'CredentialUIBroker' 0.6 $false))).how } 'in front'
Try-Case 'prompt in front: the terminal stayed in front' { (Get-PromptInFront $term @($term)).how } 'not in front'
Try-Case 'prompt in front: an empty foreground reading is ignored' { (Get-PromptInFront $term @($term, (Fg 0 0 $null 0.3 $false))).how } 'not in front'
Try-Case 'prompt in front: the taskbar is not the prompt' { (Get-PromptInFront $term @((Fg 300 30 'explorer' 0.3 $false))).value } 'False'
Try-Case 'prompt in front: moved after the owner''s input' { (Get-PromptInFront $term @((Fg 0 0 $null 0.2 $false), (Fg 200 20 'CredentialUIBroker' 2.1 $true))).how } 'owner-assisted'
Try-Case 'prompt in front: the first move decides' { (Get-PromptInFront $term @((Fg 200 20 'CredentialUIBroker' 0.5 $false), (Fg 400 40 'notepad' 3.0 $true))).value } 'True'
Try-Case 'prompt in front: nothing known before' { $null -eq (Get-PromptInFront $null @($term)) } 'True'
Try-Case 'prompt in front: an empty reading before' { $null -eq (Get-PromptInFront (Fg 0 0 $null 0 $false) @($term)) } 'True'

# ---- Scoping to one toast, and the deny list (automation review I2, I3)
Try-Case 'only this toast: its nonce alone' { Test-OnlyThisToast @('LILITH spike', 'Click the Open button (S1.3, 4 of 6) #a1b2c3', 'Open') '#a1b2c3' 1 } 'True'
Try-Case 'only this toast: another spike toast in the subtree' { Test-OnlyThisToast @('x #a1b2c3', 'y #ffee00') '#a1b2c3' 1 } 'False'
Try-Case 'only this toast: two Open buttons' { Test-OnlyThisToast @('x #a1b2c3') '#a1b2c3' 2 } 'False'
Try-Case 'only this toast: the nonce missing' { Test-OnlyThisToast @('LILITH spike') '#a1b2c3' 0 } 'False'
foreach ($deny in @(@('Clear all', ''), @('Clear all notifications', ''), @('Clear notifications', ''), @('Clear all LILITH Spike notifications', ''), @('', 'ClearAllButton'), @('Close all', ''), @('Dismiss all', ''))) {
    Try-Case "deny list: '$($deny[0])' '$($deny[1])'" { (Test-DeniedButton $deny[0] $deny[1]) -and -not (Test-DismissButton $deny[0] $deny[1]) } 'True'
}
foreach ($ok in @(@('Dismiss', ''), @('Close', ''), @('', 'DismissButton'), @('Close notification', ''))) {
    Try-Case "own dismiss button: '$($ok[0])' '$($ok[1])'" { Test-DismissButton $ok[0] $ok[1] } 'True'
}
Try-Case 'not a dismiss button: Open' { Test-DismissButton 'Open' '' } 'False'
Try-Case 'rect inside' { Test-RectInside ([pscustomobject]@{ X = 110; Y = 210; Width = 50; Height = 20 }) ([pscustomobject]@{ X = 100; Y = 200; Width = 300; Height = 100 }) } 'True'
Try-Case 'rect outside' { Test-RectInside ([pscustomobject]@{ X = 110; Y = 310; Width = 50; Height = 20 }) ([pscustomobject]@{ X = 100; Y = 200; Width = 300; Height = 100 }) } 'False'

# ---- The owner's answers, faked: these functions ask only through Ask.
$script:AskedCount = 0
$script:FakeAnswer = $true
function Ask { $script:AskedCount++; $script:FakeAnswer }

# Resolve-Popped (automation review M6): "not seen" decides only with clean readings.
$script:Uia.proven = $true
Try-Case 'Resolve-Popped: one appearance' { (Resolve-Popped ([pscustomobject]@{ Record = [ordered]@{ appearances = @([ordered]@{ from_s = 1; to_s = 5 }); clean_polls = 30 } }) 'q').value } 'True'
Try-Case 'Resolve-Popped: none in clean readings' { (Resolve-Popped ([pscustomobject]@{ Record = [ordered]@{ appearances = @(); clean_polls = 30 } }) 'q').method } 'uia'
Try-Case 'Resolve-Popped: no clean readings asks the owner' { $script:AskedCount = 0; $r = Resolve-Popped ([pscustomobject]@{ Record = [ordered]@{ method = 'none'; clean_polls = 0 } }) 'q'; "$($script:AskedCount) $($r.method)" } '1 owner (only 0 clean UIA readings)'
Try-Case 'Resolve-Popped: untrusted (control toast unseen) asks the owner' { $script:AskedCount = 0; $r = Resolve-Popped ([pscustomobject]@{ Record = [ordered]@{ appearances = @(); clean_polls = 30 } }) 'q' -Trust:$false; "$($script:AskedCount) $($r.method)" } '1 owner (the control toast just before was not seen)'
$script:Uia.proven = $false

# Owner run 3: a toast's pop is read from its own events (Get-ToastPopped), not UIA.
function Ev2([string]$Kind, [string]$Reason) { [ordered]@{ t_ms = 0; kind = $Kind; reason = $Reason } }
Try-Case 'toast popped: one TimedOut fade' { (Get-ToastPopped ([ordered]@{ toast_events = @((Ev2 'dismissed' 'TimedOut')) })).popped } 'True'
Try-Case 'toast popped: pops counts the fades' { (Get-ToastPopped ([ordered]@{ toast_events = @((Ev2 'dismissed' 'TimedOut'), (Ev2 'dismissed' 'TimedOut')) })).pops } '2'
Try-Case 'toast popped: a suppressed toast raises none' { (Get-ToastPopped ([ordered]@{ toast_events = @() })).popped } 'False'
Try-Case 'toast popped: UserCanceled is a pop' { (Get-ToastPopped ([ordered]@{ toast_events = @((Ev2 'dismissed' 'UserCanceled')) })).popped } 'True'
Try-Case 'toast popped: ApplicationHidden is not a pop' { (Get-ToastPopped ([ordered]@{ toast_events = @((Ev2 'dismissed' 'ApplicationHidden')) })).popped } 'False'
Try-Case 'toast popped: seen_any with only a failed event' { (Get-ToastPopped ([ordered]@{ toast_events = @((Ev2 'failed' '0x80070520')) })).failed } 'True'
Try-Case 'toast popped: no events seen' { (Get-ToastPopped ([ordered]@{ toast_events = @() })).seen_any } 'False'
# S1.5's verdict (owner run 3): the control toast must pop, else INCOMPLETE; then PASS/FAIL.
$popped = Get-ToastPopped ([ordered]@{ toast_events = @((Ev2 'dismissed' 'TimedOut')) })
$notPopped = Get-ToastPopped ([ordered]@{ toast_events = @() })
Try-Case 'S1.5 verdict: control popped, pass' { Get-S15Verdict $true $popped } 'PASS'
Try-Case 'S1.5 verdict: control popped, fail' { Get-S15Verdict $false $popped } 'FAIL'
Try-Case 'S1.5 verdict: no banner event seen this run' { Get-S15Verdict $true $notPopped } 'INCOMPLETE (no toast banner event was seen this run: the pop signal could not be confirmed)'

# S1.7 (owner run 3, q5): judged from the execution-state self-check, no real sleep.
$listedOk = [ordered]@{ listed_while_held = $true; gone_after_clear = $true }
$listedBad = [ordered]@{ listed_while_held = $false; gone_after_clear = $true }
Try-Case 'S1.7 self-check and listing: PASS' { Get-S17Verdict2 $true $true $true $listedOk } 'PASS'
Try-Case 'S1.7 self-check, no listing on record' { Get-S17Verdict2 $true $true $true $null } 'PASS (execution-state self-check; the owner''s powercfg listing is not on record)'
Try-Case 'S1.7 set did not report success' { Get-S17Verdict2 $false $false $false $listedOk } 'FAIL (PowerSetRequest did not report success)'
Try-Case 'S1.7 held but self-check failed' { Get-S17Verdict2 $true $true $false $listedOk } 'INCOMPLETE (the execution state did not confirm the held request)'
Try-Case 'S1.7 listing says not listed but self-check holds' { (Get-S17Verdict2 $true $true $true $listedBad) } 'PASS (execution-state self-check; the owner''s powercfg listing is not on record)'

# ---- The stay-awake wait (automation review I5, M11)
Try-Case 'input during the wait: idle shorter than the stretch' { Test-InputDuring 600 120 } 'True'
Try-Case 'no input during the wait' { Test-InputDuring 600 598 } 'False'
Try-Case 'idle unknown is not input' { Test-InputDuring 600 $null } 'False'
Try-Case 'S1.7 a Sandbox running: not decisive' { (Get-S17Verdict $listed ([ordered]@{ api = 'request'; result = 'not run'; why = 'a Windows Sandbox is running' }) $null $true).verdict } 'INCOMPLETE (the stay-awake wait was not decisive: a Windows Sandbox is running)'
Try-Case 'S1.7 another requester before the hold' { (Get-S17Verdict $listed ([ordered]@{ api = 'request'; result = 'not decisive'; why = 'another requester' }) $null $true).verdict } 'INCOMPLETE (the stay-awake wait was not decisive: another requester)'
Try-Case 'Test-NeedSte: a not-decisive wait needs no second API' { Test-NeedSte $listed ([ordered]@{ result = 'not decisive' }) } 'False'

# ---- Fix round 2: a foreign window as the "prompt" (m3)
Try-Case 'prompt in front: another app''s window is not the prompt' { $r = Get-PromptInFront $term @((Fg 400 40 'notepad' 0.5 $false)); "$($null -eq $r.value) $($r.how)" } 'True another window (notepad)'

# ---- Fix round 2: scope names and the target selection, with fake levels (N-I3, m11)
$own = @('LILITH spike', 'LILITH Spike', 'Open')
Try-Case 'scope names: this toast''s own' { (Test-ScopeNames @('LILITH spike', 'Click the Open button (S1.3, 4 of 6) #a1b2c3', 'Open', '') '#a1b2c3' $own @()).ok } 'True'
Try-Case 'scope names: another app''s entry' { $r = Test-ScopeNames @('LILITH spike', 'x #a1b2c3', 'Mail', 'New message from someone') '#a1b2c3' $own @(); "$($r.ok) $($r.unknown)" } 'False 2'
Try-Case 'scope names: a name with two nonces' { (Test-ScopeNames @('a #a1b2c3 b #ffee00') '#a1b2c3' $own @()).ok } 'False'
Try-Case 'scope names: a pattern' { (Test-ScopeNames @('42 %', 'p #a1b2c3') '#a1b2c3' @() @('^\d+ ?%$')).ok } 'True'
function Btn([string]$N, [string]$A = '', $R = $null) { [ordered]@{ name = $N; automation_id = $A; rect = $R } }
function Lv([int]$D, [bool]$Item, [string[]]$Names, $Cands, $R = $null) { [ordered]@{ depth = $D; list_item = $Item; names = $Names; rect = $R; candidates = @($Cands) } }
$isOpen = { param($n, $a) $n -eq 'Open' }
$isDismiss = { param($n, $a) Test-DismissButton $n $a }
$text = 'Click the Open button (S1.3, 4 of 6) #a1b2c3'
Try-Case 'target: this toast''s own Open one level up' {
    $r = Select-ToastTarget @((Lv 0 $false @($text) @()), (Lv 1 $false @('LILITH spike', $text, 'Open') @((Btn 'Open')))) '#a1b2c3' $own @() $isOpen; "$($r.ok) $($r.depth)"
} 'True 1'
Try-Case 'target: a foreign entry in the scope refuses' {
    $r = Select-ToastTarget @((Lv 0 $false @($text) @()), (Lv 1 $false @($text, 'Open', 'Chat', 'Hi there') @((Btn 'Open')))) '#a1b2c3' $own @() $isOpen; "$($r.ok) $($r.why)"
} "False the scope holds 2 name(s) that are not this toast's own: nothing pressed"
Try-Case 'target: never past this toast''s list item' {
    $r = Select-ToastTarget @((Lv 0 $false @($text) @()), (Lv 1 $true @($text) @()), (Lv 2 $false @($text, 'Dismiss') @((Btn 'Dismiss')))) '#a1b2c3' $own @('(?i)^(dismiss|close)\b') $isDismiss; "$($r.ok) $($r.depth)"
} 'False 1'
Try-Case 'target: Clear all is never picked' {
    $r = Select-ToastTarget @((Lv 0 $true @($text, 'Clear all') @((Btn 'Clear all' 'ClearAllButton')))) '#a1b2c3' @($own + 'Clear all') @('(?i)^(dismiss|close)\b') { param($n, $a) $true }; "$($r.ok)"
} 'False'
Try-Case 'target: the own dismiss inside the list item' {
    $r = Select-ToastTarget @((Lv 0 $false @($text) @()), (Lv 1 $true @('LILITH Spike', $text, 'Dismiss') @((Btn 'Dismiss')))) '#a1b2c3' $own @('(?i)^(dismiss|close)\b') $isDismiss; "$($r.ok) $($r.candidate['name'])"
} 'True Dismiss'
Try-Case 'target: a button outside the toast refuses' {
    $r = Select-ToastTarget @((Lv 0 $false @($text, 'Open') @((Btn 'Open' '' ([pscustomobject]@{ X = 0; Y = 900; Width = 50; Height = 20 }))) ([pscustomobject]@{ X = 100; Y = 100; Width = 300; Height = 100 }))) '#a1b2c3' $own @() $isOpen; "$($r.ok)"
} 'False'
Try-Case 'target: another spike toast in the scope refuses' {
    $r = Select-ToastTarget @((Lv 0 $false @($text, 'other #ffee00', 'Open') @((Btn 'Open')))) '#a1b2c3' $own @() $isOpen; "$($r.ok)"
} 'False'

# ---- Fix round 2: the stay-awake wait's evidence (N-I2)
function Ev($Req) { [ordered]@{ execution_state = $(if ($Req) { '0x1' } else { '0x0' }); system_required = $Req } }
Try-Case 'stay decisive: clean before, held, after' { $null -eq (Get-StayNotDecisive (Ev $false) @((Ev $true), (Ev $true)) @((Ev $false), (Ev $false))) } 'True'
Try-Case 'stay not decisive: another requester before' { Get-StayNotDecisive (Ev $true) @((Ev $true)) @((Ev $false)) } 'another requester held the PC awake before the request (execution state 0x1)'
Try-Case 'stay not decisive: self-check false' { Get-StayNotDecisive (Ev $false) @((Ev $true), (Ev $false)) @((Ev $false)) } 'the execution state did not show the spike''s own request while it was held (self-check false)'
Try-Case 'stay not decisive: still held after the release' { Get-StayNotDecisive (Ev $false) @((Ev $true)) @((Ev $false), (Ev $true)) } 'another requester still held the PC awake after the release (execution state 0x1)'

# ---- Fix round 2: the Sandbox runs' sleep timeline and verdict (N-I1)
$evs = @([ordered]@{ t_ms = 1000; kind = 'set' }, [ordered]@{ t_ms = 5000; kind = 'power_broadcast'; pbt = 'PBT_APMSUSPEND' }, [ordered]@{ t_ms = 9000; kind = 'clock_jump' })
Try-Case 'sleep in window: a suspend inside' { (Get-SleepInWindow $evs @() 4000 6000).slept } 'True'
Try-Case 'sleep in window: none inside' { (Get-SleepInWindow $evs @() 6000 8000).slept } 'False'
Try-Case 'sleep in window: a Kernel-Power resume' { (Get-SleepInWindow @() @([ordered]@{ id = 107; t_ms = 7000 }) 6000 8000).slept } 'True'
Try-Case 'sleep in window: a power-status broadcast is not a sleep' { (Get-SleepInWindow @([ordered]@{ t_ms = 7000; kind = 'power_broadcast'; pbt = 'PBT_APMPOWERSTATUSCHANGE' }) @() 6000 8000).slept } 'False'
function Run1($Pass, $Int) { [ordered]@{ pass = $Pass; interrupted_by_sleep = $Int } }
Try-Case 'sandbox verdict: 3 of 3' { Get-SandboxVerdict @((Run1 $true $false), (Run1 $true $false), (Run1 $true $false)) } 'PASS'
Try-Case 'sandbox verdict: one interrupted' { Get-SandboxVerdict @((Run1 $true $false), (Run1 $null $true), (Run1 $true $false)) } 'INCOMPLETE (1 run(s) interrupted by sleep: rerun them)'
Try-Case 'sandbox verdict: a judged FAIL decides' { Get-SandboxVerdict @((Run1 $false $false), (Run1 $null $true)) } 'FAIL'
# ---- Fix round 3: a pinned process (N2-C1). A process this test starts itself (ping, no window)
# is pinned at once; once it has exited, HasExited stays true on the pinned object and
# Stop-PinnedProcess does nothing, whatever now has its pid. The identity checks refuse a
# process that started before the given time or runs another exe.
function Start-Ping([int]$Count) {
    $si = New-Object System.Diagnostics.ProcessStartInfo
    $si.FileName = Join-Path $env:windir 'System32\PING.EXE'
    $si.Arguments = "-n $Count 127.0.0.1"
    $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.RedirectStandardOutput = $true
    [System.Diagnostics.Process]::Start($si)
}
$pingExe = Join-Path $env:windir 'System32\PING.EXE'
$t = Get-Date
# The setup runs inside cases too, so that a script without these functions fails them instead
# of ending the file (fix round 4, m6).
$pinned = $null; $pingProc = $null; $live = $null
Try-Case 'pinned: the process just started is pinned' { $script:pingProc = Start-Ping 1; $script:pinned = Get-PinnedProcess $pingProc.Id $pingExe $t; $null -ne $pinned } 'True'
Try-Case 'pinned: HasExited after it ended' { [void]$pingProc.WaitForExit(10000); if ($null -ne $pinned) { [void]$pinned.WaitForExit(10000); $pinned.HasExited } else { 'not pinned' } } 'True'
Try-Case 'pinned: Stop-PinnedProcess on an exited process does nothing' { try { Stop-PinnedProcess $pinned; 'no-op' } catch { "threw $($_.Exception.Message)" } } 'no-op'
# A ping of about 30 s: only a real stop ends it within a second (fix round 4, m6).
Try-Case 'pinned: refuses a process older than the start' { $script:live = Start-Ping 30; $null -eq (Get-PinnedProcess $live.Id $pingExe (Get-Date).AddMinutes(5)) } 'True'
Try-Case 'pinned: refuses another exe' { $null -eq (Get-PinnedProcess $live.Id 'C:\nowhere\other.exe' $t) } 'True'
Try-Case 'pinned: refuses a pid that is gone' { $null -eq (Get-PinnedProcess 2147483000 $pingExe $t) } 'True'
# Fix round 4, m1: the pid and the exact creation time pin one process, with no path and no window.
Try-Case 'pinned: by its creation time' { $null -ne (Get-PinnedProcess $live.Id '' ([datetime]::MinValue) $live.StartTime.ToFileTimeUtc()) } 'True'
Try-Case 'pinned: refuses another creation time' { $null -eq (Get-PinnedProcess $live.Id '' ([datetime]::MinValue) ($live.StartTime.ToFileTimeUtc() + 1)) } 'True'
Try-Case 'pinned: Stop-PinnedProcess ends the live one it pinned, within 1 s' {
    $p = Get-PinnedProcess $live.Id '' ([datetime]::MinValue) $live.StartTime.ToFileTimeUtc()
    Stop-PinnedProcess $p
    $live.WaitForExit(1000)
} 'True'
# winshell-tool spawn prints the creation time that pins its child (built by cargo test).
$toolBuilt = @('target\debug\winshell-tool.exe', 'target\release\winshell-tool.exe' | ForEach-Object { Join-Path (Split-Path $PSScriptRoot) $_ } | Where-Object { Test-Path -LiteralPath $_ } |
    Sort-Object { (Get-Item -LiteralPath $_).LastWriteTimeUtc } -Descending) | Select-Object -First 1   # the newest build
if ($toolBuilt) {
    Try-Case 'pinned: a winshell-tool spawn pins by its printed creation time' {
        $r = (& $toolBuilt spawn '--' $pingExe -n 2 127.0.0.1 | Out-String) | ConvertFrom-Json
        $p = Get-PinnedProcess ([int]$r.pid) '' ([datetime]::MinValue) (Get-SpawnCreated $r)
        $ok = $null -ne $p -and $p.ProcessName -eq 'PING'
        if ($null -ne $p) { [void]$p.WaitForExit(5000) }
        $ok
    } 'True'
} else { Write-Host 'skip pinned: no built winshell-tool.exe' }

# ---- Fix round 3: S1.3 refusals go to the owner (N2-I2)
Try-Case 'fallback: the button refused' { Get-ClickFallback ([ordered]@{ button_missing = $true; button_why = 'no such button inside this toast''s own list item'; clicks = @() }) } 'the script refused to act: no such button inside this toast''s own list item'
Try-Case 'fallback: every way refused' { Get-ClickFallback ([ordered]@{ clicks = @([ordered]@{ ok = $false; why = 'UIA puts no candidate point on the target' }, [ordered]@{ ok = $false; why = 'the per-monitor DPI context did not apply: nothing clicked' }) }) } 'the script refused to act: UIA puts no candidate point on the target'
Try-Case 'fallback: the owner was active' { Get-ClickFallback ([ordered]@{ clicks = @([ordered]@{ ok = $false; owner_active = $true; why = 'owner active' }) }) } 'the owner was using the PC during the synthesized click'
Try-Case 'fallback: a click was made (only the launch is missing): repost' { $null -eq (Get-ClickFallback ([ordered]@{ clicks = @([ordered]@{ ok = $true }) })) } 'True'
# The list Click-Trial and S1.5's dismissal use, not a copy (fix round 4, m6).
$chrome = $script:ToastChrome
Try-Case 'the toast chrome list is defined' { @($chrome).Count -ge 4 } 'True'
Try-Case 'S1.5 dismissal: the own dismiss with the chrome in the item' {
    $r = Select-ToastTarget @((Lv 0 $true @('Spike job running', 'LILITH Spike', 'Progress toast (S1.5) #a1b2c3', '42 %', 'Dismiss', '...', 'Notification settings', 'Notification from LILITH Spike') @((Btn 'Dismiss'), (Btn '...')))) '#a1b2c3' @('Spike job running', 'LILITH Spike') (@('^\d+ ?%$') + $chrome) $isDismiss
    "$($r.ok) $($r.candidate['name'])"
} 'True Dismiss'
Try-Case 'S1.3 chrome is known, so it does not refuse' {
    $r = Select-ToastTarget @((Lv 0 $true @('LILITH spike', $text, 'Open', 'Dismiss', '...', 'Notification settings', 'Notification from LILITH Spike') @((Btn 'Open'), (Btn 'Dismiss'), (Btn '...')))) '#a1b2c3' $own @() $isOpen
    "$($r.ok)"
} 'False'
Try-Case 'S1.3 chrome with the chrome patterns: Open is pressed, chrome never' {
    $r = Select-ToastTarget @((Lv 0 $true @('LILITH spike', $text, 'Open', 'Dismiss', '...', 'Notification settings', 'Notification from LILITH Spike') @((Btn 'Dismiss'), (Btn '...'), (Btn 'Open')))) '#a1b2c3' $own $chrome $isOpen
    "$($r.ok) $($r.candidate['name'])"
} 'True Open'
Try-Case 'unknown controls: AutomationId and control type only, never the Name' {
    $lv = Lv 0 $true @($text, 'Open', 'Hi from a friend') @((Btn 'Open'))
    $lv.elements = @([ordered]@{ name = $text; automation_id = 'Text1'; control = 'ControlType.Text'; class = 'TextBlock' }, [ordered]@{ name = 'Open'; automation_id = 'B'; control = 'ControlType.Button'; class = 'Button' }, [ordered]@{ name = 'Hi from a friend'; automation_id = 'Body2'; control = 'ControlType.Text'; class = 'TextBlock' })
    $r = Select-ToastTarget @($lv) '#a1b2c3' $own @() $isOpen
    $u = @($r.unknown_controls)
    "$($u.Count) $($u[0]['automation_id']) $($u[0]['control_type']) $(($r | ConvertTo-Json -Depth 5).Contains('Hi from a friend'))"
} '1 Body2 ControlType.Text False'

# ---- Fix round 3: S1.7 judges the hold's span only (N2-I1)
Try-Case 'hold span: from set to cleared' { $sp = Get-HoldSpan @([ordered]@{ kind = 'window'; t_ms = 1 }, [ordered]@{ kind = 'set'; t_ms = 1000 }, [ordered]@{ kind = 'cleared'; t_ms = 9000 }) 500; "$($sp.set_ms)-$($sp.cleared_ms)" } '1000-9000'
Try-Case 'hold span: no set event falls back to the start' { (Get-HoldSpan @([ordered]@{ kind = 'cleared'; t_ms = 9000 }) 500).set_ms } '500'
# ---- The CLSID read-back (rerun of 2026-10-23: S1.2p, S1.3 and S1.4 stopped at the compare).
# The root cause, reproduced: a [switch] parameter named like the script's $ToastClsid string
# hides that string inside the function, and the CLSID is compared with "True".
Try-Case 'the old shape compares with the switch' {
    & { $ToastClsid = '{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}'
        function Old([switch]$ToastClsid) { "$ToastClsid" }
        Old -ToastClsid }
} 'True'
$readOk = '{"exists":true,"target":"C:\\x\\winshell-app.exe","aumid":"LILITH.Spike.Notify","toast_clsid":"{6F1A0C52-3B7E-4E0D-9A11-5C2B8D7E4F31}"}' | ConvertFrom-Json
$readPlain = '{"exists":true,"target":"C:\\x\\winshell-app.exe","aumid":"LILITH.Spike.Notify","toast_clsid":"{00000000-0000-0000-0000-000000000000}"}' | ConvertFrom-Json
Try-Case 'read-back: the CLSID form as the tool prints it' { $null -eq (Test-ShortcutReadback $readOk $Aumid $ToastClsid) } 'True'
Try-Case 'read-back: plain form, no CLSID wanted' { $null -eq (Test-ShortcutReadback $readPlain $Aumid $null) } 'True'
Try-Case 'read-back: a missing CLSID is caught' { "$(Test-ShortcutReadback $readPlain $Aumid $ToastClsid)".StartsWith('shortcut ToastActivatorCLSID reads back as') } 'True'
# Write-Shortcut itself, with the tool, the registry writes and the sleep faked (nothing on this
# PC is touched): it must accept the tool's real read-back.
$script:FakeRead = $readOk
function Tool { if ($args[0] -eq 'shortcut-write') { [pscustomobject]@{ written = $true; error = $null } } else { $script:FakeRead } }
function New-Item { }
function Start-Sleep { }
$ClsidKey = 'HKCU:\Software\LILITH-spike-strict-test-' + [guid]::NewGuid().ToString('N')
$ClsidParent = "$ClsidKey-parent"
Try-Case 'Write-Shortcut -WithToastClsid accepts the read-back' { Write-Shortcut -WithToastClsid; $script:ShortcutForm } 'toast_clsid'
$script:FakeRead = $readPlain
Try-Case 'Write-Shortcut -WithToastClsid refuses a read-back without the CLSID' {
    try { Write-Shortcut -WithToastClsid; 'no throw' } catch { "$($_.Exception.Message)".StartsWith('shortcut ToastActivatorCLSID reads back as') }
} 'True'
Try-Case 'Write-Shortcut plain' { Write-Shortcut; $script:ShortcutForm } 'plain'

# ---- Scope cut: S1.9's Qt item (Invoke-QtItem). A missing platform plugin is recorded and the
# app never started (no modal box); the offscreen grab is never tested; a dialog is never a grab.
$qtTmp = Join-Path ([IO.Path]::GetTempPath()) ('winshell-qt-test-' + [guid]::NewGuid().ToString('N'))
try {
    [void][IO.Directory]::CreateDirectory((Join-Path $qtTmp 'platforms'))   # New-Item is stubbed above
    [IO.File]::WriteAllText((Join-Path $qtTmp 'app.exe'), '')
    $script:QtLaunched = 0
    $grab = { param($exe) $script:QtLaunched++; [ordered]@{ hwnd = 1; class = 'Qt6QWindowIcon'; nonflat = $true } }
    Try-Case 'Qt: no qwindows.dll, not launched' { $r = Invoke-QtItem $qtTmp 'app.exe' $grab; "$($r.pass) $($r.onscreen.launched) $script:QtLaunched" } 'False False 0'
    [IO.File]::WriteAllText((Join-Path $qtTmp 'platforms\qwindows.dll'), '')
    Try-Case 'Qt: qwindows.dll present, on-screen grab judged' { $r = Invoke-QtItem $qtTmp 'app.exe' $grab; "$($r.pass) $($r.onscreen.launched) $script:QtLaunched" } 'True True 1'
    Try-Case 'Qt: offscreen not tested without qoffscreen.dll' { (Invoke-QtItem $qtTmp 'app.exe' $grab).offscreen.reason } 'offscreen not tested: the trimmed deploy ships no qoffscreen.dll; CI''s S0.9 covers offscreen'
    Try-Case 'Qt: a dialog window is never a grab' { $r = Invoke-QtItem $qtTmp 'app.exe' { param($exe) [ordered]@{ hwnd = 2; class = '#32770'; nonflat = $true } }; "$($r.pass) $($r.onscreen.error_dialog)" } 'False True'
    [IO.File]::WriteAllText((Join-Path $qtTmp 'platforms\qoffscreen.dll'), '')
    Try-Case 'Qt: offscreen still not tested with qoffscreen.dll present' { $r = Invoke-QtItem $qtTmp 'app.exe' $grab; "$($r.offscreen.tested) $($r.qt_qpa_platform)" } 'False (unset: the default windows plugin)'
    Try-Case 'Qt: the app missing is recorded, not launched' { $script:QtLaunched = 0; $r = Invoke-QtItem $qtTmp 'nope.exe' $grab; "$($r.pass) $($r.onscreen.launched) $script:QtLaunched" } 'False False 0'
} finally { Remove-Item -LiteralPath $qtTmp -Recurse -Force -ErrorAction SilentlyContinue }

# ---- Fix round 4, m9: an unpinned core's result counts only once it is complete
$coreTmp = Join-Path ([IO.Path]::GetTempPath()) ('winshell-core-test-' + [guid]::NewGuid().ToString('N') + '.json')
try {
    Set-Content -LiteralPath $coreTmp -Value '{"command":"power","events":[{"kind":"set","t_ms":1}]}'
    Try-Case 'core result: a power file without its exit event is not complete' { $null -eq (Read-CoreFinal $coreTmp) } 'True'
    Set-Content -LiteralPath $coreTmp -Value '{"command":"power","events":[{"kind":"set","t_ms":1},{"kind":"exit","t_ms":2}]}'
    Try-Case 'core result: with its exit event it is' { (Read-CoreFinal $coreTmp).command } 'power'
    Set-Content -LiteralPath $coreTmp -Value '{"command":"toast","posted":true}'
    Try-Case 'core result: a file without events is complete' { (Read-CoreFinal $coreTmp).posted } 'True'
    Set-Content -LiteralPath $coreTmp -Value '{"command":"pow'
    Try-Case 'core result: a file caught mid-write is not' { $null -eq (Read-CoreFinal $coreTmp) } 'True'
} finally { Remove-Item -LiteralPath $coreTmp -ErrorAction SilentlyContinue }

# ---- Fix round 4: the pop-up, with its spawn faked by a windowless ping this test starts and
# never ends (it lives about 4 s). Nothing is shown.
$savedTool = $ToolExe
$ToolExe = $pingExe   # any existing file: Start-HandsOff only checks it is there
$script:Sleeps = [System.Collections.Generic.List[double]]::new()
$script:FakeMs = 0.0
$script:PopupState = $null
$script:PopupStates = [System.Collections.Generic.List[string]]::new()
function Start-Sleep { param([double]$Seconds = 0, [int]$Milliseconds = 0) $ms = $Seconds * 1000 + $Milliseconds; $script:Sleeps.Add($ms); $script:FakeMs += $ms }
function Get-Date { [datetime]::Now.AddMilliseconds($script:FakeMs) }
function Invoke-PopupSpawn([string]$Tool, [string[]]$SpawnArgs) {
    $script:PopupState = $SpawnArgs[[array]::IndexOf($SpawnArgs, '-StateFile') + 1]
    $script:PopupStates.Add($script:PopupState)
    $p = Start-Ping 5
    [pscustomobject]@{ pid = $p.Id; created = $p.StartTime.ToFileTimeUtc() }
}
try {
    $script:HandsOffLog = @()
    $h = $null
    # m2: a pop-up that never reports itself shown. The wait gives up after 8 s, and the warning
    # still lasts 10 s from then.
    Try-Case 'pop-up give-up: pinned, not shown' { $script:h = Start-HandsOff 'test' 5; "$($h.Record.popup_started) $($h.Record.shown)" } 'True False'
    Try-Case 'pop-up give-up: waited 8 s for it' { [math]::Round((@($script:Sleeps) | Select-Object -SkipLast 1 | Measure-Object -Sum).Sum / 1000) } '8'
    Try-Case 'pop-up give-up: then 10 s of warning' { $script:Sleeps[$script:Sleeps.Count - 1] -ge 9900 } 'True'
    Try-Case 'pop-up give-up: its start is 10 s from the give-up' { [long](Get-Content -Raw -LiteralPath "$($script:PopupState).start").Trim() -ge [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + 9000 } 'True'
    # A restarted stay-awake pop-up: not checked, so "shown" is $null.
    Try-Case 'pop-up without the warning wait: shown not checked' { $h2 = Start-HandsOff 'test again' 5 -NoWarnWait; "$($h2.Record.popup_started) $($null -eq $h2.Record.shown)" } 'True True'
    # N3-I1 and m1: the end tells the running pop-ups to close (their state file) and keeps the log.
    Try-Case 'Stop-AllPopups keeps the pop-up log' { $n = @($script:HandsOffLog).Count; Stop-AllPopups; "$n $(@($script:HandsOffLog).Count)" } '2 2'
    Try-Case 'Stop-AllPopups writes the state file and ends nothing' { "$(Test-Path -LiteralPath $h.StateFile) $($h.Process.HasExited)" } 'True False'
    Try-Case 'Stop-AllPopups once they have gone: their state files are removed' {
        [void]$h.Process.WaitForExit(10000)
        $script:Popups = @($h)
        Stop-AllPopups
        @($h.StateFile, "$($h.StateFile).start" | Where-Object { Test-Path -LiteralPath $_ }).Count
    } '0'
} finally {
    $ToolExe = $savedTool
    Remove-Item Function:\Get-Date, Function:\Invoke-PopupSpawn -ErrorAction SilentlyContinue
    foreach ($s in $script:PopupStates) { foreach ($f in $s, "$s.shown", "$s.start") { Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue } }
}

if ($failures.Count -gt 0) { Write-Host "$($failures.Count) failed"; exit 1 }
Write-Host 'all cases passed'
exit 0
