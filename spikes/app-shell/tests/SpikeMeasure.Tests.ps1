# Pester tests of the pure helpers in lib/SpikeMeasure.psm1 (S0.6's sample coverage, frame
# threshold and verdict). Run: Invoke-Pester spikes/app-shell/tests/SpikeMeasure.Tests.ps1
BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\lib\SpikeMeasure.psm1') -Force
    function New-Samples([long]$From, [long]$To, [int]$StepMs, [int]$Util = 99) {
        for ($t = $From; $t -le $To; $t += $StepMs) { [pscustomobject]@{ t = $t; util = $Util } }
    }
}

Describe 'Test-GpuCoverage' {
    It 'accepts 1 Hz samples across 60 s' {
        $c = Test-GpuCoverage -Samples @(New-Samples 0 60000 1000) -StartMs 0 -EndMs 60000
        $c.n | Should -Be 61
        $c.needed | Should -Be 48
        $c.max_gap_ms | Should -Be 1000
        $c.dense | Should -BeTrue
        $c.saturated | Should -BeTrue
    }
    It 'rejects the old loop''s ~2 s cadence (30 samples in 60 s)' {
        $c = Test-GpuCoverage -Samples @(New-Samples 0 60000 2050) -StartMs 0 -EndMs 60000
        $c.n | Should -BeLessThan 48
        $c.dense | Should -BeFalse
    }
    It 'rejects a dense series with one long hole' {
        $s = @(New-Samples 0 20000 500) + @(New-Samples 25000 60000 500)
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.n | Should -BeGreaterOrEqual 48
        $c.max_gap_ms | Should -Be 5000
        $c.dense | Should -BeFalse
    }
    It 'counts the window edges as gaps' {
        $c = Test-GpuCoverage -Samples @(New-Samples 5000 60000 500) -StartMs 0 -EndMs 60000
        $c.max_gap_ms | Should -Be 5000
        $c.dense | Should -BeFalse
    }
    It 'ignores samples outside the window' {
        $s = @(New-Samples -30000 -1000 500 10) + @(New-Samples 0 60000 1000)
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.below_min | Should -Be 0
        $c.saturated | Should -BeTrue
    }
    It 'allows brief dips (>= 95 % in at least 90 % of the samples) and records them' {
        $s = @(New-Samples 0 60000 1000)
        foreach ($k in 10, 30, 50) { $s[$k].util = 60 }
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.saturated | Should -BeTrue
        $c.below_min | Should -Be 3
        $c.dips.Count | Should -Be 3
        $c.dips[0].util | Should -Be 60
    }
    It 'rejects a run with more than 10 % of the samples under 95 %' {
        $s = @(New-Samples 0 60000 1000)
        for ($k = 0; $k -lt 7; $k++) { $s[$k * 8].util = 80 }
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.saturated_fraction | Should -BeLessThan 0.9
        $c.saturated | Should -BeFalse
    }
    It 'reports an empty series as neither dense nor saturated' {
        $c = Test-GpuCoverage -Samples @() -StartMs 0 -EndMs 60000
        $c.n | Should -Be 0
        $c.dense | Should -BeFalse
        $c.saturated | Should -BeFalse
    }
}

Describe 'Get-FrameThresholdMs' {
    It 'keeps 16 ms at 240 Hz and 144 Hz' {
        Get-FrameThresholdMs 240 | Should -Be 16
        Get-FrameThresholdMs 144 | Should -Be 16
    }
    It 'is 1.5 vsync periods below 66.7 Hz: 25 ms at 60 Hz, 20 ms at 75 Hz' {
        Get-FrameThresholdMs 60 | Should -Be 25
        Get-FrameThresholdMs 75 | Should -Be 20
    }
    It 'has no cliff at 62.5/63 Hz' {
        Get-FrameThresholdMs 62.5 | Should -Be 24
        Get-FrameThresholdMs 63 | Should -BeGreaterThan 23.5
    }
    It 'always fails a dropped vsync and passes one vsync period' {
        foreach ($hz in 50, 60, 75, 144, 240) {
            $t = Get-FrameThresholdMs $hz
            $t | Should -BeGreaterThan (1000 / $hz)
            if ($hz -lt 125) { $t | Should -BeLessThan (2000 / $hz) }
        }
    }
    It 'returns nothing without a refresh rate' {
        Get-FrameThresholdMs 0 | Should -BeNullOrEmpty
    }
}

Describe 'Get-DroppedFrames' {
    It 'counts frames over 1.5 vsync periods' {
        $d = Get-DroppedFrames @(13.3, 13.4, 26.7, 13.3) 75
        $d.over_1_5_vsync | Should -Be 1
        $d.fraction | Should -Be 0.25
    }
}

Describe 'Get-NearestRankStats' {
    It 'returns null for an empty series, not n = 1' {
        Get-NearestRankStats @() | Should -BeNullOrEmpty
        Get-NearestRankStats @($null) | Should -BeNullOrEmpty
    }
    It 'uses nearest rank' {
        $s = Get-NearestRankStats @(1..100)
        $s.p50 | Should -Be 50
        $s.p99 | Should -Be 99
        $s.max | Should -Be 100
    }
}

Describe 'Get-S06Verdict' {
    BeforeAll {
        $good = Test-GpuCoverage -Samples @(New-Samples 0 60000 1000) -StartMs 0 -EndMs 60000
        $sparse = Test-GpuCoverage -Samples @(New-Samples 0 60000 2050) -StartMs 0 -EndMs 60000
        $fast = [ordered]@{ n = 100; p50 = 1; p95 = 2; p99 = 3; max = 5 }
    }
    It 'fails a hung app before looking at the GPU' {
        Get-S06Verdict -HasMeasuredReport $false -ExitCode $null -TimedOut $true -Heartbeat $null -Frames $null -RefreshHz 240 -Coverage $sparse |
            Should -Match '^FAIL'
    }
    It 'fails a crash' {
        Get-S06Verdict -HasMeasuredReport $false -ExitCode -1073741819 -TimedOut $false -Heartbeat $null -Frames $null -RefreshHz 240 -Coverage $good |
            Should -Match '^FAIL'
    }
    It 'fails a run with no measured report' {
        Get-S06Verdict -HasMeasuredReport $false -ExitCode 0 -TimedOut $false -Heartbeat $null -Frames $null -RefreshHz 240 -Coverage $good |
            Should -Match '^FAIL'
    }
    It 'marks sparse GPU samples INVALID' {
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $fast -RefreshHz 240 -Coverage $sparse |
            Should -Match '^INVALID'
    }
    It 'passes a vsync-locked 60 Hz run' {
        $vsync60 = [ordered]@{ n = 3600; p50 = 16.7; p95 = 16.8; p99 = 17.0; max = 18 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $vsync60 -RefreshHz 60 -Coverage $good |
            Should -Be 'PASS'
    }
    It 'fails dropped frames at 60 Hz' {
        $dropped = [ordered]@{ n = 3600; p50 = 16.7; p95 = 33.3; p99 = 33.4; max = 50 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $dropped -RefreshHz 60 -Coverage $good |
            Should -Match '^FAIL'
    }
    It 'judges 75 Hz against 20 ms' {
        $jitter = [ordered]@{ n = 4500; p50 = 13.3; p95 = 14; p99 = 16.8; max = 26.5 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $jitter -RefreshHz 75 -Coverage $good |
            Should -Be 'PASS'
        $dropped = [ordered]@{ n = 4500; p50 = 13.3; p95 = 26.7; p99 = 26.7; max = 40 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $dropped -RefreshHz 75 -Coverage $good |
            Should -Match '^FAIL'
    }
    It 'keeps the heartbeat bar at 16 ms whatever the display' {
        $slowHb = [ordered]@{ n = 15000; p50 = 0.1; p95 = 2; p99 = 16.5; max = 40 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $slowHb -Frames $fast -RefreshHz 60 -Coverage $good |
            Should -Match '^FAIL'
    }
    It 'keeps the 16 ms bar at 240 Hz' {
        $slow = [ordered]@{ n = 14000; p50 = 4.2; p95 = 8; p99 = 17; max = 30 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $slow -RefreshHz 240 -Coverage $good |
            Should -Match '^FAIL'
    }
}

Describe 'Split-BuildWarnings' {
    BeforeAll {
        $qtWarning = @(
            '-- Configuring',
            'CMake Warning at C:/Program Files/CMake/share/cmake-4.4/Modules/CMakeFindDependencyMacro.cmake:93 (find_package):',
            '  Found package configuration file:',
            '',
            '    C:/Qt/6.12.0/msvc2022_64/lib/cmake/Qt6QmlAssetDownloaderPrivate/Qt6QmlAssetDownloaderPrivateConfig.cmake',
            '',
            '  but it set Qt6QmlAssetDownloaderPrivate_FOUND to FALSE.',
            '',
            'Call Stack (most recent call first):',
            '  C:/Qt/6.12.0/msvc2022_64/lib/cmake/Qt6/Qt6Config.cmake:257 (find_package)',
            '  CMakeLists.txt:29 (find_package)',
            '',
            '-- Qt 6.12.0 at C:/Qt/6.12.0/msvc2022_64/lib/cmake/Qt6')
        $ownWarning = @(
            'CMake Warning (dev) at CMakeLists.txt:42 (set):',
            '  Something about our own file.',
            'Call Stack (most recent call first):',
            '  CMakeLists.txt:42 (set)',
            'This warning is for project developers.  Use -Wno-dev to suppress it.',
            '',
            '-- Configuring done')
    }
    It 'keeps the call stack in the block' {
        $w = Split-BuildWarnings $qtWarning
        $w.toolchain.Count | Should -Be 1
        $w.toolchain[0].text | Should -Match 'Call Stack \(most recent call first\):'
        $w.toolchain[0].text | Should -Match 'CMakeLists\.txt:29 \(find_package\)'
        $w.toolchain[0].text | Should -Not -Match '-- Qt 6\.12\.0'
    }
    It 'classifies by where the warning is raised, and notes a call through our file' {
        $w = Split-BuildWarnings $qtWarning
        $w.own.Count | Should -Be 0
        $w.toolchain[0].via_own_file | Should -BeTrue
    }
    It 'matches our CMakeLists.txt by its relative path' {
        $w = Split-BuildWarnings $ownWarning
        $w.own.Count | Should -Be 1
        $w.own[0].origin | Should -Be 'CMakeLists.txt'
        $w.own[0].text | Should -Match 'This warning is for project developers'
        $w.own[0].text | Should -Not -Match 'Configuring done'
    }
    It 'puts a compiler warning in a KDDockWidgets source under kddw, and one in ours under own' {
        $w = Split-BuildWarnings @(
            'C:\work\spikes\app-shell\build\cmake\_deps\kddockwidgets-src\src\core\Group.cpp(120): warning C4996: deprecated',
            'C:\work\spikes\app-shell\shim\Heartbeat.cpp(40): warning C4267: conversion')
        $w.kddw.Count | Should -Be 1
        $w.own.Count | Should -Be 1
        $w.own[0].origin | Should -Match 'Heartbeat\.cpp'
    }
}

Describe 'Resolve-RefreshHz' {
    It 'prefers the app''s primary_refresh_hz' {
        $r = Resolve-RefreshHz ([pscustomobject]@{ primary_refresh_hz = 239.98 }) 240
        $r.hz | Should -Be 239.98
        $r.source | Should -Be 'app'
    }
    It 'reads the older report format (the owner''s first S0.6 run)' {
        $old = [pscustomobject]@{ screens = @([pscustomobject]@{ window = 'main-a'; refresh_hz = 239.984; primary = $true }) }
        $r = Resolve-RefreshHz $old 0
        $r.hz | Should -Be 239.984
        $r.source | Should -Match 'older'
    }
    It 'falls back to the Windows value, and says so' {
        $r = Resolve-RefreshHz ([pscustomobject]@{ primary_refresh_hz = 0 }) 75
        $r.hz | Should -Be 75
        $r.source | Should -Be 'windows'
    }
    It 'reports none when nothing is known' {
        (Resolve-RefreshHz $null 0).source | Should -Be 'none'
    }
}

Describe 'Get-OsPrimaryRefreshHz' {
    # Needs a real desktop: skipped in a service session (session 0) or a non-interactive one.
    It 'reads a plausible refresh rate for the primary display' -Skip:(-not [Environment]::UserInteractive -or (Get-Process -Id $PID).SessionId -eq 0) {
        Test-PlausibleRefresh (Get-OsPrimaryRefreshHz) | Should -BeTrue
    }
}

Describe 'Refresh-rate sanity' {
    It 'treats 0, 1 ("hardware default") and anything under 24 Hz as unknown' {
        foreach ($hz in 0, 1, 23.9) { Test-PlausibleRefresh $hz | Should -BeFalse }
        foreach ($hz in 24, 60, 240) { Test-PlausibleRefresh $hz | Should -BeTrue }
        (Resolve-RefreshHz ([pscustomobject]@{ primary_refresh_hz = 1 }) 1).source | Should -Be 'none'
    }
    It 'flags the app and Windows disagreeing by more than 2 Hz' {
        (Resolve-RefreshHz ([pscustomobject]@{ primary_refresh_hz = 239.984 }) 240).mismatch | Should -BeFalse
        (Resolve-RefreshHz ([pscustomobject]@{ primary_refresh_hz = 60 }) 240).mismatch | Should -BeTrue
    }
}

Describe 'Guards' {
    BeforeAll {
        $procs = @(
            [pscustomobject]@{ ProcessId = 1; Name = 'app_shell_spike.exe'; ExecutablePath = 'C:\work\spikes\app-shell\build\deploy\app_shell_spike.exe' },
            [pscustomobject]@{ ProcessId = 2; Name = 'QtWebEngineProcess.exe'; ExecutablePath = 'C:\work\spikes\app-shell\build\deploy\QtWebEngineProcess.exe' },
            [pscustomobject]@{ ProcessId = 3; Name = 'tst_app_shell.exe'; ExecutablePath = 'C:\work\spikes\app-shell\build\cmake\bin\tst_app_shell.exe' },
            [pscustomobject]@{ ProcessId = 4; Name = 'notepad.exe'; ExecutablePath = 'C:\Windows\notepad.exe' },
            [pscustomobject]@{ ProcessId = 5; Name = 'elevated.exe'; ExecutablePath = $null })
    }
    It 'finds every process under a folder whatever its name, by full path' {
        @(Get-ProcessesUnder 'C:\work\spikes\app-shell\build\deploy' $procs).ProcessId | Should -Be @(1, 2)
        @(Get-ProcessesUnder 'C:\work\spikes\app-shell\build\cmake\..\deploy' $procs).ProcessId | Should -Be @(1, 2)
        @(Get-ProcessesUnder 'C:\work\spikes\app-shell\build' $procs).ProcessId | Should -Be @(1, 2, 3)
    }
    It 'does not match a sibling folder that shares a prefix' {
        @(Get-ProcessesUnder 'C:\work\spikes\app-shell\build\dep' $procs).Count | Should -Be 0
    }
    It 'finds spike app instances anywhere' {
        @(Get-SpikeAppProcesses $procs).ProcessId | Should -Be @(1, 3)
    }
    It 'accepts only deploy and deploy-NAME (letters and digits) as a deploy folder name' {
        foreach ($ok in 'deploy', 'deploy-check', 'deploy-2') { Test-DeployName $ok | Should -BeTrue }
        foreach ($bad in '.', '..', 'runs', 'cmake', 'cargo-lint', 'sizecheck', 'deploy\..', 'deploy-', 'deploy-a/b', 'Deploy', '') {
            Test-DeployName $bad | Should -BeFalse
        }
    }
}

Describe 'S0.6 workload check' {
    BeforeAll {
        $good = [pscustomobject]@{ grid_visible_seconds = 60; grid_hidden_seconds = 0; feed_ticks = 640; decoded = 5100
                                   chart_points = 390; grid_rows_scrolled = 12900; web_shown = $true }
    }
    It 'accepts a full workload' {
        @(Test-S06Workload $good 60 10).Count | Should -Be 0
    }
    It 'rejects a missing load_counts' {
        Test-S06Workload $null 60 10 | Should -Be @('no load_counts logged')
    }
    It 'rejects zero counts (previews not decoding, feed not started, list not visible)' {
        $zero = [pscustomobject]@{ grid_visible_seconds = 0; grid_hidden_seconds = 0; feed_ticks = 0; decoded = 0
                                   chart_points = 0; grid_rows_scrolled = 0; web_shown = $false }
        @(Test-S06Workload $zero 60 10).Count | Should -Be 6   # visible, feed, decoded, charts, rows, web
    }
    It 'scales the expected counts with the fallback''s 5 Hz' {
        $half = [pscustomobject]@{ grid_visible_seconds = 60; grid_hidden_seconds = 0; feed_ticks = 160; decoded = 1300
                                   chart_points = 390; grid_rows_scrolled = 12900; web_shown = $true }
        @(Test-S06Workload $half 60 5).Count | Should -Be 0
        @(Test-S06Workload $half 60 10).Count | Should -BeGreaterThan 0
    }
    It 'turns a PASS on missing data into INVALID, and leaves a FAIL alone' {
        Complete-S06Verdict 'PASS' @('no load_counts logged') $false | Should -Match '^INVALID \(workload'
        Complete-S06Verdict 'PASS' @() $true | Should -Match '^INVALID \(the app and Windows'
        Complete-S06Verdict 'FAIL (heartbeat p99 20 ms)' @('no load_counts logged') $true | Should -Match '^FAIL'
        Complete-S06Verdict 'PASS' @() $false | Should -Be 'PASS'
    }
}

Describe 'Test-GpuSaturatedRecently (the gate before the measured minute)' {
    It 'holds the measurement back for the owner''s one-prompt-at-a-time pattern (15 of 20 samples >= 95 %)' {
        $s = for ($k = 0; $k -lt 40; $k++) { [pscustomobject]@{ t = 1000 + $k * 500; util = $(if ($k % 4 -eq 0) { 5 } else { 99 }) } }
        Test-GpuSaturatedRecently $s 21000 | Should -BeFalse
    }
    It 'opens for a queued load with rare dips' {
        $s = for ($k = 0; $k -lt 40; $k++) { [pscustomobject]@{ t = 1000 + $k * 500; util = $(if ($k -eq 17) { 40 } else { 99 }) } }
        Test-GpuSaturatedRecently $s 21000 | Should -BeTrue
    }
    It 'stays shut with no samples yet' {
        Test-GpuSaturatedRecently @() 21000 | Should -BeFalse
    }
    It 'stays shut when the sampler died (no sample in the last few seconds)' {
        $s = for ($k = 0; $k -lt 36; $k++) { [pscustomobject]@{ t = 1000 + $k * 500; util = 99 } }   # last at 18.5 s
        Test-GpuSaturatedRecently $s 21000 | Should -BeFalse
    }
}

Describe 'Test-DeployCurrent' {
    BeforeAll {
        $testRoot = Join-Path $TestDrive 'deploycheck'
        $bin = Join-Path $testRoot 'bin'; $dep = Join-Path $testRoot 'deploy'
        foreach ($d in $bin, $dep) {
            New-Item -ItemType Directory -Force (Join-Path $d 'SpikeWeb') | Out-Null
            foreach ($f in 'app_shell_spike.exe', 'spike_shell_shim.dll', 'kddockwidgets-qt62.dll', 'SpikeWeb\spike_web.dll') { Set-Content (Join-Path $d $f) 'same' }
        }
    }
    It 'accepts a deploy identical to the build' {
        @(Test-DeployCurrent $dep $bin).Count | Should -Be 0
    }
    It 'names a stale file' {
        Set-Content (Join-Path $bin 'spike_shell_shim.dll') 'rebuilt'
        Test-DeployCurrent $dep $bin | Should -Be @('spike_shell_shim.dll')
    }
    It 'names a file missing from the deploy folder' {
        Remove-Item -LiteralPath (Join-Path $dep 'SpikeWeb\spike_web.dll')
        Test-DeployCurrent $dep $bin | Should -Contain 'SpikeWeb\spike_web.dll'
    }
}

Describe 'VRAM sampling' {
    It 'keeps sampling while the process has no GPU instance yet, and Read-VramSamples drops the retries' {
        $out = Join-Path $TestDrive 'vram.jsonl'
        # A pid with no GPU Process Memory instance: the old -Continuous sampler ended at once here.
        $job = Start-VramSampler 999999 $out
        Start-Sleep -Seconds 4
        $job.State | Should -Be 'Running'
        Stop-Job $job
        Remove-Job $job -Force
        @(Read-JsonLines $out | Where-Object { $_.missing }).Count | Should -BeGreaterThan 0
        @(Read-VramSamples $out).Count | Should -Be 0
    }
}

Describe 'Kill-on-close job (comfy-load.ps1 never leaves ComfyUI or nvidia-smi behind)' {
    # Harmless children only: hidden pwsh processes that sleep. Every one is ours and is killed in
    # AfterAll if a test left it alive.
    BeforeAll {
        $script:ours = [System.Collections.Generic.List[int]]::new()
        function Start-Sleeper { $p = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 60' -WindowStyle Hidden -PassThru; $script:ours.Add($p.Id); $p }
        function Wait-Gone([int]$ProcessId, [int]$Ms = 5000) {
            $sw = [Diagnostics.Stopwatch]::StartNew()
            while ($sw.ElapsedMilliseconds -lt $Ms) { if (-not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { return $true }; Start-Sleep -Milliseconds 100 }
            $false
        }
        $module = Join-Path $PSScriptRoot '..\lib\SpikeMeasure.psm1'
    }
    AfterAll {
        foreach ($id in $script:ours) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
    }
    It 'kills an assigned process when the job handle closes, and leaves an unassigned one alone' {
        $job = New-KillOnCloseJob
        $inJob = Start-Sleeper
        $outside = Start-Sleeper
        Add-ProcessToJob $job $inJob
        $job.Dispose()
        Wait-Gone $inJob.Id | Should -BeTrue
        Get-Process -Id $outside.Id -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
    }
    It 'kills what a process started after putting itself in the job, when that process is killed (a closed console)' {
        $pidFile = Join-Path $TestDrive 'grandchild.pid'
        $helperScript = Join-Path $TestDrive 'helper.ps1'
        Set-Content $helperScript @"
Import-Module '$module'
Enable-KillOnCloseForSelf | Out-Null
`$c = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 60' -WindowStyle Hidden -PassThru
Set-Content '$pidFile' `$c.Id
Start-Sleep 60
"@
        $helper = Start-Process pwsh -ArgumentList '-NoProfile', '-File', $helperScript -WindowStyle Hidden -PassThru
        $script:ours.Add($helper.Id)
        $sw = [Diagnostics.Stopwatch]::StartNew()
        while (-not (Test-Path $pidFile) -and $sw.ElapsedMilliseconds -lt 30000) { Start-Sleep -Milliseconds 200 }
        $grandchild = [int](Get-Content $pidFile)
        $script:ours.Add($grandchild)
        Get-Process -Id $grandchild -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
        Stop-Process -Id $helper.Id -Force   # what closing the console window does to the script
        Wait-Gone $grandchild | Should -BeTrue
    }
    It 'assigns a started process and its descendants (not others) with Add-TreeToJob' {
        $pidFile = Join-Path $TestDrive 'tree.pid'
        $parentScript = Join-Path $TestDrive 'parent.ps1'
        Set-Content $parentScript @"
`$c = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 60' -WindowStyle Hidden -PassThru
Set-Content '$pidFile' `$c.Id
Start-Sleep 60
"@
        $parent = Start-Process pwsh -ArgumentList '-NoProfile', '-File', $parentScript -WindowStyle Hidden -PassThru
        $script:ours.Add($parent.Id)
        $bystander = Start-Sleeper
        $sw = [Diagnostics.Stopwatch]::StartNew()
        while (-not (Test-Path $pidFile) -and $sw.ElapsedMilliseconds -lt 30000) { Start-Sleep -Milliseconds 200 }
        $child = [int](Get-Content $pidFile)
        $script:ours.Add($child)
        $job = New-KillOnCloseJob
        Add-TreeToJob $job $parent.Id | Should -Be 2
        Test-ProcessInJob $job (Get-Process -Id $child) | Should -BeTrue
        Test-ProcessInJob $job $bystander | Should -BeFalse
        $job.Dispose()
        Wait-Gone $parent.Id | Should -BeTrue
        Wait-Gone $child | Should -BeTrue
        Get-Process -Id $bystander.Id -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
    }
}