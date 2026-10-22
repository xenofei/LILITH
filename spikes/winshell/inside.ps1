<#
.SYNOPSIS
Spike 11 (plan 4a Task 1), S1.9: runs inside Windows Sandbox as the logon command
  powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\inside.ps1
Written for Windows PowerShell 5.1 (pwsh is not in the Sandbox image).

.DESCRIPTION
C:\lil\in (read-only) holds this script, config.json, bin\ (the winshell exes with the VC++
DLLs beside them), vcrt\ (the three VC++ DLLs alone) and rusttest\ (one Rust test exe).
C:\lil\out (writable) receives started.json at once, one JSON file per item, and done.json;
then the Sandbox shuts itself down.

With -Items a,b -Suffix x it runs only those items and names their files <item>-x.json: the
de-elevated relaunch uses that (-Suffix limited). With config.observe it runs the toast,
foreground and Qt items twice more (-r2, -r3), each after the host writes go-2.flag / go-3.flag into C:\lil\out.

It refuses to run anywhere but inside Windows Sandbox (it writes HKCU keys and ends with
shutdown /s /t 0): the user must be WDAGUtilityAccount, the Sandbox's account, and the
bundle's marker file C:\lil\in\in-sandbox.marker must be mapped in. Otherwise it exits 2
before doing anything. -GuardOnly runs only that check (exit 0 inside, 2 elsewhere): the
crate's test tests/inside_guard.rs runs it on the host.
#>
param(
    [string[]]$Items = @('elevation', 'toast', 'protocol', 'foreground', 'power', 'rust', 'qt'),
    [string]$Suffix = '',
    [switch]$NoShutdown,
    [switch]$GuardOnly
)

# The guard comes first: nothing above it has any effect.
if ($env:USERNAME -ne 'WDAGUtilityAccount' -or -not (Test-Path -LiteralPath 'C:\lil\in\in-sandbox.marker')) {
    [Console]::Error.WriteLine('inside.ps1 runs only inside Windows Sandbox (user WDAGUtilityAccount and C:\lil\in\in-sandbox.marker); nothing was done.')
    exit 2
}
if ($GuardOnly) { exit 0 }

$ErrorActionPreference = 'Continue'
$In = 'C:\lil\in'
$Out = 'C:\lil\out'
$Bin = Join-Path $In 'bin'
$App = Join-Path $Bin 'winshell-app.exe'
$Core = Join-Path $Bin 'winshell-core.exe'
$ToolExe = Join-Path $Bin 'winshell-tool.exe'
$LogDir = Join-Path $env:LOCALAPPDATA 'LILITH-spike\winshell'
$Events = Join-Path $LogDir 'events.jsonl'
$Work = Join-Path $env:LOCALAPPDATA 'LILITH-spike\work'
$Aumid = 'LILITH.Spike.Notify'
$ConfirmUrl = 'lilspike:confirm/cf_0123456789abcdef'
$SchemeKey = 'HKCU:\Software\Classes\lilspike'
$PowerReason = 'LILITH spike 11 keep-awake check'
$T0 = Get-Date
# -File passes "a,b,c" as one string.
$Items = @($Items | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
$RunSuffix = $(if ($Suffix) { "-$Suffix" } else { '' })
$script:Sfx = $RunSuffix

New-Item -ItemType Directory -Force -Path $LogDir, $Work | Out-Null

function Now { (Get-Date).ToUniversalTime().ToString('o') }

function Write-Result([string]$Name, $Obj) {
    $path = Join-Path $Out "$Name.json"
    $Obj | ConvertTo-Json -Depth 10 | Set-Content -Encoding UTF8 -Path $path
}

function Tool {
    $o = & $ToolExe @args
    ($o | Out-String) | ConvertFrom-Json
}

function Get-Events {
    if (-not (Test-Path $Events)) { return @() }
    @(Get-Content $Events | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json })
}

function Wait-SpikeEvent([string]$Kind, [int]$After, [int]$TimeoutSec, [scriptblock]$Filter) {
    if (-not $Filter) { $Filter = { $true } }
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $ev = @(Get-Events)
        if ($ev.Count -gt $After) {
            foreach ($e in $ev[$After..($ev.Count - 1)]) {
                if ($e.kind -eq $Kind -and (& $Filter $e)) { return $e }
            }
        }
        Start-Sleep -Milliseconds 300
    }
    $null
}

function Stop-Spike {
    Get-Process -Name winshell-app, winshell-core -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

# Starts winshell-core detached through winshell-tool and waits for it to exit.
function Run-Core([string]$Name, [string[]]$CoreArgs, [int]$TimeoutSec) {
    $outFile = Join-Path $Work "$Name.json"
    Remove-Item $outFile -ErrorAction SilentlyContinue
    $r = Tool spawn '--' $Core @CoreArgs --out $outFile
    if ($r.pid) { Wait-Process -Id $r.pid -Timeout $TimeoutSec -ErrorAction SilentlyContinue }
    if (Test-Path $outFile) { return (Get-Content -Raw $outFile | ConvertFrom-Json) }
    $null
}

$Config = Get-Content -Raw (Join-Path $In 'config.json') | ConvertFrom-Json

# Suppresses the loader's "DLL not found" dialog in this process and its children, so the
# run without the VC++ DLLs ends with an exit code instead of waiting for a click.
Add-Type -Namespace Spike -Name Native -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);
[DllImport("user32.dll")] public static extern bool GetWindowRect(System.IntPtr hwnd, out RECT rect);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
'@
[void][Spike.Native]::SetErrorMode(0x0001 -bor 0x0002)   # SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX

if ($RunSuffix -eq '') {
    Write-Result 'started' @{ at = Now; ps = $PSVersionTable.PSVersion.ToString(); run = $Config.run; observe = $Config.observe }
}

# Explorer and the notification platform come up with the logon; give them a moment.
$waited = 0
while (-not (Get-Process explorer -ErrorAction SilentlyContinue) -and $waited -lt 60) { Start-Sleep 1; $waited++ }
Start-Sleep -Seconds 10

# ---------------------------------------------------------------- items

function Item-Elevation {
    $token = Tool token
    $integrity = (& whoami /groups | Select-String 'Mandatory Label') -join ' '
    $result = [ordered]@{ token = $token; whoami_label = $integrity; deelevated = $null }
    if ($token.elevated) {
        # Repeat the toast and foreground items under a de-elevated launch.
        $cmd = "powershell.exe -ExecutionPolicy Bypass -File $In\inside.ps1 -Items token,toast,foreground -Suffix limited -NoShutdown"
        $attempts = @()
        & schtasks /Create /TN LilSpikeLimited /TR $cmd /SC ONCE /ST 23:59 /RL LIMITED /IT /F | Out-Null
        $created = $LASTEXITCODE
        & schtasks /Run /TN LilSpikeLimited | Out-Null
        $ran = $LASTEXITCODE
        $done = Wait-File (Join-Path $Out 'done-limited.json') 180
        $tok = Read-Result 'token-limited'
        $attempts += [ordered]@{ method = 'schtasks /RL LIMITED'; create_exit = $created; run_exit = $ran; finished = $done; token = $tok }
        & schtasks /Delete /TN LilSpikeLimited /F | Out-Null
        $ok = $done -and $tok -and -not $tok.elevated
        if (-not $ok) {
            Remove-Item (Join-Path $Out '*-limited.json') -ErrorAction SilentlyContinue
            Start-Process -FilePath runas.exe -ArgumentList '/trustlevel:0x20000', "`"$cmd`"" -WindowStyle Hidden
            $done = Wait-File (Join-Path $Out 'done-limited.json') 180
            $tok = Read-Result 'token-limited'
            $attempts += [ordered]@{ method = 'runas /trustlevel:0x20000'; finished = $done; token = $tok }
            $ok = $done -and $tok -and -not $tok.elevated
        }
        $result.deelevated = [ordered]@{
            worked = [bool]$ok
            method = $(if ($ok) { $attempts[-1].method } else { $null })
            attempts = $attempts
        }
    }
    Write-Result "elevation$script:Sfx" $result
}

function Wait-File([string]$Path, [int]$TimeoutSec) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) { if (Test-Path $Path) { return $true }; Start-Sleep 1 }
    $false
}

function Read-Result([string]$Name) {
    $p = Join-Path $Out "$Name.json"
    if (Test-Path $p) { return (Get-Content -Raw $p | ConvertFrom-Json) }
    $null
}

function Item-Token { Write-Result "token$script:Sfx" (Tool token) }

function Item-Toast([string]$Tag) {
    $lnk = Join-Path (Tool programs-dir).programs 'LILITH Spike.lnk'
    $w = Tool shortcut-write --path $lnk --target $App --aumid $Aumid
    Start-Sleep -Seconds 5
    $attempts = @()
    for ($i = 1; $i -le 3; $i++) {
        $core = Run-Core "toast-$Tag-$i" @('toast', '--tag', $Tag, '--title', 'LILITH spike', '--body', "Sandbox toast $Tag") 30
        Start-Sleep -Seconds 3
        $h = Tool history --tag $Tag
        $attempts += [ordered]@{ attempt = $i; setting = $(if ($core) { $core.setting } else { $null }); posted = $(if ($core) { $core.posted } else { $false }); error = $(if ($core) { $core.error } else { 'no result' }); history = $h }
        if ($h.has_tag) { break }
        Start-Sleep -Seconds 10
    }
    $last = $attempts[-1]
    [ordered]@{
        shortcut = $w; setting = $last.setting; history_has_tag = [bool]$last.history.has_tag
        attempts = $attempts; pass = ($last.setting -eq 'Enabled' -and [bool]$last.history.has_tag)
    }
}

function Register-Scheme {
    New-Item -Path $SchemeKey -Value 'URL:LILITH (Spike)' -Force | Out-Null
    New-ItemProperty -Path $SchemeKey -Name 'URL Protocol' -Value '' -PropertyType String -Force | Out-Null
    New-Item -Path "$SchemeKey\DefaultIcon" -Value "`"$App`",0" -Force | Out-Null
    New-Item -Path "$SchemeKey\shell\open\command" -Value "`"$App`" `"%1`"" -Force | Out-Null
}

function Item-Protocol {
    Stop-Spike
    Register-Scheme
    $present = Test-Path "$SchemeKey\shell\open\command"
    $n = @(Get-Events).Count
    $se = Tool shellexec $ConfirmUrl
    $ev = Wait-SpikeEvent 'start' $n 30 $null
    Start-Sleep -Seconds 2
    Stop-Spike
    $expectedArg = "`"$ConfirmUrl`""
    $rest = $(if ($ev) { $ev.rest } else { $null })
    Write-Result "protocol$script:Sfx" ([ordered]@{
        command_key_present = $present; shell_execute = $se
        cmdline = $(if ($ev) { $ev.cmdline } else { $null }); argument = $rest; expected_argument = $expectedArg
        pass = ($present -and $rest -ceq $expectedArg)
    })
}

# One foreground attempt. It counts only when the second window really held the front before
# the hand-off (its own check: is_foreground); otherwise it is invalid and is retried, never
# passed (fix round 1, I4).
function Foreground-Attempt([int]$Attempt) {
    Stop-Spike
    $n = @(Get-Events).Count
    $a = Start-Process -FilePath $App -ArgumentList '--recipe', 'basic', '--exit-after-secs', '120' -PassThru
    $aWin = Wait-SpikeEvent 'window' $n 20 { param($e) $e.pid -eq $a.Id }
    # The second window, brought to the front by this script; once in front it launches B
    # itself (ShellExecuteW of the URL), as the toast host does after a click.
    $b = Start-Process -FilePath $App -ArgumentList '--blocker', '--launch-url', $ConfirmUrl, '--launch-after-ms', '3000', '--exit-after-secs', '120' -PassThru
    $bFront = Wait-SpikeEvent 'front_check' $n 20 { param($e) $e.pid -eq $b.Id }
    $valid = [bool]($bFront -and $bFront.is_foreground)
    $chk = Wait-SpikeEvent 'front_check' $n 30 { param($e) $e.pid -eq $a.Id -and $e.phase -eq 'activate' }
    $hand = @(Get-Events) | Where-Object { $_.kind -eq 'handoff' } | Select-Object -Last 1
    $launched = @(Get-Events) | Where-Object { $_.kind -eq 'blocker_launched' } | Select-Object -Last 1
    $after = Tool foreground
    Stop-Spike
    # Judged on A's own check only: no later snapshot stands in for it.
    $fgPid = $(if ($chk) { $chk.foreground.fg_pid } else { $null })
    [ordered]@{
        attempt = $Attempt; precondition_met = $valid
        a_pid = $a.Id; a_window = $aWin; second_window_pid = $b.Id; second_window_front = $bFront
        blocker_launched = $launched; handoff = $hand; a_check = $chk
        foreground_pid_after_handoff = $fgPid; foreground_now = $after
    }
}

function Item-Foreground([string]$Name) {
    Register-Scheme
    $attempts = @()
    $used = $null
    for ($i = 1; $i -le 3 -and -not $used; $i++) {
        $r = Foreground-Attempt $i
        $attempts += $r
        if ($r.precondition_met) { $used = $r }
    }
    if (-not $used) { $used = $attempts[-1] }
    $result = [ordered]@{}
    foreach ($k in $used.Keys) { $result[$k] = $used[$k] }
    $result.attempts = $attempts
    $result.pass = [bool]($used.precondition_met -and $null -ne $used.foreground_pid_after_handoff -and $used.foreground_pid_after_handoff -eq $used.a_pid)
    Write-Result $Name $result
}

function Item-Power {
    Remove-Item (Join-Path $LogDir 'power-*.flag') -ErrorAction SilentlyContinue
    $outFile = Join-Path $Work 'power-core.json'
    Remove-Item $outFile -ErrorAction SilentlyContinue
    $r = Tool spawn '--' $Core power --api request --max-secs 120 --out $outFile
    $set = $null
    for ($i = 0; $i -lt 40 -and -not $set; $i++) {
        Start-Sleep -Milliseconds 500
        if (Test-Path $outFile) {
            try {
                $doc = Get-Content -Raw $outFile | ConvertFrom-Json
                $set = @($doc.events | Where-Object { $_.kind -eq 'set' }) | Select-Object -First 1
            } catch { }   # caught mid-write; read again
        }
    }
    $requests = (& powercfg /requests 2>&1 | Out-String)
    $listed = $requests -match [regex]::Escape($PowerReason)
    New-Item -ItemType File -Force -Path (Join-Path $LogDir 'power-clear.flag') | Out-Null
    Start-Sleep -Seconds 2
    New-Item -ItemType File -Force -Path (Join-Path $LogDir 'power-quit.flag') | Out-Null
    if ($r.pid) { Wait-Process -Id $r.pid -Timeout 20 -ErrorAction SilentlyContinue }
    Write-Result "power$script:Sfx" ([ordered]@{
        set = $set; powercfg_requests = $requests; listed = [bool]$listed
        token = (Tool token); pass = [bool]$listed
    })
}

function Run-Exe([string]$Exe, [string]$PathValue) {
    $saved = $env:PATH
    $env:PATH = $PathValue
    try {
        $p = Start-Process -FilePath $Exe -ArgumentList '--test-threads', '1' -PassThru `
            -RedirectStandardOutput (Join-Path $Work 'rust-stdout.txt') -RedirectStandardError (Join-Path $Work 'rust-stderr.txt')
        if (-not $p.WaitForExit(60000)) { $p.Kill(); return 'timeout' }
        return $p.ExitCode
    } catch {
        return "start failed: $($_.Exception.Message)"
    } finally {
        $env:PATH = $saved
    }
}

function Item-Rust {
    $exe = Join-Path $In 'rusttest\winshell-tests.exe'
    $base = $env:PATH
    $with = Run-Exe $exe ((Join-Path $In 'vcrt') + ';' + $base)
    $without = Run-Exe $exe $base
    $sys = @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll') | ForEach-Object {
        [ordered]@{ dll = $_; in_system32 = (Test-Path (Join-Path $env:windir "System32\$_")) }
    }
    Write-Result "rust$script:Sfx" ([ordered]@{ exit_with_dlls = $with; exit_without_dlls = $without; system32 = $sys; pass = ($with -eq 0) })
}

# Distinct colours over a 64 x 64 sample grid of a PNG, and whether it is more than one colour.
function Get-GrabStats([string]$Png) {
    if (-not (Test-Path $Png)) { return [ordered]@{ png = $Png; exists = $false; nonflat = $false } }
    Add-Type -AssemblyName System.Drawing
    $bmp = [System.Drawing.Bitmap]::FromFile($Png)
    try {
        $seen = @{}
        for ($y = 0; $y -lt 64; $y++) {
            for ($x = 0; $x -lt 64; $x++) {
                $c = $bmp.GetPixel([int]($x * ($bmp.Width - 1) / 63), [int]($y * ($bmp.Height - 1) / 63))
                $seen[$c.ToArgb()] = $true
            }
        }
        [ordered]@{ png = $Png; exists = $true; width = $bmp.Width; height = $bmp.Height; distinct_colours = $seen.Count; nonflat = ($seen.Count -gt 1) }
    } finally { $bmp.Dispose() }
}

function Get-Backend([string]$Text) {
    $m = [regex]::Match($Text, '(?i)backend\W+(D3D11|D3D12|Vulkan|OpenGL|Metal|Null|Software)')
    if ($m.Success) { $m.Groups[1].Value } else { $null }
}

function Start-Qt([string]$Exe, [string]$Arguments, [string]$Name) {
    $err = Join-Path $Work "$Name-stderr.txt"
    $env:QSG_INFO = '1'
    $env:QT_FORCE_STDERR_LOGGING = '1'
    $sp = @{ FilePath = $Exe; PassThru = $true; RedirectStandardError = $err; RedirectStandardOutput = (Join-Path $Work "$Name-stdout.txt") }
    if ($Arguments) { $sp.ArgumentList = $Arguments }
    $p = Start-Process @sp
    [pscustomobject]@{ Process = $p; Stderr = $err }
}

function Item-Qt([string]$Name) {
    $deploy = $Config.deploy_dir
    if (-not $deploy -or -not (Test-Path $deploy)) {
        Write-Result $Name ([ordered]@{ pass = $false; error = "deploy folder not mapped: $deploy" })
        return
    }
    $exe = Join-Path $deploy $Config.qt_exe
    # Offscreen: the app grabs its own window (QT_QPA_PLATFORM=offscreen).
    $offPng = Join-Path $Work "$Name-offscreen.png"
    Remove-Item $offPng -ErrorAction SilentlyContinue
    $env:QT_QPA_PLATFORM = 'offscreen'
    $q = Start-Qt $exe ($Config.qt_grab_args -replace '\{png\}', $offPng) "$Name-offscreen"
    if (-not $q.Process.WaitForExit(60000)) { $q.Process.Kill() }
    Remove-Item Env:\QT_QPA_PLATFORM
    $offErr = $(if (Test-Path $q.Stderr) { Get-Content -Raw $q.Stderr } else { '' })
    $off = Get-GrabStats $offPng
    $off.exit = $(if ($q.Process.HasExited) { $q.Process.ExitCode } else { 'killed' })
    $off.backend = Get-Backend $offErr
    # On the Sandbox's own screen: a visible window, captured from the screen.
    $onPng = Join-Path $Work "$Name-onscreen.png"
    $q2 = Start-Qt $exe '' "$Name-onscreen"
    Start-Sleep -Seconds 10
    $q2.Process.Refresh()
    $hwnd = $q2.Process.MainWindowHandle
    $rect = New-Object Spike.Native+RECT
    $on = [ordered]@{ hwnd = [int64]$hwnd }
    if ($hwnd -ne [IntPtr]::Zero -and [Spike.Native]::GetWindowRect($hwnd, [ref]$rect)) {
        Add-Type -AssemblyName System.Drawing
        $w = $rect.Right - $rect.Left; $h = $rect.Bottom - $rect.Top
        $bmp = New-Object System.Drawing.Bitmap $w, $h
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
        $bmp.Save($onPng, [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        $on = Get-GrabStats $onPng
        $on.hwnd = [int64]$hwnd
    } else {
        $on.nonflat = $false
        $on.error = 'no main window'
    }
    if (-not $q2.Process.HasExited) { $q2.Process.Kill() }
    Start-Sleep -Seconds 1
    $onErr = $(if (Test-Path $q2.Stderr) { Get-Content -Raw $q2.Stderr } else { '' })
    $on.backend = Get-Backend $onErr
    foreach ($png in $offPng, $onPng) { if (Test-Path $png) { Copy-Item $png $Out -Force } }
    Write-Result $Name ([ordered]@{
        exe = $exe; offscreen = $off; onscreen = $on
        rhi_backend = $(if ($on.backend) { $on.backend } else { $off.backend })
        stderr_offscreen = $offErr; stderr_onscreen = $onErr
        pass = ([bool]$off.nonflat -and [bool]$on.nonflat)
    })
}

function Run-Items([string[]]$List, [string]$Sfx) {
    $script:Sfx = $Sfx
    foreach ($item in $List) {
        try {
            switch ($item) {
                'elevation' { Item-Elevation }
                'token' { Item-Token }
                'toast' { Write-Result "toast$Sfx" (Item-Toast "sbx-$($Config.run)$Sfx") }
                'protocol' { Item-Protocol }
                'foreground' { Item-Foreground "foreground$Sfx" }
                'power' { Item-Power }
                'rust' { Item-Rust }
                'qt' { Item-Qt "qt$Sfx" }
            }
        } catch {
            Write-Result "$item$Sfx" ([ordered]@{ pass = $false; error = $_.Exception.Message; at = "$($_.InvocationInfo.ScriptLineNumber)" })
        }
    }
}

Run-Items $Items $RunSuffix

if ($RunSuffix -eq '' -and $Config.observe) {
    # Host-state observation: the host writes go-2.flag (Sandbox window behind other windows),
    # then go-3.flag (host about to be locked; wait 30 s first).
    foreach ($round in 2, 3) {
        if (Wait-File (Join-Path $Out "go-$round.flag") 1200) {
            if ($round -eq 3) { Start-Sleep -Seconds 30 }
            Run-Items @('toast', 'foreground', 'qt') "-r$round"
            Write-Result "round-$round" @{ at = Now }
        }
    }
}

$doneName = "done$RunSuffix"
Write-Result $doneName ([ordered]@{ at = Now; items = $Items; seconds = [int]((Get-Date) - $T0).TotalSeconds })

if ($RunSuffix -eq '' -and -not $NoShutdown) { & shutdown /s /t 0 }
