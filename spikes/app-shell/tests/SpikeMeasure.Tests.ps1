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