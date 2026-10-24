#requires -Version 7
<#
Scope cut (coordinator's ruling, 2026-10-23): the default run must never wait for a person. This
asks run-all.ps1 -DryRun which stage scripts the default run starts, adds the scripts those load
(dot-sourced, copied into the Sandbox, or spawned), and fails if any of them can prompt: a call to
Read-Host, Wait-Owner, Ask, Pause, Get-Credential or Out-GridView, or a ReadLine, ReadKey,
PromptForChoice or PromptForCredential method call. It reads the scripts' syntax trees (the
PowerShell parser), not their text, so comments and strings don't count.

On 095a237 the default run started run-spike.ps1, whose Ask and Wait-Owner call Read-Host
(review C1): this fails there. Run by tests/no_prompt.rs (cargo test). Exit 0 = no prompt.
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Spike = Split-Path $PSScriptRoot

# What each stage script loads: dot-sourced helpers, the script copied into the Sandbox and run
# there, and the pop-up uia.ps1 can spawn.
$Loads = @{
    'run-all.ps1'     = @()
    'run-sandbox.ps1' = @('uia.ps1', 'inside.ps1', 'popup.ps1')
    'run-spike.ps1'   = @('uia.ps1', 'popup.ps1')
    'inside.ps1'      = @('uia.ps1')
    'uia.ps1'         = @('popup.ps1')
    'popup.ps1'       = @()
}
$PromptCommands = @('Read-Host', 'Wait-Owner', 'Ask', 'Pause', 'Get-Credential', 'Out-GridView')
$PromptMethods = @('ReadLine', 'ReadKey', 'PromptForChoice', 'PromptForCredential')

$plan = (& pwsh -NoProfile -NonInteractive -File (Join-Path $Spike 'run-all.ps1') -DryRun) | Out-String | ConvertFrom-Json
$stageScripts = @($plan.stages | ForEach-Object { $_.script })
Write-Host "default stages: $($stageScripts -join ', ')"

$reach = [System.Collections.Generic.List[string]]::new()
$queue = [System.Collections.Generic.Queue[string]]::new()
foreach ($s in @('run-all.ps1') + $stageScripts) { $queue.Enqueue($s) }
while ($queue.Count -gt 0) {
    $s = $queue.Dequeue()
    if ($reach.Contains($s)) { continue }
    $reach.Add($s)
    if (-not $Loads.ContainsKey($s)) { throw "no load list for ${s}: add it to tests/no-prompt.ps1" }
    foreach ($l in $Loads[$s]) { $queue.Enqueue($l) }
}
Write-Host "reachable by default: $($reach -join ', ')"

$found = @()
foreach ($f in $reach) {
    $tokens = $null; $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $Spike $f), [ref]$tokens, [ref]$errors)
    foreach ($c in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.CommandAst] }, $true)) {
        $name = $c.GetCommandName()
        if ($name -and $name -in $PromptCommands) { $found += "${f}:$($c.Extent.StartLineNumber): $name" }
    }
    foreach ($m in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.InvokeMemberExpressionAst] }, $true)) {
        $member = "$($m.Member)"
        if ($member -in $PromptMethods) { $found += "${f}:$($m.Extent.StartLineNumber): .$member()" }
    }
}

if ($found.Count -gt 0) {
    Write-Host 'prompts reachable by the default run:'
    $found | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host 'no prompt is reachable by the default run'
exit 0
