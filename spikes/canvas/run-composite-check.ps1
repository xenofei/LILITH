<#
.SYNOPSIS
  Builds canvas.exe, runs the golden scene on one backend and checks Task 3's criteria 1-4.
  Writes (merges into) spikes/canvas/results/composite.json under the backend's key.

.DESCRIPTION
  Run A (criteria 1-3): canvas.exe --golden <dir> --backend <b> --frames 120, with the debug layer
    (QSG_RHI_DEBUG_LAYER=1). The exe writes the CPU reference (Rust) and the GPU read-back of the
    projection and of every mip level as raw RGBA8 files; this script compares them itself (C#).
      1  GPU projection vs CPU composite: max |diff| <= 1 per channel on every pixel
      2  debug layer active, no hazard messages from it, and the exe's own read/write-disjointness
         assertion (every compute pipeline and dispatch) found no violation. A negative-control run
         (--inject-hazard: the parent pass also samples the projection it writes) must make both the
         debug layer and the assertion report the hazard, which proves the detection works.
      3  every GPU mip level k vs a box-filter downsample of GPU level k-1: max |diff| <= 1
  Run B (criterion 4): --recreate-every 120 --frames 1200 (debug layer on as well).
      4  10 recreations (11 renderers), each renderer rebuilt the full view and its read-back matched
         the CPU composite, the final read-back (after frame 1200) matches, exit 0, no device loss.

  Build output and run logs go to spikes/canvas/build/ (git-ignored); the raw golden files go to
  spikes/canvas/golden/<backend>/ (git-ignored).

.EXAMPLE
  pwsh -File spikes/canvas/run-composite-check.ps1 -Backend d3d11
#>
param(
    [ValidateSet('d3d11', 'd3d12')] [string]$Backend = 'd3d11',
    [switch]$NoBuild,
    [switch]$Clean,
    [switch]$NoDebugLayer,
    [int]$TimeoutSec = 420   # first launches of a fresh exe have stalled ~145 s before QML creation
)
$ErrorActionPreference = 'Stop'

$Root    = $PSScriptRoot
$Repo    = (Resolve-Path (Join-Path $Root '..\..')).Path
$Build   = Join-Path $Root 'build'
$BinDir  = Join-Path $Build 'cmake\bin'
$Runs    = Join-Path $Build 'runs'
$Results = Join-Path $Root 'results'
$Golden  = Join-Path $Root "golden\$Backend"
$ExeName = 'canvas.exe'
$Exe     = Join-Path $BinDir $ExeName

. (Join-Path $Repo 'spikes\toolchain\env.ps1')
New-Item -ItemType Directory -Force $Build, $Runs, $Results | Out-Null

Add-Type -TypeDefinition @'
using System;
public static class RgbaCmp {
    // Returns { maxAbsDiff, pixelsOver1, firstBadPixel (-1 if none) }.
    public static long[] Diff(byte[] a, byte[] b) {
        if (a.Length != b.Length) return new long[] { 255, -1, -1 };
        int max = 0; long over = 0; long first = -1;
        for (int i = 0; i < a.Length; i += 4) {
            int m = 0;
            for (int c = 0; c < 4; c++) { int d = Math.Abs(a[i + c] - b[i + c]); if (d > m) m = d; }
            if (m > max) max = m;
            if (m > 1) { over++; if (first < 0) first = i / 4; }
        }
        return new long[] { max, over, first };
    }
    // 2x2 box filter, edge texels clamped, rounded to nearest: (a+b+c+d+2)/4.
    public static byte[] Box(byte[] s, int w, int h, int dw, int dh) {
        var d = new byte[dw * dh * 4];
        for (int y = 0; y < dh; y++) for (int x = 0; x < dw; x++) {
            int x0 = Math.Min(2 * x, w - 1), x1 = Math.Min(2 * x + 1, w - 1);
            int y0 = Math.Min(2 * y, h - 1), y1 = Math.Min(2 * y + 1, h - 1);
            for (int c = 0; c < 4; c++) {
                int sum = s[(y0 * w + x0) * 4 + c] + s[(y0 * w + x1) * 4 + c] + s[(y1 * w + x0) * 4 + c] + s[(y1 * w + x1) * 4 + c];
                d[(y * dw + x) * 4 + c] = (byte)((sum + 2) / 4);
            }
        }
        return d;
    }
}
'@

$crit = [ordered]@{}
function Set-Check([int]$n, [bool]$pass, [System.Collections.IDictionary]$detail) {
    $o = [ordered]@{ criterion = $n; pass = $pass }
    foreach ($k in $detail.Keys) { $o[$k] = $detail[$k] }
    $script:crit["$n"] = $o
    Write-Host ("criterion {0} [{1}] : {2}" -f $n, $Backend, $(if ($pass) { 'PASS' } else { 'FAIL' }))
}

function Write-Results {
    $all = foreach ($n in 1..4) {
        if ($crit.Contains("$n")) { $crit["$n"] } else { [ordered]@{ criterion = $n; pass = $false; not_run = $true } }
    }
    $path = Join-Path $Results 'composite.json'
    $doc = [ordered]@{}
    if (Test-Path $path) {
        try { $old = Get-Content $path -Raw | ConvertFrom-Json -AsHashtable; foreach ($k in $old.Keys) { $doc[$k] = $old[$k] } } catch {}
    }
    $doc['updated'] = (Get-Date).ToString('o')
    $doc[$Backend] = [ordered]@{
        generated  = (Get-Date).ToString('o')
        qt_dir     = $env:QTDIR
        exe        = $Exe
        debug_layer_requested = (-not $NoDebugLayer)
        criteria   = @($all)
    }
    $doc | ConvertTo-Json -Depth 14 | Set-Content -Encoding utf8 $path
    $failed = @($all | Where-Object { -not $_.pass } | ForEach-Object { $_.criterion })
    if ($failed.Count) { Write-Host "[$Backend] FAILED criteria: $($failed -join ', ')"; exit 1 }
    Write-Host "[$Backend] ALL CRITERIA PASSED"; exit 0
}

function Read-Log([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content $path | Where-Object { $_.Trim() } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } | Where-Object { $_ })
}
function Get-Events($log, [string]$name) { @($log | Where-Object { $_.event -eq $name }) }

function Invoke-App([string[]]$appArgs, [hashtable]$envSet) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($a in $appArgs) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $BinDir
    foreach ($k in $envSet.Keys) { $psi.Environment[$k] = $envSet[$k] }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit($TimeoutSec * 1000)) {
        try { $p.Kill($true) } catch {}
        $p.WaitForExit()
        return [ordered]@{ exit_code = $null; timed_out = $true; wall_ms = $sw.ElapsedMilliseconds }
    }
    $p.WaitForExit()
    [ordered]@{ exit_code = $p.ExitCode; timed_out = $false; wall_ms = $sw.ElapsedMilliseconds }
}

function Read-Raw([string]$dir, [string]$name) {
    $f = Join-Path $dir $name
    if (Test-Path $f) { [System.IO.File]::ReadAllBytes($f) } else { $null }
}

# Compares <prefix>_L0.rgba with cpu_L0.rgba (criterion 1 / 4) and each mip level with a box
# filter of the GPU level above (criterion 3).
function Compare-Golden([string]$dir, [string]$prefix) {
    $metaPath = Join-Path $dir 'meta.json'
    if (-not (Test-Path $metaPath)) { return [ordered]@{ ok = $false; reason = "missing $metaPath" } }
    $meta = Get-Content $metaPath -Raw | ConvertFrom-Json
    $cpu = Read-Raw $dir 'cpu_L0.rgba'
    $gpu = Read-Raw $dir "${prefix}_L0.rgba"
    if (-not $cpu -or -not $gpu) { return [ordered]@{ ok = $false; reason = "missing cpu_L0.rgba or ${prefix}_L0.rgba in $dir" } }
    $d = [RgbaCmp]::Diff($gpu, $cpu)
    $proj = [ordered]@{ max_abs_diff = $d[0]; pixels_over_1 = $d[1]; first_bad_pixel = $d[2]
                        width = $meta.width; height = $meta.height; ok = ($d[0] -le 1) }
    # Blank-output guard: a projection that is all zero cannot pass even if the CPU were also blank.
    $nonZero = 0; foreach ($i in 0..([Math]::Min(4095, $gpu.Length - 1))) { if ($gpu[$i * 1] -ne 0) { $nonZero++ } }
    $proj.nonzero_bytes_in_first_4k = $nonZero
    $mips = @()
    $prev = $gpu; $pw = [int]$meta.width; $ph = [int]$meta.height
    for ($k = 1; $k -lt $meta.levels.Count; $k++) {
        $lw = [int]$meta.levels[$k].width; $lh = [int]$meta.levels[$k].height
        $g = Read-Raw $dir "${prefix}_L$k.rgba"
        if (-not $g) { $mips += [ordered]@{ level = $k; ok = $false; reason = 'missing' }; break }
        $ref = [RgbaCmp]::Box($prev, $pw, $ph, $lw, $lh)
        $md = [RgbaCmp]::Diff($g, $ref)
        $mips += [ordered]@{ level = $k; width = $lw; height = $lh; max_abs_diff = $md[0]; pixels_over_1 = $md[1]; ok = ($md[0] -le 1) }
        $prev = $g; $pw = $lw; $ph = $lh
    }
    [ordered]@{ ok = $proj.ok -and ($nonZero -gt 0); projection = $proj; mips = $mips
                mips_ok = ($mips.Count -ge 1) -and (@($mips | Where-Object { -not $_.ok }).Count -eq 0)
                mips_max_abs_diff = (@($mips | ForEach-Object { $_.max_abs_diff }) | Measure-Object -Maximum).Maximum }
}

# ------------------------------------------------------------------------------------------
# Build.
# ------------------------------------------------------------------------------------------
$cmakeLists = Join-Path $Root 'CMakeLists.txt'
$cmakeBuild = Join-Path $Build 'cmake'
$buildInfo = [ordered]@{}
if (-not $NoBuild) {
    if (-not (Test-Path $cmakeLists)) {
        $buildInfo.reason = "no CMake project: $cmakeLists does not exist"
    } else {
        if ($Clean -and (Test-Path $cmakeBuild)) { Remove-Item -Recurse -Force $cmakeBuild }
        & cmake -S $Root -B $cmakeBuild -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo *>&1 | Tee-Object -FilePath (Join-Path $Build 'configure.log') | Out-Null
        $buildInfo.configure_exit = $LASTEXITCODE
        if ($buildInfo.configure_exit -eq 0) {
            & cmake --build $cmakeBuild *>&1 | Tee-Object -FilePath (Join-Path $Build 'build.log') | Out-Null
            $buildInfo.build_exit = $LASTEXITCODE
        }
    }
}
if (-not (Test-Path $Exe)) {
    $why = if ($buildInfo.reason) { $buildInfo.reason } else { "canvas.exe missing at $Exe" }
    Set-Check 1 $false ([ordered]@{ reason = $why; build = $buildInfo })
    Write-Results
}

$envSet = @{ QSG_RHI_BACKEND = $Backend }
if (-not $NoDebugLayer) { $envSet.QSG_RHI_DEBUG_LAYER = '1' }

# ------------------------------------------------------------------------------------------
# Run A: criteria 1-3.
# ------------------------------------------------------------------------------------------
$dirA = Join-Path $Golden 'golden'
if (Test-Path $dirA) { Remove-Item -Recurse -Force $dirA }
New-Item -ItemType Directory -Force $dirA | Out-Null
$logA = Join-Path $Runs "$Backend-golden.jsonl"; Remove-Item $logA -ErrorAction SilentlyContinue
Write-Host "[$Backend] run A: golden scene ..."
$runA = Invoke-App @('--golden', $dirA, '--backend', $Backend, '--frames', '120', '--log', $logA,
                     '--screenshot', (Join-Path $Runs "$Backend-golden.png")) $envSet
$evA = Read-Log $logA
$cmpA = Compare-Golden $dirA 'gpu_first'
$backendsA = @(Get-Events $evA 'backend' | ForEach-Object { $_.backend } | Select-Object -Unique)
$exeCmpA = Get-Events $evA 'golden_compare' | Select-Object -First 1
$backendOk = @($backendsA | Where-Object { $_ -notlike "$($Backend.ToUpper())*" }).Count -eq 0 -and $backendsA.Count -ge 1
$errorsA = @(Get-Events $evA 'error' | ForEach-Object { $_.what })

Set-Check 1 ([bool]($cmpA.ok -and $backendOk -and $runA.exit_code -eq 0)) ([ordered]@{
    run = $runA; log = $logA; backends = $backendsA; errors = $errorsA
    projection = $cmpA.projection; reason = $cmpA.reason
    exe_self_check = $exeCmpA })

# Criterion 2: debug layer + hazard assertion.
$dbg = Get-Events $evA 'debug_layer' | Select-Object -First 1
$msgs = @(Get-Events $evA 'd3d_msg')
# D3D11: "...still bound on input/output!", "Forcing ... to NULL" (DEVICE_*_HAZARD ids).
# D3D12: "Resource state (...) ... is invalid for use as ...", ResourceBarrier warnings.
$hazardRe = 'hazard|still bound|forcing|resource state|invalid for use as|resourcebarrier'
$hazardMsgs = @($msgs | Where-Object { "$($_.text) $($_.id_name)" -match $hazardRe })
$warnOrErr = @($msgs | Where-Object { $_.severity -in 'corruption', 'error', 'warning' })
$hzA = Get-Events $evA 'hazard_checks' | Select-Object -Last 1
$violA = @(Get-Events $evA 'hazard_violation')
# Negative control: the same scene with a deliberate read+write of the projection.
$dirN = Join-Path $Golden 'negative-control'
if (Test-Path $dirN) { Remove-Item -Recurse -Force $dirN }
New-Item -ItemType Directory -Force $dirN | Out-Null
$logN = Join-Path $Runs "$Backend-negative-control.jsonl"; Remove-Item $logN -ErrorAction SilentlyContinue
Write-Host "[$Backend] negative control: --inject-hazard ..."
$runN = Invoke-App @('--golden', $dirN, '--backend', $Backend, '--frames', '30', '--inject-hazard', '--log', $logN) $envSet
$evN = Read-Log $logN
$hazN = @(Get-Events $evN 'd3d_msg' | Where-Object { "$($_.text) $($_.id_name)" -match $hazardRe })
$violN = @(Get-Events $evN 'hazard_violation').Count
$cmpN = Get-Events $evN 'golden_compare' | Select-Object -First 1
$control = [ordered]@{ run = $runN; log = $logN; debug_layer_hazard_messages = $hazN.Count
                       first_messages = @($hazN | Select-Object -First 3 | ForEach-Object { "$($_.severity) $($_.id): $($_.text)" })
                       assertion_violations = $violN; projection_max_abs_diff = $cmpN.projection_max_abs_diff
                       detected = ($hazN.Count -gt 0) -and ($violN -gt 0) }

$c2 = ($null -ne $dbg) -and [bool]$dbg.info_queue -and ($hazardMsgs.Count -eq 0) -and ($violA.Count -eq 0) -and
      ($null -ne $hzA) -and ($hzA.checks -gt 0) -and ($runA.exit_code -eq 0) -and $control.detected
Set-Check 2 $c2 ([ordered]@{
    debug_layer = $dbg
    d3d_messages_total = $msgs.Count; d3d_warnings_or_errors = $warnOrErr.Count
    hazard_messages = @($hazardMsgs | Select-Object -First 20)
    other_warnings_or_errors = @($warnOrErr | Where-Object { $_ -notin $hazardMsgs } | Select-Object -First 20 | ForEach-Object { "$($_.severity) $($_.id): $($_.text)" })
    assertion = $hzA; assertion_violations = $violA.Count; negative_control = $control })

Set-Check 3 ([bool]($cmpA.mips_ok -and $runA.exit_code -eq 0)) ([ordered]@{
    max_abs_diff = $cmpA.mips_max_abs_diff; levels = $cmpA.mips })

# ------------------------------------------------------------------------------------------
# Run B: criterion 4.
# ------------------------------------------------------------------------------------------
$dirB = Join-Path $Golden 'recreate'
if (Test-Path $dirB) { Remove-Item -Recurse -Force $dirB }
New-Item -ItemType Directory -Force $dirB | Out-Null
$logB = Join-Path $Runs "$Backend-recreate.jsonl"; Remove-Item $logB -ErrorAction SilentlyContinue
Write-Host "[$Backend] run B: --recreate-every 120 --frames 1200 ..."
$runB = Invoke-App @('--golden', $dirB, '--backend', $Backend, '--recreate-every', '120', '--frames', '1200', '--log', $logB) $envSet
$evB = Read-Log $logB
$cmpB = Compare-Golden $dirB 'gpu_final'
$created = @(Get-Events $evB 'renderer_created')
$recreations = @(Get-Events $evB 'recreate')
$views = @(Get-Events $evB 'golden_compare')
$renderersWithGoodView = @($views | Where-Object { $_.projection_max_abs_diff -le 1 -and $_.mips_max_abs_diff -le 1 } | ForEach-Object { $_.renderer } | Select-Object -Unique)
$finalCmp = $views | Where-Object { $_.final } | Select-Object -Last 1
$lost = @(Get-Events $evB 'device_lost')
$removedMsgs = @(Get-Events $evB 'qt_msg' | Where-Object { $_.msg -match 'device.*(removed|lost|reset)|DEVICE_REMOVED|DXGI_ERROR' })
$c4 = ($runB.exit_code -eq 0) -and ($recreations.Count -eq 10) -and ($created.Count -eq 11) -and
      ($renderersWithGoodView.Count -eq 11) -and ($null -ne $finalCmp) -and ($finalCmp.frame -ge 1200) -and
      $cmpB.ok -and ($lost.Count -eq 0) -and ($removedMsgs.Count -eq 0)
Set-Check 4 $c4 ([ordered]@{
    run = $runB; log = $logB
    recreations = $recreations.Count; renderers_created = $created.Count
    renderers_with_matching_view = $renderersWithGoodView.Count
    views = @($views | ForEach-Object { [ordered]@{ renderer = $_.renderer; frame = $_.frame; final = $_.final; projection_max_abs_diff = $_.projection_max_abs_diff; mips_max_abs_diff = $_.mips_max_abs_diff } })
    final_frame = $finalCmp.frame; final_projection = $cmpB.projection; final_reason = $cmpB.reason
    device_lost_events = $lost.Count; device_removed_messages = @($removedMsgs | ForEach-Object { $_.msg })
    d3d_hazard_messages = @(Get-Events $evB 'd3d_msg' | Where-Object { "$($_.text) $($_.id_name)" -match $hazardRe }).Count
    hazard_violations = @(Get-Events $evB 'hazard_violation').Count })

Write-Results
