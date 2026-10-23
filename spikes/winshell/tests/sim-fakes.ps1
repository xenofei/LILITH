#requires -Version 7
<#
Fix round 4 (coordinator's ruling): the fakes behind -Simulate, used only by tests/simulate.ps1.

run-all.ps1, run-spike.ps1 and run-sandbox.ps1 dot-source this file when they get -Simulate,
after their own functions are defined and before their main part runs. Their real flow then runs
end to end under their own Set-StrictMode -Version Latest and $ErrorActionPreference = 'Stop',
with every call that would touch this PC replaced:
- winshell-tool, winshell-hello, cargo, powercfg and the Kernel-Power log are fakes: no toast,
  shortcut or Hello credential is written, and no power request is held;
- the registry the scripts write is an in-memory one under SIMREG:\ (a write to the real
  registry throws);
- SpikeUi.Win is a fake class with the real one's members and no P/Invoke, and the UI Automation
  searches are fakes: no input is injected, nothing is clicked, no key is pressed;
- the Sandbox is a state machine that writes the files inside.ps1 would write (inside.ps1 itself
  can't run here: its guard comes first);
- the pop-up's spawn is a fake: no pop-up is shown.
Time is a fake clock: Start-Sleep advances it (and really sleeps 2 % of it, at most 0.3 s, so that
a process ended through its pinned object has exited when the script looks again), and Get-Date
reads it.

Two kinds of real process are started, both by these fakes, both ping.exe without a window:
- the stand-in for WindowsSandbox.exe, a real Start-Process -PassThru object, so that
  Start-Sandbox's record holds what it holds on the owner's PC (review N3-C2). It lives about 7 s,
  or until the Sandbox stop path ends it through its pinned object;
- each pop-up's stand-in (about 1 s), so that pop-ups are pinned and filtered as the real ones.
Every path goes to $env:WINSHELL_SIM_DIR, a folder tests/simulate.ps1 made (it holds sim.marker).
#>
param([Parameter(Mandatory = $true)][ValidateSet('run-all', 'run-spike', 'run-sandbox')][string]$SimFor)

$SimDir = $env:WINSHELL_SIM_DIR
if (-not $SimDir -or -not [IO.File]::Exists((Join-Path $SimDir 'sim.marker'))) {
    throw '-Simulate runs only in a folder made by tests/simulate.ps1 (WINSHELL_SIM_DIR with sim.marker). Nothing was done.'
}
$SimBin = Join-Path $SimDir 'bin'
$SimResults = Join-Path $SimDir 'results'
$SimLocal = Join-Path $SimDir 'localappdata'
$SimPing = Join-Path $env:windir 'System32\PING.EXE'

# ---------------------------------------------------------------- SpikeUi.Win, without P/Invoke

if ('SpikeUi.Win' -as [type]) {
    if ($null -eq [SpikeUi.Win].GetField('IsSimulation')) { throw 'simulation: the real SpikeUi.Win is loaded in this session. Nothing was done.' }
} else {
    Add-Type -TypeDefinition @'
namespace SpikeUi {
    public static class Win {
        public const bool IsSimulation = true;
        public static int LandedX, LandedY;
        public static bool PerMonitorAware() { return true; }
        public static bool Shown(long hwnd) { return false; }
        public static int[] VirtualDesk() { return new int[] { 0, 0, 1920, 1080 }; }
        public static uint LastInputTick() { return 1000; }
        public static uint Now() { return 900000; }
        public static long IdleMs() { return 900000; }
        public static long OwnerIdleMs() { return long.MaxValue; }
        public static long[] Foreground() { return new long[] { 0, 0 }; }
        public static string ClassOf(long hwnd) { return "SimClass"; }
        public static long PidOf(long hwnd) { return 0; }
        public static long[] RootAt(int x, int y) { return new long[] { 0, 0 }; }
        public static int ClickAt(int x, int y, long wantPid, string wantClass) { return 2; }
        public static int[] MoveTo(int x, int y) { return new int[] { 0, 0 }; }
        public static void MoveBack(int x, int y) { }
        public static int Keys(ushort[] vks) { return vks.Length * 2; }
        public static bool Place(long hwnd, int x, int y, int w, int h) { return true; }
        public static int SessionFlags() { return 1; }
        public static long ExecutionState() { return 0; }
        public static long[] PowerInfo() { return new long[] { 0, 0, 0, 0 }; }
        public static uint ResetIdleOnce() { return 0x80000000; }
    }
}
'@
}
# run-spike and run-sandbox dot-source uia.ps1 (so $script:Uia exists); run-all does not.
if (Get-Variable -Name Uia -Scope Script -ErrorAction SilentlyContinue) { $script:Uia.ready = $true }

# ---------------------------------------------------------------- the clock, the owner, programs

$script:SimClockMs = 0.0
if ($SimFor -ne 'run-all') { function Get-Date { [datetime]::Now.AddMilliseconds($script:SimClockMs) } }
function Start-Sleep {
    param([Parameter(Position = 0)][double]$Seconds = 0, [int]$Milliseconds = 0)
    $ms = $Seconds * 1000 + $Milliseconds
    $script:SimClockMs += $ms
    $real = [int][math]::Min(300, $ms / 50)
    if ($real -gt 0) { [Threading.Thread]::Sleep($real) }
}
# The default run must never prompt (scope cut): a prompt in the simulation is a failure.
function Read-Host {
    param([Parameter(Position = 0)]$Prompt)
    throw "simulation: a prompt was reached: $Prompt"
}
function Beep([int]$Frequency, [int]$Ms) { }
function cargo { $global:LASTEXITCODE = 0 }
function rustc { 'rustc 0.0.0 (simulation)' }
function git { $global:LASTEXITCODE = 0 }
function icacls { $global:LASTEXITCODE = 0 }
function Get-CimInstance { }
function Get-WinEvent { throw 'No events were found that match the specified selection criteria.' }
function powercfg {
    $a = @($args | ForEach-Object { "$_" })
    if ($a -contains '/query') {
        return @('Power Setting GUID: 29f6c1db-86da-48c5-9fdb-f2b67b1f44da  (Sleep after)', '    Current AC Power Setting Index: 0x0000012c', '    Current DC Power Setting Index: 0x00000384')
    }
    if ($a -contains '/a') {
        return @('The following sleep states are available on this system:', '    Standby (S3)', '', 'The following sleep states are not available on this system:', '    Standby (S0 Low Power Idle)')
    }
    throw "simulation: unexpected powercfg $($a -join ' ')"
}
function Get-Command {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Name)
    if (@($Name) -contains 'wsb') { return $null }   # this PC's case: the inbox Sandbox, no CLI
    Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
}

$script:SimPid = 900001
function New-SimPid { $script:SimPid += 2; $script:SimPid }   # odd: never a real Windows pid

function Write-SimJson([string]$Path, $Obj) {
    [void][IO.Directory]::CreateDirectory((Split-Path $Path))
    [IO.File]::WriteAllText($Path, ($Obj | ConvertTo-Json -Depth 8))
}

function Get-SimArg($Argv, [string]$Name) {
    $a = @($Argv | ForEach-Object { "$_" })
    $i = [array]::IndexOf($a, "--$Name")
    if ($i -ge 0 -and $i + 1 -lt $a.Count) { $a[$i + 1] } else { $null }
}

# A process object for the spike's own programs: HasExited turns true after $Polls readings.
function New-SimProc([int]$Id, [int]$Polls, [string]$Name) {
    $p = [pscustomobject]@{ Id = $Id; ProcessName = $Name; Left = $Polls; ExitCode = 0; Handle = [IntPtr]::Zero; MainWindowHandle = [IntPtr]::Zero }
    $p | Add-Member -MemberType ScriptProperty -Name HasExited -Value { if ($this.Left -le 0) { return $true }; $this.Left = $this.Left - 1; $false }
    # As .NET: WaitForExit() returns nothing, WaitForExit(ms) a bool.
    $p | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { $this.Left = 0; if ($args.Count -gt 0) { $true } }
    $p | Add-Member -MemberType ScriptMethod -Name Kill -Value { $this.Left = 0 }
    $p | Add-Member -MemberType ScriptMethod -Name Refresh -Value { }
    $p
}

# ---------------------------------------------------------------- the registry, in memory

$script:SimReg = @{}
function ConvertTo-SimKey([string]$Path) { $Path.TrimEnd('\').ToLowerInvariant() }
function Test-SimReg([string]$Path) { "$Path".StartsWith('SIMREG:\', [StringComparison]::OrdinalIgnoreCase) }
function Assert-NoRealRegistry($Paths) {
    foreach ($p in @($Paths)) {
        if ("$p" -match '^(?i)(HK[A-Z]{1,4}:|Registry::|Microsoft\.PowerShell\.Core\\Registry::)') { throw "simulation: a write to the real registry was attempted ($p); nothing was written" }
    }
}
function Add-SimKey([string]$Path) {
    $parts = @($Path.TrimEnd('\') -split '\\')
    for ($i = 1; $i -lt $parts.Count; $i++) {
        $k = ConvertTo-SimKey ($parts[0..$i] -join '\')
        if (-not $script:SimReg.ContainsKey($k)) { $script:SimReg[$k] = @{ Default = $null; Props = [ordered]@{} } }
    }
    $script:SimReg[(ConvertTo-SimKey $Path)]
}
function Get-SimSubKeys([string]$Path) {
    $k = (ConvertTo-SimKey $Path) + '\'
    @($script:SimReg.Keys | Where-Object { $_.StartsWith($k) -and $_.Substring($k.Length) -notmatch '\\' })
}
function Copy-Bound($Bound) { $h = @{}; foreach ($k in $Bound.Keys) { $h[$k] = $Bound[$k] }; $h }

function New-Item {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [string]$ItemType, [object]$Value, [switch]$Force)
    $rest = @()
    foreach ($p in @($Path)) {
        if (Test-SimReg $p) { $k = Add-SimKey $p; if ($PSBoundParameters.ContainsKey('Value')) { $k.Default = $Value } }
        else { $rest += $p }
    }
    if ($rest.Count -eq 0) { return }
    Assert-NoRealRegistry $rest
    # The spike core's quit flag releases its (fake) power request.
    if (@($rest | Where-Object { "$_" -like '*power-quit.flag' }).Count -gt 0) { $script:SimHolding = $false }
    $bp = Copy-Bound $PSBoundParameters; $bp['Path'] = $rest
    Microsoft.PowerShell.Management\New-Item @bp
}

function Remove-Item {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [string[]]$LiteralPath, [switch]$Recurse, [switch]$Force)
    $all = @(@($Path) + @($LiteralPath) | Where-Object { $null -ne $_ })
    $sim = @($all | Where-Object { Test-SimReg $_ })
    foreach ($p in $sim) {
        $k = ConvertTo-SimKey $p
        if (-not $script:SimReg.ContainsKey($k)) { throw "simulation registry: no key $p" }
        $kids = @($script:SimReg.Keys | Where-Object { $_.StartsWith("$k\") })
        if ($kids.Count -gt 0 -and -not $Recurse) { throw "simulation registry: $p has subkeys" }
        foreach ($c in $kids) { $script:SimReg.Remove($c) }
        $script:SimReg.Remove($k)
    }
    if ($all.Count -gt 0 -and $sim.Count -eq $all.Count) { return }
    Assert-NoRealRegistry $all
    Microsoft.PowerShell.Management\Remove-Item @PSBoundParameters
}

function Test-Path {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [string[]]$LiteralPath, [Microsoft.PowerShell.Commands.TestPathType]$PathType = 'Any')
    $all = @(@($Path) + @($LiteralPath) | Where-Object { $null -ne $_ })
    if ($all.Count -gt 0 -and @($all | Where-Object { Test-SimReg $_ }).Count -eq $all.Count) {
        return (@($all | Where-Object { -not $script:SimReg.ContainsKey((ConvertTo-SimKey $_)) }).Count -eq 0)
    }
    if ($SimFor -eq 'run-sandbox') { Invoke-SimSandboxTick }
    Microsoft.PowerShell.Management\Test-Path @PSBoundParameters
}

function Get-Item {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [string[]]$LiteralPath)
    $all = @(@($Path) + @($LiteralPath) | Where-Object { $null -ne $_ })
    if ($all.Count -gt 0 -and (Test-SimReg $all[0])) {
        $k = ConvertTo-SimKey $all[0]
        if (-not $script:SimReg.ContainsKey($k)) { return $null }
        $e = $script:SimReg[$k]
        $o = [pscustomobject]@{ SubKeyCount = @(Get-SimSubKeys $all[0]).Count; ValueCount = $e.Props.Count; Names = @($e.Props.Keys) }
        $o | Add-Member -MemberType ScriptMethod -Name GetValueNames -Value { @($this.Names) }
        return $o
    }
    Microsoft.PowerShell.Management\Get-Item @PSBoundParameters
}

function Get-ItemProperty {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [Parameter(Position = 1)][string[]]$Name, [string[]]$LiteralPath)
    $all = @(@($Path) + @($LiteralPath) | Where-Object { $null -ne $_ })
    if ($all.Count -gt 0 -and (Test-SimReg $all[0])) {
        $k = ConvertTo-SimKey $all[0]
        if (-not $script:SimReg.ContainsKey($k)) { return $null }
        $h = [ordered]@{}
        foreach ($n in $script:SimReg[$k].Props.Keys) { if (-not $Name -or $n -in $Name) { $h[$n] = $script:SimReg[$k].Props[$n] } }
        return [pscustomobject]$h
    }
    Microsoft.PowerShell.Management\Get-ItemProperty @PSBoundParameters
}

function New-ItemProperty {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [Parameter(Position = 1)][string]$Name, [object]$Value, [string]$PropertyType, [switch]$Force)
    if (Test-SimReg $Path[0]) { (Add-SimKey $Path[0]).Props[$Name] = $Value; return }
    Assert-NoRealRegistry $Path
    Microsoft.PowerShell.Management\New-ItemProperty @PSBoundParameters
}

function Remove-ItemProperty {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [Parameter(Position = 1)][string[]]$Name)
    if (Test-SimReg $Path[0]) { foreach ($n in $Name) { $script:SimReg[(ConvertTo-SimKey $Path[0])].Props.Remove($n) }; return }
    Assert-NoRealRegistry $Path
    Microsoft.PowerShell.Management\Remove-ItemProperty @PSBoundParameters
}

function Set-ItemProperty {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string[]]$Path, [Parameter(Position = 1)][string]$Name, [Parameter(Position = 2)][object]$Value)
    if (Test-SimReg $Path[0]) { (Add-SimKey $Path[0]).Props[$Name] = $Value; return }
    Assert-NoRealRegistry $Path
    Microsoft.PowerShell.Management\Set-ItemProperty @PSBoundParameters
}

# What a Windows PC already has: the CLSID parent, and an association-toast value for the scheme
# that the cleanup removes (and one of another app's that it keeps).
[void](Add-SimKey 'SIMREG:\Software\Classes\CLSID')
$assoc = Add-SimKey 'SIMREG:\Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts'
$assoc.Props['lilspike_lilspike'] = 0
$assoc.Props['someapp_https'] = 0

# ---------------------------------------------------------------- the paths

if ($SimFor -eq 'run-spike') {
    $Bin = $SimBin
    $Core = Join-Path $Bin 'winshell-core.exe'
    $App = Join-Path $Bin 'winshell-app.exe'
    $Hello = Join-Path $Bin 'winshell-hello.exe'
    $ToolExe = Join-Path $Bin 'winshell-tool.exe'
    $Results = $SimResults
    $Raw = Join-Path $Results 'raw'
    $LogDir = Join-Path $SimLocal 'LILITH-spike\winshell'
    $Events = Join-Path $LogDir 'events.jsonl'
    $SchemeKey = "SIMREG:\Software\Classes\$Scheme"
    $AumidKey = "SIMREG:\Software\Classes\AppUserModelId\$Aumid"
    $AumidParent = 'SIMREG:\Software\Classes\AppUserModelId'
    $NotifySettingsKey = "SIMREG:\Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\$Aumid"
    $AssocToastsKey = 'SIMREG:\Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts'
    $ClsidKey = "SIMREG:\Software\Classes\CLSID\$ToastClsid"
    $ClsidParent = 'SIMREG:\Software\Classes\CLSID'
} elseif ($SimFor -eq 'run-sandbox') {
    $Bin = $SimBin
    $ToolExe = Join-Path $Bin 'winshell-tool.exe'
    $Build = Join-Path $SimDir 'build\sandbox'
    $ResultsRoot = $SimResults
} else {
    $TimeoutFile = Join-Path $SimResults 'sleep-timeout-at-attended.json'
}

# ---------------------------------------------------------------- the spike's programs

$script:SimHistory = @{}        # the toast tags in the notification centre
$script:SimLastToast = $null    # @{ name; suppress } of the last toast posted
$script:SimTrial = ''           # the core's result name of the last toast: which scenario runs
$script:SimHolding = $false     # whether a fake power request is held
$script:SimLastA = 0            # S1.4's instance A
$script:SimBlockers = 0
$script:SimShortcut = $null
$script:SimWindows = @()        # top-level windows the fake UIA lists: @{ pid; hwnd; class; name }
$script:SimFg = [System.Collections.Generic.Queue[object]]::new()
$script:SimTerminal = [ordered]@{ hwnd = 100; pid = 11; process = 'WindowsTerminal'; class = 'CASCADIA_HOSTING_WINDOW_CLASS' }
$script:SimIdleCalls = 0
$script:SimLockCalls = 0
$script:SimPopups = 0
$script:SimSandbox = $null
$script:SimTicking = $false
$script:SimQtLaunches = 0
$script:SimTopAtPoint = 0

function Add-SimEvent($E) {
    if ($SimFor -ne 'run-spike') { return }
    [void][IO.Directory]::CreateDirectory($LogDir)
    [IO.File]::AppendAllText($Events, (([pscustomobject]$E | ConvertTo-Json -Compress -Depth 5) + "`n"))
}

function Invoke-SimTool([string[]]$Argv) {
    $a = @($Argv)
    switch ($a[0]) {
        'spawn' { return Invoke-SimSpawn @($a | Select-Object -Skip 2) }
        'history-clear' { $script:SimHistory = @{}; return [pscustomobject]@{ cleared = $true } }
        'history' { $tag = Get-SimArg $a 'tag'; return [pscustomobject]@{ tag = $tag; has_tag = $script:SimHistory.ContainsKey($tag) } }
        'shortcut-write' {
            $path = Get-SimArg $a 'path'
            [IO.File]::WriteAllText($path, 'simulation shortcut')
            $script:SimShortcut = [pscustomobject]@{ exists = $true; target = (Get-SimArg $a 'target'); aumid = (Get-SimArg $a 'aumid'); toast_clsid = $(if ($a -contains '--toast-clsid') { $ToastClsid } else { '{00000000-0000-0000-0000-000000000000}' }) }
            return [pscustomobject]@{ written = $true; error = $null }
        }
        'shortcut-read' { return $script:SimShortcut }
        'shellexec' {
            # ShellExecute of the lilspike scheme: winshell-app starts with the URL as one quoted
            # argument, the same a toast click delivers (S1.3's activation grammar).
            $url = $a[1]
            Add-SimEvent ([ordered]@{ kind = 'start'; pid = (New-SimPid); cmdline = ('"' + $App + '" "' + $url + '"'); rest = ('"' + $url + '"') })
            return [pscustomobject]@{ shell_execute = 42 }
        }
        'programs-dir' { return [pscustomobject]@{ programs = (Join-Path $SimDir 'programs') } }
        'setting' { return [pscustomobject]@{ setting = 'Enabled' } }
        'token' { return [pscustomobject]@{ elevated = $false; integrity_rid = 8192; integrity = 'Medium' } }
        'b64url' {
            $b = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("$($a[1])")).TrimEnd('=').Replace('+', '-').Replace('/', '_')
            return [pscustomobject]@{ text = $a[1]; b64url = $b }
        }
        'foreground' { $f = Get-ForegroundNow; return [pscustomobject]@{ fg_pid = $f.pid; fg_hwnd = $f.hwnd } }
        'wsb' { $out = Get-SimArg $a 'out'; [IO.File]::WriteAllText($out, '<Configuration><!-- simulation --></Configuration>'); return [pscustomobject]@{ written = $true; path = $out } }
    }
    throw "simulation: unexpected winshell-tool $($a -join ' ')"
}
function Tool { Invoke-SimTool @($args | ForEach-Object { "$_" }) }
function Invoke-ToolAt([string]$Exe) { Invoke-SimTool @($args | ForEach-Object { "$_" }) }

# winshell-core started detached: its result file, written at once.
function Invoke-SimSpawn([string[]]$Argv) {
    $a = @($Argv)
    if ($a[0] -notlike '*winshell-core.exe') { throw "simulation: unexpected spawn of $($a[0])" }
    $cmd = $a[1]
    $out = Get-SimArg $a 'out'
    $name = [IO.Path]::GetFileNameWithoutExtension($out)
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $watch = ($a -contains '--watch-secs')
    # The toast's own events, as the core returns them with --watch-secs: a popped banner fades out
    # (dismissed/TimedOut); a SuppressPopup toast raises none.
    $popEvents = { param($suppress) @(if ($watch) { if (-not $suppress) { , [ordered]@{ t_ms = $now + 6000; kind = 'dismissed'; reason = 'TimedOut' } } }) }
    switch ($cmd) {
        'toast' {
            $tag = Get-SimArg $a 'tag'
            $suppress = ($a -contains '--suppress')
            $script:SimHistory[$tag] = $true
            $script:SimLastToast = @{ name = $name; suppress = $suppress }
            $script:SimTrial = $name
            if ($SimFor -eq 'run-spike') { [void](Add-SimKey $NotifySettingsKey) }   # Windows adds it on the first toast
            $doc = [ordered]@{ command = 'toast'; tag = $tag; posted = $true; setting = 'Enabled'; error = $null; shown_ms = $now; toast_events = (& $popEvents $suppress) }
        }
        'progress' {
            $script:SimHistory['j_spike'] = $true
            $script:SimLastToast = @{ name = $name; suppress = $false }
            $script:SimTrial = $name
            $doc = [ordered]@{ command = 'progress'; posted = $true; all_succeeded = $true; updates = @(1..20 | ForEach-Object { [ordered]@{ seq = $_; result = 'Succeeded' } }); toast_events = (& $popEvents $false) }
        }
        'update' { $doc = [ordered]@{ command = 'update'; seq = 100; result = $(if ($script:SimHistory.ContainsKey('j_spike')) { 'Succeeded' } else { 'NotificationNotFound' }) } }
        'remove-repost' {
            $script:SimLastToast = @{ name = $name; suppress = $false }
            $doc = [ordered]@{ command = 'remove-repost'; removed = $true; posted = $true; toast_events = (& $popEvents $false) }
        }
        'power' {
            $api = Get-SimArg $a 'api'
            if ($api -in 'request', 'ste') { $script:SimHolding = $true }
            $ev = @([ordered]@{ t_ms = $now; kind = 'window'; hwnd = 4242 }, [ordered]@{ t_ms = $now + 5; kind = 'set'; api = $api; ok = $true })
            if ($name -like 's17-sleep*') {
                $ev += [ordered]@{ t_ms = $now + 60000; kind = 'power_broadcast'; pbt = 'PBT_APMSUSPEND' }
                $ev += [ordered]@{ t_ms = $now + 120000; kind = 'power_broadcast'; pbt = 'PBT_APMRESUMEAUTOMATIC' }
            }
            $ev += [ordered]@{ t_ms = $now + 700000; kind = 'cleared'; api = $api; ok = $true }
            $ev += [ordered]@{ t_ms = $now + 700001; kind = 'exit' }
            $doc = [ordered]@{ command = 'power'; api = $api; reason = 'LILITH spike 11 keep-awake check'; events = $ev; cleared = $true }
        }
        default { throw "simulation: unexpected winshell-core $cmd" }
    }
    Write-SimJson $out $doc
    [pscustomobject]@{ pid = (New-SimPid); created = 1 }
}

function HelloCmd {
    $a = @($args | ForEach-Object { "$_" })
    switch ($a[0]) {
        'supported' { return [pscustomobject]@{ status = 'Success'; supported = $true } }
        'tpm' { return [pscustomobject]@{ tpm_2_0 = $true; tpm_version = '2.0' } }
        'dpapi' { return [pscustomobject]@{ round_trip = $true } }
        'sign' { return [pscustomobject]@{ signatures = @(1..5 | ForEach-Object { [pscustomobject]@{ status = 'Success'; signature_hex = 'ab12' } }) } }
        'delete' { return [pscustomobject]@{ deleted = $true } }
        'exists' { return [pscustomobject]@{ status = 'NotFound' } }
        'verify' {
            # The placement probe: the Hello prompt comes to the front over the blocker and is
            # cancelled (no gesture). The real behaviour is proven by first real use.
            if ($a -contains '--probe') {
                $blk = Get-SimArg $a 'blocker-hwnd'
                return [pscustomobject]@{ check = 'verify'; mode = 'probe'; owner_hwnd = (New-SimPid)
                    placement = [pscustomobject]@{ found = $true; dialog_hwnd = (New-SimPid); is_foreground = $true; above_blocker = $true; blocker_hwnd = [int64]$blk }
                    result_after_cancel = 'Canceled' }
            }
            return [pscustomobject]@{ check = 'verify'; result = 'Verified' }
        }
    }
    throw "simulation: unexpected winshell-hello $($a -join ' ')"
}

function Start-Process {
    [CmdletBinding()]
    param([Parameter(Position = 0)][string]$FilePath, [Parameter(Position = 1)][string[]]$ArgumentList, [switch]$PassThru, [switch]$NoNewWindow,
        [string]$RedirectStandardOutput, [string]$RedirectStandardError, [string]$WindowStyle)
    $leaf = Split-Path -Leaf $FilePath
    $argv = @($ArgumentList)
    if ($leaf -eq 'winshell-hello.exe') {
        $doc = $(if ($argv[0] -eq 'create') { [ordered]@{ status = 'Success' } } else { [ordered]@{ status = 'Success'; result = 'Verified' } })
        Write-SimJson $RedirectStandardOutput $doc
        # The Hello prompt comes in front while the call runs.
        $script:SimFg.Enqueue([ordered]@{ hwnd = 200; pid = 21; process = 'CredentialUIBroker'; class = 'Credential Dialog Xaml Host' })
        return New-SimProc (New-SimPid) 3 'winshell-hello'
    }
    if ($leaf -eq 'winshell-app.exe') {
        $id = New-SimPid
        if ($argv -contains '--blocker') {
            $script:SimBlockers++
            $hwnd = 7000 + $script:SimBlockers
            # A 'window' event (S1.8's placement probe reads the blocker's hwnd from it), and a
            # 'front_check' (S1.4's old path, kept).
            Add-SimEvent ([ordered]@{ kind = 'window'; pid = $id; hwnd = $hwnd; role = 'blocker' })
            Add-SimEvent ([ordered]@{ kind = 'front_check'; pid = $id; phase = 'start'; is_foreground = ($script:SimBlockers -ne 2); has_focus = ($script:SimBlockers -ne 2); hwnd = $hwnd })
            $script:SimWindows = @($script:SimWindows) + @(@{ pid = $id; hwnd = $hwnd; class = 'WinshellBlocker'; name = 'winshell blocker' })
            $script:SimTopAtPoint = $hwnd
        } else {
            $script:SimLastA = $id
            Add-SimEvent ([ordered]@{ kind = 'window'; pid = $id; hwnd = 6000 + $id % 1000 })
        }
        return New-SimProc $id 1000000 'winshell-app'
    }
    if ($leaf -eq 'WindowsSandbox.exe') {
        $p = Microsoft.PowerShell.Management\Start-Process -FilePath $SimPing -ArgumentList '-n', '8', '127.0.0.1' -WindowStyle Hidden -PassThru
        [void]$p.Handle
        Start-SimSandbox ("$($argv[0])".Trim('"')) $p
        return $p
    }
    throw "simulation: unexpected Start-Process $FilePath"
}

# ---------------------------------------------------------------- UI Automation, faked

function Get-ForegroundNow {
    $f = $(if ($script:SimFg.Count -gt 0) { $script:SimFg.Dequeue() } else { $script:SimTerminal })
    $o = [ordered]@{}
    foreach ($k in $f.Keys) { $o[$k] = $f[$k] }
    $o
}
function Get-IdleSeconds {
    $script:SimIdleCalls++
    # One touch during the stay-awake wait: it restarts once.
    $(if ($script:SimIdleCalls -eq 3) { 20.0 } else { 99999.0 })
}
function Get-PowerEvidence {
    [ordered]@{
        at = (Get-Date).ToUniversalTime().ToString('o')
        execution_state = $(if ($script:SimHolding) { '0x1' } else { '0x0' })
        system_required = [bool]$script:SimHolding
        idle_timer = [ordered]@{ status = 0; max_idleness_allowed_s = 300; idleness = 0; time_remaining_s = 300 }
        idle_s = 1.0
    }
}
function Get-SessionLocked { $script:SimLockCalls++; $script:SimLockCalls -le 2 }
function Get-SandboxRunning { @() }

function New-SimRect([int]$X, [int]$Y, [int]$W, [int]$H) { [pscustomobject]@{ X = $X; Y = $Y; Width = $W; Height = $H; IsEmpty = $false } }
function New-SimElement([string]$Name, [string]$Aid, $Rect = (New-SimRect 1600 900 80 30)) {
    [pscustomobject]@{ Current = [pscustomobject]@{ ControlType = [pscustomobject]@{ ProgrammaticName = 'ControlType.Button' }; Name = $Name; AutomationId = $Aid; ClassName = 'Button'; BoundingRectangle = $Rect; IsOffscreen = $false; NativeWindowHandle = 0 } }
}
function New-SimTop($W) {
    [pscustomobject]@{ Current = [pscustomobject]@{ ProcessId = $W.pid; NativeWindowHandle = $W.hwnd; ClassName = $W.class; Name = $W.name; BoundingRectangle = (New-SimRect 200 150 1280 800) } }
}

function Get-UiaTop {
    $live = @(Get-PopupPids)
    @(@($script:SimWindows) | Where-Object { $_.pid -notin $live } | ForEach-Object { New-SimTop $_ })
}
function Get-TopAtPoint([int]$X, [int]$Y) { [int64]$script:SimTopAtPoint }

# A toast's popup or entry: found (with this toast's own level records, Get-ToastLevels), unless
# it was posted with SuppressPopup.
function Watch-Needle([string]$Needle, $Before, [double]$Seconds, [switch]$UntilFound, [int]$IntervalMs = 250) {
    $quiet = $null -ne $script:SimLastToast -and $script:SimLastToast.suppress
    $script:SimClockMs += 1000 * [math]::Min($Seconds, 2)
    $info = [ordered]@{ hwnd = 500; pid = 51; process = 'ShellExperienceHost'; class = 'Windows.UI.Core.CoreWindow'; name = 'New notification' }
    $rec = [ordered]@{
        method = 'uia'; needle = $Needle; seconds = $Seconds; polls = 20; clean_polls = 20
        first_seen_s = $(if ($quiet) { $null } else { 0.5 }); appearances = @(if (-not $quiet) { [ordered]@{ from_s = 0.5; to_s = 5.0 } })
        host = $(if ($quiet) { $null } else { $info }); element = $null; samples = @()
    }
    if ($quiet) { return [pscustomobject]@{ Hit = $null; Record = $rec } }
    Add-UiaHost $info
    $script:Uia.proven = $true
    [pscustomobject]@{ Hit = [pscustomobject]@{ Element = (New-SimElement "toast $Needle" 'Text1' (New-SimRect 1500 800 400 150)); Top = $null; TopInfo = $info; Needle = $Needle }; Record = $rec }
}

# The level records Select-ToastTarget judges: this toast's text, then its list item with
# Windows' own chrome ("Dismiss", "...", "Notification settings", "Notification from LILITH
# Spike"). S1.3 trials 2 and 4 also hold another app's text in the item.
function Get-ToastLevels($Hit, [switch]$Invoke) {
    $needle = $Hit.Needle
    $progress = "$script:SimTrial" -like 's15-progress*'
    $text = $(if ($progress) { "Progress toast (S1.5) $needle" } else { "Click the toast (simulation) $needle" })
    $names = @($(if ($progress) { @('Spike job running', 'LILITH Spike', 'sampling', '42 %', '0.42') } else { @('LILITH spike') })) + @($text)
    if (-not $progress) { $names += 'Open' }
    $names += @('Dismiss', '...', 'Notification settings', 'Notification from LILITH Spike')
    if ("$script:SimTrial" -match '^s13-(2|4)-') { $names += 'Hi from a friend' }
    $els = @($names | ForEach-Object { [ordered]@{ name = $_; automation_id = ('Sim' + ($_ -replace '[^A-Za-z0-9]', '')); control = 'ControlType.Text'; class = 'TextBlock' } })
    $item = New-SimRect 1500 800 400 150
    $l0 = [ordered]@{ depth = 0; list_item = $false; names = @($text); elements = @($els | Where-Object { $_['name'] -eq $text }); rect = $item; candidates = @() }
    if ($Invoke) {
        $pattern = [pscustomobject]@{}
        $pattern | Add-Member -MemberType ScriptMethod -Name Invoke -Value { [void](Invoke-SimClick 'invoke') }
        $cands = @([ordered]@{ name = 'Notification from LILITH Spike'; automation_id = 'ListItem'; rect = $null; element = (New-SimElement 'Notification from LILITH Spike' 'ListItem' $item); pattern = $pattern })
    } else {
        $buttons = @($(if (-not $progress) { 'Open' })) + @('Dismiss', '...') | Where-Object { $_ }
        $cands = @($buttons | ForEach-Object { [ordered]@{ name = $_; automation_id = ('Sim' + ($_ -replace '[^A-Za-z0-9]', '') + 'Button'); rect = (New-SimRect 1600 900 80 30); element = (New-SimElement $_ ('Sim' + $_)) } })
    }
    $l1 = [ordered]@{ depth = 1; list_item = $true; names = $names; elements = $els; rect = $item; candidates = $cands }
    @($l0, $l1)
}

# A click on a found toast (UIA invoke, a button invoke, or a synthesized click), by scenario:
#   s13-2: the synthesized click finds no point;  s13-3: the invoke fails, the owner is active;
#   s13-6, post 1: clicks are made but bring no launch (a repost).
function Invoke-SimClick([string]$Way) {
    $t = "$script:SimTrial"
    $mouse = [ordered]@{ mode = 'synthetic-mouse'; ok = $true; point = '1640,915'; landed = '1640,915'; missed_by_px = 0; landed_off = $false; virtual_desk = '0,0,1920,1080'; tries = 1 }
    if ($t -match '^s13-2-' -and $Way -eq 'mouse') { return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'UIA puts no candidate point on the target' } }
    if ($t -match '^s13-3-') {
        if ($Way -eq 'invoke') { throw 'simulation: InvokePattern.Invoke failed' }
        return [ordered]@{ mode = 'synthetic-mouse'; ok = $false; why = 'owner active: the owner gave input within 2 s of each try'; owner_active = $true }
    }
    if ($t -ne 's13-6-1') { Add-SimLaunch }
    if ($Way -eq 'mouse') { return $mouse }
    [ordered]@{ mode = 'uia-invoke'; ok = $true; invoked = $null }
}
function Click-ElementCentre($Element, $Top) { Invoke-SimClick 'mouse' }
function Invoke-UiaButton($Button) {
    if ("$($Button.Current.Name)" -match '^(Dismiss|Close)') { $script:SimHistory.Remove('j_spike'); return [ordered]@{ mode = 'uia-invoke'; ok = $true; invoked = (Get-ElementInfo $Button) } }
    $r = Invoke-SimClick 'invoke-button'
    $r.invoked = Get-ElementInfo $Button
    $r
}
function Click-WindowVisible([int64]$Hwnd) {
    $w = @(@($script:SimWindows) | Where-Object { $_.hwnd -eq $Hwnd }) | Select-Object -First 1
    if ($null -ne $w) { $script:SimFg.Enqueue([ordered]@{ hwnd = $w.hwnd; pid = $w.pid; process = 'winshell-app'; class = $w.class }) }
    [ordered]@{ mode = 'synthetic-mouse'; ok = ($null -ne $w); point = '640,550'; tries = 1 }
}

# The launch a click starts (winshell-app B's events).
function Add-SimLaunch {
    $t = "$script:SimTrial"
    $b = New-SimPid
    Add-SimEvent ([ordered]@{ kind = 'start'; pid = $b; cmdline = "`"$App`" `"$ConfirmUrl`""; rest = "`"$ConfirmUrl`"" })
    if ($t -like 's14-noa-*') {
        Add-SimEvent ([ordered]@{ kind = 'front_check'; pid = $b; phase = 'start'; is_foreground = $true; has_focus = $true })
    } elseif ($t -like 's14-*') {
        Add-SimEvent ([ordered]@{ kind = 'handoff'; pid = $b; a_pid = $script:SimLastA; allow_set_foreground = $true })
        Add-SimEvent ([ordered]@{ kind = 'front_check'; pid = $script:SimLastA; phase = 'activate'; is_foreground = $true; has_focus = $true })
    }
}

function Invoke-RunBox([string]$Url) {
    Add-SimEvent ([ordered]@{ kind = 'start'; pid = (New-SimPid); cmdline = "`"$App`" `"$Url`""; rest = "`"$Url`"" })
    [ordered]@{ method = 'uia (Win+R, ValuePattern.SetValue, Enter)'; ok = $true; window = [ordered]@{ process = 'explorer'; class = '#32770' } }
}

# The owner does what a YOUR TURN or YOUR CLICK line asks.
function Say([string]$Text, [string]$Color = 'Cyan') {
    Write-Host $Text -ForegroundColor $Color
    if ($SimFor -ne 'run-spike') { return }
    if ($Text -like "Click the toast's *" -or $Text -like "YOUR CLICK: click the toast's BODY*") { Add-SimLaunch }
    elseif ($Text -like 'YOUR TURN: open the notification centre (Win+N) and close*') { $script:SimHistory.Remove('j_spike') }
}

# ---------------------------------------------------------------- the pop-up

# The pop-up's stand-in: a windowless ping that lives about 1 s, with the pop-up's "shown" file
# written at once. Every seventh never shows (the handshake's give-up path).
function Invoke-PopupSpawn([string]$Tool, [string[]]$SpawnArgs) {
    $state = $SpawnArgs[[array]::IndexOf($SpawnArgs, '-StateFile') + 1]
    $si = [Diagnostics.ProcessStartInfo]::new($SimPing, '-n 2 127.0.0.1')
    $si.UseShellExecute = $false; $si.CreateNoWindow = $true; $si.RedirectStandardOutput = $true
    $p = [Diagnostics.Process]::Start($si)
    $script:SimPopups++
    if ($script:SimPopups % 7 -ne 0) { [IO.File]::WriteAllText("$state.shown", "$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())") }
    [pscustomobject]@{ pid = $p.Id; created = $p.StartTime.ToFileTimeUtc() }
}

# ---------------------------------------------------------------- the Sandbox

function Get-SandboxInfo { [ordered]@{ kind = 'inbox'; wsb_cli = $null; package = $null; exe_present = $true; exe_version = '10.0.0.0 (simulation)'; wsb_list_exit = $null } }
function Build-Spike { Join-Path $SimDir 'rusttest\winshell-0000.exe' }
function Find-VcRedist { @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll' | ForEach-Object { Join-Path $SimDir "vcredist\$_" }) }
function Get-WsbIds { @() }
function Assert-NoSandboxRunning { if (@(Get-SandboxPids).Count -gt 0) { throw 'simulation: a Sandbox of an earlier run is still running' } }

function Write-SimItems([string]$Res, [string]$Sfx, [string]$Deploy, [string]$QtExe) {
    Write-SimJson (Join-Path $Res "toast$Sfx.json") ([ordered]@{ setting = 'Enabled'; history_has_tag = $true; popup = [ordered]@{ method = 'uia'; appearances = @([ordered]@{ from_s = 1; to_s = 5 }) }; popup_seen = $true; pass = $true })
    Write-SimJson (Join-Path $Res "foreground$Sfx.json") ([ordered]@{ precondition_met = $true; a_pid = 4242; foreground_pid_after_handoff = 4242; pass = $true })
    # The Qt item is the real Invoke-QtItem (uia.ps1) over the deploy copy the run mapped (Task 0's
    # trimmed deploy: platforms\qwindows.dll only), with a fake launcher that shows nothing: the
    # plugin decision, the missing offscreen plugin and the record are the real code's.
    $script:SimQtLaunches++
    $qt = Invoke-QtItem $Deploy $QtExe { param($exe) [ordered]@{ hwnd = 9001; class = 'Qt6QWindowIcon'; nonflat = $true; exit = 0; stderr = 'rhi: backend D3D11 (simulation)'; backend = 'D3D11' } }
    Write-SimJson (Join-Path $Res "qt$Sfx.json") $qt
}

function Start-SimSandbox([string]$WsbFile, $Launcher) {
    $runDir = Split-Path $WsbFile
    $res = Join-Path $runDir 'results'
    $cfg = [IO.File]::ReadAllText((Join-Path $runDir 'bundle\config.json')) | ConvertFrom-Json
    $pids = @((New-SimPid), (New-SimPid))
    # Run 2 doesn't close by itself: the stop path ends it through its pinned object.
    $script:SimSandbox = @{ Res = $res; Run = [int]$cfg.run; Pids = $pids; Launcher = $Launcher; Sticky = ([int]$cfg.run -eq 2); Deploy = [string]$cfg.deploy_dir; QtExe = [string]$cfg.qt_exe }
    $script:SimWindows = @(@($script:SimWindows) | Where-Object { $_.class -ne 'WindowsSandboxWindow' }) + @(@{ pid = $pids[1]; hwnd = 8001; class = 'WindowsSandboxWindow'; name = 'Windows Sandbox' })
    Write-SimJson (Join-Path $res 'started.json') ([ordered]@{ at = (Get-Date).ToUniversalTime().ToString('o'); ps = '5.1 (simulation)'; run = $cfg.run })
}

# What inside.ps1 has written by now: the items and done.json.
function Invoke-SimSandboxTick {
    $s = $script:SimSandbox
    if ($null -eq $s -or $script:SimTicking) { return }   # Invoke-QtItem calls Test-Path, which ticks
    $res = $s.Res
    if ([IO.File]::Exists((Join-Path $res 'done.json'))) { return }
    $script:SimTicking = $true
    try { Write-SimItems $res '' $s.Deploy $s.QtExe } finally { $script:SimTicking = $false }
    Write-SimJson (Join-Path $res 'elevation.json') ([ordered]@{ token = [ordered]@{ elevated = $false; integrity = 'Medium' }; whoami_label = 'Mandatory Label\Medium Mandatory Level (simulation)'; deelevated = $null })
    Write-SimJson (Join-Path $res 'protocol.json') ([ordered]@{ command_key_present = $true; argument = '"lilspike:confirm/cf_0123456789abcdef"'; pass = $true })
    Write-SimJson (Join-Path $res 'power.json') ([ordered]@{ listed = $true; pass = $true })
    Write-SimJson (Join-Path $res 'rust.json') ([ordered]@{ exit_with_dlls = 0; exit_without_dlls = -1073741515; pass = $true })
    Write-SimJson (Join-Path $res 'done.json') ([ordered]@{ at = 'simulation'; seconds = 300 })
}

function Get-SandboxPids {
    Invoke-SimSandboxTick
    $s = $script:SimSandbox
    if ($null -eq $s) { return @() }
    $done = [IO.File]::Exists((Join-Path $s.Res 'done.json'))
    if ($done -and (-not $s.Sticky -or $s.Launcher.HasExited)) { return @() }
    @($s.Pids)
}
function Get-SandboxProcesses { $(if (@(Get-SandboxPids).Count -gt 0) { @('WindowsSandbox', 'WindowsSandboxRemoteSession') } else { @() }) }
