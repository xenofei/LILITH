<#
.SYNOPSIS
lil dev desktop-tests (QA-042; P4a-R31, P4a-R35): runs inside Windows Sandbox as the logon command
  powershell.exe -ExecutionPolicy Bypass -File C:\lil\in\run-inside.ps1
Written for Windows PowerShell 5.1: the Sandbox image has no pwsh.

.DESCRIPTION
C:\lil\in (read-only) is the bundle the runner built: this script, config.json, in-sandbox.marker
and vcrt\ (the three VC++ runtime DLLs); for the desktop suite also cargo-nextest.exe,
desktop.tar.zst (the nextest archive), src\ (the tracked source files, for --workspace-remap),
testbins\ (the test kit's binaries) and nextest-sandbox.toml; for qml and grabs ctest.json.
C:\lil\deploy (read-only) is the deploy folder, when there is one. C:\lil\out (writable) gets
started.json at once, then the JUnit files, run-inside.log and done.json
{suite, exit, tests, passed, failed, nextest_version}. Then the Sandbox shuts itself down,
unless config.json says keep_open.

desktop: copies src\ to C:\lil\work\src (nextest's store must be writable), creates
C:\lil\work\bin (--extract-to fails on a missing folder) and runs the archive's ignored
desktop_* tests, without the host set (the toast and foreground tests, P4a-R39: they run on
the PC with `lil dev desktop-tests --host`, because every token in the Sandbox is elevated).
qml and grabs: runs each ctest.json command with Qt's bin on PATH, without QT_QPA_PLATFORM, with
`-o C:\lil\out\<test>.xml,junitxml`, and with LIL_GRAB_DIR=C:\lil\out\grabs (the build tree is
read-only).

It refuses to run anywhere but inside Windows Sandbox, since it ends with shutdown /s /t 0: the
user must be WDAGUtilityAccount, the Sandbox's account, and the bundle's marker file must be
mapped in. Otherwise it exits 2 before doing anything.
#>

# The guard comes first: nothing above it has any effect.
if ($env:USERNAME -ne 'WDAGUtilityAccount' -or -not (Test-Path -LiteralPath 'C:\lil\in\in-sandbox.marker')) {
    [Console]::Error.WriteLine('run-inside.ps1 runs only inside Windows Sandbox (user WDAGUtilityAccount and C:\lil\in\in-sandbox.marker); nothing was done.')
    exit 2
}

$ErrorActionPreference = 'Continue'
$In = 'C:\lil\in'
$Out = 'C:\lil\out'
$Work = 'C:\lil\work'
$Log = Join-Path $Out 'run-inside.log'

function Write-Json([string]$Name, $Obj) {
    $Obj | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 -LiteralPath (Join-Path $Out $Name)
}

function Say([string]$Text) {
    Add-Content -Encoding UTF8 -LiteralPath $Log -Value $Text
    Write-Host $Text
}

# Runs a native command with its output in the log and on the Sandbox's console; its exit code.
# Each stderr line arrives as an error record and is written as its plain text.
function Invoke-Logged([string]$Exe, [string[]]$Arguments) {
    Say ("> {0} {1}" -f $Exe, ($Arguments -join ' '))
    & $Exe @Arguments 2>&1 | ForEach-Object { Say "$_" }
    $LASTEXITCODE
}

# The test cases of a JUnit file: {total, failed}. A case is failed when it has a <failure> or
# <error> child.
function Get-JunitCounts([string]$Path) {
    $doc = [xml](Get-Content -Raw -LiteralPath $Path)
    $cases = @($doc.SelectNodes('//testcase'))
    $failed = @($cases | Where-Object { $_.SelectSingleNode('failure|error') }).Count
    [pscustomobject]@{ total = $cases.Count; failed = $failed }
}

Write-Json 'started.json' ([ordered]@{ started = (Get-Date).ToUniversalTime().ToString('o') })

$config = Get-Content -Raw -LiteralPath (Join-Path $In 'config.json') | ConvertFrom-Json
$done = [ordered]@{ suite = $config.suite; exit = $null; tests = 0; passed = 0; failed = 0; nextest_version = $null }

# SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX: a missing DLL fails the exe instead of
# showing a dialog that nobody can close; the setting is inherited by every child.
try {
    Add-Type -Name ErrorMode -Namespace LilSandbox -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);'
    [void][LilSandbox.ErrorMode]::SetErrorMode(0x0001 -bor 0x8000)
} catch { Say "SetErrorMode: $_" }

$env:LIL_SANDBOX = '1'
$env:LIL_DEPLOY_DIR = 'C:\lil\deploy'
$env:LIL_TESTBINS_DIR = Join-Path $In 'testbins'
$env:PATH = (Join-Path $In 'vcrt') + ';' + $env:PATH

try {
    if ($config.suite -eq 'desktop') {
        $Nextest = Join-Path $In 'cargo-nextest.exe'
        $done.nextest_version = ((& $Nextest nextest --version 2>&1 | Select-Object -First 1) | Out-String).Trim()
        # The remapped workspace must be writable for nextest's store, and --extract-to fails on
        # a missing folder.
        New-Item -ItemType Directory -Force -Path $Work | Out-Null
        Copy-Item -LiteralPath (Join-Path $In 'src') -Destination (Join-Path $Work 'src') -Recurse -Force
        New-Item -ItemType Directory -Force -Path (Join-Path $Work 'bin') | Out-Null
        # The desktop group without the host set (P4a-R39); the runner's unit tests check that
        # this is the filter it builds.
        $Filter = 'test(/^desktop_/) - (test(=desktop_toast_posts_under_the_shortcut_aumid) | test(=desktop_toasts_state_is_ready_on_this_pc) | test(=desktop_second_launch_comes_to_front) | test(=desktop_toast_url_brings_the_confirm_window_to_front))'
        $done.exit = Invoke-Logged $Nextest @(
            'nextest', 'run',
            '--archive-file', (Join-Path $In 'desktop.tar.zst'),
            '--workspace-remap', 'C:\lil\work\src',
            '--extract-to', 'C:\lil\work\bin',
            '--config-file', (Join-Path $In 'nextest-sandbox.toml'),
            '--profile', 'desktop',
            '--run-ignored', 'only',
            '-E', $Filter)
        $junit = 'C:\lil\work\src\target\nextest\desktop\junit.xml'
        if (Test-Path -LiteralPath $junit) {
            Copy-Item -LiteralPath $junit -Destination $Out
            $c = Get-JunitCounts $junit
            $done.tests = $c.total; $done.failed = $c.failed; $done.passed = $c.total - $c.failed
        } else {
            Say "no JUnit file at $junit"
        }
    } else {
        # qml / grabs (P4a-R35): the CTest commands, unchanged, on the Sandbox's own screen.
        $env:PATH = (Join-Path $config.qt_dir 'bin') + ';' + $env:PATH
        Remove-Item Env:\QT_QPA_PLATFORM -ErrorAction SilentlyContinue
        $env:LIL_GRAB_DIR = Join-Path $Out 'grabs'
        New-Item -ItemType Directory -Force -Path $env:LIL_GRAB_DIR | Out-Null
        $ctest = Get-Content -Raw -LiteralPath (Join-Path $In 'ctest.json') | ConvertFrom-Json
        $worst = 0
        foreach ($t in @($ctest.tests)) {
            $cmd = @($t.command)
            if ($cmd.Count -eq 0) { continue }
            $props = @($t.properties)
            $wd = ($props | Where-Object { $_.name -eq 'WORKING_DIRECTORY' } | Select-Object -First 1).value
            $vars = @(($props | Where-Object { $_.name -eq 'ENVIRONMENT' } | Select-Object -First 1).value) |
                Where-Object { $_ -and $_ -notmatch '^QT_QPA_PLATFORM=' }
            $saved = @{}
            foreach ($v in $vars) {
                $k, $val = $v -split '=', 2
                $saved[$k] = [Environment]::GetEnvironmentVariable($k)
                [Environment]::SetEnvironmentVariable($k, $val)
            }
            $xml = Join-Path $Out ("{0}.xml" -f $t.name)
            $rest = @($cmd | Select-Object -Skip 1) + @('-o', "$xml,junitxml")
            if ($wd) { Push-Location -LiteralPath $wd }
            try { $code = Invoke-Logged $cmd[0] $rest } finally { if ($wd) { Pop-Location } }
            foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
            $done.tests++
            if ($code -eq 0) { $done.passed++ } else { $done.failed++; $worst = $code }
            if (-not (Test-Path -LiteralPath $xml)) { Say "no JUnit file from $($t.name) (exit $code)" }
        }
        $done.exit = $worst
    }
} catch {
    Say "run-inside.ps1 failed: $_"
    $done.error = "$_"
} finally {
    Write-Json 'done.json' $done
    if (-not $config.keep_open) { shutdown /s /t 0 }
}
