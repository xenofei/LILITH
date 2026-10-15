<#
.SYNOPSIS
  Samples the canvas process's GPU memory (PDH "GPU Process Memory", summed over the process's
  adapter instances) and the GPU's utilization (nvidia-smi, if present) about once a second, until
  the process exits. Writes one JSON line per sample to -Out. Used by run-frametimes.ps1 (criterion 3).
#>
param(
    [Parameter(Mandatory)] [int]$ProcessId,
    [Parameter(Mandatory)] [string]$Out
)
$ErrorActionPreference = 'Continue'
# Measurement-condition checks, once per sample: milliseconds since the last keyboard/mouse input in
# the session (GetLastInputInfo), and whether the canvas window is minimized / in the foreground.
Add-Type -Namespace WwSpike -Name User32 -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)] public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
[DllImport("user32.dll")] public static extern bool GetLastInputInfo(ref LASTINPUTINFO plii);
[DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("kernel32.dll")] public static extern uint GetTickCount();
public static long IdleMs() { var i = new LASTINPUTINFO(); i.cbSize = (uint)Marshal.SizeOf(i); if (!GetLastInputInfo(ref i)) return -1; return (long)unchecked(GetTickCount() - i.dwTime); }
'@
$counters = foreach ($n in 'Dedicated Usage', 'Shared Usage', 'Total Committed') { "\GPU Process Memory(pid_${ProcessId}_*)\$n" }
$smi = Get-Command nvidia-smi.exe -ErrorAction SilentlyContinue
$w = [System.IO.StreamWriter]::new($Out, $false, [System.Text.UTF8Encoding]::new($false))
try {
    while ($true) {
        $p = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
        if (-not $p -or $p.HasExited) { break }
        $row = [ordered]@{ epoch_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
        try {
            $c = Get-Counter -Counter $counters -ErrorAction Stop
            if ($c.Timestamp) { $row.epoch_ms = ([DateTimeOffset]$c.Timestamp).ToUnixTimeMilliseconds() }
            foreach ($key in 'dedicated usage', 'shared usage', 'total committed') {
                $sum = ($c.CounterSamples | Where-Object { $_.Path -like "*\$key" } | Measure-Object -Property CookedValue -Sum).Sum
                $row[($key -replace ' ', '_')] = [int64]$sum
            }
        } catch {
            $row.error = "$($_.Exception.Message)"
        }
        $row.user_idle_ms = [WwSpike.User32]::IdleMs()
        $h = $p.MainWindowHandle
        if ($h -ne [IntPtr]::Zero) {
            $row.window_minimized = [WwSpike.User32]::IsIconic($h)
            $row.window_foreground = ([WwSpike.User32]::GetForegroundWindow() -eq $h)
        }
        if ($smi) {
            $q = & $smi.Source --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits 2>$null | Select-Object -First 1
            if ($q -match '^\s*(\d+)\s*,\s*(\d+)') { $row.gpu_util_percent = [int]$Matches[1]; $row.gpu_mem_used_mib = [int]$Matches[2] }
        }
        $w.WriteLine(($row | ConvertTo-Json -Compress))
        $w.Flush()
    }
} finally {
    $w.Dispose()
}
