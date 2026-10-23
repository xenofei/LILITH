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
    It 'allows brief dips (>= 90 % in at least 90 % of the samples) and records them' {
        $s = @(New-Samples 0 60000 1000)
        foreach ($k in 10, 30, 50) { $s[$k].util = 60 }
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.saturated | Should -BeTrue
        $c.below_min | Should -Be 3
        $c.dips.Count | Should -Be 3
        $c.dips[0].util | Should -Be 60
    }
    It 'rejects a run with more than 10 % of the samples under 90 %' {
        $s = @(New-Samples 0 60000 1000)
        for ($k = 0; $k -lt 7; $k++) { $s[$k * 8].util = 80 }
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.saturated_fraction | Should -BeLessThan 0.9
        $c.saturated | Should -BeFalse
    }
    It 'counts owner run 3''s pattern as loaded: 99 % with a 92-93 % sample every 3 s and a dip at each prompt (12.5 s)' {
        $s = @(New-Samples 0 60000 500)
        for ($k = 0; $k -lt $s.Count; $k++) { if ($k % 6 -eq 5) { $s[$k].util = 93 } else { $s[$k].util = 99 } }
        foreach ($k in 20, 45, 70, 95, 120) { if ($k -lt $s.Count) { $s[$k].util = 6 } }
        $c = Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000
        $c.level | Should -Be 90
        $c.saturated | Should -BeTrue
        (Test-GpuCoverage -Samples $s -StartMs 0 -EndMs 60000 -MinUtil 95).saturated | Should -BeFalse   # the old level
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
        foreach ($bad in '.', '..', 'runs', 'cmake', 'cargo-lint', 'sizecheck', 'deploy\..', 'deploy-', 'deploy-a/b', 'Deploy', "deploy`n", '') {
            Test-DeployName $bad | Should -BeFalse
        }
    }
}

Describe 'S0.7 UI Automation items' {
    BeforeAll {
        # Rows as s07-uia.ps1 writes them (taken from a real run, trimmed).
        function Row($w, $n, $t, $f = 'Qt', $procId = 42) {
            [pscustomobject]@{ window = "LILITH spike - $w"; name = $n; control_type = "ControlType.$t"; localized_control_type = $t.ToLower()
                               framework_id = $f; process_id = $procId } }
        $script:els = @(
            (Row 'main-a' 'LILITH spike - main-a' 'TitleBar' 'Win32'),
            (Row 'main-a' 'Jobs' 'TitleBar'), (Row 'main-a' 'Grid' 'TitleBar'),
            (Row 'main-a' 'Grid' 'TabItem'), (Row 'main-a' 'Web' 'TabItem'),
            (Row 'main-b' 'Increment the counter' 'Button'),
            (Row 'main-b' 'GPU load chart, latest 44.0 %' 'Custom'),
            (Row 'main-b' 'VRAM chart, latest 7.2 GiB' 'Custom'))
        $script:note = @([pscustomobject]@{ display_string = 'Count is now 1'; process_id = 42; kind = 'Other'; processing = 'All'
                                            source_name = 'Increment the counter'; source_control_type = 'ControlType.Button' })
        function Uia($elements = $els, $notifications = $note, $invoked = $true, $found = $true, $ready = $true, $readMs = 2500) {
            [pscustomobject]@{ elements = $elements; notifications = $notifications; invoked = $invoked; invoke_error = 'no button'
                               windows_found = $found; items_ready = $ready; read_app_ms = $readMs } }
    }
    It 'passes every item of a full read, and records the chart''s ControlType' {
        $items = Get-S07Items (Uia) 42
        @($items.Keys | Where-Object { $items[$_].verdict -ne 'PASS' }).Count | Should -Be 0
        $items.chart_tile.found[0].control_type | Should -Be 'ControlType.Custom'
        Get-S07Verdict $items @(Get-S07Problems (Uia) 1750) | Should -Be 'PASS'
    }
    It 'counts only Qt''s own elements of the app''s process (not the Win32 title bar or another process) and wants both tabs in main-a' {
        $u = Uia @((Row 'main-a' 'Jobs' 'TitleBar' 'Win32'), (Row 'main-a' 'Grid' 'TitleBar' 'Qt' 7), (Row 'main-a' 'Grid' 'TabItem'),
                   (Row 'main-b' 'Web' 'TabItem'), (Row 'main-b' 'Increment the counter' 'Button'), (Row 'main-b' 'GPU load chart, latest 1 %' 'Custom'))
        $items = Get-S07Items $u 42
        $items.kddw_title_bar.verdict | Should -Be 'FAIL'
        $items.kddw_dock_tabs.verdict | Should -Be 'FAIL'
        # The spike's own controls pass, so the check passes and names the KDDockWidgets items for the owner.
        Get-S07Verdict $items | Should -Be 'PASS; KDDockWidgets not read, for the owner: kddw_title_bar, kddw_dock_tabs'
    }
    It 'fails a missing announcement or one from another process, and is INVALID when the client never clicked' {
        $other = Uia -notifications @([pscustomobject]@{ display_string = 'Count is now 1'; process_id = 7 })
        Get-S07Verdict (Get-S07Items $other 42) | Should -Be 'FAIL (announce)'
        Get-S07Verdict (Get-S07Items (Uia -notifications @() -invoked $false) 42) | Should -Match '^INVALID \(announce\)'
    }
    It 'fails a counter button without its accessible name' {
        $u = Uia (@($els | Where-Object { $_.name -ne 'Increment the counter' }) + @(Row 'main-b' 'Increment' 'Button'))
        Get-S07Verdict (Get-S07Items $u 42) | Should -Match '^FAIL \(counter_button'
    }
    It 'is INVALID, not FAIL, when nothing was measured: no windows, or items read before the layout was built' {
        $none = Uia @() @() $false $false $false 20000
        Get-S07Verdict (Get-S07Items $none 42) @(Get-S07Problems $none 1750) | Should -Match '^INVALID \(the app''s windows never appeared'
        $early = Uia @() @() $false $true $false 900
        Get-S07Problems $early 1750 | Should -Match 'before the app''s layout was built'
        Get-S07Problems $early $null | Should -Match 'before the app''s layout was built'
        # Read after the layout was built and still missing: a real FAIL.
        $late = Uia @() @() $false $true $false 16000
        @(Get-S07Problems $late 1750).Count | Should -Be 0
        Get-S07Verdict (Get-S07Items $late 42) @(Get-S07Problems $late 1750) | Should -Match '^FAIL'
    }
}

Describe 'Results folder (agent and test runs never write the owner''s results)' {
    It 'writes results/ only for the owner''s runs, else results/agent-runs/(stamp)/, and an explicit folder wins' {
        Get-ResultsDir 'C:\x' '' $true '20261023-1' | Should -Be 'C:\x\results'
        Get-ResultsDir 'C:\x' '' $false '20261023-1' | Should -Be 'C:\x\results\agent-runs\20261023-1'
        Get-ResultsDir 'C:\x' 'D:\elsewhere' $true '20261023-1' | Should -Be 'D:\elsewhere'
    }
    It 'leaves results/checks.json and results/s07.json untouched in a harness run on another deploy folder' {
        # A real run-spike.ps1 that checks and launches nothing (-NoBuild, -Only none) but still
        # writes its checks.json; the folders it creates are removed afterwards (found by name, new).
        $spike = Join-Path $PSScriptRoot '..'
        $res = Join-Path $spike 'results'
        $files = 'checks.json', 's07.json'
        $before = @{}; foreach ($f in $files) { $p = Join-Path $res $f; $before[$f] = if (Test-Path $p) { (Get-FileHash $p).Hash + (Get-Item $p).LastWriteTimeUtc.Ticks } else { 'absent' } }
        $agentDir = Join-Path $res 'agent-runs'; $runsDir = Join-Path $spike 'build\runs'
        $oldAgent = @(Get-ChildItem $agentDir -Directory -ErrorAction SilentlyContinue | ForEach-Object Name)
        $oldRuns = @(Get-ChildItem $runsDir -Directory -ErrorAction SilentlyContinue | ForEach-Object Name)
        & pwsh -NoProfile -File (Join-Path $spike 'run-spike.ps1') -NoBuild -DeployName deploy-pester -Only none *> $null
        $newAgent = @(Get-ChildItem $agentDir -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -notin $oldAgent })
        $newRuns = @(Get-ChildItem $runsDir -Directory -ErrorAction SilentlyContinue | Where-Object { $_.Name -notin $oldRuns })
        try {
            foreach ($f in $files) {
                $p = Join-Path $res $f
                $now = if (Test-Path $p) { (Get-FileHash $p).Hash + (Get-Item $p).LastWriteTimeUtc.Ticks } else { 'absent' }
                $now | Should -Be $before[$f]
            }
            $newAgent.Count | Should -Be 1
            Test-Path (Join-Path $newAgent[0].FullName 'checks.json') | Should -BeTrue
        } finally {
            foreach ($d in @($newAgent) + @($newRuns)) { Remove-Item -Recurse -Force $d.FullName -ErrorAction SilentlyContinue }
        }
    }
}

Describe 'run-owner.ps1 refusal (by path, never by name)' {
    BeforeAll {
        $main = 'C:\work\ComfyUI\ComfyUI\main.py'; $build = 'C:\work\spikes\app-shell\build'
        function P($id, $path, $cmd = '') { [pscustomobject]@{ ProcessId = $id; Name = 'x.exe'; ExecutablePath = $path; CommandLine = $cmd } }
    }
    It 'runs when nothing of the spike or ComfyUI is running, whatever the names' {
        $procs = @((P 1 'C:\Windows\notepad.exe'), (P 2 'C:\other\app_shell_spike.exe'), (P 3 'C:\py\python.exe' 'python other.py'), (P 4 $null))
        @(Get-OwnerRunBlockers $build $main $procs).Count | Should -Be 0
    }
    It 'refuses a spike process under build (any name), the ComfyUI venv python, and a base interpreter running main.py' {
        $procs = @((P 5 "$build\deploy\QtWebEngineProcess.exe"), (P 6 'C:\work\ComfyUI\.venv\Scripts\python.exe'),
                   (P 7 'C:\uv\python\cpython-3.12\python.exe' 'python.exe C:\work\ComfyUI\ComfyUI\main.py --listen 127.0.0.1'))
        $why = @(Get-OwnerRunBlockers $build $main $procs)
        $why.Count | Should -Be 2
        $why[0] | Should -Match 'pid 5'
        $why[1] | Should -Match 'ComfyUI is running \(pid 6, 7\)'
    }
}

Describe 'Get-OwnerRunBlockers and the harness''s own command lines (owner run 2)' {
    BeforeAll {
        $main = 'C:\work\ComfyUI\ComfyUI\main.py'; $build = 'C:\work\spikes\app-shell\build'
        $t0 = [datetime]'2026-10-23T11:00:00'
        function Q($id, $parent, $min, $path, $cmd) { [pscustomobject]@{ ProcessId = $id; ParentProcessId = $parent; CreationDate = $t0.AddMinutes($min)
                                                                           Name = 'x.exe'; ExecutablePath = $path; CommandLine = $cmd } }
        # The owner's shell (80) ran run-owner (90), which started comfy-load (100) with -ComfyMain.
        $script:tree = @(
            (Q 80 1 0 'C:\pwsh\pwsh.exe' 'pwsh'),
            (Q 90 80 1 'C:\pwsh\pwsh.exe' "pwsh -NoProfile -File run-owner.ps1 -ComfyMain $main"),
            (Q 100 90 2 'C:\pwsh\pwsh.exe' "pwsh -NoProfile -File comfy-load.ps1 -Port 8188 -ComfyMain $main"))
    }
    It 'does not block on the harness''s own pwsh (this process and its ancestors) that carry -ComfyMain' {
        @(Get-OwnerRunBlockers $build $main $tree 100).Count | Should -Be 0
        # run-owner's own check runs before comfy-load exists: run-owner (90) with -ComfyMain doesn't block it.
        @(Get-OwnerRunBlockers $build $main @($tree[0], $tree[1]) 90).Count | Should -Be 0
    }
    It 'still blocks a python running main.py, and a pwsh outside the chain that names it' {
        $py = Q 7 50 0 'C:\uv\python\python.exe' "python.exe $main --listen 127.0.0.1"
        $other = Q 120 80 3 'C:\pwsh\pwsh.exe' "pwsh -File comfy-load.ps1 -ComfyMain $main"   # a second harness, not an ancestor
        $why = @(Get-OwnerRunBlockers $build $main (@($tree) + @($py, $other)) 100)
        $why | Should -Match 'ComfyUI is running \(pid 7, 120\)'
    }
    It 'does not take a reused parent pid (a parent newer than its child) for an ancestor' {
        # 90's pid now belongs to a newer process that names main.py: not this harness's parent.
        $reused = @((Q 90 80 30 'C:\py\python.exe' "python $main"), (Q 100 90 2 'C:\pwsh\pwsh.exe' 'pwsh -File comfy-load.ps1'))
        @(Get-OwnerRunBlockers $build $main $reused 100) | Should -Match 'pid 90'
    }
}

Describe 'S0.6 web view check' {
    BeforeAll {
        function Ev($e, $t, $more = @{}) { $o = [ordered]@{ event = $e; t_ms = $t }; foreach ($k in $more.Keys) { $o[$k] = $more[$k] }; [pscustomobject]$o }
        $script:shown = Ev 'web_dock_shown' 2000
        $script:loaded = Ev 'web_load' 3100 @{ status = 'succeeded'; url = 'data:text/html;charset=utf-8,<!doctype html>...' }
        $script:titled = Ev 'web_title' 3150 @{ title = 's06-rendered-42' }
    }
    It 'accepts the data: page loaded after the dock was shown, with the title its script sets' {
        @(Test-S06Web @($shown, (Ev 'web_title' 2950 @{ title = 'pending' }), $loaded, $titled)).Count | Should -Be 0
    }
    It 'loads a local page whose script sets that title (no network, no about:blank)' {
        $u = Get-S06WebUrl
        $u | Should -Match '^data:text/html'
        $u | Should -Match "fillRect\(.*requestAnimationFrame\(function\(\)\{requestAnimationFrame\(function\(\)\{document\.title='s06-rendered-'\+\(6\*7\)\}\)\}\)"
        $u | Should -Not -Match '#'      # a bare # would start a fragment
        $u | Should -Not -Match '"'      # no inner double quotes to escape on the command line
    }
    It 'rejects about:blank, a page whose script never ran, a failed load and a terminated renderer' {
        (@(Test-S06Web @($shown, (Ev 'web_load' 3000 @{ status = 'succeeded'; url = 'about:blank' }))) -join '; ') | Should -Match 'did not load'
        (@(Test-S06Web @($shown, $loaded)) -join '; ') | Should -Match 'script did not run'
        (@(Test-S06Web @($shown, (Ev 'web_load' 3000 @{ status = 'failed'; url = 'data:text/html,x'; error_string = 'x' }), $titled)) -join '; ') |
            Should -Match 'did not load \(last load event: failed'
        (@(Test-S06Web @($shown, $loaded, $titled, (Ev 'web_render_process_terminated' 9000 @{ status = 2; exit_code = 5 }))) -join '; ') |
            Should -Match 'renderer terminated'
        # A title from before the dock was shown doesn't count.
        (@(Test-S06Web @((Ev 'web_title' 1000 @{ title = 's06-rendered-42' }), $shown, $loaded)) -join '; ') | Should -Match 'script did not run'
    }
    It 'leaves a dock never shown to the workload check (no duplicate message)' {
        @(Test-S06Web @()).Count | Should -Be 0
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
    It 'does not read a long GUI stall (dropped 1 s ticks) as the list not being visible, but never allows a hidden second' {
        $stalled = [pscustomobject]@{ grid_visible_seconds = 57; grid_hidden_seconds = 0; feed_ticks = 600; decoded = 4800
                                      chart_points = 370; grid_rows_scrolled = 12000; web_shown = $true }
        @(Test-S06Workload $stalled 60 10).Count | Should -Be 0
        $stalled.grid_visible_seconds = 20
        Test-S06Workload $stalled 60 10 | Should -Match 'list visible only 20 s'
        $stalled.grid_visible_seconds = 59; $stalled.grid_hidden_seconds = 1
        Test-S06Workload $stalled 60 10 | Should -Match 'list hidden 1 s'
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
    It 'keeps an earlier INVALID cause and appends the workload and refresh causes' {
        $v = Complete-S06Verdict 'INVALID (GPU >= 95 % in only 80 % of the samples; 90 % needed)' @('no load_counts logged') $true
        $v | Should -Match '^INVALID \(GPU >= 95 % in only 80 % of the samples; 90 % needed; workload not running as the bar requires: no load_counts logged; the app and Windows report refresh rates more than 2 Hz apart\)$'
        Complete-S06Verdict 'INVALID (GPU samples too sparse)' @() $false | Should -Be 'INVALID (GPU samples too sparse)'
    }
}

Describe 'S0.6 re-score and gate diagnostics (owner run 3)' {
    It 'summarises the gate''s last 20 s and 60 s with real values (they came out null)' {
        $s = for ($k = 0; $k -lt 130; $k++) { [pscustomobject]@{ t = 1000 + $k * 500; util = $(if ($k % 10 -eq 0) { 93 } else { 99 }) } }
        $g = Get-GateSummary $s 66000
        $g.last_20_s.n | Should -BeGreaterThan 30
        $g.last_60_s.n | Should -BeGreaterThan 100
        $g.last_60_s.level | Should -Be 90
        $g.last_60_s.saturated | Should -BeTrue
    }
    It 'recovers the measured window from the recorded coverage and re-scores it, keeping the original verdict' {
        # A synthetic run: 500 ms samples, 99 % with a 93 % sample every 3 s and a dip every 12.5 s.
        $dir = Join-Path $TestDrive 'run'; New-Item -ItemType Directory $dir | Out-Null
        $base = [datetime]'2026-10-23T12:00:00'
        $lines = for ($k = 0; $k -lt 300; $k++) {
            $u = if ($k % 25 -eq 7) { 6 } elseif ($k % 6 -eq 5) { 93 } else { 99 }
            '{0}, {1}, 11000' -f $base.AddMilliseconds(500 * $k).ToString('yyyy/MM/dd HH:mm:ss.fff', [Globalization.CultureInfo]::InvariantCulture), $u
        }
        Set-Content (Join-Path $dir 'gpu.csv') $lines
        $samples = @(Read-GpuSamples (Join-Path $dir 'gpu.csv'))
        $s0 = $samples[40].t - 120; $e0 = $s0 + 60000          # the live window, not recorded in old results
        $orig = Test-GpuCoverage -Samples $samples -StartMs $s0 -EndMs $e0 -MinUtil 95
        $truth = Test-GpuCoverage -Samples $samples -StartMs $s0 -EndMs $e0 -MinUtil 90
        $result = [ordered]@{ runs = $dir; gpu_coverage = $orig; verdict = 'INVALID (GPU >= 95 % in only 80 % of the samples; 90 % needed)'
                              heartbeat_lateness_ms = @{ n = 100; p99 = 3 }; frame_interval_ms = @{ n = 100; p99 = 8 }
                              app_exit_code = 0; app_timed_out = $false; primary_refresh_hz = 240; workload_problems = @(); refresh_mismatch = $false }
        $file = Join-Path $TestDrive 's06.json'
        $result | ConvertTo-Json -Depth 6 | Set-Content $file
        & pwsh -NoProfile -File (Join-Path $PSScriptRoot '..\rescore-s06.ps1') -Result $file | Out-Null
        $r = Get-Content $file -Raw | ConvertFrom-Json
        $r.verdict | Should -Be 'INVALID (GPU >= 95 % in only 80 % of the samples; 90 % needed)'
        $r.verdict_rescored | Should -Be 'PASS'
        $r.gpu_coverage_rescored.n | Should -Be $truth.n
        $r.gpu_coverage_rescored.below_min | Should -Be $truth.below_min
        $r.rescore_rule.window | Should -Match 'matched to the recorded coverage'
    }
}

Describe 'Hands-off notice timing' {
    It 'shows its warning for at least the lead time before the caller may start, and records it' {
        $done = Join-Path $TestDrive 'ho.done'; $log = Join-Path $TestDrive 'ho.json'
        $n = Start-HandsOffNotice 'a Pester test' 1 $done $log 2
        try {
            $w = Wait-HandsOffStart $n 30
            $after = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            $w.shown | Should -BeTrue
            $w.warning_ms | Should -BeGreaterOrEqual 2000
            $after | Should -BeGreaterOrEqual $w.hands_off_epoch_ms   # the caller didn't start early
        } finally {
            Set-Content $done 'done'
            if (-not $n.Process.WaitForExit(15000)) { try { $n.Process.Kill() } catch {} }
        }
        $l = Get-Content $log -Raw | ConvertFrom-Json
        $l.end | Should -Be 'done'
        $l.ever_foreground | Should -BeFalse
        $l.warning_ms | Should -BeGreaterOrEqual 2000
    }
}

Describe 'Test-GpuSaturatedRecently (the gate before the measured minute)' {
    It 'holds the measurement back for the owner''s one-prompt-at-a-time pattern (15 of 20 samples loaded)' {
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
        $sampler = Start-VramSampler 999999 $out
        Start-Sleep -Seconds 4
        $sampler.HasExited | Should -BeFalse
        $sampler.Kill(); [void]$sampler.WaitForExit(5000)
        @(Read-JsonLines $out | Where-Object { $_.missing }).Count | Should -BeGreaterThan 0
        @(Read-VramSamples $out).Count | Should -Be 0
    }
}

Describe 'Kill-on-close job (comfy-load.ps1 never leaves ComfyUI or nvidia-smi behind)' {
    # Harmless hidden children only: pwsh processes that sleep, and cmd running timeout (cmd is
    # outside the Store pwsh's package, like python, nvidia-smi and the app, which is the case
    # that escaped the job). Every one is ours, kept as a Process object (its handle pins the pid,
    # so a reused pid can't be hit), and is killed through that object in AfterAll if a test left
    # it alive.
    BeforeAll {
        $script:ours = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
        function Start-Sleeper { $p = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'Start-Sleep 60' -WindowStyle Hidden -PassThru; $script:ours.Add($p); $p }
        function Wait-Gone([int]$ProcessId, [int]$Ms = 5000) {
            $sw = [Diagnostics.Stopwatch]::StartNew()
            while ($sw.ElapsedMilliseconds -lt $Ms) { if (-not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) { return $true }; Start-Sleep -Milliseconds 100 }
            $false
        }
        # The timeout.exe that our cmd (pinned by its Process handle, so its pid can't be reused)
        # started after itself: by parent pid and creation time, never by name alone.
        function Wait-TimeoutChild([System.Diagnostics.Process]$Cmd, [int]$Ms = 10000) {
            $sw = [Diagnostics.Stopwatch]::StartNew()
            while ($sw.ElapsedMilliseconds -lt $Ms) {
                $c = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($Cmd.Id) AND Name='timeout.exe'" |
                    Where-Object { $_.CreationDate -ge $Cmd.StartTime.AddSeconds(-1) } | Select-Object -First 1
                if ($c) {
                    $p = [System.Diagnostics.Process]::GetProcessById([int]$c.ProcessId)
                    if ([math]::Abs(($p.StartTime - $c.CreationDate).TotalMilliseconds) -le 1) { $script:ours.Add($p); return $p }
                }
                Start-Sleep -Milliseconds 100
            }
            $null
        }
        $module = Join-Path $PSScriptRoot '..\lib\SpikeMeasure.psm1'
    }
    AfterAll {
        foreach ($p in $script:ours) { if (-not $p.HasExited) { try { $p.Kill() } catch {} } }
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
    It 'starts an out-of-package child (cmd) suspended in the job; it and its own grandchild are in it and die with the handle' {
        $job = New-KillOnCloseJob
        $bystander = Start-Sleeper
        $cmd = Start-ProcessInJob $job $env:ComSpec -ArgumentList '/c', 'timeout /t 60 /nobreak'
        $script:ours.Add($cmd)
        $grandchild = Wait-TimeoutChild $cmd
        $grandchild | Should -Not -BeNullOrEmpty
        Test-ProcessInJob $job $cmd | Should -BeTrue
        Test-ProcessInJob $job $grandchild | Should -BeTrue
        Test-ProcessInJob $job $bystander | Should -BeFalse
        $job.Dispose()
        Wait-Gone $cmd.Id | Should -BeTrue
        Wait-Gone $grandchild.Id | Should -BeTrue
        Get-Process -Id $bystander.Id -ErrorAction SilentlyContinue | Should -Not -BeNullOrEmpty
    }
    It 'kills what the script started (cmd and its grandchild) when the script''s pwsh is killed (a closed console), the pwsh itself not being in the job' {
        $pidFile = Join-Path $TestDrive 'cmd.pid'
        $selfFile = Join-Path $TestDrive 'self-in-job.txt'
        $helperScript = Join-Path $TestDrive 'helper.ps1'
        Set-Content $helperScript @"
Import-Module '$module'
`$job = New-KillOnCloseJob
Set-Content '$selfFile' (Test-ProcessInJob `$job (Get-Process -Id `$PID))
`$c = Start-ProcessInJob `$job `$env:ComSpec -ArgumentList '/c', 'timeout /t 60 /nobreak'
Set-Content '$pidFile' `$c.Id
Start-Sleep 60
"@
        $helper = Start-Process pwsh -ArgumentList '-NoProfile', '-File', $helperScript -WindowStyle Hidden -PassThru
        $script:ours.Add($helper)
        $sw = [Diagnostics.Stopwatch]::StartNew()
        while (-not ((Test-Path $pidFile) -and (Get-Content $pidFile)) -and $sw.ElapsedMilliseconds -lt 30000) { Start-Sleep -Milliseconds 200 }
        $cmd = [System.Diagnostics.Process]::GetProcessById([int](Get-Content $pidFile))
        $script:ours.Add($cmd)
        $grandchild = Wait-TimeoutChild $cmd
        $grandchild | Should -Not -BeNullOrEmpty
        Get-Content $selfFile | Should -Be 'False'
        Stop-Process -Id $helper.Id -Force   # what closing the console window does to the script
        Wait-Gone $cmd.Id | Should -BeTrue
        Wait-Gone $grandchild.Id | Should -BeTrue
    }
    It 'holds the child suspended and already in the job until it is resumed (it runs no code before)' {
        $job = New-KillOnCloseJob
        $marker = Join-Path $TestDrive 'ran.txt'
        $marker | Should -Not -Match '\s'   # cmd /c doesn't read CRT-escaped quotes, so the path goes unquoted
        $obs = @{}
        $p = Start-ProcessInJob $job $env:ComSpec -ArgumentList '/c', "echo ran>$marker" -BeforeResume {
            param($proc)
            $script:ours.Add($proc)
            $obs.in_job = Test-ProcessInJob $job $proc
            $proc.Refresh()
            $obs.threads = $proc.Threads.Count
            $obs.all_suspended = @($proc.Threads | Where-Object { -not ($_.ThreadState -eq 'Wait' -and $_.WaitReason -eq 'Suspended') }).Count -eq 0
            Start-Sleep -Milliseconds 500   # cmd writes the marker in a few ms once it runs
            $obs.marker_before_resume = Test-Path $marker
        }
        $obs.in_job | Should -BeTrue
        $obs.threads | Should -BeGreaterThan 0   # else "all suspended" would hold vacuously
        $obs.all_suspended | Should -BeTrue
        $obs.marker_before_resume | Should -BeFalse
        $p.WaitForExit(10000) | Should -BeTrue
        Test-Path $marker | Should -BeTrue   # and it does run once resumed
        $job.Dispose()
    }
    It 'ends a child it cannot put in the job before the child runs, and throws' {
        $dead = New-KillOnCloseJob
        $dead.Dispose()
        $tag = [guid]::NewGuid().ToString('N')
        $marker = Join-Path $TestDrive "never-$tag.txt"
        $marker | Should -Not -Match '\s'
        { Start-ProcessInJob $dead $env:ComSpec -ArgumentList '/c', "echo ran>$marker" } | Should -Throw
        Start-Sleep -Milliseconds 500
        Test-Path $marker | Should -BeFalse
        @(Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($tag) }).Count | Should -Be 0
    }
    It 'refuses to use a closed job (IsProcessInJob with no job would mean "in any job")' {
        $dead = New-KillOnCloseJob
        $dead.Dispose()
        $me = Get-Process -Id $PID
        { Test-ProcessInJob $dead $me } | Should -Throw
        { Add-ProcessToJob $dead $me } | Should -Throw
    }
    It 'puts the VRAM sampler in a job that already holds an out-of-package process (as comfy-load does)' {
        # A pwsh sampler failed here under the Store pwsh: it is born in the package's job and
        # can't then join ours (fix round 5).
        $job = New-KillOnCloseJob
        $cmd = Start-ProcessInJob $job $env:ComSpec -ArgumentList '/c', 'timeout /t 60 /nobreak'
        $script:ours.Add($cmd)
        $sampler = Start-VramSampler 999999 (Join-Path $TestDrive 'vram-job.jsonl') -Job $job
        $script:ours.Add($sampler)
        Test-ProcessInJob $job $sampler | Should -BeTrue
        $job.Dispose()
        Wait-Gone $sampler.Id | Should -BeTrue
        Wait-Gone $cmd.Id | Should -BeTrue
    }
    It 'redirects stdout and stderr to files and reports the exit code' {
        $job = New-KillOnCloseJob
        $o = Join-Path $TestDrive 'out.log'; $e = Join-Path $TestDrive 'err.log'
        $p = Start-ProcessInJob $job $env:ComSpec -ArgumentList '/c', 'echo to-out& echo to-err 1>&2& exit /b 7' -StdOut $o -StdErr $e
        $script:ours.Add($p)
        $p.WaitForExit(10000) | Should -BeTrue
        $p.ExitCode | Should -Be 7
        (Get-Content $o) | Should -Be 'to-out'
        (Get-Content $e) | Should -Match 'to-err'
        $job.Dispose()
    }
    It 'quotes arguments the way the child reads them back' {
        ConvertTo-CommandLine @('C:\a\b.exe', 'plain', 'with space', '', 'say "hi"', 'C:\dir with space\', 'a\\b') |
            Should -Be 'C:\a\b.exe plain "with space" "" "say \"hi\"" "C:\dir with space\\" a\\b'
    }
}
