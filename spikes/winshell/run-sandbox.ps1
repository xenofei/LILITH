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
starts the Sandbox, waits for started.json (120 s) and done.json (600 s), makes sure the Sandbox
has closed, and judges every item from the files inside.ps1 wrote. Nothing is written outside
spikes/winshell/build and spikes/winshell/results on the host, except the Sandbox's own start and
the optional keep-awake (-KeepAwake).

It runs unattended (scope cut, 2026-10-23): no prompt, no watched run, no lock step, every wait
bounded, cleanup in a finally on every exit path. Qt: the on-screen grab only. The offscreen grab
is not tested here: Task 0's trimmed deploy ships no qoffscreen.dll, and CI's S0.9 covers it.

.PARAMETER Runs
How many runs (the bar is 3 of 3).
.PARAMETER DeployDir
Task 0's deploy folder. When not given: -Task0Repo's spikes\app-shell\build\deploy, else the
environment variable LILITH_TASK0_REPO's, else this worktree's own. If none exists, the script
prints the exact command to use and stops.
.PARAMETER Task0Repo
The worktree (or checkout) where Task 0 built its deploy folder.
.PARAMETER QtExe
The spike app's exe, relative to DeployDir; empty = app_shell_spike.exe when present, else the
one .exe in it.
.PARAMETER KeepAwake
Holds a keep-awake for the whole run: the spike core's PowerSetRequest(PowerRequestSystemRequired),
watching this script's process (--parent-pid), released in the finally (run-all.ps1 passes it).
Without it the same core runs with --api none and only listens for sleep. Either way a run during
which the PC slept (a suspend broadcast, a clock jump, or a Kernel-Power sleep entry in the run's
time) is recorded as interrupted by sleep, never as a FAIL.
.PARAMETER Preflight
Starts nothing: prints, as JSON, whether a run could start now (the Sandbox feature, Task 0's
deploy folder, the Qt exe) and what to type when it couldn't (run-all.ps1, tests/sandbox_paths.rs).
.PARAMETER Simulate
For tests/simulate.ps1 only (through run-all.ps1 -Simulate): runs with every Windows call faked
(tests/sim-fakes.ps1), the Sandbox included. Nothing on this PC is touched, and every path goes
to the folder that test made (WINSHELL_SIM_DIR).
#>
param(
    [int]$Runs = 3,
    [string]$DeployDir = '',
    [string]$Task0Repo = '',
    [string]$QtExe = '',
    [switch]$SkipBuild,
    [switch]$Preflight,
    [switch]$KeepAwake,
    [switch]$Simulate
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Spike = $PSScriptRoot
$Repo = (Resolve-Path (Join-Path $Spike '..\..')).Path
$DeployRel = 'spikes\app-shell\build\deploy'
. (Join-Path $Spike 'uia.ps1')
$Bin = Join-Path $Spike 'target\release'
$ToolExe = Join-Path $Bin 'winshell-tool.exe'
$Build = Join-Path $Spike 'build\sandbox'
$ResultsRoot = Join-Path $Spike 'results'
$RunLimitSec = 600
# What S1.9's Qt bar means in this harness (scope cut): recorded in the summary.
$QtBarNote = 'on-screen grab only; offscreen not tested: the trimmed deploy ships no qoffscreen.dll; CI''s S0.9 covers offscreen'

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }

function Tool { Invoke-ToolAt $ToolExe @args }

# One winshell-tool command from the given exe: its one JSON line. A seam: under -Simulate,
# tests/sim-fakes.ps1 replaces it.
function Invoke-ToolAt([string]$Exe) {
    $out = & $Exe @args
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

# ---------------------------------------------------------------- Task 0's deploy folder

# Where Task 0's deploy folder is: -DeployDir, else -Task0Repo's, else LILITH_TASK0_REPO's, else
# this worktree's own. $null when the chosen one (or, with nothing given, this worktree's) is
# missing. No path is guessed: other worktrees are only suggested (Get-DeploySuggestions).
function Resolve-DeployDir([string]$Given, [string]$Task0, [string]$EnvRepo, [string]$OwnRepo) {
    # A path pasted as "C:\x\" from Windows PowerShell 5.1 or cmd arrives with a literal quote.
    $Given = Get-CleanPath $Given; $Task0 = Get-CleanPath $Task0; $EnvRepo = Get-CleanPath $EnvRepo
    $pick = if ($Given) { [ordered]@{ path = $Given; from = '-DeployDir' } }
            elseif ($Task0) { [ordered]@{ path = ([IO.Path]::Combine($Task0, $DeployRel)); from = '-Task0Repo' } }
            elseif ($EnvRepo) { [ordered]@{ path = ([IO.Path]::Combine($EnvRepo, $DeployRel)); from = 'LILITH_TASK0_REPO' } }
            else { [ordered]@{ path = ([IO.Path]::Combine($OwnRepo, $DeployRel)); from = 'this worktree' } }
    $pick.exists = [bool](Test-Path -LiteralPath $pick.path -PathType Container)
    $pick
}

# A stray quote removed, and a trailing backslash (but not a drive root's). Pure; tested.
function Get-CleanPath([string]$Path) {
    $p = "$Path".Trim().Trim('"')
    while ($p.Length -gt 3 -and $p.EndsWith('\')) { $p = $p.Substring(0, $p.Length - 1) }
    $p
}

# The worktrees of this repository (git worktree list) that hold a deploy folder: each becomes
# an exact command for the owner to run. Nothing is used without being named.
function Get-DeploySuggestions([string]$OwnRepo) {
    $out = @()
    try {
        $list = & git -C $OwnRepo worktree list --porcelain 2>$null
        foreach ($l in @($list)) {
            if ("$l" -match '^worktree (.+)$') {
                $wt = $Matches[1] -replace '/', '\'
                if (Test-Path -LiteralPath (Join-Path $wt $DeployRel) -PathType Container) { $out += $wt }
            }
        }
    } catch { }
    @($out)
}

# The spike app's exe in the deploy folder: -QtExe, else app_shell_spike.exe, else the only exe.
function Resolve-QtExe([string]$Dir, [string]$Given) {
    if ($Given) { return $Given }
    if (Test-Path -LiteralPath (Join-Path $Dir 'app_shell_spike.exe')) { return 'app_shell_spike.exe' }
    $exes = @(Get-ChildItem -LiteralPath $Dir -Filter *.exe -File)
    if ($exes.Count -eq 1) { return $exes[0].Name }
    throw "pass -QtExe: $Dir holds $($exes.Count) .exe files and no app_shell_spike.exe"
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

# The pids of WindowsSandbox* processes. The ones that exist before this script starts a
# Sandbox are never touched (fix round 1, I2).
function Get-SandboxPids {
    @(Get-Process | Where-Object { $_.Name -match '^WindowsSandbox' } | ForEach-Object { $_.Id })
}

function Get-WsbIds {
    if (-not (Get-Command wsb -ErrorAction SilentlyContinue)) { return @() }
    $list = (& wsb list 2>&1 | Out-String)
    @([regex]::Matches($list, '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}') | ForEach-Object { $_.Value })
}

# Refuses to start while any Sandbox runs: the stop steps below must only ever meet ours.
function Assert-NoSandboxRunning {
    if (Get-Command wsb -ErrorAction SilentlyContinue) {
        $ids = @(Get-WsbIds)
        if ($ids.Count -gt 0) { throw "A Windows Sandbox is already running ($($ids -join ', ')). Close it, then run this again." }
    } else {
        # Processes of the previous run's Sandbox may linger a little after its shutdown: up to
        # 30 s is allowed for them to go (scope-cut review M11), then it refuses.
        $names = 'WindowsSandbox', 'WindowsSandboxClient', 'WindowsSandboxRemoteSession'
        $deadline = (Get-Date).AddSeconds(30)
        while ((Get-Date) -lt $deadline -and @(Get-Process | Where-Object { $_.Name -in $names }).Count -gt 0) { Start-Sleep -Seconds 2 }
        $open = @(Get-Process | Where-Object { $_.Name -in $names })
        if ($open.Count -gt 0) { throw "A Windows Sandbox is already running ($(@($open | ForEach-Object { $_.Name } | Sort-Object -Unique) -join ', ')). Close it, then run this again." }
    }
}

# This run's own Sandbox processes, pinned (open handles) for Stop-Sandbox. They are kept here and
# never in a record: ConvertTo-Json throws on a Start-Process object (its StartInfo.Environment),
# takes seconds on a Get-Process one, and either would carry the whole environment block (fix
# round 4, N3-C2). The record holds {pid, name, start} for each (Get-ProcRecord).
$script:SandboxPinned = @()

function Get-ProcRecord($P) {
    $rec = [ordered]@{ pid = $null; name = $null; start = $null }
    try { $rec.pid = [int]$P.Id } catch { }
    try { $rec.name = [string]$P.ProcessName } catch { }
    try { $rec.start = $P.StartTime.ToUniversalTime().ToString('o') } catch { }
    $rec
}

# The WindowsSandbox* pids that appeared after this run started its Sandbox: its own.
function Get-OwnPids($Started) {
    $now = @(Get-SandboxPids)
    $fresh = @($now | Where-Object { $_ -notin $Started.pids_before })
    foreach ($p in $fresh) {
        if ($Started.own_pids -notcontains $p) {
            $Started.own_pids += $p
            # Pinned the moment it is first seen (an open handle, so a later stop can only reach
            # this process; fix round 3, N2-C1). Its name is checked again on the pinned object.
            $pp = Get-PinnedProcess $p '' $Started.started_at
            if ($null -ne $pp -and $pp.ProcessName -match '^WindowsSandbox') {
                $script:SandboxPinned = @($script:SandboxPinned) + @($pp)
                $Started.own_procs = @($Started.own_procs) + @(, (Get-ProcRecord $pp))
            }
        }
    }
    $fresh
}

# With the CLI and an id: that id only. Otherwise: the run's own pinned WindowsSandbox* processes
# (the launcher and the client it started) that are still running (scope-cut review M11). A new
# WindowsSandbox* process that could not be pinned (another user's, such as SYSTEM's) or that
# lingers is not counted here; Get-SandboxLingering records its name.
function Test-SandboxRunning($Started) {
    if ($Started.id) { return @(Get-WsbIds) -contains $Started.id }
    [void](Get-OwnPids $Started)
    @($script:SandboxPinned | Where-Object {
        $pp = $_
        $alive = $false
        try { $alive = $null -ne $pp -and -not $pp.HasExited -and $pp.ProcessName -match '^WindowsSandbox' } catch { }
        $alive
    }).Count -gt 0
}

# The names of the WindowsSandbox* processes that appeared after this run's start, are not pinned,
# and still run (recorded in lifecycle.json; scope-cut review M11).
function Get-SandboxLingering($Started) {
    $pinnedIds = @($script:SandboxPinned | ForEach-Object { try { [int]$_.Id } catch { } })
    @(@(Get-SandboxPids) | Where-Object { $_ -notin $Started.pids_before -and $_ -notin $pinnedIds } | ForEach-Object {
        try { (Get-Process -Id $_ -ErrorAction Stop).ProcessName } catch { }   # read-only pid lookup: a name
    } | Sort-Object -Unique)
}

function Start-Sandbox([string]$WsbFile) {
    $startedAt = Get-Date
    $before = @(Get-SandboxPids)
    $script:SandboxPinned = @()
    if (Get-Command wsb -ErrorAction SilentlyContinue) {
        # One line: the configuration is passed as a single command-line argument.
        $xml = (Get-Content -Raw $WsbFile) -replace '\r?\n\s*', ''
        $out = (& wsb start --config $xml 2>&1 | Out-String)
        $id = [regex]::Match($out, '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}').Value
        # The logon command needs a user session: connect opens the Sandbox window.
        if ($id) { Start-Process -FilePath wsb -ArgumentList 'connect', '--id', $id | Out-Null }
        return [ordered]@{ via = 'wsb start + wsb connect'; id = $id; start_output = $out.Trim(); process = $null; pids_before = $before; own_pids = @(); own_procs = @(); started_at = $startedAt }
    }
    $p = Start-Process -FilePath (Join-Path $env:windir 'System32\WindowsSandbox.exe') -ArgumentList "`"$WsbFile`"" -PassThru
    [void]$p.Handle   # pinned: a Start-Process object, its handle held from now on
    $script:SandboxPinned = @($p)
    [ordered]@{ via = 'WindowsSandbox.exe <file.wsb>'; id = $null; start_output = $null; process = $p.Id; pids_before = $before; own_pids = @(); own_procs = @(, (Get-ProcRecord $p)); started_at = $startedAt }
}

# Waits for the Sandbox to close by itself (inside.ps1 ends with shutdown /s /t 0), then tries
# wsb stop --id with the id this run started, then ends only the WindowsSandbox* processes that
# appeared after this run started (the Sandbox it started and its client).
function Stop-Sandbox($Started, [switch]$Now) {
    $tries = @()
    $deadline = (Get-Date).AddSeconds($(if ($Now) { 0 } else { 90 }))
    while ((Get-Date) -lt $deadline -and (Test-SandboxRunning $Started)) { Start-Sleep -Seconds 2 }
    if (-not (Test-SandboxRunning $Started)) { return [ordered]@{ method = 'shutdown /s /t 0 inside'; tries = $tries } }
    if ($Started.id) {
        & wsb stop --id $Started.id *> $null
        $tries += "wsb stop --id $($Started.id): exit $LASTEXITCODE"
        Start-Sleep -Seconds 15
        if (-not (Test-SandboxRunning $Started)) { return [ordered]@{ method = 'wsb stop --id'; tries = $tries } }
    }
    [void](Get-OwnPids $Started)
    $procs = @($script:SandboxPinned | Where-Object { $null -ne $_ -and -not $_.HasExited })
    $tries += "Kill (the run's own pinned WindowsSandbox* processes): $(($procs | ForEach-Object { "$($_.ProcessName) $($_.Id)" }) -join ', ')"
    foreach ($pp in $procs) { Stop-PinnedProcess $pp }
    Start-Sleep -Seconds 10
    $still = if ($Started.id) { Test-SandboxRunning $Started } else { @($script:SandboxPinned | Where-Object { $null -ne $_ -and -not $_.HasExited }).Count -gt 0 }
    [ordered]@{ method = $(if ($still) { 'none worked' } else { 'Kill (the run''s own pinned WindowsSandbox* processes)' }); tries = $tries }
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

# The release build alone (the host keep-awake needs winshell-core and winshell-tool first).
function Build-Release {
    Push-Location $Spike
    $ErrorActionPreference = 'Continue'   # cargo writes its progress to stderr
    try {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    } finally { Pop-Location }
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

# An empty folder, or a throw: a run is never judged on files an earlier run left (M3).
function New-EmptyDir([string]$Dir) {
    Remove-Item -LiteralPath $Dir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    if (Get-ChildItem -LiteralPath $Dir -Force) { throw "could not empty $Dir (is a file in it open?)" }
}

function New-Bundle([string]$Dir, [string]$TestExe, [string[]]$Dlls, [int]$Run, [string]$DeploySandboxPath, [string]$Exe) {
    New-EmptyDir $Dir
    # The marker inside.ps1's guard needs (fix round 1, I3). It exists only in the bundle,
    # which only the Sandbox sees at C:\lil\in.
    Set-Content -Encoding ascii -Path (Join-Path $Dir 'in-sandbox.marker') -Value 'LILITH spike 11 Sandbox bundle'
    foreach ($sub in 'bin', 'vcrt', 'rusttest') { New-Item -ItemType Directory -Force -Path (Join-Path $Dir $sub) | Out-Null }
    Copy-Item (Join-Path $Spike 'inside.ps1') $Dir
    Copy-Item (Join-Path $Spike 'uia.ps1') $Dir   # toast detection inside the Sandbox
    foreach ($name in 'winshell-core', 'winshell-app', 'winshell-hello', 'winshell-tool') {
        Copy-Item (Join-Path $Bin "$name.exe") (Join-Path $Dir 'bin')
    }
    # App-local VC++ DLLs beside the spike's exes, as the deploy folder has them; the same
    # three alone in vcrt\ for the Rust exe item, whose exe has none beside it.
    foreach ($d in $Dlls) { Copy-Item $d (Join-Path $Dir 'bin'); Copy-Item $d (Join-Path $Dir 'vcrt') }
    Copy-Item $TestExe (Join-Path $Dir 'rusttest\winshell-tests.exe')
    [ordered]@{
        run = $Run
        deploy_dir = $DeploySandboxPath; qt_exe = $Exe
    } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Dir 'config.json')
}

# The results folder must be writable through the Sandbox's share, which runs as SYSTEM.
function New-ResultsFolder([string]$Dir) {
    New-EmptyDir $Dir
    $hasSystem = @((Get-Acl $Dir).Access | Where-Object { $_.IdentityReference -match 'SYSTEM' }).Count -gt 0
    if (-not $hasSystem) {
        & icacls $Dir /grant '*S-1-5-18:(OI)(CI)M' | Out-Null
    }
    [ordered]@{ system_inherited = $hasSystem; granted = -not $hasSystem }
}

# ---------------------------------------------------------------- judging

# Each item is judged here from the raw fields inside.ps1 recorded, as the bar says; the
# `pass` inside.ps1 wrote is kept beside it as `inside_pass` only (fix round 1, M11).
$ExpectedArgument = '"lilspike:confirm/cf_0123456789abcdef"'
$Judges = [ordered]@{
    toast      = { param($d) (P $d 'setting') -eq 'Enabled' -and (P $d 'history_has_tag') -eq $true }
    protocol   = { param($d) (P $d 'command_key_present') -eq $true -and [string](P $d 'argument') -ceq $ExpectedArgument }
    foreground = { param($d) (P $d 'precondition_met') -eq $true -and $null -ne (P $d 'a_pid') -and (P $d 'foreground_pid_after_handoff') -eq (P $d 'a_pid') }
    power      = { param($d) (P $d 'listed') -eq $true }
    rust       = { param($d) $e = P $d 'exit_with_dlls'; $e -is [ValueType] -and $e -eq 0 }
    # On-screen grab only (scope cut): the offscreen grab is CI's S0.9, recorded as not tested.
    qt         = { param($d) $on = P $d 'onscreen'; (P $on 'launched') -eq $true -and (P $on 'nonflat') -eq $true }
}

function Judge-Run([string]$Res, [string]$Sfx = '') {
    $elev = Read-Json (Join-Path $Res 'elevation.json')
    $elevated = [bool](P (P $elev 'token') 'elevated')
    $limited = $elevated -and [bool](P (P $elev 'deelevated') 'worked')
    $items = [ordered]@{}
    foreach ($item in $Judges.Keys) {
        # The toast and foreground items are judged on the de-elevated run when the logon
        # process is elevated; with no non-elevated token they fail.
        if ($elevated -and $item -in 'toast', 'foreground') {
            if (-not $limited) {
                $items[$item] = [ordered]@{ pass = $false; why = 'elevated, and no de-elevated launch worked'; file = $null }
                continue
            }
            $doc = Read-Json (Join-Path $Res "$item-limited.json")
        } else {
            $doc = Read-Json (Join-Path $Res "$item$Sfx.json")
        }
        $pass = [bool]($doc -and (& $Judges[$item] $doc))
        $items[$item] = [ordered]@{ pass = $pass; inside_pass = (P $doc 'pass'); file = $doc }
    }
    $items['elevation'] = [ordered]@{ recorded = $elev }
    $items
}

# ---------------------------------------------------------------- the host's power during the runs

# The spike core on the host for the whole of this script: with -KeepAwake it holds the request,
# otherwise it holds nothing (--api none); either way its hidden window logs every suspend and
# resume and any clock jump, with times. It watches this script's process and stops with it.
# Its log and flags live under build\sandbox\host-power, not the user's profile.
# It runs from a copy of winshell-core.exe and winshell-tool.exe in build\sandbox\host-power\bin-*
# (fix round 4, m4): cargo can't replace target\release\winshell-core.exe while the core runs
# from it, so Build-Spike would fail on stale binaries. When they aren't built yet, they are
# built first (only that build runs before the keep-awake).
$script:HostPower = $null
function Start-HostPowerWatch([switch]$Hold) {
    $dir = Join-Path $Build 'host-power'
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Remove-Item (Join-Path $dir 'power-*.flag') -ErrorAction SilentlyContinue
    $out = Join-Path $dir 'power.json'
    Remove-Item -LiteralPath $out -ErrorAction SilentlyContinue
    $need = @('winshell-core.exe', 'winshell-tool.exe')
    if (@($need | Where-Object { -not (Test-Path -LiteralPath (Join-Path $Bin $_)) }).Count -gt 0) { Build-Release }
    # Earlier copies go when nothing runs from them (a running exe can't be deleted: that copy stays).
    Get-ChildItem -LiteralPath $dir -Directory -Filter 'bin-*' -ErrorAction SilentlyContinue | ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force -ErrorAction SilentlyContinue }
    $copy = Join-Path $dir ('bin-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Force -Path $copy | Out-Null
    foreach ($n in $need) { Copy-Item -LiteralPath (Join-Path $Bin $n) -Destination $copy }
    $env:WINSHELL_LOG_DIR = $dir
    try { $r = Invoke-ToolAt (Join-Path $copy 'winshell-tool.exe') spawn '--' (Join-Path $copy 'winshell-core.exe') power --api $(if ($Hold) { 'request' } else { 'none' }) --max-secs 43200 --parent-pid "$PID" --out $out }
    finally { Remove-Item Env:\WINSHELL_LOG_DIR -ErrorAction SilentlyContinue }
    $p = $null
    # Pinned by its pid and creation time (fix round 4, m1).
    $created = Get-SpawnCreated $r
    if ($r -and $r.PSObject.Properties['pid'] -and $created -ne 0) { $p = Get-PinnedProcess ([int]$r.pid) '' ([datetime]::MinValue) $created }
    $script:HostPower = [pscustomobject]@{ Process = $p; Out = $out; Dir = $dir; Hold = [bool]$Hold }
    Say $(if ($Hold) { 'The host is kept awake for these runs (a parent-watched power request).' } else { 'The host''s sleep and wake are watched during these runs.' })
}

function Stop-HostPowerWatch {
    $h = $script:HostPower
    if ($null -eq $h) { return $null }
    New-Item -ItemType File -Force -Path (Join-Path $h.Dir 'power-quit.flag') | Out-Null
    if ($null -ne $h.Process) {
        try { if (-not $h.Process.WaitForExit(20000)) { Stop-PinnedProcess $h.Process } } catch { }
    }
    Remove-Item (Join-Path $h.Dir 'power-*.flag') -ErrorAction SilentlyContinue
    Read-Json $h.Out
}

# The sleeps between two times (unix ms): the core's events, and the System log's Kernel-Power
# sleep (42) and resume (107) entries.
function Get-RunSleep([long]$FromMs, [long]$ToMs) {
    $doc = $null
    if ($script:HostPower) {
        $doc = Read-Json $script:HostPower.Out
        if ($null -eq $doc) { Start-Sleep -Milliseconds 200; $doc = Read-Json $script:HostPower.Out }   # caught mid-write
    }
    $kernel = @()
    try {
        $from = [DateTimeOffset]::FromUnixTimeMilliseconds($FromMs).LocalDateTime
        $kernel = @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = 'Microsoft-Windows-Kernel-Power'; Id = 42, 107, 506, 507; StartTime = $from } -ErrorAction Stop |
            ForEach-Object { [ordered]@{ id = $_.Id; t_ms = ([DateTimeOffset]$_.TimeCreated).ToUnixTimeMilliseconds() } })
    } catch { }
    Get-SleepInWindow @(P $doc 'events') $kernel $FromMs $ToMs
}

# -Simulate (fix round 4): the fakes replace every Windows call and redirect the paths; the
# main part below then runs as it does for the owner.
if ($Simulate) { . (Join-Path $PSScriptRoot 'tests\sim-fakes.ps1') -SimFor 'run-sandbox' }

if ($Preflight) {
    $i = Get-SandboxInfo
    $d = Resolve-DeployDir $DeployDir $Task0Repo $env:LILITH_TASK0_REPO $Repo
    $qt = $null
    $why = @()
    if ($i.kind -eq 'absent') { $why += 'Windows Sandbox is not turned on (OWNER-STEPS.md, "Turn on Windows Sandbox")' }
    if (-not $d.exists) { $why += "Task 0's deploy folder is missing: $($d.path) (from $($d.from))" }
    else { try { $qt = Resolve-QtExe $d.path $QtExe } catch { $why += $_.Exception.Message } }
    [ordered]@{
        ready = ($why.Count -eq 0); why = $why; sandbox = $i.kind; deploy = $d; qt_exe = $qt
        task0_worktrees_with_a_deploy = @(Get-DeploySuggestions $Repo)
    } | ConvertTo-Json -Depth 5 -Compress
    return
}


# ---------------------------------------------------------------- main

# Unattended by design (scope cut, 2026-10-23): no prompt and no watched run, every wait below has
# a time limit, and the host's keep-awake, the Sandbox this run started and the pop-ups are cleaned
# up in a finally on every exit path, Ctrl+C and errors included. The summary is written from the
# finally too, so the runs that finished are kept when a later step fails.
$summary = $null
try {
    # The host's keep-awake (or sleep watch) starts before the build and the deploy copy.
    Start-HostPowerWatch -Hold:$KeepAwake

    $info = Get-SandboxInfo
    Say "Windows Sandbox: $($info.kind) $($info.package) $($info.exe_version)"
    if ($info.kind -eq 'absent') { throw 'Windows Sandbox is not turned on: see spikes/winshell/OWNER-STEPS.md, "Turn on Windows Sandbox".' }
    $deploy = Resolve-DeployDir $DeployDir $Task0Repo $env:LILITH_TASK0_REPO $Repo
    if (-not $deploy.exists) {
        $cmd = 'pwsh -NoProfile -File spikes/winshell/run-sandbox.ps1'
        $hint = @(Get-DeploySuggestions $Repo | ForEach-Object { "  $cmd -Task0Repo `"$_`"" })
        if ($hint.Count -eq 0) { $hint = @("  $cmd -Task0Repo `"<the folder of Task 0's worktree>`"") }
        throw ("Task 0's deploy folder is missing: $($deploy.path) (from $($deploy.from)). Run this instead (from the repository folder):`n" + ($hint -join "`n"))
    }
    $DeployDir = $deploy.path
    Say "Task 0's deploy folder: $DeployDir (from $($deploy.from))"
    $QtExe = Resolve-QtExe $DeployDir $QtExe

    $testExe = if ($SkipBuild) { (Get-ChildItem (Join-Path $Bin 'deps') -Filter 'winshell-*.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName } else { Build-Spike }
    $dlls = Find-VcRedist
    if ($null -eq $script:HostPower.Process) { Start-HostPowerWatch -Hold:$KeepAwake }

    # The deploy copy, mapped read-only at its own host path inside the Sandbox. It is Task 0's
    # trimmed deploy as it is: nothing is added to it (the Qt step tests the on-screen grab; the
    # offscreen grab is CI's S0.9).
    $deployCopy = Join-Path $Build 'deploy-copy'
    New-EmptyDir $deployCopy
    Copy-Item (Join-Path $DeployDir '*') $deployCopy -Recurse -Force

    Assert-NoSandboxRunning

    $summary = [ordered]@{ spike = 11; item = 'S1.9'; sandbox = $info; runs_requested = $Runs; runs = @() }

    for ($run = 1; $run -le $Runs; $run++) {
        Say "---- run $run of $Runs" 'Yellow'
        $runDir = Join-Path $Build "run-$run"
        $bundle = Join-Path $runDir 'bundle'
        $res = Join-Path $runDir 'results'
        New-Bundle $bundle $testExe $dlls $run $deployCopy $QtExe
        $acl = New-ResultsFolder $res
        $wsbFile = Join-Path $runDir 'spike.wsb'
        $w = Tool wsb --bundle $bundle --deploy $deployCopy --results $res --out $wsbFile
        if (-not (P $w 'written')) { throw "wsb not written: $(P $w 'error')" }

        $t0 = Get-Date
        $t0ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $started = $null
        $stop = $null
        $procs = @()
        $startedAt = $null
        $doneAt = $null
        # The host's lock state, sampled read-only (WTSQuerySessionInformation) through both waits
        # (scope-cut review I2): recorded, not controlled.
        $lockSamples = [System.Collections.Generic.List[object]]::new()
        $sampleLock = { $lockSamples.Add([ordered]@{ t_s = [int]((Get-Date) - $t0).TotalSeconds; locked = (Get-SessionLocked) }) }
        # Ctrl+C or a throw still stops this run's Sandbox (fix round 1, M1).
        try {
            Assert-NoSandboxRunning
            & $sampleLock
            $started = Start-Sandbox $wsbFile
            # Both waits are bounded: 120 s for started.json, $RunLimitSec for done.json.
            while (((Get-Date) - $t0).TotalSeconds -lt 120 -and -not $startedAt) {
                if (Test-Path (Join-Path $res 'started.json')) { $startedAt = ((Get-Date) - $t0).TotalSeconds }
                $procs += Get-SandboxProcesses
                [void](Get-OwnPids $started)
                & $sampleLock
                Start-Sleep -Seconds 2
            }
            if ($startedAt) { Say ("started.json after {0:N0} s" -f $startedAt) } else { Say 'no started.json within 120 s' 'Red' }
            while ($startedAt -and ((Get-Date) - $t0).TotalSeconds -lt $RunLimitSec -and -not $doneAt) {
                if (Test-Path (Join-Path $res 'done.json')) { $doneAt = ((Get-Date) - $t0).TotalSeconds }
                $procs += Get-SandboxProcesses
                [void](Get-OwnPids $started)
                & $sampleLock
                Start-Sleep -Seconds 3
            }
            $stop = Stop-Sandbox $started
        }
        finally {
            if ($started -and -not $stop) {
                Say 'Interrupted: stopping the Sandbox this run started.' 'Red'
                $stop = Stop-Sandbox $started -Now
            }
        }
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
            lingering_after_stop = $(if ($started) { @(Get-SandboxLingering $started) } else { @() })
            host_locked_at_start = $(if ($lockSamples.Count -gt 0) { $lockSamples[0].locked } else { $null })
            host_locked_any = (@($lockSamples | Where-Object { $_.locked -eq $true }).Count -gt 0)
            host_lock_unknown_samples = @($lockSamples | Where-Object { $null -eq $_.locked }).Count
            host_locked_samples = @($lockSamples)
        }
        $detected = $lifecycle.detection.wsb_list_ok -or ($startedAt -and $startedAt -le 120)
        $stopped = $stop.method -ne 'none worked'
        $lifecyclePass = [bool]($detected -and $stopped -and $doneAt -and $closedAt -lt $RunLimitSec)
        $lifecycle.pass = $lifecyclePass
        $lifecycle | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $res 'lifecycle.json')

        $items = Judge-Run $res
        $items['lifecycle'] = [ordered]@{ pass = $lifecyclePass; file = $lifecycle }
        $judgement = Get-SandboxRunPass $items $lifecycle.host_locked_any
        # A run during which the PC slept says nothing about the Sandbox (review N-I1).
        $sleep = Get-RunSleep $t0ms ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
        $interrupted = $sleep.slept
        if ($interrupted) { Say '  interrupted by sleep: this run is not judged' 'Yellow' }
        if ($lifecycle.host_locked_any) { Say '  the host was locked during this run (recorded in lifecycle.json)' 'Yellow' }
        if (@($judgement.host_locked_not_judged).Count -gt 0) { Say "  host locked: not judged for $(@($judgement.host_locked_not_judged) -join ', ')" 'Yellow' }
        $summary.runs += [ordered]@{ run = $run; pass = $(if ($interrupted) { $null } else { $judgement.pass }); interrupted_by_sleep = $interrupted; host_locked_any = $lifecycle.host_locked_any; host_locked_not_judged = @($judgement.host_locked_not_judged); sleep = $sleep; items = $items }

        $keep = Join-Path $ResultsRoot "sandbox\run-$run"
        New-EmptyDir $keep
        Copy-Item (Join-Path $res '*') $keep -Recurse -Force
        foreach ($k in $items.Keys) {
            $v = $items[$k]
            $text = if ($v.Contains('pass')) { if ($v.pass) { 'PASS' } elseif ($k -in @($judgement.host_locked_not_judged)) { 'host locked: not judged' } else { 'FAIL' } } else { 'recorded' }
            Say ("  {0,-11} {1}" -f $k, $text) $(if ($text -eq 'FAIL') { 'Red' } elseif ($text -eq 'PASS') { 'Green' } else { 'Gray' })
        }
        Say ("  start to closed: {0:N0} s; stop: {1}" -f $closedAt, $stop.method)
    }
}
finally {
    $hp = Stop-HostPowerWatch
    if ($null -ne $summary) {
        $v = Get-SandboxVerdict @($summary.runs) $Runs
        $summary.pass = $v -eq 'PASS'
        # The Qt bar is half-tested here (scope-cut review M6): the verdict says so itself.
        $summary.verdict = $(if ($summary.pass) { 'PASS (Qt: on-screen grab only; offscreen not tested)' } else { $v })
        $summary.qt_bar = $QtBarNote
        $summary.host_power = $hp; $summary.host_kept_awake = [bool]$KeepAwake
        $summary | ConvertTo-Json -Depth 14 | Set-Content -Encoding utf8 (Join-Path $ResultsRoot 'sandbox-checks.json')
        Say ("S1.9: {0}" -f $summary.verdict) $(if ($summary.pass) { 'Green' } elseif ($summary.verdict -like 'INCOMPLETE*') { 'Yellow' } else { 'Red' })
        Say "Results: $(Join-Path $ResultsRoot 'sandbox-checks.json')"
    }
}
