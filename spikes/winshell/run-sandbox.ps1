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
again with the Sandbox window covered by another window (the script places one of its own over
it), then with the host locked (the owner presses Win+L; the script sees the lock itself).
.PARAMETER DeployDir
Task 0's deploy folder. When not given: -Task0Repo's spikes\app-shell\build\deploy, else the
environment variable LILITH_TASK0_REPO's, else this worktree's own. If none exists, the script
prints the exact command to use and stops.
.PARAMETER Task0Repo
The worktree (or checkout) where Task 0 built its deploy folder.
.PARAMETER QtExe
The spike app's exe, relative to DeployDir; empty = app_shell_spike.exe when present, else the
one .exe in it.
.PARAMETER QtGrabArgs
The arguments that make the spike app grab its window offscreen to {png} and exit (Task 0's
app: --screenshot grabs only just before an automatic quit, so --quit-after is needed).
.PARAMETER Preflight
Starts nothing: prints, as JSON, whether a run could start now (the Sandbox feature, Task 0's
deploy folder, the Qt exe) and what to type when it couldn't (run-all.ps1, tests/sandbox_paths.rs).
#>
param(
    [int]$Runs = 3,
    [switch]$Observe,
    [string]$DeployDir = '',
    [string]$Task0Repo = '',
    [string]$QtExe = '',
    [string]$QtGrabArgs = '--screenshot {png} --quit-after 5000 --no-activate',
    [switch]$SkipBuild,
    [switch]$Preflight
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
# The blocker window the observation places over the Sandbox window: started by this script and
# ended by its pid only.
$script:BlockerProcess = $null

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }

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

# ---------------------------------------------------------------- Task 0's deploy folder

# Where Task 0's deploy folder is: -DeployDir, else -Task0Repo's, else LILITH_TASK0_REPO's, else
# this worktree's own. $null when the chosen one (or, with nothing given, this worktree's) is
# missing. No path is guessed: other worktrees are only suggested (Get-DeploySuggestions).
function Resolve-DeployDir([string]$Given, [string]$Task0, [string]$EnvRepo, [string]$OwnRepo) {
    $pick = if ($Given) { [ordered]@{ path = $Given; from = '-DeployDir' } }
            elseif ($Task0) { [ordered]@{ path = ([IO.Path]::Combine($Task0, $DeployRel)); from = '-Task0Repo' } }
            elseif ($EnvRepo) { [ordered]@{ path = ([IO.Path]::Combine($EnvRepo, $DeployRel)); from = 'LILITH_TASK0_REPO' } }
            else { [ordered]@{ path = ([IO.Path]::Combine($OwnRepo, $DeployRel)); from = 'this worktree' } }
    $pick.exists = [bool](Test-Path -LiteralPath $pick.path -PathType Container)
    $pick
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
        $open = @(Get-Process | Where-Object { $_.Name -in 'WindowsSandbox', 'WindowsSandboxClient', 'WindowsSandboxRemoteSession' })
        if ($open.Count -gt 0) { throw 'A Windows Sandbox is already running. Close it, then run this again.' }
    }
}

# The WindowsSandbox* pids that appeared after this run started its Sandbox: its own.
function Get-OwnPids($Started) {
    $now = @(Get-SandboxPids)
    $fresh = @($now | Where-Object { $_ -notin $Started.pids_before })
    foreach ($p in $fresh) { if ($Started.own_pids -notcontains $p) { $Started.own_pids += $p } }
    $fresh
}

# With the CLI and an id: that id only. Otherwise: the run's own pids.
function Test-SandboxRunning($Started) {
    if ($Started.id) { return @(Get-WsbIds) -contains $Started.id }
    @(Get-OwnPids $Started).Count -gt 0
}

function Start-Sandbox([string]$WsbFile) {
    $before = @(Get-SandboxPids)
    if (Get-Command wsb -ErrorAction SilentlyContinue) {
        # One line: the configuration is passed as a single command-line argument.
        $xml = (Get-Content -Raw $WsbFile) -replace '\r?\n\s*', ''
        $out = (& wsb start --config $xml 2>&1 | Out-String)
        $id = [regex]::Match($out, '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}').Value
        # The logon command needs a user session: connect opens the Sandbox window.
        if ($id) { Start-Process -FilePath wsb -ArgumentList 'connect', '--id', $id | Out-Null }
        return [ordered]@{ via = 'wsb start + wsb connect'; id = $id; start_output = $out.Trim(); process = $null; pids_before = $before; own_pids = @() }
    }
    $p = Start-Process -FilePath (Join-Path $env:windir 'System32\WindowsSandbox.exe') -ArgumentList "`"$WsbFile`"" -PassThru
    [ordered]@{ via = 'WindowsSandbox.exe <file.wsb>'; id = $null; start_output = $null; process = $p.Id; pids_before = $before; own_pids = @() }
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
    $procs = @(Get-OwnPids $Started | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
    $tries += "Stop-Process (own pids): $(($procs | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')"
    $procs | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 10
    $still = if ($Started.id) { Test-SandboxRunning $Started } else { @(Get-OwnPids $Started).Count -gt 0 }
    [ordered]@{ method = $(if ($still) { 'none worked' } else { 'Stop-Process (the run''s own WindowsSandbox* pids)' }); tries = $tries }
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
        run = $Run; observe = [bool]$Observe
        deploy_dir = $DeploySandboxPath; qt_exe = $Exe; qt_grab_args = $QtGrabArgs
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
    qt         = { param($d) (P (P $d 'offscreen') 'nonflat') -eq $true -and (P (P $d 'onscreen') 'nonflat') -eq $true }
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

# ---------------------------------------------------------------- the host-state observation

function Wait-ResultFile([string]$Path, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) { if (Test-Path -LiteralPath $Path) { return $true }; Start-Sleep -Seconds 2 }
    $false
}

# Round 2's host state: one of the spike's own windows (a blocker) placed over the Sandbox
# window and clicked to the front by the script; UIA then checks the Sandbox window's centre is
# covered. The blocker logs into build\sandbox, not the user's profile.
function Cover-SandboxWindow($Started) {
    $rec = [ordered]@{ method = 'a spike blocker window placed over the Sandbox window (SetWindowPos), clicked to the front (synthetic)'; covered = $false }
    if (-not (Initialize-SpikeUi)) { $rec.why = "UI Automation not available: $($script:Uia.error)"; return $rec }
    $own = @(Get-OwnPids $Started) + @($Started.own_pids) | Sort-Object -Unique
    $win = @(Get-UiaTop | Where-Object { $_.Current.ProcessId -in $own }) | Select-Object -First 1
    if ($null -eq $win) { $rec.why = 'no visible window of the Sandbox this run started'; return $rec }
    $sb = Get-TopInfo $win
    $r = $win.Current.BoundingRectangle
    $rec.sandbox_window = [ordered]@{ process = $sb.process; class = $sb.class }
    $env:WINSHELL_LOG_DIR = Join-Path $Build 'host-blocker-log'
    try { $p = Start-Process -FilePath (Join-Path $Bin 'winshell-app.exe') -ArgumentList '--blocker', '--exit-after-secs', '1200' -PassThru }
    finally { Remove-Item Env:\WINSHELL_LOG_DIR -ErrorAction SilentlyContinue }
    $script:BlockerProcess = $p
    $bw = $null
    for ($i = 0; $i -lt 40 -and $null -eq $bw; $i++) {
        Start-Sleep -Milliseconds 250
        $bw = @(Get-UiaTop | Where-Object { $_.Current.ProcessId -eq $p.Id }) | Select-Object -First 1
    }
    if ($null -eq $bw) { $rec.why = 'the blocker window did not appear'; return $rec }
    $bh = [int64]$bw.Current.NativeWindowHandle
    $rec.placed = [SpikeUi.Win]::Place($bh, [int]$r.X - 20, [int]$r.Y - 20, [int]$r.Width + 40, [int]$r.Height + 40)
    Start-Sleep -Milliseconds 500
    $rec.click = Click-WindowVisible $bh
    Start-Sleep -Milliseconds 500
    $cx = [int]($r.X + $r.Width / 2); $cy = [int]($r.Y + $r.Height / 2)
    $rec.covered = (Get-TopAtPoint $cx $cy) -eq $bh
    $fg = Get-ForegroundNow
    $rec.blocker_in_front = ($null -ne $fg -and $fg.pid -eq $p.Id)
    $rec
}

# The observation: no question asked. Round 1 runs uncovered (inside.ps1 detects its own toast
# popup by UIA); round 2 with the Sandbox window covered; round 3 with the host locked, which
# the owner does (Win+L) and the script detects (WTS session flags).
function Invoke-Observation([string]$Res, $Started) {
    $obs = [ordered]@{}
    $obs.round1_done = Wait-ResultFile (Join-Path $Res 'round-1.json') 900
    $obs.round2_cover = Cover-SandboxWindow $Started
    Say ("Round 2: the Sandbox window is {0}." -f $(if ($obs.round2_cover.covered) { 'covered by a spike window' } else { "not covered ($(P $obs.round2_cover 'why'))" }))
    New-Item -ItemType File -Force -Path (Join-Path $Res 'go-2.flag') | Out-Null
    $obs.round2_done = Wait-ResultFile (Join-Path $Res 'round-2.json') 600
    if ($script:BlockerProcess) { Stop-Process -Id $script:BlockerProcess.Id -Force -ErrorAction SilentlyContinue; $script:BlockerProcess = $null }

    [console]::Beep(880, 300)
    Say 'YOUR TURN: lock the PC now (Win+L). Stay locked for about 4 minutes, then sign in again. The script sees the lock and the sign-in by itself.' 'Yellow'
    $lock = [ordered]@{ method = 'WTSQuerySessionInformation(WTSSessionInfoEx) SessionFlags, every 2 s'; locked_after_s = $null; locked_throughout = $null; unlocked_after_s = $null }
    $t0 = Get-Date
    while (((Get-Date) - $t0).TotalSeconds -lt 300 -and (Get-SessionLocked) -ne $true) { Start-Sleep -Seconds 2 }
    if ((Get-SessionLocked) -eq $true) { $lock.locked_after_s = [int]((Get-Date) - $t0).TotalSeconds }
    else { $lock.why = 'not locked within 5 minutes: round 3 runs unlocked' }
    New-Item -ItemType File -Force -Path (Join-Path $Res 'go-3.flag') | Out-Null
    $states = @()
    $deadline = (Get-Date).AddSeconds(600)
    while ((Get-Date) -lt $deadline -and -not (Test-Path -LiteralPath (Join-Path $Res 'round-3.json'))) { $states += (Get-SessionLocked); Start-Sleep -Seconds 2 }
    $obs.round3_done = Test-Path -LiteralPath (Join-Path $Res 'round-3.json')
    if ($null -ne $lock.locked_after_s) { $lock.locked_throughout = @($states | Where-Object { $_ -ne $true }).Count -eq 0 }
    $deadline = (Get-Date).AddSeconds(1200)
    while ((Get-Date) -lt $deadline -and (Get-SessionLocked) -eq $true) { Start-Sleep -Seconds 2 }
    if ((Get-SessionLocked) -ne $true) { $lock.unlocked_after_s = [int]((Get-Date) - $t0).TotalSeconds }
    $obs.round3_lock = $lock
    $obs
}

if ($Preflight) {
    $i = Get-SandboxInfo
    $d = Resolve-DeployDir $DeployDir $Task0Repo $env:LILITH_TASK0_REPO $Repo
    $qt = $null
    $why = @()
    if ($i.kind -eq 'absent') { $why += 'Windows Sandbox is not turned on (OWNER-STEPS.md, "Turn on Windows Sandbox")' }
    if (-not $d.exists) { $why += "Task 0's deploy folder is missing: $($d.path) (from $($d.from))" }
    else { try { $qt = Resolve-QtExe $d.path $QtExe } catch { $why += $_.Exception.Message } }
    [ordered]@{
        ready = ($why.Count -eq 0); why = $why; sandbox = $i.kind; deploy = $d; qt_exe = $qt; qt_grab_args = $QtGrabArgs
        task0_worktrees_with_a_deploy = @(Get-DeploySuggestions $Repo)
    } | ConvertTo-Json -Depth 5 -Compress
    return
}

# ---------------------------------------------------------------- main

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

# The deploy copy, mapped read-only at its own host path inside the Sandbox.
$deployCopy = Join-Path $Build 'deploy-copy'
New-EmptyDir $deployCopy
Copy-Item (Join-Path $DeployDir '*') $deployCopy -Recurse -Force

Assert-NoSandboxRunning

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
    $started = $null
    $stop = $null
    $procs = @()
    $startedAt = $null
    $doneAt = $null
    # Ctrl+C or a throw still stops this run's Sandbox (fix round 1, M1).
    try {
        Assert-NoSandboxRunning
        $started = Start-Sandbox $wsbFile
        while (((Get-Date) - $t0).TotalSeconds -lt 120 -and -not $startedAt) {
            if (Test-Path (Join-Path $res 'started.json')) { $startedAt = ((Get-Date) - $t0).TotalSeconds }
            $procs += Get-SandboxProcesses
            [void](Get-OwnPids $started)
            Start-Sleep -Seconds 2
        }
        if ($startedAt) { Say ("started.json after {0:N0} s" -f $startedAt) } else { Say 'no started.json within 120 s' 'Red' }

        if ($Observe -and $startedAt) {
            Say 'Host-state observation: round 1 runs with the Sandbox window uncovered; hands off until the script asks for the lock.'
            $summary.observation = Invoke-Observation $res $started
        }

        $limit = if ($Observe) { 2400 } else { $RunLimitSec }
        while (((Get-Date) - $t0).TotalSeconds -lt $limit -and -not $doneAt) {
            if (Test-Path (Join-Path $res 'done.json')) { $doneAt = ((Get-Date) - $t0).TotalSeconds }
            $procs += Get-SandboxProcesses
            [void](Get-OwnPids $started)
            Start-Sleep -Seconds 3
        }
        $stop = Stop-Sandbox $started
    }
    finally {
        if ($script:BlockerProcess) { Stop-Process -Id $script:BlockerProcess.Id -Force -ErrorAction SilentlyContinue; $script:BlockerProcess = $null }
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
        # Round 1's toast popup, detected inside the Sandbox by UIA (replaces the owner watching).
        if ($summary.Contains('observation')) { $summary.observation.round1_toast_popup = P (Read-Json (Join-Path $res 'toast.json')) 'popup' }
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
    New-EmptyDir $keep
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
