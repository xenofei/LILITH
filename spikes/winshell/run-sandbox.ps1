#requires -Version 7
<#
.SYNOPSIS
Spike 11 (plan 4a Task 1), Step 2b: S1.9, the desktop checks inside Windows Sandbox.
Writes spikes/winshell/results/sandbox/run-<n>/ and results/sandbox-checks.json.

.DESCRIPTION
Needs the Windows Sandbox feature on (OWNER-STEPS.md) and Task 0's deploy folder
(spikes/app-shell/build/deploy). For each run it builds a bundle, writes a .wsb that maps
  - the bundle read-only at C:\lil\in,
  - a copy of the deploy folder read-only at its own host path,
  - an empty results folder writable at C:\lil\out,
starts the Sandbox, waits for started.json (120 s) and done.json, makes sure the Sandbox has
closed, and judges every item from the files inside.ps1 wrote. Nothing is written outside
spikes/winshell/build and spikes/winshell/results on the host.

.PARAMETER Runs
How many runs (the bar is 3 of 3).
.PARAMETER Observe
The host-state observation (one run, owner present): the toast, foreground and Qt items run
again with the Sandbox window behind other windows, then with the host locked.
.PARAMETER DeployDir
Task 0's deploy folder.
.PARAMETER QtExe
The spike app's exe, relative to DeployDir; empty = the one .exe in it.
.PARAMETER QtGrabArgs
The arguments that make the spike app grab its window offscreen to {png} and exit.
#>
param(
    [int]$Runs = 3,
    [switch]$Observe,
    [string]$DeployDir = '',
    [string]$QtExe = '',
    [string]$QtGrabArgs = '--grab {png}',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Spike = $PSScriptRoot
$Repo = (Resolve-Path (Join-Path $Spike '..\..')).Path
if (-not $DeployDir) { $DeployDir = Join-Path $Repo 'spikes\app-shell\build\deploy' }
$Bin = Join-Path $Spike 'target\release'
$ToolExe = Join-Path $Bin 'winshell-tool.exe'
$Build = Join-Path $Spike 'build\sandbox'
$ResultsRoot = Join-Path $Spike 'results'
$RunLimitSec = 600

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }
function Wait-Owner([string]$Text) { [void](Read-Host "$Text  (press Enter)") }

function Tool {
    $out = & $ToolExe @args
    ($out | Out-String) | ConvertFrom-Json
}

function Read-Json([string]$Path) {
    if (-not (Test-Path $Path)) { return $null }
    try { Get-Content -Raw $Path | ConvertFrom-Json } catch { $null }
}

function P($Obj, [string]$Name) {
    if ($null -eq $Obj) { return $null }
    if ($Obj -is [System.Collections.IDictionary]) { return $Obj[$Name] }
    $p = $Obj.PSObject.Properties[$Name]
    if ($p) { $p.Value } else { $null }
}

# ---------------------------------------------------------------- the Sandbox itself

function Get-SandboxInfo {
    $wsb = Get-Command wsb -ErrorAction SilentlyContinue
    $exe = Join-Path $env:windir 'System32\WindowsSandbox.exe'
    $pkg = Get-AppxPackage -Name '*WindowsSandbox*' -ErrorAction SilentlyContinue | Select-Object -First 1
    $info = [ordered]@{
        kind          = $(if ($wsb) { 'store (wsb CLI)' } elseif (Test-Path $exe) { 'inbox' } else { 'absent' })
        wsb_cli       = $(if ($wsb) { $wsb.Source } else { $null })
        package       = $(if ($pkg) { "$($pkg.Name) $($pkg.Version)" } else { $null })
        exe_present   = [bool](Test-Path $exe)
        exe_version   = $(if (Test-Path $exe) { (Get-Item $exe).VersionInfo.ProductVersion } else { $null })
        wsb_list_exit = $null
    }
    if ($wsb) {
        & wsb list *> $null
        $info.wsb_list_exit = $LASTEXITCODE
    }
    $info
}

function Get-SandboxProcesses {
    @(Get-Process | Where-Object { $_.Name -match 'Sandbox|vmmem' } | ForEach-Object { $_.Name } | Sort-Object -Unique)
}

function Test-SandboxRunning([string]$Id) {
    if ($Id -and (Get-Command wsb -ErrorAction SilentlyContinue)) {
        $list = (& wsb list 2>&1 | Out-String)
        return $list -match [regex]::Escape($Id)
    }
    @(Get-Process | Where-Object { $_.Name -match '^WindowsSandbox' }).Count -gt 0
}

function Start-Sandbox([string]$WsbFile) {
    if (Get-Command wsb -ErrorAction SilentlyContinue) {
        # One line: the configuration is passed as a single command-line argument.
        $xml = (Get-Content -Raw $WsbFile) -replace '\r?\n\s*', ''
        $out = (& wsb start --config $xml 2>&1 | Out-String)
        $id = [regex]::Match($out, '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}').Value
        # The logon command needs a user session: connect opens the Sandbox window.
        if ($id) { Start-Process -FilePath wsb -ArgumentList 'connect', '--id', $id | Out-Null }
        return [ordered]@{ via = 'wsb start + wsb connect'; id = $id; start_output = $out.Trim(); process = $null }
    }
    $p = Start-Process -FilePath (Join-Path $env:windir 'System32\WindowsSandbox.exe') -ArgumentList "`"$WsbFile`"" -PassThru
    [ordered]@{ via = 'WindowsSandbox.exe <file.wsb>'; id = $null; start_output = $null; process = $p.Id }
}

# Waits for the Sandbox to close by itself (inside.ps1 ends with shutdown /s /t 0), then tries
# wsb stop, then ends the processes this script started and the Sandbox client.
function Stop-Sandbox($Started) {
    $id = $Started.id
    $tries = @()
    $deadline = (Get-Date).AddSeconds(90)
    while ((Get-Date) -lt $deadline -and (Test-SandboxRunning $id)) { Start-Sleep -Seconds 2 }
    if (-not (Test-SandboxRunning $id)) { return [ordered]@{ method = 'shutdown /s /t 0 inside'; tries = $tries } }
    if ($id) {
        & wsb stop --id $id *> $null
        $tries += "wsb stop --id: exit $LASTEXITCODE"
        Start-Sleep -Seconds 15
        if (-not (Test-SandboxRunning $id)) { return [ordered]@{ method = 'wsb stop --id'; tries = $tries } }
    }
    $procs = @(Get-Process | Where-Object { $_.Name -match '^WindowsSandbox' -or ($Started.process -and $_.Id -eq $Started.process) })
    $tries += "Stop-Process: $(($procs | ForEach-Object { $_.Name }) -join ', ')"
    $procs | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 10
    [ordered]@{ method = $(if (Test-SandboxRunning $id) { 'none worked' } else { 'Stop-Process (WindowsSandbox* processes)' }); tries = $tries }
}

# ---------------------------------------------------------------- the bundle

function Find-VcRedist {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { throw 'vswhere.exe not found: Visual Studio Build Tools are needed for the VC++ DLLs' }
    $vs = & $vswhere -latest -products * -property installationPath
    $crt = Get-ChildItem (Join-Path $vs 'VC\Redist\MSVC') -Directory |
        Where-Object { $_.Name -match '^\d' } | Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Get-ChildItem (Join-Path $_.FullName 'x64') -Directory -Filter 'Microsoft.VC*.CRT' -ErrorAction SilentlyContinue } |
        Select-Object -First 1
    if (-not $crt) { throw "no x64 Microsoft.VC*.CRT folder under $vs\VC\Redist\MSVC" }
    $dlls = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll' | ForEach-Object { Join-Path $crt.FullName $_ }
    foreach ($d in $dlls) { if (-not (Test-Path $d)) { throw "missing $d" } }
    $dlls
}

function Build-Spike {
    Push-Location $Spike
    $ErrorActionPreference = 'Continue'   # cargo writes its progress to stderr
    try {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        # The dynamically linked Rust test exe for the "Rust exe" item: the crate's own unit tests.
        $msgs = & cargo test --release --locked --lib --no-run --message-format json 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'cargo test --no-run failed' }
        $exe = $msgs | ForEach-Object { try { $_ | ConvertFrom-Json } catch { } } |
            Where-Object { $_ -and $_.reason -eq 'compiler-artifact' -and $_.executable } |
            Select-Object -Last 1 -ExpandProperty executable
        if (-not $exe) { throw 'no test executable found' }
        $exe
    } finally { Pop-Location }
}

function New-Bundle([string]$Dir, [string]$TestExe, [string[]]$Dlls, [int]$Run, [string]$DeploySandboxPath, [string]$Exe) {
    Remove-Item $Dir -Recurse -Force -ErrorAction SilentlyContinue
    foreach ($sub in 'bin', 'vcrt', 'rusttest') { New-Item -ItemType Directory -Force -Path (Join-Path $Dir $sub) | Out-Null }
    Copy-Item (Join-Path $Spike 'inside.ps1') $Dir
    foreach ($name in 'winshell-core', 'winshell-app', 'winshell-hello', 'winshell-tool') {
        Copy-Item (Join-Path $Bin "$name.exe") (Join-Path $Dir 'bin')
    }
    # App-local VC++ DLLs beside the spike's exes, as the deploy folder has them; the same
    # three alone in vcrt\ for the Rust exe item, whose exe has none beside it.
    foreach ($d in $Dlls) { Copy-Item $d (Join-Path $Dir 'bin'); Copy-Item $d (Join-Path $Dir 'vcrt') }
    Copy-Item $TestExe (Join-Path $Dir 'rusttest\winshell-tests.exe')
    [ordered]@{
        run = $Run; observe = [bool]$Observe
        deploy_dir = $DeploySandboxPath; qt_exe = $Exe; qt_grab_args = $QtGrabArgs
    } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Dir 'config.json')
}

# The results folder must be writable through the Sandbox's share, which runs as SYSTEM.
function New-ResultsFolder([string]$Dir) {
    Remove-Item $Dir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $hasSystem = @((Get-Acl $Dir).Access | Where-Object { $_.IdentityReference -match 'SYSTEM' }).Count -gt 0
    if (-not $hasSystem) {
        & icacls $Dir /grant '*S-1-5-18:(OI)(CI)M' | Out-Null
    }
    [ordered]@{ system_inherited = $hasSystem; granted = -not $hasSystem }
}

# ---------------------------------------------------------------- judging

function Judge-Run([string]$Res, [string]$Sfx = '') {
    $elev = Read-Json (Join-Path $Res 'elevation.json')
    $limited = $false
    if ($elev -and (P (P $elev 'token') 'elevated')) {
        $limited = [bool](P (P $elev 'deelevated') 'worked')
    }
    $pick = {
        param([string]$Item)
        # The toast and foreground items are judged on the de-elevated run when the logon
        # process is elevated; with no non-elevated token they fail.
        if ($elev -and (P (P $elev 'token') 'elevated') -and $Item -in 'toast', 'foreground') {
            if (-not $limited) { return [ordered]@{ pass = $false; why = 'elevated, and no de-elevated launch worked' } }
            return Read-Json (Join-Path $Res "$Item-limited.json")
        }
        Read-Json (Join-Path $Res "$Item$Sfx.json")
    }
    $items = [ordered]@{}
    foreach ($item in 'toast', 'protocol', 'foreground', 'power', 'rust', 'qt') {
        $doc = & $pick $item
        $items[$item] = [ordered]@{ pass = [bool](P $doc 'pass'); file = $doc }
    }
    $items['elevation'] = [ordered]@{ recorded = $elev }
    $items
}

# ---------------------------------------------------------------- main

$info = Get-SandboxInfo
Say "Windows Sandbox: $($info.kind) $($info.package) $($info.exe_version)"
if ($info.kind -eq 'absent') { throw 'Windows Sandbox is not turned on: see spikes/winshell/OWNER-STEPS.md, "Turn on Windows Sandbox".' }
if (-not (Test-Path $DeployDir)) { throw "Task 0's deploy folder is missing: $DeployDir (S1.9 runs after Task 0's Step 2)" }
if (-not $QtExe) {
    $exes = @(Get-ChildItem $DeployDir -Filter *.exe -File)
    if ($exes.Count -ne 1) { throw "pass -QtExe: $DeployDir holds $($exes.Count) .exe files" }
    $QtExe = $exes[0].Name
}

$testExe = if ($SkipBuild) { (Get-ChildItem (Join-Path $Bin 'deps') -Filter 'winshell-*.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName } else { Build-Spike }
$dlls = Find-VcRedist

# The deploy copy, mapped read-only at its own host path inside the Sandbox.
$deployCopy = Join-Path $Build 'deploy-copy'
Remove-Item $deployCopy -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $deployCopy | Out-Null
Copy-Item (Join-Path $DeployDir '*') $deployCopy -Recurse -Force

$label = if ($Observe) { 'observe' } else { 'run' }
$summary = [ordered]@{ spike = 11; item = 'S1.9'; observe = [bool]$Observe; sandbox = $info; runs = @() }
$runCount = if ($Observe) { 1 } else { $Runs }

for ($run = 1; $run -le $runCount; $run++) {
    Say "---- $label $run of $runCount" 'Yellow'
    $runDir = Join-Path $Build "$label-$run"
    $bundle = Join-Path $runDir 'bundle'
    $res = Join-Path $runDir 'results'
    New-Bundle $bundle $testExe $dlls $run $deployCopy $QtExe
    $acl = New-ResultsFolder $res
    $wsbFile = Join-Path $runDir 'spike.wsb'
    $w = Tool wsb --bundle $bundle --deploy $deployCopy --results $res --out $wsbFile
    if (-not (P $w 'written')) { throw "wsb not written: $(P $w 'error')" }

    $t0 = Get-Date
    $started = Start-Sandbox $wsbFile
    $procs = @()
    $startedAt = $null
    while (((Get-Date) - $t0).TotalSeconds -lt 120 -and -not $startedAt) {
        if (Test-Path (Join-Path $res 'started.json')) { $startedAt = ((Get-Date) - $t0).TotalSeconds }
        $procs += Get-SandboxProcesses
        Start-Sleep -Seconds 2
    }
    if ($startedAt) { Say ("started.json after {0:N0} s" -f $startedAt) } else { Say 'no started.json within 120 s' 'Red' }

    if ($Observe -and $startedAt) {
        Say 'Host-state observation: watch the Sandbox window. A toast should pop inside it within a minute or two.'
        $toastSeen = (Read-Host 'Did the toast pop inside the Sandbox window? [y/n]') -match '^y'
        Wait-Owner 'Wait until the Sandbox has finished its first round (the console in it goes quiet), then put other windows OVER the Sandbox window'
        New-Item -ItemType File -Force -Path (Join-Path $res 'go-2.flag') | Out-Null
        $deadline = (Get-Date).AddSeconds(600)
        while ((Get-Date) -lt $deadline -and -not (Test-Path (Join-Path $res 'round-2.json'))) { Start-Sleep -Seconds 2 }
        Wait-Owner 'Round 2 done. Next: after Enter, lock the PC (Win+L) within 30 s, stay locked for 4 minutes, then sign in again'
        New-Item -ItemType File -Force -Path (Join-Path $res 'go-3.flag') | Out-Null
        $summary.observation = [ordered]@{ toast_seen_in_sandbox_window = $toastSeen }
    }

    $limit = if ($Observe) { 2400 } else { $RunLimitSec }
    $doneAt = $null
    while (((Get-Date) - $t0).TotalSeconds -lt $limit -and -not $doneAt) {
        if (Test-Path (Join-Path $res 'done.json')) { $doneAt = ((Get-Date) - $t0).TotalSeconds }
        $procs += Get-SandboxProcesses
        Start-Sleep -Seconds 3
    }
    $stop = Stop-Sandbox $started
    $closedAt = ((Get-Date) - $t0).TotalSeconds

    $lifecycle = [ordered]@{
        sandbox = $info; started = $started; acl = $acl
        detection = [ordered]@{
            wsb_list_ok = ($info.wsb_list_exit -eq 0)
            exe_present_and_started_within_120s = ($info.exe_present -and [bool]$startedAt)
            started_s = $startedAt
        }
        done_s = $doneAt; stop = $stop; start_to_closed_s = [int]$closedAt
        process_names = @($procs | Sort-Object -Unique)
    }
    $detected = $lifecycle.detection.wsb_list_ok -or ($startedAt -and $startedAt -le 120)
    $stopped = $stop.method -ne 'none worked'
    $lifecyclePass = [bool]($detected -and $stopped -and $doneAt -and $closedAt -lt $RunLimitSec)
    if ($Observe) { $lifecyclePass = [bool]($detected -and $stopped -and $doneAt) }
    $lifecycle.pass = $lifecyclePass
    $lifecycle | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $res 'lifecycle.json')

    $items = Judge-Run $res
    $items['lifecycle'] = [ordered]@{ pass = $lifecyclePass; file = $lifecycle }
    if ($Observe) {
        foreach ($r in 2, 3) {
            $items["round$r"] = [ordered]@{
                recorded = [ordered]@{
                    toast = Read-Json (Join-Path $res "toast-r$r.json")
                    foreground = Read-Json (Join-Path $res "foreground-r$r.json")
                    qt = Read-Json (Join-Path $res "qt-r$r.json")
                }
            }
        }
    }
    $runPass = @($items.Values | Where-Object { $_.Contains('pass') -and -not $_.pass }).Count -eq 0
    $summary.runs += [ordered]@{ run = $run; pass = $runPass; items = $items }

    $keep = Join-Path $ResultsRoot "sandbox\$label-$run"
    Remove-Item $keep -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $keep | Out-Null
    Copy-Item (Join-Path $res '*') $keep -Recurse -Force
    foreach ($k in $items.Keys) {
        $v = $items[$k]
        $text = if ($v.Contains('pass')) { if ($v.pass) { 'PASS' } else { 'FAIL' } } else { 'recorded' }
        Say ("  {0,-11} {1}" -f $k, $text) $(if ($text -eq 'FAIL') { 'Red' } elseif ($text -eq 'PASS') { 'Green' } else { 'Gray' })
    }
    Say ("  start to closed: {0:N0} s; stop: {1}" -f $closedAt, $stop.method)
}

$summary.pass = @($summary.runs | Where-Object { -not $_.pass }).Count -eq 0
$name = if ($Observe) { 'sandbox-observe.json' } else { 'sandbox-checks.json' }
$summary | ConvertTo-Json -Depth 14 | Set-Content -Encoding utf8 (Join-Path $ResultsRoot $name)
Say ("S1.9 {0}: {1}" -f $label, $(if ($summary.pass) { 'PASS' } else { 'FAIL' })) $(if ($summary.pass) { 'Green' } else { 'Red' })
Say "Results: $(Join-Path $ResultsRoot $name)"
