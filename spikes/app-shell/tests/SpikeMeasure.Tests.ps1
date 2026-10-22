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
    It 'flags a sample under 95 %' {
        $s = @(New-Samples 0 60000 1000)
        $s[10].util = 94
        (Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000).saturated | Should -BeFalse
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
    It 'allows one vsync period, not a dropped frame, at 60 Hz' {
        $t = Get-FrameThresholdMs 60
        $t | Should -BeGreaterThan (1000 / 60)
        $t | Should -BeLessThan (2000 / 60)
    }
    It 'returns nothing without a refresh rate' {
        Get-FrameThresholdMs 0 | Should -BeNullOrEmpty
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
    It 'keeps the 16 ms bar at 240 Hz' {
        $slow = [ordered]@{ n = 14000; p50 = 4.2; p95 = 8; p99 = 17; max = 30 }
        Get-S06Verdict -HasMeasuredReport $true -ExitCode 0 -TimedOut $false -Heartbeat $fast -Frames $slow -RefreshHz 240 -Coverage $good |
            Should -Match '^FAIL'
    }
}
