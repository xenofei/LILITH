# OPS-079 spike: can a NON-ELEVATED shell register and run a per-user scheduled task?
#   pwsh -File spikes\winops\register-task.ps1
# Registers WorldwrightSpikeTask (daily trigger, StartWhenAvailable, current user,
# LogonType Interactive = "run only when user is logged on", RunLevel Limited, no
# stored password), runs it once, checks %TEMP%\ww-spike-task.log, reads back
# StartWhenAvailable, and ALWAYS unregisters it (try/finally).
# Touches no other scheduled task. Exit 0 = every check passed.
$ErrorActionPreference = 'Stop'
$taskName = 'WorldwrightSpikeTask'
$log      = Join-Path $env:TEMP 'ww-spike-task.log'
$touch    = Join-Path $PSScriptRoot 'touch.ps1'
$exit     = 1

# Refuse to run elevated: the whole point is proving it works without admin.
$label = (& (Join-Path $env:SystemRoot 'System32\whoami.exe') /groups | Select-String 'Mandatory Label').Line
Write-Output "integrity: $(($label -split '\s{2,}')[0].Trim())"
if ($label -notmatch 'Medium Mandatory Level') {
    Write-Output 'FAIL: shell is not Medium integrity (elevated?). Re-run from a non-elevated shell.'
    exit 2
}
if (-not (Test-Path -LiteralPath $touch)) { Write-Output "FAIL: missing $touch"; exit 3 }

try {
    if (Test-Path -LiteralPath $log) { Remove-Item -LiteralPath $log -Force }   # our own log only

    $user      = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    # Task Scheduler does not resolve the Store execution alias 'pwsh' (first run: 0x80070002),
    # so register the absolute path of the real pwsh.exe.
    $pwsh      = (Get-Command pwsh -ErrorAction Stop).Source
    Write-Output "pwsh: $pwsh"
    $action    = New-ScheduledTaskAction -Execute $pwsh `
                    -Argument "-NoProfile -WindowStyle Hidden -File `"$touch`"" `
                    -WorkingDirectory (Split-Path $PSScriptRoot -Parent | Split-Path -Parent)
    $trigger   = New-ScheduledTaskTrigger -Daily -At '03:00'
    $settings  = New-ScheduledTaskSettingsSet -StartWhenAvailable
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger `
        -Settings $settings -Principal $principal -ErrorAction Stop | Out-Null
    Write-Output 'registered'

    Start-ScheduledTask -TaskName $taskName
    Write-Output 'ran'
    Start-Sleep -Seconds 10

    $ok = $true
    if ((Test-Path -LiteralPath $log) -and ((Get-Content -LiteralPath $log -Raw) -match 'touch pid=')) {
        Write-Output 'log ok'
        Write-Output ("log line: " + (Get-Content -LiteralPath $log -Tail 1))
    } else { Write-Output 'FAIL: log line missing'; $ok = $false }

    $t = Get-ScheduledTask -TaskName $taskName
    $swa = $t.Settings.StartWhenAvailable
    Write-Output "StartWhenAvailable=$swa"
    if ($swa -ne $true) { $ok = $false }
    Write-Output ("principal: LogonType={0} RunLevel={1} UserId={2}" -f $t.Principal.LogonType, $t.Principal.RunLevel, $t.Principal.UserId)
    $info = Get-ScheduledTaskInfo -TaskName $taskName
    Write-Output "LastTaskResult=$($info.LastTaskResult)"

    if ($ok) { $exit = 0 }
}
catch {
    Write-Output "FAIL: $($_.Exception.Message)"
    $exit = 1
}
finally {
    if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }
    if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
        Write-Output 'FAIL: task still present'; $exit = 1
    } else { Write-Output 'removed' }
}
exit $exit
