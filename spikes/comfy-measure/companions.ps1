<#
.SYNOPSIS
  Task 8 (IMG-155, ruling R2): starts or stops the two suite stand-ins that share the GPU with ComfyUI
  during the VRAM-fit runs, and records their per-process VRAM.

    -Start   canvas.exe --doc 8192x8192 --layers 20 --auto-pan --on-top --gpu-cache-mb 1536 (Task 3/4 canvas,
             tile cache budget 1.5 GB) and webengine.exe showing ComfyUI at http://127.0.0.1:8188 (Task 6,
             no --phase: it just shows the ComfyUI frontend). Waits -SettleSeconds, then writes
             results/companions-<tag>.json with a PDH snapshot of both processes (dedicated VRAM on the
             NVIDIA adapter, every process of each app: canvas.exe, webengine.exe, QtWebEngineProcess.exe).
    -Stop    stops exactly the processes this script started (PIDs from build/companions.pids.json).

  Both apps write only under their own spike build folders (logs, the WebEngine profile).
#>
param(
    [switch]$Start,
    [switch]$Stop,
    [string]$Tag = 'cu128',
    [int]$SettleSeconds = 30,
    [int]$GpuCacheMb = 1536
)
$ErrorActionPreference = 'Stop'
$Here   = $PSScriptRoot
$Repo   = (Resolve-Path (Join-Path $Here '..\..')).Path
$Build  = Join-Path $Here 'build'
$PidFile = Join-Path $Build 'companions.pids.json'
New-Item -ItemType Directory -Force $Build | Out-Null
New-Item -ItemType Directory -Force (Join-Path $Here 'results') | Out-Null

function Get-ProcVram([int[]]$Ids) {
    # Dedicated VRAM (MiB) on the NVIDIA adapter, summed per process image.
    $smiLuid = $null
    $adapters = (Get-Counter '\GPU Adapter Memory(*)\Dedicated Usage').CounterSamples
    $smi = [double](nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits -i 0)
    $best = $adapters | Sort-Object { [math]::Abs($_.CookedValue / 1MB - $smi) } | Select-Object -First 1
    $smiLuid = ($best.InstanceName -split '_phys')[0]
    $rows = @()
    foreach ($id in $Ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if (-not $p) { continue }
        $samples = @()
        try { $samples = (Get-Counter "\GPU Process Memory(pid_$($id)_$($smiLuid)*)\*" -ErrorAction Stop).CounterSamples } catch {}
        $row = [ordered]@{ pid = $id; name = $p.ProcessName }
        foreach ($k in @('dedicated usage', 'local usage', 'shared usage', 'total committed')) {
            $mib = ($samples | Where-Object { $_.Path -like "*\$k" } | Measure-Object CookedValue -Sum).Sum / 1MB
            $row[($k -replace ' usage', '' -replace ' ', '_') + '_mib'] = [math]::Round($mib, 1)
        }
        $rows += $row
    }
    return [ordered]@{ luid = $smiLuid; total_smi_mib = $smi; processes = $rows }
}

function Get-Tree([int]$RootId) {
    $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name
    $ids = @($RootId); $frontier = @($RootId)
    while ($frontier.Count) {
        $kids = @($all | Where-Object { $frontier -contains $_.ParentProcessId } | ForEach-Object ProcessId)
        $ids += $kids; $frontier = $kids
    }
    return $ids
}

if ($Start) {
    . (Join-Path $Repo 'spikes\toolchain\env.ps1') *> $null
    $canvasExe = Join-Path $Repo 'spikes\canvas\build\cmake\bin\canvas.exe'
    $webExe    = Join-Path $Repo 'spikes\webengine\build\cmake\bin\webengine.exe'
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $canvasLog = Join-Path $Build "canvas-$Tag-$stamp.jsonl"
    $webLog    = Join-Path $Build "webengine-$Tag-$stamp.jsonl"
    $canvasArgs = @('--doc', '8192x8192', '--layers', '20', '--auto-pan', '--on-top', '--gpu-cache-mb', "$GpuCacheMb", '--log', "`"$canvasLog`"")
    $webArgs    = @('--origin', 'http://127.0.0.1:8188', '--log', "`"$webLog`"")
    $c = Start-Process -FilePath $canvasExe -ArgumentList $canvasArgs -PassThru
    $w = Start-Process -FilePath $webExe -ArgumentList $webArgs -PassThru
    @{ canvas = $c.Id; webengine = $w.Id; started = (Get-Date -Format o) } | ConvertTo-Json | Set-Content $PidFile
    Start-Sleep -Seconds $SettleSeconds
    $canvasIds = Get-Tree $c.Id
    $webIds    = Get-Tree $w.Id
    $snap = [ordered]@{
        tag = $Tag; timestamp = (Get-Date -Format o); settle_s = $SettleSeconds
        canvas_command = "canvas.exe $($canvasArgs -join ' ')"; webengine_command = "webengine.exe $($webArgs -join ' ')"
        canvas = Get-ProcVram $canvasIds; webengine = Get-ProcVram $webIds
        canvas_log_reserve = @(Get-Content $canvasLog -ErrorAction SilentlyContinue | Where-Object { $_ -match 'cache_reserve|cache_grow' } | Select-Object -Last 6)
    }
    foreach ($app in @('canvas', 'webengine')) {
        foreach ($k in @('dedicated', 'local', 'total_committed')) {
            $snap["$($app)_$($k)_mib"] = [math]::Round((@($snap[$app].processes) | ForEach-Object { $_["$($k)_mib"] } | Measure-Object -Sum).Sum, 1)
        }
    }
    $snap.canvas_total_mib = $snap.canvas_dedicated_mib
    $snap.webengine_total_mib = $snap.webengine_dedicated_mib
    $out = Join-Path $Here "results\companions-$Tag.json"
    $snap | ConvertTo-Json -Depth 6 | Set-Content $out
    "canvas pid $($c.Id) dedicated/local/committed $($snap.canvas_dedicated_mib)/$($snap.canvas_local_mib)/$($snap.canvas_total_committed_mib) MiB; webengine tree $($webIds -join ',') $($snap.webengine_dedicated_mib)/$($snap.webengine_local_mib)/$($snap.webengine_total_committed_mib) MiB -> $out"
}

if ($Stop) {
    if (-not (Test-Path $PidFile)) { 'no companions.pids.json'; return }
    $ids = Get-Content $PidFile | ConvertFrom-Json
    foreach ($id in @($ids.canvas, $ids.webengine)) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { $p.CloseMainWindow() | Out-Null; if (-not $p.WaitForExit(5000)) { Stop-Process -Id $id -Force } }
    }
    Remove-Item $PidFile
    "stopped canvas $($ids.canvas), webengine $($ids.webengine)"
}
