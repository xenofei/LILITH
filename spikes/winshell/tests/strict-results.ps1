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

# S1.5's second pop (automation review C1): 0 never decides, 1 is the owner's, 2+ is FAIL.
Try-Case 'second pop: 0 popups asks the owner' { $script:AskedCount = 0; $script:FakeAnswer = $true; $r = Resolve-SecondPop 0 { Ask 'q' }; "$($script:AskedCount) $($r.value) $($r.method)" } '1 True owner (UIA did not see the progress toast)'
Try-Case 'second pop: 1 popup, the owner decides' { $script:AskedCount = 0; $script:FakeAnswer = $false; $r = Resolve-SecondPop 1 { Ask 'q' }; "$($script:AskedCount) $($r.value) $($r.method)" } '1 False owner decides (UIA saw 1 popup)'
Try-Case 'second pop: 2 popups is FAIL without asking' { $script:AskedCount = 0; $r = Resolve-SecondPop 2 { Ask 'q' }; "$($script:AskedCount) $($r.value) $($r.method)" } '0 False uia'
Try-Case 'S1.5 verdict: 2 popups' { Get-S15Verdict $false (Resolve-SecondPop 2 { $true }) } 'FAIL'
Try-Case 'S1.5 verdict: 1 popup, owner saw no second' { Get-S15Verdict $true (Resolve-SecondPop 1 { $true }) } 'PASS (UIA saw 1 popup; the owner saw no second pop)'
Try-Case 'S1.5 verdict: 1 popup, owner saw a second' { Get-S15Verdict $false (Resolve-SecondPop 1 { $false }) } 'FAIL (UIA saw 1 popup; the owner saw a second pop, for example an in-place re-animation)'
Try-Case 'S1.5 verdict: 0 popups, owner says once' { Get-S15Verdict $true (Resolve-SecondPop 0 { $true }) } 'PASS'

# ---- The stay-awake wait (automation review I5, M11)
Try-Case 'input during the wait: idle shorter than the stretch' { Test-InputDuring 600 120 } 'True'
Try-Case 'no input during the wait' { Test-InputDuring 600 598 } 'False'
Try-Case 'idle unknown is not input' { Test-InputDuring 600 $null } 'False'
Try-Case 'S1.7 a Sandbox running: not decisive' { (Get-S17Verdict $listed ([ordered]@{ api = 'request'; result = 'not run'; why = 'a Windows Sandbox is running' }) $null $true).verdict } 'INCOMPLETE (the stay-awake wait was not decisive: a Windows Sandbox is running)'
Try-Case 'S1.7 another requester before the hold' { (Get-S17Verdict $listed ([ordered]@{ api = 'request'; result = 'not decisive'; why = 'another requester' }) $null $true).verdict } 'INCOMPLETE (the stay-awake wait was not decisive: another requester)'
Try-Case 'Test-NeedSte: a not-decisive wait needs no second API' { Test-NeedSte $listed ([ordered]@{ result = 'not decisive' }) } 'False'
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

if ($failures.Count -gt 0) { Write-Host "$($failures.Count) failed"; exit 1 }
Write-Host 'all cases passed'
exit 0
