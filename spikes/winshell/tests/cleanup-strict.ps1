#requires -Version 7
<#
Part B run 1 analysis: run-spike.ps1's cleanup failed with "The property 'Count' cannot be
found on this object" (strict mode on a function result that was empty or one object). This
test dot-sources run-spike.ps1 with -LibraryOnly, so its own Set-StrictMode -Version Latest and
$ErrorActionPreference = 'Stop' apply, replaces everything that would touch this PC with fakes
(processes, Windows Hello, winshell-tool) and points every path at a fresh temp folder or at a
registry path that doesn't exist, then runs Invoke-Cleanup in each state.

Run by tests/cleanup_strict.rs (cargo test). Exit 0 = every case passed.
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\run-spike.ps1') -LibraryOnly

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('winshell-cleanup-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
$Lnk = Join-Path $tmp 'LILITH Spike.lnk'
$LogDir = Join-Path $tmp 'LILITH-spike\winshell'
$Events = Join-Path $LogDir 'events.jsonl'
$Raw = Join-Path $tmp 'raw'
$nowhere = 'HKCU:\Software\LILITH-spike-cleanup-test-' + [guid]::NewGuid().ToString('N')
$SchemeKey = "$nowhere\lilspike"
$AumidKey = "$nowhere\AppUserModelId\LILITH.Spike.Notify"
$AumidParent = "$nowhere\AppUserModelId"
$NotifySettingsKey = "$nowhere\notify"
$AssocToastsKey = "$nowhere\assoc"
$ClsidKey = "$nowhere\clsid"
$ToolExe = Join-Path $tmp 'winshell-tool-not-built.exe'   # so 'clear history' is skipped

# Fakes. Get-SpikeCores emits its objects one by one, as the real one does once unrolled:
# nothing, one object, or several.
$script:FakeCores = @()
$script:FakeHello = @{ status = 'NotFound' }
function Stop-SpikeProcesses { }
function Get-SpikeCores { foreach ($c in $script:FakeCores) { $c } }
function HelloCmd { [pscustomobject]$script:FakeHello }

$failures = [System.Collections.Generic.List[string]]::new()
function Expect([string]$Case, [bool]$Ok, [string]$What) {
    if ($Ok) { Write-Host "ok   $Case : $What" } else { Write-Host "FAIL $Case : $What"; $failures.Add("$Case : $What") }
}

function Run-Case([string]$Case, [scriptblock]$Arrange, [scriptblock]$Assert) {
    $script:FakeCores = @()
    $script:FakeHello = @{ status = 'NotFound' }
    & $Arrange
    try {
        $c = Invoke-Cleanup -Final
        & $Assert $c
    } catch {
        Expect $Case $false "threw: $($_.Exception.Message) (line $($_.InvocationInfo.ScriptLineNumber))"
    }
}

try {
    Run-Case 'everything gone' { } {
        param($c)
        Expect 'everything gone' ($c.pass -eq $true) 'pass is true'
        Expect 'everything gone' ($c.errors.Count -eq 0) "no step errors ($(($c.errors.Keys) -join ', '))"
        Expect 'everything gone' ($c.state.core_running -eq $false) 'no core running'
        Expect 'everything gone' ($c.state.credential -eq 'gone') 'credential gone'
    }
    Run-Case 'leftovers removed' {
        Set-Content -LiteralPath $Lnk -Value 'x'
        New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
        Set-Content -LiteralPath $Events -Value '{"kind":"start"}'
    } {
        param($c)
        Expect 'leftovers removed' ($c.pass -eq $true) 'pass is true'
        Expect 'leftovers removed' (-not (Test-Path -LiteralPath $Lnk)) 'shortcut removed'
        Expect 'leftovers removed' (-not (Test-Path -LiteralPath $LogDir)) 'log folder removed'
        Expect 'leftovers removed' (Test-Path -LiteralPath (Join-Path $Raw 'events.jsonl')) 'event log kept in raw\'
    }
    Run-Case 'one core still running' { $script:FakeCores = @([pscustomobject]@{ Id = 1 }) } {
        param($c)
        Expect 'one core still running' ($c.pass -eq $false) 'pass is false'
        Expect 'one core still running' ($c.state.core_running -eq $true) 'core_running is true'
        Expect 'one core still running' ($c.errors.Count -eq 0) "no step errors ($(($c.errors.Keys) -join ', '))"
    }
    Run-Case 'two cores still running' { $script:FakeCores = @([pscustomobject]@{ Id = 1 }, [pscustomobject]@{ Id = 2 }) } {
        param($c)
        Expect 'two cores still running' ($c.pass -eq $false) 'pass is false'
        Expect 'two cores still running' ($c.errors.Count -eq 0) 'no step errors'
    }
    Run-Case 'Hello answers with an error' { $script:FakeHello = @{ status = 'error'; error = 'no output' } } {
        param($c)
        Expect 'Hello answers with an error' ($c.pass -eq $false) 'pass is false'
        Expect 'Hello answers with an error' ("$($c.state.credential)".StartsWith('unknown')) 'credential unknown'
    }
    Run-Case 'Hello not supported' { $script:FakeHello = @{ status = 'UnknownError'; supported = $false } } {
        param($c)
        Expect 'Hello not supported' ($c.pass -eq $true) 'pass is true'
        Expect 'Hello not supported' ($c.state.credential -eq 'gone (Hello not supported)') 'credential gone (Hello not supported)'
    }
    # Get-Events skips a line caught mid-write instead of throwing.
    New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
    Set-Content -LiteralPath $Events -Value @('{"kind":"start","pid":1}', '{"kind":"front_ch')
    $ev = @(Get-Events)
    Expect 'partial event line' ($ev.Count -eq 1 -and $ev[0].kind -eq 'start') 'one whole event read, the partial line skipped'
    Remove-Item -LiteralPath $LogDir -Recurse -Force
}
finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { Write-Host "$($failures.Count) failed"; exit 1 }
Write-Host 'all cases passed'
exit 0
