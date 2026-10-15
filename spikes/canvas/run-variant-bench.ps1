<#
.SYNOPSIS
  Task 4, D3D12 decision condition 3 (R5): composite time of the two-pass composite (Task 3, IMG-031)
  against variants without the isolated-group intermediate pass, including the in-place read-write
  composite. Writes spikes/canvas/results/composite-variants.json.

.DESCRIPTION
  Variants (canvas.exe --composite-variant):
    twopass        Task 3: group pass -> RGBA32F intermediate, then the parent pass (DXBC SM 5.0, fxc)
    twopass-sm66   the same two shaders as DXIL SM 6.6 (dxc), D3D12 only
    single         one pass, the isolated group accumulated in registers (DXBC SM 5.0)
    single-sm66    the same as DXIL SM 6.6, D3D12 only
    inplace        R5's in-place read-write composite: one pass, the projection bound read-write
                   (RGBA8 typed UAV load), DXIL SM 6.6, D3D12 only
    inplace-sm50   the same shader as DXBC SM 5.0 (fxc accepts the typed UAV load), any backend
  Benchmark (Task 3's): --layers 20 --doc 8192x8192 --recomposite --gpu-timestamps, 60 output tiles per
  frame, GPU time per frame from lastCompletedGpuTime(); 1200 frames, the first second excluded. Idle =
  the same without --recomposite (display only). Composite time = p50(variant) - p50(idle) of the same
  round. Rounds are interleaved (every variant once per round) and the median over rounds is reported.

  Correctness: each variant also renders the golden scene once (--golden, debug layer on); the exe's
  own comparison with the Rust CPU reference (max |diff|) and the debug-layer messages are recorded.

  Condition 3 passes when composite(inplace) <= 0.75 x composite(twopass-sm66) on D3D12 (same shader
  model, so only the pass structure differs). The ratio against the DXBC two-pass is reported too.

.EXAMPLE
  pwsh -File spikes/canvas/run-variant-bench.ps1 -Rounds 3
#>
param(
    [int]$Rounds = 3,
    [int]$Frames = 1200,
    [switch]$SkipD3D11,
    [switch]$SkipGolden
)
$ErrorActionPreference = 'Stop'
$Root    = $PSScriptRoot
$BinDir  = Join-Path $Root 'build\cmake\bin'
$Exe     = Join-Path $BinDir 'canvas.exe'
$Out     = Join-Path $Root 'build\variant-bench'
$Results = Join-Path $Root 'results'
if (-not (Test-Path $Exe)) { throw "canvas.exe missing at $Exe (build with run-frametimes.ps1 or run-composite-check.ps1 first)" }
$env:PATH = "C:\Qt\6.12.0\msvc2022_64\bin;$env:PATH"
New-Item -ItemType Directory -Force $Out, $Results | Out-Null

function Invoke-Canvas([string[]]$a, [hashtable]$envSet = @{}) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new($Exe)
    foreach ($x in $a) { $psi.ArgumentList.Add($x) }
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $BinDir
    foreach ($k in $envSet.Keys) { $psi.Environment[$k] = $envSet[$k] }
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit(180000)) { try { $p.Kill($true) } catch {}; $p.WaitForExit(); return $null }
    $p.WaitForExit()
    $p.ExitCode
}
function Read-Log([string]$path) {
    if (-not (Test-Path $path)) { return @() }
    @(Get-Content $path | Where-Object { $_.Trim() } | ForEach-Object { try { $_ | ConvertFrom-Json } catch { $null } } | Where-Object { $_ })
}
function Median([double[]]$v) { $s = @($v | Sort-Object); if (-not $s.Count) { return $null }; $m = [int][Math]::Floor($s.Count / 2); if ($s.Count % 2) { $s[$m] } else { ($s[$m - 1] + $s[$m]) / 2 } }

$plan = [ordered]@{
    d3d12 = @('idle', 'twopass', 'twopass-sm66', 'single', 'single-sm66', 'inplace', 'inplace-sm50')
    d3d11 = @('idle', 'twopass', 'single', 'inplace-sm50')
}
if ($SkipD3D11) { $plan.Remove('d3d11') }

# --- Correctness (golden scene) per variant.
$golden = [ordered]@{}
if (-not $SkipGolden) {
    foreach ($be in $plan.Keys) {
        $golden[$be] = [ordered]@{}
        foreach ($v in $plan[$be] | Where-Object { $_ -ne 'idle' }) {
            $dir = Join-Path $Out "golden-$be-$v"
            if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
            New-Item -ItemType Directory -Force $dir | Out-Null
            $log = Join-Path $dir 'log.jsonl'
            $rc = Invoke-Canvas @('--golden', $dir, '--backend', $be, '--frames', '120', '--composite-variant', $v, '--log', $log) @{ QSG_RHI_DEBUG_LAYER = '1' }
            $ev = Read-Log $log
            $cmp = $ev | Where-Object { $_.event -eq 'golden_compare' } | Select-Object -First 1
            $msgs = @($ev | Where-Object { $_.event -eq 'd3d_msg' })
            $res = $ev | Where-Object { $_.event -eq 'resources' } | Select-Object -First 1
            $golden[$be][$v] = [ordered]@{
                exit_code = $rc
                composite_shader = $res.composite_shader; shader_model = $res.shader_model
                projection_max_abs_diff = $cmp.projection_max_abs_diff
                projection_pixels_over_1 = $cmp.projection_pixels_over_1
                mips_max_abs_diff = $cmp.mips_max_abs_diff
                d3d_messages = $msgs.Count
                d3d_message_ids = @($msgs | Group-Object id | ForEach-Object { "$($_.Name) x$($_.Count)" })
                hazard_violations = @($ev | Where-Object { $_.event -eq 'hazard_violation' }).Count
                errors = @($ev | Where-Object { $_.event -eq 'error' } | ForEach-Object { $_.what })
            }
            Write-Host ("golden {0} {1,-13}: exit {2}, max|GPU-CPU| {3} ({4} px > 1), mips {5}, d3d msgs {6}" -f $be, $v, $rc,
                $cmp.projection_max_abs_diff, $cmp.projection_pixels_over_1, $cmp.mips_max_abs_diff, $msgs.Count)
        }
    }
}

# --- Timing rounds.
$runs = @()
for ($r = 1; $r -le $Rounds; $r++) {
    foreach ($be in $plan.Keys) {
        foreach ($v in $plan[$be]) {
            $name = "$be-$v-r$r"
            $ft = Join-Path $Out "$name.json"
            Remove-Item $ft -ErrorAction SilentlyContinue
            $a = @('--layers', '20', '--doc', '8192x8192', '--backend', $be, '--frames', "$Frames", '--gpu-timestamps',
                   '--measure-after', '1000', '--frametimes', $ft, '--log', (Join-Path $Out "$name.jsonl"))
            if ($v -ne 'idle') { $a += @('--recomposite', '--composite-variant', $v) }
            $rc = Invoke-Canvas $a
            $j = if (Test-Path $ft) { Get-Content $ft -Raw | ConvertFrom-Json } else { $null }
            $row = [ordered]@{ backend = $be; variant = $v; round = $r; exit_code = $rc
                               n = $j.gpu_frame_ms.n; p50 = $j.gpu_frame_ms.p50; p95 = $j.gpu_frame_ms.p95; p99 = $j.gpu_frame_ms.p99
                               tiles_per_frame_p50 = $j.composited_tiles_per_frame.p50 }
            $runs += [pscustomobject]$row
            Write-Host ("round {0} {1} {2,-13}: GPU ms/frame p50 {3:N4}  p95 {4:N4}  (n {5}, {6} tiles/frame)" -f $r, $be, $v, $row.p50, $row.p95, $row.n, $row.tiles_per_frame_p50)
        }
    }
}

# --- Summary.
$summary = [ordered]@{}
foreach ($be in $plan.Keys) {
    $summary[$be] = [ordered]@{}
    foreach ($v in $plan[$be]) {
        $comp = @()
        for ($r = 1; $r -le $Rounds; $r++) {
            $x = $runs | Where-Object { $_.backend -eq $be -and $_.variant -eq $v -and $_.round -eq $r }
            $i = $runs | Where-Object { $_.backend -eq $be -and $_.variant -eq 'idle' -and $_.round -eq $r }
            if ($x -and $i -and $null -ne $x.p50 -and $null -ne $i.p50) { $comp += ($x.p50 - $i.p50) }
        }
        $p50s = @($runs | Where-Object { $_.backend -eq $be -and $_.variant -eq $v } | ForEach-Object { $_.p50 })
        $summary[$be][$v] = [ordered]@{
            frame_gpu_ms_p50_per_round = $p50s
            frame_gpu_ms_p50_median = Median $p50s
            composite_ms_per_round = $comp
            composite_ms_median = if ($v -eq 'idle') { $null } else { Median $comp }
        }
    }
}
$c12 = $summary['d3d12']
$ratioSame = $c12['inplace'].composite_ms_median / $c12['twopass-sm66'].composite_ms_median
$ratioDxbc = $c12['inplace'].composite_ms_median / $c12['twopass'].composite_ms_median
$cond3 = [ordered]@{
    rule = 'composite(inplace, D3D12, SM 6.6) <= 0.75 x composite(twopass-sm66, D3D12)'
    inplace_ms = $c12['inplace'].composite_ms_median
    twopass_sm66_ms = $c12['twopass-sm66'].composite_ms_median
    twopass_dxbc_ms = $c12['twopass'].composite_ms_median
    ratio_vs_twopass_sm66 = $ratioSame
    cut_vs_twopass_sm66_percent = (1 - $ratioSame) * 100
    ratio_vs_twopass_dxbc = $ratioDxbc
    cut_vs_twopass_dxbc_percent = (1 - $ratioDxbc) * 100
    single_sm66_cut_vs_twopass_sm66_percent = (1 - $c12['single-sm66'].composite_ms_median / $c12['twopass-sm66'].composite_ms_median) * 100
    single_dxbc_cut_vs_twopass_dxbc_percent = (1 - $c12['single'].composite_ms_median / $c12['twopass'].composite_ms_median) * 100
    pass = ($ratioSame -le 0.75)
}
$doc = [ordered]@{
    generated = (Get-Date).ToString('o')
    benchmark = "--layers 20 --doc 8192x8192 --recomposite --gpu-timestamps --frames $Frames --measure-after 1000; idle = without --recomposite"
    rounds = $Rounds
    condition_3 = $cond3
    summary = $summary
    golden = $golden
    runs = $runs
}
$doc | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $Results 'composite-variants.json')
Write-Host ("D3D12 composite (median of {0} rounds): twopass {1:N4} ms, twopass-sm66 {2:N4}, single-sm66 {3:N4}, inplace {4:N4}" -f $Rounds,
    $c12['twopass'].composite_ms_median, $c12['twopass-sm66'].composite_ms_median, $c12['single-sm66'].composite_ms_median, $c12['inplace'].composite_ms_median)
Write-Host ("condition 3: in-place cuts {0:N1} % vs two-pass SM 6.6 ({1:N1} % vs two-pass DXBC); needs >= 25 % -> {2}" -f $cond3.cut_vs_twopass_sm66_percent,
    $cond3.cut_vs_twopass_dxbc_percent, $(if ($cond3.pass) { 'PASS' } else { 'FAIL' }))
