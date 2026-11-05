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

spill never changes the ComfyUI install: it only reads it. Its user, input, output and temp
folders are in results\raw\spill\comfy\, custom nodes are not loaded, Python writes no bytecode,
and a finally block stops the whole process tree it started (also after an error or Ctrl+C).

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
    [string[]]$ComfyArgs = @()
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

function Get-SpillPlan {
    if (-not $ComfyRoot -or -not $Python) { throw 'spill needs -ComfyRoot <ComfyUI folder> and -Python <python.exe>.' }
    $root = (Resolve-Path -LiteralPath $ComfyRoot).Path.TrimEnd('\')
    $py = (Resolve-Path -LiteralPath $Python).Path
    if (-not (Test-Path -LiteralPath (Join-Path $root 'main.py'))) { throw "No main.py in the ComfyUI folder given." }
    $runDir = Join-Path $RawRoot 'spill'
    $comfy = Join-Path $runDir 'comfy'
    $arguments = @('main.py', '--listen', '127.0.0.1', '--port', "$Port", '--disable-auto-launch', '--disable-all-custom-nodes',
        '--user-directory', (Join-Path $comfy 'user'), '--input-directory', (Join-Path $comfy 'input'),
        '--output-directory', (Join-Path $comfy 'output'), '--temp-directory', (Join-Path $comfy 'temp')) + $ComfyArgs
    [ordered]@{ root = $root; python = $py; run_dir = $runDir; comfy = $comfy; arguments = $arguments }
}

# Why the owned ComfyUI must not start: a listener on the port, or a ComfyUI of that install
# already running (a process from its folder, one naming the folder on its command line, or its
# Python running a main.py). An elevated process hides its path and command line, so it can't
# be seen here.
function Get-SpillRefusals($Plan) {
    $why = @()
    # The listener table, and a connect as well: a sandboxed or filtered view can miss a listener.
    $listeners = @([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners() | Where-Object { $_.Port -eq $Port })
    $answers = $false
    $client = [Net.Sockets.TcpClient]::new()
    try { $answers = $client.ConnectAsync('127.0.0.1', $Port).Wait(1000) -and $client.Connected } catch { $answers = $false } finally { $client.Dispose() }
    if ($listeners.Count -gt 0 -or $answers) { $why += "something already listens on port $Port" }
    $cmp = [StringComparison]::OrdinalIgnoreCase
    $procs = @(Get-CimInstance Win32_Process | Where-Object {
            $_.ProcessId -ne $PID -and -not ("$($_.CommandLine)".IndexOf('run-spike.ps1', $cmp) -ge 0) -and (
                ("$($_.ExecutablePath)".StartsWith($Plan.root + '\', $cmp)) -or
                ("$($_.CommandLine)".IndexOf($Plan.root, $cmp) -ge 0) -or
                ("$($_.ExecutablePath)" -ieq $Plan.python -and "$($_.CommandLine)" -match 'main\.py'))
        })
    if ($procs.Count -gt 0) { $why += "$($procs.Count) process(es) of that ComfyUI install already run ($(@($procs | ForEach-Object { "$($_.Name) $($_.ProcessId)" }) -join ', '))" }
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

function Get-UnixMs { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }

# Queues one graph and waits until /history has it, or the timeout (then /interrupt).
function Invoke-Graph([string]$Base, $Graph, [string]$Kind, [int]$Index, [string]$GraphFile, $Proc) {
    $Graph | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 $GraphFile
    $start = Get-UnixMs
    $body = @{ prompt = $Graph; client_id = [guid]::NewGuid().ToString() } | ConvertTo-Json -Depth 8
    $queued = Invoke-RestMethod -Method Post -Uri "$Base/prompt" -Body $body -ContentType 'application/json'
    $id = $queued.prompt_id
    $state = 'timeout'
    $deadline = [DateTime]::UtcNow.AddSeconds($RunTimeoutSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Proc.HasExited) { throw "ComfyUI exited during $Kind run $Index (exit $($Proc.ExitCode))." }
        Start-Sleep -Milliseconds 1000
        $h = Invoke-RestMethod -Uri "$Base/history/$id"
        if ($h.PSObject.Properties[$id]) {
            $st = $h.$id.status
            $state = if ($st -and $st.PSObject.Properties['status_str']) { $st.status_str } else { 'done' }
            break
        }
    }
    if ($state -eq 'timeout') {
        try { Invoke-RestMethod -Method Post -Uri "$Base/interrupt" | Out-Null } catch { }
        try { Invoke-RestMethod -Method Post -Uri "$Base/queue" -Body (@{ delete = @($id) } | ConvertTo-Json) -ContentType 'application/json' | Out-Null } catch { }
    }
    $end = Get-UnixMs
    Say ("  {0} run {1}: {2} in {3:N0} s" -f $Kind, $Index, $state, (($end - $start) / 1000))
    [ordered]@{ kind = $Kind; index = $Index; start_ms = $start; end_ms = $end; state = $state; seconds = ($end - $start) / 1000 }
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
            refusals          = $refusals
            would_refuse      = ($refusals.Count -gt 0)
            checkpoint        = $(if ($Checkpoint) { $Checkpoint } else { '<not given: required for a real run>' })
            graphs            = [ordered]@{ fit = "$FitWidth x $FitHeight, batch $FitBatch, $FitSteps steps"; spill = "$SpillWidth x $SpillHeight, batch $SpillBatch, $SpillSteps steps" }
            phases            = "idle $IdleSeconds s, then 3 fitting and 3 spilling runs (timeout $RunTimeoutSeconds s each)"
        } | ConvertTo-Json -Depth 4
        return
    }
    if (-not $Checkpoint) { throw 'spill needs -Checkpoint <SDXL checkpoint name>.' }
    if ($refusals.Count -gt 0) { throw "Refused, nothing was started: $($refusals -join '; ')." }

    $runDir = New-RawDir 'spill'
    foreach ($sub in 'user', 'input', 'output', 'temp') { New-Item -ItemType Directory -Force -Path (Join-Path $plan.comfy $sub) | Out-Null }
    $graphs = Join-Path $runDir 'graphs'
    New-Item -ItemType Directory -Force -Path $graphs | Out-Null
    $samples = Join-Path $runDir 'samples.jsonl'
    $stopFile = Join-Path $runDir 'stop'
    $base = "http://127.0.0.1:$Port"
    $proc = $null
    $probe = $null
    $phases = [System.Collections.Generic.List[object]]::new()
    try {
        Say "Starting the owned ComfyUI on 127.0.0.1:$Port"
        $proc = Start-Process -FilePath $plan.python -ArgumentList (Format-CommandLine $plan.arguments) -WorkingDirectory $plan.root `
            -NoNewWindow -PassThru -RedirectStandardOutput (Join-Path $runDir 'comfy-out.log') -RedirectStandardError (Join-Path $runDir 'comfy-err.log') `
            -Environment @{ PYTHONDONTWRITEBYTECODE = '1'; PYTHONUNBUFFERED = '1' }
        $probe = Start-Process -FilePath (Exe 'spill-probe') -ArgumentList @('sample', '--root-pid', "$($proc.Id)", '--out', (Format-CommandLine @($samples)), '--stop-file', (Format-CommandLine @($stopFile))) `
            -NoNewWindow -PassThru -RedirectStandardOutput (Join-Path $runDir 'probe-out.log') -RedirectStandardError (Join-Path $runDir 'probe-err.log')
        $ready = $false
        $deadline = [DateTime]::UtcNow.AddSeconds(300)
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($proc.HasExited) { throw "ComfyUI exited while starting (exit $($proc.ExitCode)); see results\raw\spill\comfy-err.log." }
            try { Invoke-RestMethod -Uri "$base/system_stats" -TimeoutSec 5 | Out-Null; $ready = $true; break } catch { Start-Sleep -Seconds 2 }
        }
        if (-not $ready) { throw 'ComfyUI did not answer /system_stats within 300 s.' }
        $info = Invoke-RestMethod -Uri "$base/object_info/CheckpointLoaderSimple"
        $combo = $info.CheckpointLoaderSimple.input.required.ckpt_name
        $names = if ($combo[0] -is [array]) { $combo[0] } elseif ($combo.Count -gt 1 -and $combo[1].PSObject.Properties['options']) { $combo[1].options } else { @() }
        if ($Checkpoint -notin $names) { throw "ComfyUI doesn't list the checkpoint given ($($names.Count) listed)." }

        Say "Idle for $IdleSeconds s"
        $s = Get-UnixMs
        Start-Sleep -Seconds $IdleSeconds
        $phases.Add([ordered]@{ kind = 'idle'; index = 0; start_ms = $s; end_ms = (Get-UnixMs); state = 'done' })
        $seed = 1200
        foreach ($i in 1..3) {
            $g = New-Graph $FitWidth $FitHeight $FitBatch $FitSteps ($seed + $i)
            $phases.Add((Invoke-Graph $base $g 'fit' $i (Join-Path $graphs "fit-$i.json") $proc))
            Start-Sleep -Seconds 5
        }
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
        $left = @(Get-SpillRefusals $plan)
        if ($left.Count -gt 0) { Say "After the stop: $($left -join '; ')" 'Red' }
    }
    $phasesFile = Join-Path $runDir 'phases.json'
    ConvertTo-Json -InputObject @($phases) -Depth 4 | Set-Content -Encoding utf8 $phasesFile
    Invoke-Bin 'spill-probe' @('analyze', '--samples', $samples, '--phases', $phasesFile, '--out', (Join-Path $runDir 'result.json'))
    $r = Read-Json (Join-Path $runDir 'result.json')
    $states = @($phases | Where-Object { $_.kind -ne 'idle' } | ForEach-Object { "$($_.kind)$($_.index)=$($_.state)" })
    $verdict = $r.verdict
    if (@($phases | Where-Object { $_.kind -ne 'idle' -and $_.state -ne 'success' }).Count -gt 0) { $verdict = "$verdict (not every run succeeded: $($states -join ', '))" }
    Set-Check 'spill' $verdict ([ordered]@{
            idle_mib = $r.idle_mib; fit = $r.fit; spill = $r.spill; gap = $r.gap; margin_mib = $r.margin_mib
            value_inside_gap = $r.value_inside_gap; runs = $phases; graphs = [ordered]@{ fit = "$FitWidth x $FitHeight b$FitBatch s$FitSteps"; spill = "$SpillWidth x $SpillHeight b$SpillBatch s$SpillSteps" }
            adapter_shared = $r.adapter_shared; process_dedicated = $r.process_dedicated
        })
}

# ---------------------------------------------------------------- the run

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
            compute = [ordered]@{ busiest_type_counts = $r.compute.busiest_type_counts; median_busiest_engine_percent = $r.compute.median_busiest_engine_percent; median_nvml_percent = $r.compute.median_nvml_percent }
        })
}

if (Want 'spill') {
    Say '==== spill (S0.4; starts ComfyUI, about 30 minutes of GPU load)' 'Yellow'
    Invoke-Spill
}

Say "Results: $ChecksFile"
