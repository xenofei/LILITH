#requires -Version 7.4
<#
.SYNOPSIS
Spike 12 (plan 4b Task 0): PDH counters, decode cost, facet queries and the spill margin.
Writes spikes/browse-core/results/checks.json.

.DESCRIPTION
Checks (docs/spikes/12-browse-core.md):
  pdh-read  S0.1's readability part: the three PDH counters from this (non-elevated) process
  decode    S0.2: decode, resize and encode cost, the header bomb, broken files, concurrency
  facets    S0.3: the page of 200 and six disjunctive facets at 10,000 and 50,000 assets
  pdh-load  S0.1's load part (owner says go): 1 GiB of STAGING buffers, a 30 s compute loop
  spill     S0.4 (owner says go): starts its own ComfyUI on 127.0.0.1 and measures the margin

Without -Only, the three checks that load nothing run (pdh-read, decode, facets). pdh-load and
spill run only when named.

Every binary is a console program in this console; no window opens. The script refuses to run
elevated. A check that runs again replaces only its own entry in results\checks.json; its
earlier raw folder (results\raw\<check>) is moved into results\run-<UTC time>\ first.
Generated inputs go to work\ and are removed after each check.

spill never changes the ComfyUI install: it only reads it. Its user, input, output, temp and
previews folders are in results\raw\spill\comfy\, custom nodes and the assets system are off,
Python writes no bytecode, and every flag is checked against the install's comfy\cli_args.py
first. Its children run in a kill-on-close job object that only this pwsh holds, so they end
whenever pwsh ends (an error, Ctrl+C, a closed console, a killed process); a finally block also
stops them in order. It refuses to start while anything listens on its port or on 127.0.0.1:8188,
or while a ComfyUI-like process runs.

.PARAMETER Only
The checks to run, for example -Only pdh-read,decode. An unknown name stops the script before
anything runs.

.PARAMETER DryRun
Prints what would run and starts nothing. For spill: the refusal checks' result, the exact
command line and the graphs' sizes.

.PARAMETER ComfyRoot
spill only: the ComfyUI folder (the one holding main.py).

.PARAMETER Python
spill only: the Python executable of that install.

.PARAMETER Port
spill only: the port for the owned ComfyUI (default 8199, not ComfyUI's 8188). The script
refuses to start if anything listens on it.

.PARAMETER Checkpoint
spill only: the SDXL checkpoint name, as ComfyUI's CheckpointLoaderSimple lists it.

.PARAMETER SelfTestJob
The job object's own check, with no ComfyUI: starts a stand-in in the job as ComfyUI would be
started, writes its PID to -SelfTestPidFile and waits to be killed. The stand-in is
`ping -t 127.0.0.1`, or with -Python that python.exe running a one-line script that starts a
grandchild python (standard library only; nothing of ComfyUI is imported or opened).
#>
param(
    [string[]]$Only = @(),
    [switch]$SkipBuild,
    [switch]$DryRun,
    [string]$ComfyRoot = '',
    [string]$Python = '',
    [int]$Port = 8199,
    [string]$Checkpoint = '',
    [int]$FitWidth = 1024,
    [int]$FitHeight = 1024,
    [int]$FitBatch = 1,
    [int]$FitSteps = 20,
    [int]$SpillWidth = 2048,
    [int]$SpillHeight = 2048,
    [int]$SpillBatch = 4,
    [int]$SpillSteps = 10,
    [int]$IdleSeconds = 60,
    [int]$RunTimeoutSeconds = 900,
    [string[]]$ComfyArgs = @(),
    [switch]$SelfTestJob,
    [string]$SelfTestPidFile = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# `pwsh -File` hands "a,b" over as one string, so names are split on commas here.
$KnownChecks = @('pdh-read', 'decode', 'facets', 'pdh-load', 'spill')
$Only = @($Only | ForEach-Object { "$_" -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$unknown = @($Only | Where-Object { $_ -notin $KnownChecks })
if ($unknown.Count -gt 0) {
    throw "Unknown check name in -Only: $($unknown -join ', '). The names are: $($KnownChecks -join ', '). Nothing was done."
}
if ($Only.Count -eq 0) { $Only = @('pdh-read', 'decode', 'facets') }

$Spike = $PSScriptRoot
$Bin = Join-Path $Spike 'target\release'
$Results = Join-Path $Spike 'results'
$RawRoot = Join-Path $Results 'raw'
$Work = Join-Path $Spike 'work'
$ChecksFile = Join-Path $Results 'checks.json'

function Say([string]$Text, [string]$Color = 'Cyan') { Write-Host $Text -ForegroundColor $Color }
function Want([string]$Id) { $Only -contains $Id }
function Exe([string]$Name) { Join-Path $Bin "$Name.exe" }

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'This shell is elevated. Run the spike from a normal (not elevated) shell: S0.1 measures what a non-elevated process can read.'
}

# ---------------------------------------------------------------- the report

$Report = [ordered]@{ spike = 12; checks = [ordered]@{} }
if (Test-Path $ChecksFile) {
    $old = Get-Content -Raw $ChecksFile | ConvertFrom-Json -AsHashtable
    if ($old -and $old.ContainsKey('checks')) { foreach ($k in $old.checks.Keys) { $Report.checks[$k] = $old.checks[$k] } }
}

function Save-Report {
    New-Item -ItemType Directory -Force -Path $Results | Out-Null
    $Report | ConvertTo-Json -Depth 12 | Set-Content -Encoding utf8 $ChecksFile
}

function Set-Check([string]$Id, [string]$Verdict, $Details) {
    $Report.checks[$Id] = [ordered]@{ verdict = $Verdict; utc = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'); details = $Details }
    Save-Report
    $color = switch -Wildcard ($Verdict) { 'PASS*' { 'Green' } 'FAIL*' { 'Red' } default { 'Gray' } }
    Say "$Id -> $Verdict" $color
}

# A fresh raw folder for a check; an earlier one moves into results\run-<UTC time>\.
function New-RawDir([string]$Id) {
    $dir = Join-Path $RawRoot $Id
    if (Test-Path $dir) {
        $stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
        $dest = Join-Path $Results "run-$stamp"
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        Move-Item $dir (Join-Path $dest $Id)
    }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $dir
}

function Invoke-Bin([string]$Name, [string[]]$Arguments) {
    & (Exe $Name) @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Name $($Arguments[0]) exited with $LASTEXITCODE" }
}

function Read-Json([string]$Path) { Get-Content -Raw $Path | ConvertFrom-Json }

# ---------------------------------------------------------------- S0.4's ComfyUI

# A kill-on-close job object that only this pwsh holds. Each child is created suspended,
# assigned to the job and only then resumed, so it and every process it starts are in the job
# from their first instruction (a child of the Store's packaged pwsh is created outside every
# job pwsh is in, so inheriting pwsh's own job wouldn't work; spike 10 found that). When this
# pwsh ends for any reason (an error, Ctrl+C, a closed console, TerminateProcess), Windows
# closes the handle and kills everything in the job. The orderly path still stops each child in
# `finally`.
$JobSource = @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

public static class SpikeJob {
    [StructLayout(LayoutKind.Sequential)]
    struct BasicLimit {
        public long PerProcessUserTimeLimit; public long PerJobUserTimeLimit; public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize; public UIntPtr MaximumWorkingSetSize; public uint ActiveProcessLimit;
        public UIntPtr Affinity; public uint PriorityClass; public uint SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct IoCounters { public ulong A, B, C, D, E, F; }
    [StructLayout(LayoutKind.Sequential)]
    struct ExtendedLimit {
        public BasicLimit Basic; public IoCounters Io; public UIntPtr ProcessMemoryLimit; public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed; public UIntPtr PeakJobMemoryUsed;
    }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct StartupInfo {
        public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct StartupInfoEx { public StartupInfo StartupInfo; public IntPtr lpAttributeList; }
    [StructLayout(LayoutKind.Sequential)]
    struct ProcessInformation { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateJobObjectW(IntPtr attributes, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetInformationJobObject(IntPtr job, int infoClass, ref ExtendedLimit info, uint length);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool CreateProcessW(string app, StringBuilder commandLine, IntPtr pa, IntPtr ta, bool inherit,
        uint flags, IntPtr environment, string cwd, ref StartupInfoEx si, out ProcessInformation pi);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attribute, IntPtr value, IntPtr size,
        IntPtr previous, IntPtr returnSize);
    [DllImport("kernel32.dll")]
    static extern void DeleteProcThreadAttributeList(IntPtr list);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);

    const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000;
    const int JobObjectExtendedLimitInformation = 9;
    const uint CREATE_SUSPENDED = 0x4;
    const uint CREATE_NO_WINDOW = 0x08000000;
    const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
    const int PROC_THREAD_ATTRIBUTE_HANDLE_LIST = 0x00020002;
    const int STARTF_USESTDHANDLES = 0x100;
    const uint HANDLE_FLAG_INHERIT = 1;

    static IntPtr job = IntPtr.Zero;

    public static void Create() {
        if (job != IntPtr.Zero) return;
        IntPtr j = CreateJobObjectW(IntPtr.Zero, null);
        if (j == IntPtr.Zero) throw new Win32Exception();
        ExtendedLimit info = new ExtendedLimit();
        info.Basic.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(j, JobObjectExtendedLimitInformation, ref info, (uint)Marshal.SizeOf(typeof(ExtendedLimit)))) {
            int e = Marshal.GetLastWin32Error(); CloseHandle(j); throw new Win32Exception(e);
        }
        job = j;
    }

    static IntPtr Inheritable(FileStream f) {
        IntPtr h = f.SafeFileHandle.DangerousGetHandle();
        if (!SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)) throw new Win32Exception();
        return h;
    }

    // Starts commandLine in cwd, suspended, with CREATE_NO_WINDOW (a windowless console that its
    // own console children share, so nothing opens a window whatever the host); assigns it to the
    // job, then resumes it. Its stdin is NUL, its stdout and stderr go to the two files, and those
    // three handles are the only ones it inherits (PROC_THREAD_ATTRIBUTE_HANDLE_LIST). Returns the
    // Process, opened before the resume, so a child that exits at once still has its exit code.
    public static Process Start(string commandLine, string cwd, string outPath, string errPath) {
        if (job == IntPtr.Zero) throw new InvalidOperationException("Create the job first.");
        using (FileStream nul = new FileStream("NUL", FileMode.Open, FileAccess.Read))
        using (FileStream o = new FileStream(outPath, FileMode.Create, FileAccess.Write, FileShare.ReadWrite))
        using (FileStream e = new FileStream(errPath, FileMode.Create, FileAccess.Write, FileShare.ReadWrite)) {
            IntPtr[] handles = new IntPtr[] { Inheritable(nul), Inheritable(o), Inheritable(e) };
            IntPtr size = IntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref size);
            IntPtr list = Marshal.AllocHGlobal(size);
            IntPtr handleArray = Marshal.AllocHGlobal(IntPtr.Size * handles.Length);
            bool listReady = false;
            try {
                if (!InitializeProcThreadAttributeList(list, 1, 0, ref size)) throw new Win32Exception();
                listReady = true;
                Marshal.Copy(handles, 0, handleArray, handles.Length);
                if (!UpdateProcThreadAttribute(list, 0, (IntPtr)PROC_THREAD_ATTRIBUTE_HANDLE_LIST, handleArray,
                        (IntPtr)(IntPtr.Size * handles.Length), IntPtr.Zero, IntPtr.Zero)) throw new Win32Exception();
                StartupInfoEx si = new StartupInfoEx();
                si.StartupInfo.cb = Marshal.SizeOf(typeof(StartupInfoEx));
                si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                si.StartupInfo.hStdInput = handles[0];
                si.StartupInfo.hStdOutput = handles[1];
                si.StartupInfo.hStdError = handles[2];
                si.lpAttributeList = list;
                ProcessInformation pi;
                if (!CreateProcessW(null, new StringBuilder(commandLine), IntPtr.Zero, IntPtr.Zero, true,
                        CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT, IntPtr.Zero, cwd, ref si, out pi))
                    throw new Win32Exception();
                try {
                    if (!AssignProcessToJobObject(job, pi.hProcess)) {
                        int err = Marshal.GetLastWin32Error();
                        TerminateProcess(pi.hProcess, 1);
                        throw new Win32Exception(err);
                    }
                    Process p = Process.GetProcessById(pi.dwProcessId);
                    IntPtr keep = p.Handle; // opened now, while the child can't have exited
                    if (ResumeThread(pi.hThread) == 0xFFFFFFFF) {
                        int err = Marshal.GetLastWin32Error();
                        TerminateProcess(pi.hProcess, 1);
                        throw new Win32Exception(err, "ResumeThread failed; the child was terminated");
                    }
                    return p;
                } finally {
                    CloseHandle(pi.hThread);
                    CloseHandle(pi.hProcess);
                }
            } finally {
                if (listReady) DeleteProcThreadAttributeList(list);
                Marshal.FreeHGlobal(list);
                Marshal.FreeHGlobal(handleArray);
            }
        }
    }}
'@

function Initialize-SpikeJob {
    if (-not ('SpikeJob' -as [type])) { Add-Type -TypeDefinition $JobSource -Language CSharp }
    [SpikeJob]::Create()
}

# Starts an executable in the job (see above) and returns its Process object. A child that has
# already exited a second later is an error, with its exit code and its stderr log named.
function Start-InJob([string]$Exe, [string[]]$Arguments, [string]$Cwd, [string]$OutLog, [string]$ErrLog) {
    $line = "$(Format-CommandLine @($Exe)) $(Format-CommandLine $Arguments)"
    $p = [SpikeJob]::Start($line, $Cwd, $OutLog, $ErrLog)
    if ($p.WaitForExit(1000)) {
        throw "$(Split-Path -Leaf $Exe) exited at once with code $($p.ExitCode); see $ErrLog."
    }
    $p
}

# The flags the owned ComfyUI gets; each must exist in the install's comfy/cli_args.py, so a
# ComfyUI that dropped one fails before anything starts instead of writing into the install.
$RequiredComfyFlags = @('--listen', '--port', '--disable-auto-launch', '--disable-all-custom-nodes', '--disable-assets',
    '--user-directory', '--input-directory', '--output-directory', '--temp-directory', '--previews-directory')

function Get-SpillPlan {
    if (-not $ComfyRoot -or -not $Python) { throw 'spill needs -ComfyRoot <ComfyUI folder> and -Python <python.exe>.' }
    $root = (Resolve-Path -LiteralPath $ComfyRoot).Path.TrimEnd('\')
    $py = (Resolve-Path -LiteralPath $Python).Path
    if (-not (Test-Path -LiteralPath (Join-Path $root 'main.py'))) { throw "No main.py in the ComfyUI folder given." }
    $runDir = Join-Path $RawRoot 'spill'
    $comfy = Join-Path $runDir 'comfy'
    $arguments = @('main.py', '--listen', '127.0.0.1', '--port', "$Port", '--disable-auto-launch', '--disable-all-custom-nodes', '--disable-assets',
        '--user-directory', (Join-Path $comfy 'user'), '--input-directory', (Join-Path $comfy 'input'),
        '--output-directory', (Join-Path $comfy 'output'), '--temp-directory', (Join-Path $comfy 'temp'),
        '--previews-directory', (Join-Path $comfy 'previews')) + $ComfyArgs
    [ordered]@{ root = $root; python = $py; run_dir = $runDir; comfy = $comfy; arguments = $arguments }
}

# Whether anything listens on 127.0.0.1:$P: the listener table, and a connect as well (a
# sandboxed or filtered view can miss a listener).
function Test-PortTaken([int]$P) {
    $listeners = @([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners() | Where-Object { $_.Port -eq $P })
    $answers = $false
    $client = [Net.Sockets.TcpClient]::new()
    try { $answers = $client.ConnectAsync('127.0.0.1', $P).Wait(1000) -and $client.Connected } catch { $answers = $false } finally { $client.Dispose() }
    $listeners.Count -gt 0 -or $answers
}

# Why the owned ComfyUI must not start:
# - something listens on the chosen port, or on ComfyUI's default 127.0.0.1:8188 (the owner's
#   own ComfyUI would share the GPU and invalidate the measurement);
# - a ComfyUI already runs: a process from the install's folder, one naming the folder on its
#   command line, the install's Python running a main.py, or any python*.exe running a main.py
#   (conservative: the GPU should be free anyway);
# - the install's cli_args.py lacks one of the flags the script relies on.
# An elevated process hides its path and command line, so it can't be seen here.
function Get-SpillRefusals($Plan) {
    $why = @()
    if (Test-PortTaken $Port) { $why += "something already listens on port $Port" }
    if ($Port -ne 8188 -and (Test-PortTaken 8188)) { $why += 'something listens on 127.0.0.1:8188 (ComfyUI''s default port): another ComfyUI would share the GPU' }
    $cmp = [StringComparison]::OrdinalIgnoreCase
    $procs = @(Get-CimInstance Win32_Process | Where-Object {
            $cl = "$($_.CommandLine)"
            $_.ProcessId -ne $PID -and -not ($cl.IndexOf('run-spike.ps1', $cmp) -ge 0) -and (
                ("$($_.ExecutablePath)".StartsWith($Plan.root + '\', $cmp)) -or
                ($cl.IndexOf($Plan.root, $cmp) -ge 0) -or
                ("$($_.ExecutablePath)" -ieq $Plan.python -and $cl -match 'main\.py') -or
                ("$($_.Name)" -like 'python*.exe' -and $cl -match 'main\.py'))
        })
    if ($procs.Count -gt 0) { $why += "$($procs.Count) ComfyUI-like process(es) already run ($(@($procs | ForEach-Object { "$($_.Name) $($_.ProcessId)" }) -join ', '))" }
    $argsFile = Join-Path $Plan.root 'comfy\cli_args.py'
    if (-not (Test-Path -LiteralPath $argsFile)) { $why += 'the install has no comfy\cli_args.py to check the flags against' }
    else {
        $text = Get-Content -Raw -LiteralPath $argsFile
        $missing = @($RequiredComfyFlags | Where-Object { -not $text.Contains("`"$_`"") })
        if ($missing.Count -gt 0) { $why += "this ComfyUI lacks the flag(s) $($missing -join ', ')" }
    }
    $why
}

# The spike's own API-format graph: an SDXL text-to-image at the given size.
function New-Graph([int]$Width, [int]$Height, [int]$Batch, [int]$Steps, [long]$Seed) {
    [ordered]@{
        '1' = @{ class_type = 'CheckpointLoaderSimple'; inputs = @{ ckpt_name = $Checkpoint } }
        '2' = @{ class_type = 'CLIPTextEncode'; inputs = @{ text = 'a lighthouse on a rocky cliff at dusk, waves, detailed oil painting'; clip = @('1', 1) } }
        '3' = @{ class_type = 'CLIPTextEncode'; inputs = @{ text = 'blurry, low quality'; clip = @('1', 1) } }
        '4' = @{ class_type = 'EmptyLatentImage'; inputs = @{ width = $Width; height = $Height; batch_size = $Batch } }
        '5' = @{ class_type = 'KSampler'; inputs = @{ seed = $Seed; steps = $Steps; cfg = 6.0; sampler_name = 'euler'; scheduler = 'normal'; denoise = 1.0
                model = @('1', 0); positive = @('2', 0); negative = @('3', 0); latent_image = @('4', 0) } }
        '6' = @{ class_type = 'VAEDecode'; inputs = @{ samples = @('5', 0); vae = @('1', 2) } }
        '7' = @{ class_type = 'SaveImage'; inputs = @{ filename_prefix = 'spike12'; images = @('6', 0) } }
    }
}

function Format-CommandLine([string[]]$Arguments) {
    ($Arguments | ForEach-Object { if ($_ -match '[\s"]' -or $_ -eq '') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
}

function Get-Prop($Object, [string]$Name) { $p = $Object.PSObject.Properties[$Name]; if ($p) { $p.Value } }

function Get-UnixMs { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }

# Polls GET /queue until nothing runs or waits, for at most $Seconds (after an interrupt).
function Wait-QueueEmpty([string]$Base, [int]$Seconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        try {
            $q = Invoke-RestMethod -Uri "$Base/queue" -TimeoutSec 10
            if (@($q.queue_running).Count -eq 0 -and @($q.queue_pending).Count -eq 0) { return $true }
        } catch { }
        Start-Sleep -Seconds 1
    }
    $false
}

# Queues one graph and waits until /history has it, or the timeout (then /interrupt, a queue
# delete and a wait for the queue to empty). A poll that fails is retried; 30 failures in a
# row end the run as `unreachable`.
function Invoke-Graph([string]$Base, $Graph, [string]$Kind, [int]$Index, [string]$GraphFile, $Proc) {
    $Graph | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 $GraphFile
    $start = Get-UnixMs
    $body = @{ prompt = $Graph; client_id = [guid]::NewGuid().ToString() } | ConvertTo-Json -Depth 8
    $queued = Invoke-RestMethod -Method Post -Uri "$Base/prompt" -Body $body -ContentType 'application/json' -TimeoutSec 30
    $id = $queued.prompt_id
    $state = 'timeout'
    $failures = 0
    $deadline = [DateTime]::UtcNow.AddSeconds($RunTimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Proc.HasExited) { throw "ComfyUI exited during $Kind run $Index (exit $($Proc.ExitCode))." }
        Start-Sleep -Milliseconds 1000
        try {
            $h = Invoke-RestMethod -Uri "$Base/history/$id" -TimeoutSec 10
            $failures = 0
        } catch {
            $failures++
            if ($failures -ge 30) { $state = 'unreachable'; break }
            continue
        }
        if ($h.PSObject.Properties[$id]) {
            $st = $h.$id.status
            $state = if ($st -and $st.PSObject.Properties['status_str']) { $st.status_str } else { 'done' }
            break
        }
    }
    $drained = $null
    if ($state -in 'timeout', 'unreachable') {
        try { Invoke-RestMethod -Method Post -Uri "$Base/interrupt" -TimeoutSec 10 | Out-Null } catch { }
        try { Invoke-RestMethod -Method Post -Uri "$Base/queue" -Body (@{ delete = @($id) } | ConvertTo-Json) -ContentType 'application/json' -TimeoutSec 10 | Out-Null } catch { }
        $drained = Wait-QueueEmpty $Base 60
    }
    $end = Get-UnixMs
    Say ("  {0} run {1}: {2} in {3:N0} s" -f $Kind, $Index, $state, (($end - $start) / 1000))
    [ordered]@{ kind = $Kind; index = $Index; start_ms = $start; end_ms = $end; state = $state; seconds = ($end - $start) / 1000; queue_drained = $drained }
}

function Add-IdlePhase($Phases, [int]$Index) {
    Say "Idle for $IdleSeconds s (idle $Index)"
    $s = Get-UnixMs
    Start-Sleep -Seconds $IdleSeconds
    $Phases.Add([ordered]@{ kind = 'idle'; index = $Index; start_ms = $s; end_ms = (Get-UnixMs); state = 'done' })
}

function Invoke-Spill {
    $plan = Get-SpillPlan
    $refusals = @(Get-SpillRefusals $plan)
    $commandLine = "$(Format-CommandLine @($plan.python)) $(Format-CommandLine $plan.arguments)"
    if ($DryRun) {
        [ordered]@{
            check             = 'spill'
            would_start       = $commandLine
            working_directory = $plan.root
            environment       = @{ PYTHONDONTWRITEBYTECODE = '1'; PYTHONUNBUFFERED = '1' }
            job_object        = 'kill-on-close, held only by this pwsh; each child created suspended, assigned, resumed'
            refusals          = $refusals
            would_refuse      = ($refusals.Count -gt 0)
            checkpoint        = $(if ($Checkpoint) { $Checkpoint } else { '<not given: required for a real run>' })
            graphs            = [ordered]@{ fit = "$FitWidth x $FitHeight, batch $FitBatch, $FitSteps steps"; spill = "$SpillWidth x $SpillHeight, batch $SpillBatch, $SpillSteps steps" }
            phases            = "idle 0 ($IdleSeconds s), 3 fitting runs, idle 1 ($IdleSeconds s), 3 spilling runs (timeout $RunTimeoutSeconds s each)"
        } | ConvertTo-Json -Depth 4
        return
    }
    if (-not $Checkpoint) { throw 'spill needs -Checkpoint <SDXL checkpoint name>.' }
    if ($refusals.Count -gt 0) { throw "Refused, nothing was started: $($refusals -join '; ')." }

    $runDir = New-RawDir 'spill'
    foreach ($sub in 'user', 'input', 'output', 'temp', 'previews') { New-Item -ItemType Directory -Force -Path (Join-Path $plan.comfy $sub) | Out-Null }
    $graphs = Join-Path $runDir 'graphs'
    New-Item -ItemType Directory -Force -Path $graphs | Out-Null
    $samples = Join-Path $runDir 'samples.jsonl'
    $stopFile = Join-Path $runDir 'stop'
    $base = "http://127.0.0.1:$Port"
    $proc = $null
    $probe = $null
    $phases = [System.Collections.Generic.List[object]]::new()
    Initialize-SpikeJob
    try {
        Say "Starting the owned ComfyUI on 127.0.0.1:$Port"
        $saved = @{ B = $env:PYTHONDONTWRITEBYTECODE; U = $env:PYTHONUNBUFFERED }
        $env:PYTHONDONTWRITEBYTECODE = '1'; $env:PYTHONUNBUFFERED = '1'
        try {
            $proc = Start-InJob $plan.python $plan.arguments $plan.root (Join-Path $runDir 'comfy-out.log') (Join-Path $runDir 'comfy-err.log')
        } finally {
            $env:PYTHONDONTWRITEBYTECODE = $saved.B; $env:PYTHONUNBUFFERED = $saved.U
        }
        $probe = Start-InJob (Exe 'spill-probe') @('sample', '--root-pid', "$($proc.Id)", '--out', $samples, '--stop-file', $stopFile) $runDir `
            (Join-Path $runDir 'probe-out.log') (Join-Path $runDir 'probe-err.log')
        # The probe samples once a second from its start; no sample within 15 s fails the run now
        # rather than as INVALID after half an hour.
        $sampleDeadline = [DateTime]::UtcNow.AddSeconds(15)
        while (-not ((Test-Path -LiteralPath $samples) -and (Get-Item -LiteralPath $samples).Length -gt 0)) {
            if ($probe.HasExited -or [DateTime]::UtcNow -ge $sampleDeadline) {
                throw "spill-probe wrote no sample within 15 s (exited: $($probe.HasExited)); see results\raw\spill\probe-err.log."
            }
            Start-Sleep -Milliseconds 500
        }
        $ready = $false
        $deadline = [DateTime]::UtcNow.AddSeconds(300)
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($proc.HasExited) { throw "ComfyUI exited while starting (exit $($proc.ExitCode)); see results\raw\spill\comfy-err.log." }
            try { Invoke-RestMethod -Uri "$base/system_stats" -TimeoutSec 5 | Out-Null; $ready = $true; break } catch { Start-Sleep -Seconds 2 }
        }
        if (-not $ready) { throw 'ComfyUI did not answer /system_stats within 300 s.' }
        $info = Invoke-RestMethod -Uri "$base/object_info/CheckpointLoaderSimple" -TimeoutSec 30
        $combo = $info.CheckpointLoaderSimple.input.required.ckpt_name
        $names = if ($combo[0] -is [array]) { $combo[0] } elseif ($combo.Count -gt 1 -and $combo[1].PSObject.Properties['options']) { $combo[1].options } else { @() }
        if ($Checkpoint -notin $names) { throw "ComfyUI doesn't list the checkpoint given ($($names.Count) listed)." }

        Add-IdlePhase $phases 0
        $seed = 1200
        foreach ($i in 1..3) {
            $g = New-Graph $FitWidth $FitHeight $FitBatch $FitSteps ($seed + $i)
            $phases.Add((Invoke-Graph $base $g 'fit' $i (Join-Path $graphs "fit-$i.json") $proc))
            Start-Sleep -Seconds 5
        }
        # Recorded: the warm idle, with the model resident (P4b-R37's idle in production).
        Add-IdlePhase $phases 1
        foreach ($i in 1..3) {
            $g = New-Graph $SpillWidth $SpillHeight $SpillBatch $SpillSteps ($seed + 10 + $i)
            $phases.Add((Invoke-Graph $base $g 'spill' $i (Join-Path $graphs "spill-$i.json") $proc))
            Start-Sleep -Seconds 5
        }
    } finally {
        New-Item -ItemType File -Force -Path $stopFile | Out-Null
        if ($probe) { if (-not $probe.WaitForExit(5000)) { $probe.Kill($true) } }
        if ($proc -and -not $proc.HasExited) {
            Say 'Stopping the owned ComfyUI (the whole process tree)'
            $proc.Kill($true)
            [void]$proc.WaitForExit(30000)
        }
        # The port can still answer for a moment while its socket closes (Step 3 saw that once), so
        # the check after the stop retries for up to 10 s before it reports anything left.
        $left = @()
        foreach ($try in 1..5) {
            $left = @(Get-SpillRefusals $plan | Where-Object { $_ -notlike '*lacks the flag*' })
            if ($left.Count -eq 0) { break }
            Start-Sleep -Seconds 2
        }
        if ($left.Count -gt 0) { Say "After the stop (checked 5 times over 10 s): $($left -join '; ')" 'Red' }
    }
    $phasesFile = Join-Path $runDir 'phases.json'
    ConvertTo-Json -InputObject @($phases) -Depth 4 | Set-Content -Encoding utf8 $phasesFile
    Invoke-Bin 'spill-probe' @('analyze', '--samples', $samples, '--phases', $phasesFile, '--out', (Join-Path $runDir 'result.json'))
    $r = Read-Json (Join-Path $runDir 'result.json')
    $states = @($phases | Where-Object { $_.kind -ne 'idle' } | ForEach-Object { "$($_.kind)$($_.index)=$($_.state)" })
    # A run that didn't succeed makes its level meaningless: the analysis stays in the details.
    $verdict = if (@($phases | Where-Object { $_.kind -ne 'idle' -and $_.state -ne 'success' }).Count -gt 0) {
        "INVALID (not every run succeeded: $($states -join ', '))"
    } else { $r.verdict }
    Set-Check 'spill' $verdict ([ordered]@{
            analysis_verdict = $r.verdict; idle_mib = $r.idle_mib; fit = $r.fit; spill = $r.spill; gap = $r.gap; margin_mib = $r.margin_mib; margin_midpoint_mib = (Get-Prop $r 'margin_midpoint_mib'); why = $r.why
            warm_idle_mib = (Get-Prop $r 'warm_idle_mib'); margin_vs_warm_idle_mib = (Get-Prop $r 'margin_vs_warm_idle_mib')
            runs = $phases; graphs = [ordered]@{ fit = "$FitWidth x $FitHeight b$FitBatch s$FitSteps"; spill = "$SpillWidth x $SpillHeight b$SpillBatch s$SpillSteps" }
            adapter_shared = $r.adapter_shared; process_dedicated = $r.process_dedicated
        })
}

# The job object's own check (no ComfyUI): starts a stand-in in the job exactly as ComfyUI is
# started, writes its PID to $SelfTestPidFile and waits to be killed. Whoever runs it kills this
# pwsh and checks that the stand-in and everything it started are gone.
# - Without -Python: `ping -t 127.0.0.1`.
# - With -Python (the launcher's shape): that python.exe runs a one-line script that starts a
#   grandchild `python -c` sleeping 600 s. Both write their PID and their console window's handle
#   and visibility (GetConsoleWindow, IsWindowVisible) to child.txt and grandchild.txt beside the
#   PID file. Nothing is imported but the standard library, and the working folder is the PID
#   file's folder.
function Invoke-JobSelfTest {
    Initialize-SpikeJob
    $dir = Split-Path -Parent $SelfTestPidFile
    if ($Python) {
        $py = (Resolve-Path -LiteralPath $Python).Path
        $report = "import ctypes, os; h = ctypes.windll.kernel32.GetConsoleWindow(); v = ctypes.windll.user32.IsWindowVisible(h) if h else 0"
        $grand = "$report; open(r'$(Join-Path $dir 'grandchild.txt')', 'w').write('%d %d %d' % (os.getpid(), h, v)); import time; time.sleep(600)"
        $code = "$report; import subprocess, sys, time; c = subprocess.Popen([sys.executable, '-c', r`"$grand`"]); open(r'$(Join-Path $dir 'child.txt')', 'w').write('%d %d %d %d' % (os.getpid(), h, v, c.pid)); time.sleep(600)"
        $stand = Start-InJob $py @('-c', $code) $dir (Join-Path $dir 'python-out.log') (Join-Path $dir 'python-err.log')
    } else {
        $stand = Start-InJob (Join-Path $env:SystemRoot 'System32\PING.EXE') @('-t', '127.0.0.1') $dir (Join-Path $dir 'ping-out.log') (Join-Path $dir 'ping-err.log')
    }
    Set-Content -LiteralPath $SelfTestPidFile -Value $stand.Id
    Start-Sleep -Seconds 300
}

# ---------------------------------------------------------------- the run

if ($SelfTestJob) {
    if (-not $SelfTestPidFile) { throw '-SelfTestJob needs -SelfTestPidFile <path>.' }
    Invoke-JobSelfTest
    return
}

if ($DryRun) {
    foreach ($id in $Only) {
        if ($id -eq 'spill') { Invoke-Spill } else { Say "would run $id" }
    }
    return
}

if (-not $SkipBuild) {
    Say 'Building (release)'
    cargo build --release --locked --manifest-path (Join-Path $Spike 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
}

if (Want 'pdh-read') {
    Say '==== pdh-read (S0.1, readability)' 'Yellow'
    $d = New-RawDir 'pdh-read'
    Invoke-Bin 'pdh-probe' @('read', '--out', (Join-Path $d 'read.json'))
    $r = Read-Json (Join-Path $d 'read.json')
    $grammar = [ordered]@{}
    foreach ($p in $r.counters.PSObject.Properties) { $grammar[$p.Name] = @($p.Value.grammar_discrete.PSObject.Properties.Name) }
    Set-Check 'pdh-read' $r.verdict ([ordered]@{
            elevated = $r.elevated; add = $r.add; all_three_arrive_every_collect = $r.all_three_arrive_every_collect
            collect_ms = $r.collect_ms; read_arrays_ms = $r.read_arrays_ms; grammar = $grammar
        })
}

if (Want 'decode') {
    Say '==== decode (S0.2)' 'Yellow'
    $d = New-RawDir 'decode'
    $w = Join-Path $Work 'decode'
    try {
        Invoke-Bin 'decode-bench' @('gen', '--work', $w)
        Invoke-Bin 'decode-bench' @('timing', '--work', $w, '--out', (Join-Path $d 'timing.json'))
        Invoke-Bin 'decode-bench' @('safety', '--work', $w, '--out', (Join-Path $d 'safety.json'))
        Invoke-Bin 'decode-bench' @('concurrent', '--work', $w, '--out', (Join-Path $d 'concurrent.json'))
        Invoke-Bin 'decode-bench' @('cold', '--work', $w, '--out', (Join-Path $d 'cold.json'))
    } finally {
        if (Test-Path $w) { Remove-Item -Recurse -Force $w }
    }
    $t = Read-Json (Join-Path $d 'timing.json'); $s = Read-Json (Join-Path $d 'safety.json')
    $c = Read-Json (Join-Path $d 'concurrent.json'); $k = Read-Json (Join-Path $d 'cold.json')
    $verdict = if ($t.pass -and $s.bomb.pass -and $s.broken.pass -and $c.pass) { 'PASS' } else { 'FAIL' }
    Set-Check 'decode' $verdict ([ordered]@{
            png_1024_to_256_p95_ms = $t.png_1024_to_256_p95_ms
            cases = @($t.cases | ForEach-Object { [ordered]@{ input = $_.input; px = $_.px; total_ms = $_.total_ms } })
            bomb = [ordered]@{ outcome = $s.bomb.result.outcome; peak_above_baseline_mib = $s.bomb.peak_above_baseline_mib; pass = $s.bomb.pass }
            broken = [ordered]@{ half = $s.broken.jpeg_cut_to_half.outcome; head = $s.broken.jpeg_cut_to_200_bytes.outcome; empty = $s.broken.empty.outcome; pass = $s.broken.pass }
            concurrent = [ordered]@{ all_decoded = $c.all_decoded; peak_above_baseline_mib = $c.peak_above_baseline_mib; pass = $c.pass }
            cold = [ordered]@{ count = $k.count; workers = $k.workers; total_s = $k.total_s; failures = $k.failures }
        })
}

if (Want 'facets') {
    Say '==== facets (S0.3)' 'Yellow'
    $d = New-RawDir 'facets'
    $w = Join-Path $Work 'facets'
    try {
        Invoke-Bin 'facet-bench' @('--work', $w, '--out', (Join-Path $d 'facets.json'))
    } finally {
        if (Test-Path $w) { Remove-Item -Recurse -Force $w }
    }
    $r = Read-Json (Join-Path $d 'facets.json')
    $sizes = @($r.sizes | ForEach-Object { [ordered]@{ assets = $_.assets; sets = @($_.sets | ForEach-Object { [ordered]@{ filters = $_.filters; total_ms = $_.total_ms } }) } })
    Set-Check 'facets' $(if ($r.pass) { 'PASS' } else { 'FAIL' }) ([ordered]@{
            p95_10000_worst_ms = $r.p95_10000_worst_ms; budget_10000_ms = $r.budget_10000_ms
            p95_50000_worst_ms = $r.p95_50000_worst_ms; budget_50000_ms = $r.budget_50000_ms; sizes = $sizes; sqlite = $r.sqlite
        })
}

if (Want 'pdh-load') {
    Say '==== pdh-load (S0.1, load part; about a minute of GPU load)' 'Yellow'
    $d = New-RawDir 'pdh-load'
    Invoke-Bin 'pdh-probe' @('load', '--out', (Join-Path $d 'load.json'))
    $r = Read-Json (Join-Path $d 'load.json')
    Set-Check 'pdh-load' $r.verdict ([ordered]@{
            staging = [ordered]@{ shared_mib_median = $r.staging.shared_mib_median; held_minus_idle_mib = $r.staging.held_minus_idle_mib; released_minus_idle_mib = $r.staging.released_minus_idle_mib }
            compute = [ordered]@{ busiest_type_counts = $r.compute.busiest_type_counts; median_busiest_engine_percent = $r.compute.median_busiest_engine_percent; median_nvml_percent = $r.compute.median_nvml_percent; nvml = $r.compute.nvml }
        })
}

if (Want 'spill') {
    Say '==== spill (S0.4; starts ComfyUI, about 30 minutes of GPU load)' 'Yellow'
    Invoke-Spill
}

Say "Results: $ChecksFile"
