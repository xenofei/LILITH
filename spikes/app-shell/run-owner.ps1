<#
.SYNOPSIS
  Spike 10's owner run in one command: redeploy (S0.4), the UI Automation check (S0.7), then
  S0.6 under ComfyUI load (comfy-load.ps1), with the 5 Hz fallback only if S0.6 FAILs. Prints a
  time estimate first and one summary line per check at the end.

.DESCRIPTION
  It refuses at once, before anything runs, if a spike app or ComfyUI is already running:
    - any process whose executable is under spikes/app-shell/build (a spike window, or
      QtWebEngineProcess, from any build or deploy folder);
    - any process whose executable is under the ComfyUI folder (its venv's python), or whose
      command line names ComfyUI's main.py, or anything answering on 127.0.0.1:<Port>.
  Each step is the existing script, run in its own pwsh, so each keeps its own guards, logs and
  results (results/checks.json, results/s07.json, results/s06[-fallback].json). Rerun any of
  them alone the same way. Ctrl+C or closing this window stops whatever is running;
  comfy-load.ps1 then stops ComfyUI and everything it started.

.EXAMPLE
  pwsh -NoProfile -File spikes/app-shell/run-owner.ps1
#>
param(
    [int]$Port = 8188,
    [string]$ComfyPython = (Join-Path $env:USERPROFILE 'ComfyUI\.venv\Scripts\python.exe'),
    [string]$ComfyMain   = (Join-Path $env:USERPROFILE 'ComfyUI\ComfyUI\main.py')
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot
$Results = Join-Path $Root 'results'
Import-Module (Join-Path $Root 'lib\SpikeMeasure.psm1') -Force

# ---- refuse at once: a spike app or ComfyUI already running (by path, never by name) ----------
$why = @(Get-OwnerRunBlockers (Join-Path $Root 'build') $ComfyMain)
$answers = try { (Invoke-WebRequest "http://127.0.0.1:$Port/system_stats" -TimeoutSec 3 -UseBasicParsing).StatusCode -eq 200 } catch { $false }
if ($answers) { $why += "something answers on 127.0.0.1:$Port; stop it first" }
if ($why.Count) { Write-Host "NOT RUN: $($why -join '; '). Nothing was started."; exit 1 }

Write-Host @'
Spike 10 owner run. Expected time on an idle machine:
  S0.4  rebuild (incremental) and redeploy ........ 2-5 min
  S0.7  UI Automation check ...................... 1 min
  S0.6  ComfyUI load, gate, 65 s measured run ..... usually 6-10 min, at most about 12
        (ComfyUI's start and the GPU gate are each capped at 5 min)
        the 5 Hz fallback, only if S0.6 FAILs ..... the same again
  Total: about 10-16 min; at most about 30 min with the fallback.
Spike windows open without taking focus; S0.7's takes it when it clicks the counter (about
0.1-0.2 s, at most about 2 s). Those S0.4 and S0.7 open close by themselves within a minute. During S0.6 the spike stays on top for about a minute: don't use the mouse or
keyboard then. Ctrl+C or closing this window stops everything.
'@

function Read-Json([string]$f) { if (Test-Path $f) { try { Get-Content $f -Raw | ConvertFrom-Json } catch { $null } } else { $null } }
$t0 = Get-Date
$lines = [ordered]@{}

# ---- S0.4 and S0.7: one harness run ----------------------------------------------------------
$before = (Get-Date).AddSeconds(-1)
& pwsh -NoProfile -File (Join-Path $Root 'run-spike.ps1') -Only 'S0.4,S0.7'
$checks = Read-Json (Join-Path $Results 'checks.json')
$fresh = $checks -and ([datetime]$checks.generated) -ge $before
foreach ($id in 'S0.4', 'S0.7') {
    $c = if ($fresh) { @($checks.checks) | Where-Object { $_.check -eq $id } | Select-Object -First 1 } else { $null }
    $pre = if ($fresh) { @($checks.checks) | Where-Object { $_.check -eq 'preflight' } | Select-Object -First 1 } else { $null }
    $lines[$id] = if ($c) { $c.verdict } elseif ($pre) { "NOT RUN ($($pre.reason))" } else { 'NOT RUN (no result written; see the output above)' }
}

# ---- S0.6, and its fallback only on FAIL ----------------------------------------------------
# S0.6 needs this round's build deployed. It is skipped when the build failed or S0.4 didn't run
# (an old deploy would match the old binaries) and when the deploy doesn't match the build. An
# S0.4 FAIL for another reason (a size, or a restore off by a few px) and an S0.7 result don't stop
# it. comfy-load.ps1 refuses an incomplete or stale deploy itself as well.
$build = if ($fresh) { @($checks.checks) | Where-Object { $_.check -eq 'S0.1' } | Select-Object -First 1 } else { $null }
$stale = @(Test-DeployCurrent (Join-Path $Root 'build\deploy') (Join-Path $Root 'build\cmake\bin'))
$skip = if ($build -and $build.verdict -match '^FAIL') { "the build failed ($($build.verdict)); the deploy would be the previous build's" }
        elseif ($lines['S0.4'] -match '^NOT RUN') { "S0.4 didn't run, so this round's build isn't deployed: $($lines['S0.4'])" }
        elseif ($stale.Count) { "the deploy folder doesn't match the build: $($stale -join ', ')" }
        else { $null }
if ($skip) {
    $lines['S0.6'] = "NOT RUN ($skip)"
    Write-Host "Skipping S0.6: $skip"
} else {
    $s06 = Join-Path $Results 's06.json'
    $before = Get-Date
    & pwsh -NoProfile -File (Join-Path $Root 'comfy-load.ps1') -Port $Port -ComfyPython $ComfyPython -ComfyMain $ComfyMain
    $r = Read-Json $s06
    $lines['S0.6'] = if ($r -and (Get-Item $s06).LastWriteTime -ge $before) { $r.verdict } else { 'NOT RUN (no result written; see the output above)' }
    if ($lines['S0.6'] -match '^FAIL') {
        $fb = Join-Path $Results 's06-fallback.json'
        $before = Get-Date
        & pwsh -NoProfile -File (Join-Path $Root 'comfy-load.ps1') -Port $Port -ComfyPython $ComfyPython -ComfyMain $ComfyMain -RateHz 5 -ChartsBatched
        $r = Read-Json $fb
        $lines['S0.6 fallback'] = if ($r -and (Get-Item $fb).LastWriteTime -ge $before) { $r.verdict } else { 'NOT RUN (no result written; see the output above)' }
    }
}

Write-Host ''
Write-Host ("Spike 10 owner run, {0:N1} min:" -f ((Get-Date) - $t0).TotalMinutes)
foreach ($k in $lines.Keys) { Write-Host ("  {0,-14} {1}" -f $k, $lines[$k]) }
Write-Host "Results: $Results (checks.json, s07.json, s06*.json); give them to the agent."
$bad = @($lines.Keys | Where-Object { $lines[$_] -notmatch '^PASS' -and $_ -ne 'S0.6 fallback' })
exit $(if ($bad.Count) { 1 } else { 0 })
