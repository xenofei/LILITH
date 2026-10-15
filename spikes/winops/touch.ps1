# OPS-079 spike: the action the scheduled task runs. Appends one timestamp line to
# %TEMP%\ww-spike-task.log and writes nothing else anywhere.
$ErrorActionPreference = 'Stop'
$log = Join-Path $env:TEMP 'ww-spike-task.log'
Add-Content -LiteralPath $log -Value ("{0:o} touch pid={1} user={2}" -f (Get-Date), $PID, $env:USERNAME)
