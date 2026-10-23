<#
  Measurement helpers for the app-shell spike (S0.6, and the CPU load beside every timing).
  Pure functions (stats, coverage, verdict) are tested by tests/SpikeMeasure.Tests.ps1 (Pester).
#>

# Nearest-rank percentiles of a series; $null for an empty one (never a fake n = 1).
function Get-NearestRankStats([object[]]$Values) {
    # Untyped on purpose: a [double[]] parameter would turn $null into 0.
    $v = @($Values | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
    if ($v.Count -eq 0) { return $null }
    $s = @($v | Sort-Object)
    $pick = { param($p) $s[[math]::Max(0, [math]::Ceiling($p / 100 * $s.Count) - 1)] }
    [ordered]@{ n = $s.Count; p50 = (& $pick 50); p95 = (& $pick 95); p99 = (& $pick 99); max = $s[-1] }
}

# Was the GPU loaded through the window [StartMs, EndMs]? Two conditions (controller ruling):
# - dense: at least 0.8 samples a second (48 in 60 s) and no gap over 2 s, the window's edges
#   counting as gaps;
# - saturated: utilisation >= 95 % in at least 90 % of the samples, so brief dips between prompts
#   are allowed. Every dip is recorded.
function Test-GpuCoverage {
    param(
        [object[]]$Samples,            # objects with t (epoch ms) and util (%)
        [long]$StartMs,
        [long]$EndMs,
        [int]$MinUtil = 95,
        [int]$MaxGapMs = 2000,
        [double]$MinPerSecond = 0.8,
        [double]$MinSaturatedFraction = 0.9
    )
    $in = @($Samples | Where-Object { $_.t -ge $StartMs -and $_.t -le $EndMs } | Sort-Object t)
    $needed = [math]::Floor(($EndMs - $StartMs) / 1000 * $MinPerSecond)
    $edges = @($StartMs) + @($in | ForEach-Object { [long]$_.t }) + @($EndMs)
    $maxGap = 0
    for ($i = 1; $i -lt $edges.Count; $i++) { $maxGap = [math]::Max($maxGap, $edges[$i] - $edges[$i - 1]) }
    $dips = @($in | Where-Object { $_.util -lt $MinUtil })
    $below = $dips.Count
    $fraction = if ($in.Count) { [math]::Round(($in.Count - $below) / $in.Count, 4) } else { 0 }
    [ordered]@{
        n = $in.Count; needed = [int]$needed; max_gap_ms = [long]$maxGap
        min_util = $(if ($in.Count) { ($in | Measure-Object util -Minimum).Minimum } else { $null })
        below_min = $below; saturated_fraction = $fraction
        dips = @($dips | ForEach-Object { [ordered]@{ t = [long]$_.t; util = $_.util } })
        dense = ($in.Count -ge $needed) -and ($maxGap -le $MaxGapMs)
        saturated = ($in.Count -gt 0) -and ($fraction -ge $MinSaturatedFraction)
    }
}

# The frame-interval threshold for the primary display (controller ruling):
# max(16 ms, 1.5 vsync periods). It is continuous, never stricter than the bar's 16 ms, and always
# fails a dropped vsync (two periods): 240 and 144 Hz give 16, 75 Hz 20, 60 Hz 25.
function Get-FrameThresholdMs([double]$RefreshHz) {
    if (-not $RefreshHz -or $RefreshHz -le 0) { return $null }
    [math]::Round([math]::Max(16.0, 1.5 * 1000.0 / $RefreshHz), 3)
}

# Frames slower than 1.5 vsync periods (dropped vsyncs), counted directly for the reader.
function Get-DroppedFrames([object[]]$IntervalsMs, [double]$RefreshHz) {
    $v = @($IntervalsMs | Where-Object { $null -ne $_ } | ForEach-Object { [double]$_ })
    if (-not $RefreshHz -or $RefreshHz -le 0 -or $v.Count -eq 0) { return $null }
    $limit = 1.5 * 1000.0 / $RefreshHz
    $n = @($v | Where-Object { $_ -gt $limit }).Count
    [ordered]@{ over_1_5_vsync = $n; fraction = [math]::Round($n / $v.Count, 4); limit_ms = [math]::Round($limit, 3) }
}
# S0.6's verdict. A crash, a hang or a missing measured report is FAIL (that is the APP-135
# failure), decided before the measurement's validity; the GPU condition only makes a completed
# run INVALID.
function Get-S06Verdict {
    param(
        [bool]$HasMeasuredReport,
        $ExitCode,
        [bool]$TimedOut,
        $Heartbeat,            # Get-NearestRankStats output or $null
        $Frames,               # Get-NearestRankStats output or $null
        [double]$RefreshHz,
        $Coverage              # Test-GpuCoverage output
    )
    if ($TimedOut) { return 'FAIL (the app hung and was killed)' }
    if ($null -eq $ExitCode -or $ExitCode -ne 0) { return "FAIL (the app exited with $ExitCode)" }
    if (-not $HasMeasuredReport -or -not $Heartbeat -or -not $Frames) { return 'FAIL (no measured stall report)' }
    $threshold = Get-FrameThresholdMs $RefreshHz
    if ($null -eq $threshold) { return 'INVALID (the screen refresh rate was not recorded)' }
    if (-not $Coverage.dense) { return "INVALID (GPU samples too sparse: $($Coverage.n) of $($Coverage.needed), largest gap $($Coverage.max_gap_ms) ms)" }
    if (-not $Coverage.saturated) { return "INVALID (GPU >= 95 % in only $([math]::Round(100 * $Coverage.saturated_fraction, 1)) % of the samples; 90 % needed)" }
    $hbOk = $Heartbeat.p99 -lt 16
    $frOk = $Frames.p99 -lt $threshold
    if ($hbOk -and $frOk) { 'PASS' } else { "FAIL (heartbeat p99 $($Heartbeat.p99) ms, frame p99 $($Frames.p99) ms against $threshold ms)" }
}

# nvidia-smi samples its own clock every $IntervalMs into $Out (CSV: timestamp, util, memory),
# started inside $Job (Start-ProcessInJob; none if $Job is $null).
function Start-GpuSampler([string]$Out, [int]$IntervalMs = 500, $Job = $null) {
    $smi = (Get-Command nvidia-smi.exe -ErrorAction Stop).Source
    Start-ProcessInJob $Job $smi -StdOut $Out -ArgumentList @(
        '--query-gpu=timestamp,utilization.gpu,memory.used', '--format=csv,noheader,nounits', "-lms", "$IntervalMs")
}

function Read-GpuSamples([string]$File) {
    if (-not (Test-Path $File)) { return @() }
    @(Get-Content $File | ForEach-Object {
        $p = $_ -split '\s*,\s*'
        if ($p.Count -ge 3) {
            $ts = [datetime]::ParseExact($p[0].Trim(), 'yyyy/MM/dd HH:mm:ss.fff', [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeLocal)
            [pscustomobject]@{ t = ([DateTimeOffset]$ts).ToUnixTimeMilliseconds(); util = [int]$p[1]; mem_mib = [int]$p[2] }
        }
    } | Where-Object { $_ })
}

# PDH "GPU Process Memory(pid_<pid>_*)" Local Usage and Dedicated Usage, about once a second, as
# JSON lines in $Out, until the returned sampler process is stopped. The instance exists only once
# the process has a D3D device, so each sample is its own Get-Counter call and a missing instance
# is retried, not fatal (a -Continuous Get-Counter on a not-yet-existing instance ends at once:
# the owner's first S0.6 run got 0 samples). Retries are written as {"t":..,"missing":true}
# lines. The sampler is a hidden Windows PowerShell 5.1 started inside $Job (Start-ProcessInJob;
# none if $Job is $null), so it is in the job from its start. Not pwsh: a pwsh child of the Store
# pwsh stays in the package, so it is born inside the package's job, and a process already in a
# job can't be assigned to our job once that holds processes from outside the package
# (AssignProcessToJobObject: access denied; fix round 5 hit exactly this). powershell.exe is
# outside the package like python and nvidia-smi. It also stops by itself once the pwsh that
# started it is gone (run-spike.ps1 has no job).
function Start-VramSampler([int]$ProcessId, [string]$Out, $Job = $null) {
    $body = {
        param($procId, $out, $parentPid)
        $parent = [System.Diagnostics.Process]::GetProcessById($parentPid); [void]$parent.Handle   # pinned
        $paths = foreach ($n in 'Local Usage', 'Dedicated Usage') { "\GPU Process Memory(pid_${procId}_*)\$n" }
        while (-not $parent.HasExited) {
            $t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            try {
                $c = Get-Counter -Counter $paths -SampleInterval 1 -MaxSamples 1 -ErrorAction Stop
                $row = [ordered]@{ t = ([DateTimeOffset]$c.Timestamp).ToUnixTimeMilliseconds() }
                foreach ($key in 'local usage', 'dedicated usage') {
                    $sum = ($c.CounterSamples | Where-Object { $_.Path -like "*\$key" } | Measure-Object CookedValue -Sum).Sum
                    $row[($key -replace ' ', '_') + '_mib'] = [math]::Round($sum / 1MB, 1)
                }
            } catch {
                $row = [ordered]@{ t = $t; missing = $true }
                Start-Sleep -Milliseconds 500
            }
            Add-Content -Path $out -Value ($row | ConvertTo-Json -Compress)
        }
    }
    $cmd = "& { $body } $ProcessId '$($Out -replace "'", "''")' $PID"
    $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
    $ps51 = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    Start-ProcessInJob $Job $ps51 @('-NoProfile', '-NonInteractive', '-EncodedCommand', $enc)
}

# VRAM samples that hold values (the "missing" retries left out).
function Read-VramSamples([string]$File) {
    @(Read-JsonLines $File | Where-Object { -not $_.missing -and $null -ne $_.local_usage_mib })
}

$script:DisplaySource = @'
[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct DEVMODE {
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmDeviceName;
    public short dmSpecVersion; public short dmDriverVersion; public short dmSize; public short dmDriverExtra;
    public int dmFields; public int dmPositionX; public int dmPositionY; public int dmDisplayOrientation;
    public int dmDisplayFixedOutput; public short dmColor; public short dmDuplex; public short dmYResolution;
    public short dmTTOption; public short dmCollate;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string dmFormName;
    public short dmLogPixels; public int dmBitsPerPel; public int dmPelsWidth; public int dmPelsHeight;
    public int dmDisplayFlags; public int dmDisplayFrequency;
    public int dmICMMethod; public int dmICMIntent; public int dmMediaType; public int dmDitherType;
    public int dmReserved1; public int dmReserved2; public int dmPanningWidth; public int dmPanningHeight;
}
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern bool EnumDisplaySettingsW(string deviceName, int modeNum, ref DEVMODE devMode);
public static int PrimaryHz() {
    var dm = new DEVMODE(); dm.dmSize = (short)Marshal.SizeOf(typeof(DEVMODE));
    return EnumDisplaySettingsW(null, -1, ref dm) ? dm.dmDisplayFrequency : 0;
}
'@

# The primary display's refresh rate from Windows (EnumDisplaySettingsW with
# ENUM_CURRENT_SETTINGS for the primary device), as a cross-check of what the app reports.
# 0 if it can't be read.
function Get-OsPrimaryRefreshHz {
    if (-not ('SpikeMeasure.Display' -as [type])) {
        Add-Type -Namespace SpikeMeasure -Name Display -MemberDefinition $script:DisplaySource
    }
    [double][SpikeMeasure.Display]::PrimaryHz()
}

# Which refresh rate S0.6 judges with, and where it came from: the app's report
# (`primary_refresh_hz`; builds before fix round 2 logged `screens[].refresh_hz`), else Windows'.
# Values under 24 Hz count as unknown. `mismatch` is set when the app and Windows both gave a
# plausible value more than 2 Hz apart (S0.6 then calls the run INVALID).
function Resolve-RefreshHz($StallReport, [double]$OsHz) {
    $app = 0.0; $source = $null
    if ($StallReport -and (Test-PlausibleRefresh ([double]$StallReport.primary_refresh_hz))) {
        $app = [double]$StallReport.primary_refresh_hz; $source = 'app'
    } elseif ($StallReport -and $StallReport.screens) {
        $p = @($StallReport.screens) | Where-Object { $_.primary -and (Test-PlausibleRefresh ([double]$_.refresh_hz)) } | Select-Object -First 1
        if ($p) { $app = [double]$p.refresh_hz; $source = 'app (older report format)' }
    }
    $osOk = Test-PlausibleRefresh $OsHz
    $mismatch = ($app -gt 0) -and $osOk -and ([math]::Abs($app - $OsHz) -gt 2)
    if ($app -gt 0) { return [ordered]@{ hz = $app; source = $source; os_hz = $OsHz; mismatch = $mismatch } }
    if ($osOk) { return [ordered]@{ hz = $OsHz; source = 'windows'; os_hz = $OsHz; mismatch = $false } }
    [ordered]@{ hz = 0.0; source = 'none'; os_hz = $OsHz; mismatch = $false }
}
# Is the deploy folder built from the current build? Our binaries in it must match the build
# tree's byte for byte; the owner's first S0.6 run used a deploy from before fix round 2. Returns
# the stale or missing files (empty when current).
function Test-DeployCurrent([string]$Deploy, [string]$BinDir) {
    $ours = 'app_shell_spike.exe', 'spike_shell_shim.dll', 'kddockwidgets-qt62.dll', 'SpikeWeb\spike_web.dll'
    @($ours | Where-Object {
        $a = Join-Path $Deploy $_; $b = Join-Path $BinDir $_
        -not (Test-Path $a) -or -not (Test-Path $b) -or (Get-FileHash $a).Hash -ne (Get-FileHash $b).Hash
    })
}

# The saturation gate before the measured minute: the same conditions as the measured window
# (dense, and >= 95 % in at least 90 % of the samples) over the last $WindowMs of samples.
function Test-GpuSaturatedRecently([object[]]$Samples, [long]$NowMs, [long]$WindowMs = 20000) {
    $c = Test-GpuCoverage -Samples $Samples -StartMs ($NowMs - $WindowMs) -EndMs $NowMs
    [bool]($c.dense -and $c.saturated)
}

function Read-JsonLines([string]$File) {
    if (-not (Test-Path $File)) { return @() }
    @(Get-Content $File | ForEach-Object { try { $_ | ConvertFrom-Json } catch {} } | Where-Object { $_ })
}

# S0.1's warning lines, as whole blocks, split by where each warning is raised:
#   own        - raised in one of our files: a relative path (CMake prints our files relative to
#                the project, e.g. `CMakeLists.txt:29`), or a path under spikes/app-shell;
#   kddw       - raised in KDDockWidgets' sources (_deps/kddockwidgets...);
#   toolchain  - raised in Qt, CMake or the cxx-qt/Corrosion helpers.
# A CMake message keeps its body, its "Call Stack (most recent call first):" lines and the
# "This warning is for project developers" note; it ends at the next other unindented line.
# via_own_file is set when the call stack passes through one of our files (recorded, not judged).
function Split-BuildWarnings([string[]]$Lines) {
    $out = [ordered]@{ own = @(); kddw = @(); toolchain = @() }
    $foreign = '(?i)^[A-Za-z]:[\\/]|_deps[\\/]'
    for ($i = 0; $i -lt $Lines.Count; $i++) {
        $l = $Lines[$i]
        if ($l -notmatch '(?i)\bwarning\b') { continue }
        if ($l -match '(?i)warning(s)? (generated|emitted)|0 warning') { continue }
        $isCMake = $l -match '^CMake Warning'
        $endIdx = [math]::Min($i + 3, $Lines.Count - 1)
        if ($isCMake) {
            $j = $i + 1
            while ($j -lt $Lines.Count -and ($Lines[$j] -match '^\s' -or -not $Lines[$j].Trim() -or
                                             $Lines[$j] -match '^Call Stack \(most recent call first\):' -or
                                             $Lines[$j] -match '^This warning is for project developers')) { $j++ }
            $endIdx = [math]::Max($i, $j - 1)
        }
        $block = ($Lines[$i..$endIdx] -join "`n").TrimEnd()
        # Where it is raised: "CMake Warning [(dev)] at <file>:<line>" or "<file>(<line>): warning".
        $origin = if ($isCMake -and $l -match '^CMake Warning(?: \(dev\))? at (.+?):\d+') { $Matches[1] }
                  elseif ($l -match '^\s*(.+?)\(\d+(?:,\d+)?\)\s*:\s*warning') { $Matches[1] }
                  else { $null }
        $stack = @($block -split "`n" | Where-Object { $_ -match '^\s+\S.*:\d+ \(' } | ForEach-Object { $_.Trim() })
        $viaOwn = @($stack | Where-Object { $_ -notmatch $foreign -and $_ -match '(^|[\\/])CMakeLists\.txt:\d+' }).Count -gt 0
        $entry = [ordered]@{ origin = $origin; via_own_file = $viaOwn; text = $block }
        if ("$origin$l" -match '(?i)_deps[\\/]kddockwidgets') { $out.kddw += $entry }
        elseif ($origin -and ($origin -match '(?i)spikes[\\/]app-shell[\\/]' -or $origin -notmatch $foreign)) { $out.own += $entry }
        elseif ($origin -or "$l" -match '(?i)C:[\\/]Qt[\\/]|Program Files|_deps[\\/](cxxqt|corrosion)') { $out.toolchain += $entry }
        else { $out.own += $entry }   # no origin we can place: counted as ours, the strict way
        if ($isCMake) { $i = $endIdx }   # the block's own lines (its note included) are not new warnings
    }
    $out
}
# ---- guards for the owner's PC -----------------------------------------------------------------

# Processes whose executable lives under $Root (any name: QtWebEngineProcess.exe runs from the
# deploy folder too). Processes whose ExecutablePath can't be read (elevated, or another user's)
# are skipped; a spike started by the owner is readable.
function Get-ProcessesUnder([string]$Root, [object[]]$Processes = $null) {
    # Resolved against PowerShell's location (not .NET's working directory), then normalised, so
    # 'build\..\x' and relative paths compare like the full ExecutablePath.
    $abs = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Root)
    $full = [IO.Path]::GetFullPath($abs).TrimEnd('\') + '\'
    if ($null -eq $Processes) { $Processes = @(Get-CimInstance Win32_Process) }
    @($Processes | Where-Object { $_.ExecutablePath -and
        [IO.Path]::GetFullPath($_.ExecutablePath).StartsWith($full, [StringComparison]::OrdinalIgnoreCase) })
}

# Spike app instances anywhere (they all share %LOCALAPPDATA%\LILITH-spike\image).
function Get-SpikeAppProcesses([object[]]$Processes = $null) {
    if ($null -eq $Processes) { $Processes = @(Get-CimInstance Win32_Process) }
    @($Processes | Where-Object { $_.Name -in 'app_shell_spike.exe', 'tst_app_shell.exe' })
}

# -DeployName is one folder under build/: "deploy" or "deploy-<letters/digits>", nothing else, so
# a slip can't point the deploy folder's delete at build/, runs/, cmake/ or the sources.
function Test-DeployName([string]$Name) { [bool]($Name -cmatch '^deploy(-[A-Za-z0-9]+)?\z') }

# run-owner.ps1's refusal, by path only: a process whose executable is under the spike's build
# folder (a spike window or QtWebEngineProcess from any build or deploy folder), or ComfyUI: an
# executable under the ComfyUI folder (its venv's python) or a command line naming its main.py.
function Get-OwnerRunBlockers([string]$BuildDir, [string]$ComfyMain, [object[]]$Processes = $null) {
    if ($null -eq $Processes) { $Processes = @(Get-CimInstance Win32_Process) }
    $why = @()
    $spike = @(Get-ProcessesUnder $BuildDir $Processes)
    if ($spike.Count) { $why += "a spike process runs from spikes/app-shell/build (pid $(($spike | ForEach-Object ProcessId) -join ', ')); close its windows" }
    $comfyDir = Split-Path (Split-Path $ComfyMain)   # <ComfyUI folder>\ComfyUI\main.py; the venv is under the folder
    $main = [IO.Path]::GetFullPath($ComfyMain)
    $comfy = @(@(Get-ProcessesUnder $comfyDir $Processes) + @($Processes | Where-Object {
        $_.CommandLine -and $_.CommandLine.IndexOf($main, [StringComparison]::OrdinalIgnoreCase) -ge 0 }) | Sort-Object ProcessId -Unique)
    if ($comfy.Count) { $why += "ComfyUI is running (pid $(($comfy | ForEach-Object ProcessId) -join ', ')); stop it first" }
    $why
}

# Where a harness run writes its results. The owner's runs (run-owner.ps1, or the scripts with
# their defaults) write results/; every other run (another -DeployName, comfy-load -SamplersOnly)
# writes results/agent-runs/<stamp>/, so an agent's or a test's rerun never replaces the owner's
# checks.json, s07.json or s06*.json. An explicit -ResultsDir wins.
function Get-ResultsDir([string]$SpikeRoot, [string]$Requested, [bool]$OwnerRun, [string]$Stamp) {
    if ($Requested) { return $Requested }
    if ($OwnerRun) { return (Join-Path $SpikeRoot 'results') }
    Join-Path $SpikeRoot "results\agent-runs\$Stamp"
}

# ---- S0.6 workload and refresh-rate checks ----------------------------------------------------

# Did the bar's workload actually run? load_counts is logged at the end of the measured window,
# with cumulative counters from the load's start (5 s of warm-up and the measured window), so
# each count must be at least half of what the measured window alone should give. The list's
# visible seconds follow the same rule: they come from a 1 s QML Timer, which drops the ticks a
# GUI stall misses, so one long stall (the thing S0.6 measures) must not read as "not visible".
# A hidden second is still never allowed.
function Test-S06Workload($Counts, [int]$MeasureSeconds = 60, [int]$RateHz = 10) {
    if (-not $Counts) { return @('no load_counts logged') }
    $p = @()
    $half = 0.5 * $MeasureSeconds
    if ([double]$Counts.grid_visible_seconds -lt $half) { $p += "list visible only $($Counts.grid_visible_seconds) s of $MeasureSeconds (expected >= $half)" }
    if ([double]$Counts.grid_hidden_seconds -gt 0) { $p += "list hidden $($Counts.grid_hidden_seconds) s" }
    if ([double]$Counts.feed_ticks -lt $half * $RateHz) { $p += "feed ticks $($Counts.feed_ticks) (expected >= $($half * $RateHz))" }
    if ([double]$Counts.decoded -lt $half * $RateHz * 8) { $p += "previews decoded $($Counts.decoded) (expected >= $($half * $RateHz * 8))" }
    if ([double]$Counts.chart_points -lt $half * 6) { $p += "chart points $($Counts.chart_points) (expected >= $($half * 6))" }
    if ([double]$Counts.grid_rows_scrolled -lt $half * 200) { $p += "rows scrolled $([math]::Round([double]$Counts.grid_rows_scrolled)) (expected >= $($half * 200))" }
    if (-not $Counts.web_shown) { $p += 'web dock not shown' }
    $p
}

# The page S0.6's Web dock loads (--web-url): local, no network. Its body is coloured, a canvas is
# drawn by script, and the script sets the title only after two requestAnimationFrame callbacks,
# which run only when the compositor asks the visible view for frames. So a logged web_title shows
# that the page loaded, ran its script and reached a painted frame inside the job (it doesn't
# prove GPU raster). about:blank has no content, no script and nothing to raster. `#` is %23 (else
# it starts a fragment), and the HTML uses single quotes only. S0.3 and S0.9 keep about:blank,
# which their bars name.
$script:S06WebTitle = 's06-rendered-42'
$script:S06WebUrl = "data:text/html;charset=utf-8,<!doctype html><title>pending</title><body style='margin:0;background:%2320a060'>" +
    "<canvas id=c width=512 height=512></canvas><script>var g=c.getContext('2d');g.fillStyle='%23f0c000';g.fillRect(64,64,384,384);" +
    "requestAnimationFrame(function(){requestAnimationFrame(function(){document.title='s06-rendered-'+(6*7)})})</script>"
function Get-S06WebUrl { $script:S06WebUrl }

# Did the Web dock's page really load and run (S0.6 runs the app inside the kill-on-close job, and
# WebEngine's sandboxed renderer must work there)? From the app's own log, after web_dock_shown:
# a web_load "succeeded" for the data: page, a web_title equal to the one its script sets, and no
# web_render_process_terminated. S0.6 takes no grabs. Returns the problems (empty when it ran).
# A dock never shown is left to Test-S06Workload, which already reports it.
function Test-S06Web([object[]]$Events) {
    $shown = @($Events | Where-Object { $_.event -eq 'web_dock_shown' }) | Select-Object -First 1
    if (-not $shown) { return @() }
    $after = @($Events | Where-Object { [double]$_.t_ms -ge [double]$shown.t_ms })
    $p = @()
    $ok = @($after | Where-Object { $_.event -eq 'web_load' -and $_.status -eq 'succeeded' -and "$($_.url)".StartsWith('data:text/html') })
    if (-not $ok.Count) {
        $last = @($Events | Where-Object { $_.event -eq 'web_load' }) | Select-Object -Last 1
        $p += if ($last) { "web page did not load (last load event: $($last.status) $("$($last.url)".Substring(0, [math]::Min(40, "$($last.url)".Length))) $($last.error_string))".TrimEnd() }
              else { 'web page did not load (no web_load event)' }
    }
    if (-not @($after | Where-Object { $_.event -eq 'web_title' -and $_.title -eq $script:S06WebTitle }).Count) {
        $p += "web page's script did not run (no web_title '$script:S06WebTitle')"
    }
    $dead = @($Events | Where-Object { $_.event -eq 'web_render_process_terminated' })
    if ($dead.Count) { $p += "web renderer terminated ($($dead[0].status), exit $($dead[0].exit_code))" }
    $p
}

# The final S0.6 verdict: a FAIL stays a FAIL; otherwise a missing workload or a refresh rate the
# app and Windows disagree on makes the run INVALID, because PASS would then rest on missing data.
# Every cause is kept: an INVALID from the GPU check gets these appended, not replaced.
function Complete-S06Verdict([string]$Verdict, [string[]]$WorkloadProblems, [bool]$RefreshMismatch) {
    if ($Verdict -match '^FAIL') { return $Verdict }
    $problems = @($WorkloadProblems | Where-Object { $_ })
    $extra = @()
    if ($problems.Count) { $extra += "workload not running as the bar requires: $($problems -join '; ')" }
    if ($RefreshMismatch) { $extra += 'the app and Windows report refresh rates more than 2 Hz apart' }
    if (-not $extra.Count) { return $Verdict }
    if ($Verdict -match '^INVALID \((.*)\)$') { return "INVALID ($($Matches[1]); $($extra -join '; '))" }
    "INVALID ($($extra -join '; '))"
}

# A refresh rate S0.6 can judge with: 0, 1 ("hardware default") and anything under 24 Hz count as
# unknown.
function Test-PlausibleRefresh([double]$Hz) { $Hz -ge 24 -and $Hz -le 1000 }

# ---- S0.7: the UI Automation check (APP-156) ----------------------------------------------------

# S0.7's items, judged from s07-uia.ps1's output (elements of the app's windows and the UIA
# notifications it received). Each item is PASS or FAIL with the UIA values it matched. Only
# elements of the app's own process from Qt's own provider count (FrameworkId "Qt"), not Windows'
# non-client title bars or another process's hosted window. The client clicks the counter itself
# (InvokePattern) after it has subscribed; if it never clicked, the announcement is INVALID.
function Get-S07Items($Uia, [int]$AppPid) {
    $qt = @($Uia.elements | Where-Object { $_.framework_id -eq 'Qt' -and [int]$_.process_id -eq $AppPid })
    $pick = { param($rows) @($rows | ForEach-Object { [ordered]@{ window = $_.window; name = $_.name; control_type = $_.control_type
                                                                  localized_control_type = $_.localized_control_type } }) }
    $items = [ordered]@{}
    $tb = @($qt | Where-Object { $_.control_type -eq 'ControlType.TitleBar' -and $_.name -in 'Jobs', 'Grid' })
    $items.kddw_title_bar = [ordered]@{ verdict = $(if ($tb.Count) { 'PASS' } else { 'FAIL' }); kddw = $true
        wanted = 'a dock title bar (ControlType.TitleBar) named "Jobs" or "Grid"'; found = @(& $pick $tb) }
    $tabs = @($qt | Where-Object { $_.control_type -eq 'ControlType.TabItem' -and $_.name -in 'Grid', 'Web' -and $_.window -match 'main-a' })
    $both = (@($tabs | Where-Object name -eq 'Grid').Count -gt 0) -and (@($tabs | Where-Object name -eq 'Web').Count -gt 0)
    $items.kddw_dock_tabs = [ordered]@{ verdict = $(if ($both) { 'PASS' } else { 'FAIL' }); kddw = $true
        wanted = 'tabs (ControlType.TabItem) "Grid" and "Web" in main-a'; found = @(& $pick $tabs) }
    $btn = @($qt | Where-Object { $_.control_type -eq 'ControlType.Button' -and $_.name -eq 'Increment the counter' })
    $items.counter_button = [ordered]@{ verdict = $(if ($btn.Count) { 'PASS' } else { 'FAIL' }); kddw = $false
        wanted = 'a button named "Increment the counter"'; found = @(& $pick $btn) }
    $chart = @($qt | Where-Object { $_.name -match '^GPU load chart, latest \d+(\.\d+)? %$' -and $_.window -match 'main-b' })
    $items.chart_tile = [ordered]@{ verdict = $(if ($chart.Count) { 'PASS' } else { 'FAIL' }); kddw = $false
        wanted = 'the chart tile in main-b named "GPU load chart, latest <n> %" (its ControlType is recorded)'; found = @(& $pick $chart) }
    $note = @($Uia.notifications | Where-Object { $_.display_string -eq 'Count is now 1' -and [int]$_.process_id -eq $AppPid })
    $av = if ($note.Count) { 'PASS' } elseif (-not $Uia.invoked) { "INVALID (the client never clicked the button: $($Uia.invoke_error))" } else { 'FAIL' }
    $items.announce = [ordered]@{ verdict = $av; kddw = $false
        wanted = 'a UIA notification event "Count is now 1" from the app (Accessible.announce), after the client clicked the button'
        found = @($note | ForEach-Object { [ordered]@{ display_string = $_.display_string; kind = $_.kind; processing = $_.processing
                                                       source_name = $_.source_name; source_control_type = $_.source_control_type } })
        notifications_received = @($Uia.notifications).Count }
    $items
}

# Conditions under which S0.7 measured nothing (INVALID, not FAIL): the app's windows never
# appeared to UIA, or the tree was last read before the app logged layout_built ($LayoutBuiltMs,
# ms since the app's start) while items were still missing.
function Get-S07Problems($Uia, $LayoutBuiltMs) {
    $p = @()
    if (-not $Uia.windows_found) { $p += "the app's windows never appeared to UI Automation" }
    elseif (-not $Uia.items_ready -and ($null -eq $LayoutBuiltMs -or [double]$Uia.read_app_ms -lt [double]$LayoutBuiltMs)) {
        $p += "the tree was read before the app's layout was built" }
    $p
}

# S0.7's verdict (the brief): PASS when the spike's own controls are read (the counter button,
# the chart tile, the announcement). The KDDockWidgets items are recorded; a FAIL there doesn't
# fail the check but is named for the owner (APP-156). $Problems (measurement conditions not met)
# make it INVALID first.
function Get-S07Verdict($Items, [string[]]$Problems = @()) {
    $problems = @($Problems | Where-Object { $_ })
    if ($problems.Count) { return "INVALID ($($problems -join '; '))" }
    $own = @($Items.Keys | Where-Object { -not $Items[$_].kddw })
    $bad = @($own | Where-Object { $Items[$_].verdict -match '^FAIL' })
    $invalid = @($own | Where-Object { $Items[$_].verdict -match '^INVALID' })
    $kddwNo = @($Items.Keys | Where-Object { $Items[$_].kddw -and $Items[$_].verdict -ne 'PASS' })
    $v = if ($bad.Count) { "FAIL ($($bad -join ', '))" } elseif ($invalid.Count) { "INVALID ($($invalid -join ', '))" } else { 'PASS' }
    if ($kddwNo.Count) { $v += "; KDDockWidgets not read, for the owner: $($kddwNo -join ', ')" }
    $v
}

# ---- a kill-on-close job object (comfy-load.ps1) -----------------------------------------------
# Every process in the job dies when the last handle to the job closes. comfy-load.ps1 creates
# one and holds its only handle (never inheritable), so if the owner closes its console window,
# everything the script started dies with it, and closing the handle in `finally` ends exactly
# the processes it put in the job. The script's own pwsh is not in the job.
#
# Children do NOT reliably join their parent's job by themselves here. The Store pwsh is an
# MSIX-packaged app, and Windows creates a packaged app's out-of-package children (cmd, python,
# nvidia-smi, the spike app) outside the package's environment and outside every job its parent
# is in. Measured on the owner's PC (fix round 4): from a packaged pwsh in a kill-on-close job,
# `cmd /c timeout` was in no job at all, while `pwsh -c Start-Sleep` (in the package) was in ours;
# from Windows PowerShell 5.1 (not packaged), cmd was in the job. So every process the script
# starts is created suspended, put in the job, and only then resumed (Start-ProcessInJob). A
# process that is in the job before it runs passes membership on to its own children (they are
# not packaged), so no later sweep is needed and nothing the script didn't start is ever put in.
$script:JobSource = @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public sealed class KillOnCloseJob : IDisposable {
    [StructLayout(LayoutKind.Sequential)] struct BASIC {
        public long PerProcessUserTimeLimit; public long PerJobUserTimeLimit; public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize; public UIntPtr MaximumWorkingSetSize; public uint ActiveProcessLimit;
        public UIntPtr Affinity; public uint PriorityClass; public uint SchedulingClass; }
    [StructLayout(LayoutKind.Sequential)] struct IO { public ulong a, b, c, d, e, f; }
    [StructLayout(LayoutKind.Sequential)] struct EXTENDED {
        public BASIC Basic; public IO Io; public UIntPtr ProcessMemoryLimit; public UIntPtr JobMemoryLimit;
        public UIntPtr PeakProcessMemoryUsed; public UIntPtr PeakJobMemoryUsed; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct STARTUPINFO {
        public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError; }
    [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
    [StructLayout(LayoutKind.Sequential)] struct SECURITY_ATTRIBUTES { public int nLength; public IntPtr lpSecurityDescriptor; public int bInheritHandle; }
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern IntPtr CreateJobObjectW(IntPtr attrs, string name);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetInformationJobObject(IntPtr job, int cls, ref EXTENDED info, int len);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool result);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool CreateProcessW(string app, StringBuilder cmd,
        IntPtr procAttrs, IntPtr threadAttrs, bool inheritHandles, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern IntPtr CreateFileW(string name, uint access, uint share,
        ref SECURITY_ATTRIBUTES sa, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)] static extern uint ResumeThread(IntPtr thread);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool TerminateProcess(IntPtr process, uint code);
    const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000;
    const uint CREATE_SUSPENDED = 0x4, CREATE_NO_WINDOW = 0x08000000;
    const int STARTF_USESTDHANDLES = 0x100;
    IntPtr handle;   // no finaliser: only Dispose() or the process's exit closes it
    public KillOnCloseJob() {
        handle = CreateJobObjectW(IntPtr.Zero, null);
        if (handle == IntPtr.Zero) throw new System.ComponentModel.Win32Exception();
        var info = new EXTENDED();
        info.Basic.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(handle, 9, ref info, Marshal.SizeOf(typeof(EXTENDED)))) {
            int e = Marshal.GetLastWin32Error(); CloseHandle(handle); handle = IntPtr.Zero;
            throw new System.ComponentModel.Win32Exception(e);
        }
    }
    // A closed job must fail loudly: IsProcessInJob with a NULL job means "in any job".
    void ThrowIfDisposed() { if (handle == IntPtr.Zero) throw new ObjectDisposedException("KillOnCloseJob"); }
    public void Assign(IntPtr process) {
        ThrowIfDisposed();
        if (!AssignProcessToJobObject(handle, process)) throw new System.ComponentModel.Win32Exception();
    }
    public bool Contains(IntPtr process) { ThrowIfDisposed(); bool r; return IsProcessInJob(process, handle, out r) && r; }
    public void Dispose() { if (handle != IntPtr.Zero) { CloseHandle(handle); handle = IntPtr.Zero; } }

    // One argument quoted the way CommandLineToArgvW and the C runtime read it back.
    public static string Quote(string a) {
        if (a.Length > 0 && a.IndexOfAny(new[] { ' ', '\t', '\n', '\v', '"' }) < 0) return a;
        var sb = new StringBuilder("\"");
        int slashes = 0;
        foreach (char c in a) {
            if (c == '\\') { slashes++; continue; }
            if (c == '"') sb.Append('\\', 2 * slashes + 1); else sb.Append('\\', slashes);
            slashes = 0; sb.Append(c);
        }
        sb.Append('\\', 2 * slashes).Append('"');
        return sb.ToString();
    }

    static IntPtr OpenStd(string path, bool write) {
        var sa = new SECURITY_ATTRIBUTES { nLength = Marshal.SizeOf(typeof(SECURITY_ATTRIBUTES)), bInheritHandle = 1 };
        // Writers truncate (like Start-Process -RedirectStandardOutput) and share read and write,
        // so the script can read a file (nvidia-smi's samples) while the child appends to it.
        IntPtr h = write ? CreateFileW(path, 0x40000000, 3, ref sa, 2, 0x80, IntPtr.Zero)    // GENERIC_WRITE, CREATE_ALWAYS
                         : CreateFileW(path, 0x80000000, 3, ref sa, 3, 0x80, IntPtr.Zero);   // GENERIC_READ, OPEN_EXISTING
        if (h == new IntPtr(-1)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "cannot open " + path);
        return h;
    }

    // Starts `app` with `commandLine` suspended, without a console window (a GUI exe ignores
    // that), puts it in `job` (none if null) and only then lets it run. With stdOut or stdErr,
    // stdin is NUL, the named streams go to those files and the other to NUL; handles are then
    // inherited (bInheritHandles must be TRUE for the redirect), otherwise nothing is. If the
    // process can't be put in the job it is ended before it ran. The returned Process holds its
    // own handle, opened while ours still pinned the pid, so its Id can't be reused under it.
    // beforeResume (tests only; null otherwise) sees the assigned, still suspended process.
    public static Process Start(KillOnCloseJob job, string app, string commandLine, string cwd, string stdOut, string stdErr,
                                Action<Process> beforeResume) {
        var si = new STARTUPINFO(); si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
        bool redirect = !string.IsNullOrEmpty(stdOut) || !string.IsNullOrEmpty(stdErr);
        var opened = new System.Collections.Generic.List<IntPtr>();
        PROCESS_INFORMATION pi;
        try {
            if (redirect) {
                si.dwFlags = STARTF_USESTDHANDLES;
                si.hStdInput = OpenStd("NUL", false); opened.Add(si.hStdInput);
                si.hStdOutput = OpenStd(string.IsNullOrEmpty(stdOut) ? "NUL" : stdOut, true); opened.Add(si.hStdOutput);
                si.hStdError = OpenStd(string.IsNullOrEmpty(stdErr) ? "NUL" : stdErr, true); opened.Add(si.hStdError);
            }
            if (!CreateProcessW(app, new StringBuilder(commandLine), IntPtr.Zero, IntPtr.Zero, redirect,
                                CREATE_SUSPENDED | CREATE_NO_WINDOW, IntPtr.Zero, string.IsNullOrEmpty(cwd) ? null : cwd, ref si, out pi))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "cannot start " + app);
        } finally {
            foreach (var h in opened) CloseHandle(h);   // the child has its own copies
        }
        Process p = null;
        try {
            if (job != null && !job.Contains(pi.hProcess)) job.Assign(pi.hProcess);
            p = Process.GetProcessById(pi.dwProcessId);
            var pinned = p.Handle;   // Process opens and keeps its own handle now
            if (beforeResume != null) beforeResume(p);
            if (ResumeThread(pi.hThread) == 0xFFFFFFFF) throw new System.ComponentModel.Win32Exception();
            return p;
        } catch {
            TerminateProcess(pi.hProcess, 1);   // never let it run outside the job
            if (p != null) p.Dispose();
            throw;
        } finally {
            CloseHandle(pi.hThread); CloseHandle(pi.hProcess);
        }
    }
}
'@

function Initialize-JobType {
    if (-not ('KillOnCloseJob' -as [type])) { Add-Type -TypeDefinition $script:JobSource }
}

# A new kill-on-close job; Add-ProcessToJob assigns a running process, Dispose() closes it.
function New-KillOnCloseJob { Initialize-JobType; [KillOnCloseJob]::new() }
function Add-ProcessToJob($Job, [System.Diagnostics.Process]$Process) { $Job.Assign($Process.Handle) }

# Is this process in the job?
function Test-ProcessInJob($Job, [System.Diagnostics.Process]$Process) { $Job.Contains($Process.Handle) }

# A command line from separate arguments, each quoted the way the child reads it back.
function ConvertTo-CommandLine([string[]]$Arguments) {
    Initialize-JobType
    (@($Arguments) | ForEach-Object { [KillOnCloseJob]::Quote("$_") }) -join ' '
}

# Starts $FilePath (a full path) with $ArgumentList inside $Job (or in no job if $Job is $null):
# created suspended, assigned, then resumed, so it and all its descendants are in the job from
# their first instruction. No console window. -StdOut/-StdErr redirect to files (stdin is then
# NUL); without them nothing is redirected. Returns a System.Diagnostics.Process. -BeforeResume
# is for the tests: it gets the Process while it is assigned and still suspended.
function Start-ProcessInJob {
    param($Job, [string]$FilePath, [string[]]$ArgumentList = @(), [string]$WorkingDirectory, [string]$StdOut, [string]$StdErr,
          [scriptblock]$BeforeResume)
    Initialize-JobType
    $hook = if ($BeforeResume) { [Action[System.Diagnostics.Process]]$BeforeResume } else { $null }
    [KillOnCloseJob]::Start($Job, $FilePath, (ConvertTo-CommandLine (@($FilePath) + @($ArgumentList))), $WorkingDirectory, $StdOut, $StdErr, $hook)
}

# The machine's CPU load (%), averaged over one second; recorded beside every timing.
function Get-CpuPercent {
    try { [math]::Round((Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 1).CounterSamples[0].CookedValue, 1) }
    catch { -1 }
}

Export-ModuleMember -Function Get-NearestRankStats, Test-GpuCoverage, Get-FrameThresholdMs, Get-DroppedFrames, Get-S06Verdict, Split-BuildWarnings,
    Get-OsPrimaryRefreshHz, Resolve-RefreshHz, Test-DeployCurrent, Test-GpuSaturatedRecently, Read-VramSamples,
    Get-ProcessesUnder, Get-SpikeAppProcesses, Test-DeployName, Test-S06Workload, Test-PlausibleRefresh, Complete-S06Verdict,
    Get-OwnerRunBlockers, Get-ResultsDir, Get-S06WebUrl, Test-S06Web, Get-S07Items, Get-S07Problems, Get-S07Verdict,
    New-KillOnCloseJob, Add-ProcessToJob, Test-ProcessInJob, ConvertTo-CommandLine, Start-ProcessInJob,
    Start-GpuSampler, Read-GpuSamples, Start-VramSampler, Read-JsonLines, Get-CpuPercent
