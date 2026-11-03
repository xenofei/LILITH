<#
.SYNOPSIS
  What the hands-off checks in this folder share (confirm-check, top-bar-check, usage-export-check,
  home-check, diagnostics-check, settings-check). Not run by itself: each check dot-sources it.

.DESCRIPTION
  The caller has, in its own scope, `$lil` (the deployed lil.exe), `$root` (its temporary test root),
  `$Repo`, and `Fail`, which these functions use.

  - Invoke-LilTimed: every `lil` call, with a limit. A hung core can't freeze the pop-up's blue cue or
    hang the cleanup.
  - Start-Stretch and Assert-InStretch: the deadline that keeps a check inside the hands-off pop-up's cap,
    so no input is sent after the pop-up has said the PC is free.
  - Retry-Walk: UI Automation can throw for a whole walk while the tree moves.
  - Hide-Paths and Copy-MaskedLogs: a failure's copy of the app's logs, without this PC's paths.
  - Get-ChannelPipes: a test channel already served by a core or an app.
#>

# A `lil` call with a limit. ProcessStartInfo with ArgumentList keeps each argument (the JSON ones)
# whole, and its exit code is the caller's to read. Output is read as it comes, so a full pipe can't
# stop the call. Past the limit the process is killed and the call fails and throws. The working
# folder is the shell's current location, which is where `lil` looks for a workspace.
# Returns {ExitCode, Output}: stdout then stderr, as lines.
function Invoke-LilTimed([string[]]$a, [int]$timeoutMs = 30000) {
    $call = 'lil ' + ((@($a) | Where-Object { $_ -notlike '{*' } | Select-Object -First 3) -join ' ')
    $psi = [System.Diagnostics.ProcessStartInfo]::new($lil)
    foreach ($x in $a) { $psi.ArgumentList.Add($x) }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.WorkingDirectory = (Get-Location -PSProvider FileSystem).ProviderPath
    $p = [System.Diagnostics.Process]::Start($psi)
    try {
        $out = $p.StandardOutput.ReadToEndAsync()
        $err = $p.StandardError.ReadToEndAsync()
        if (-not $p.WaitForExit($timeoutMs)) {
            try { $p.Kill() } catch { }
            Fail "$call timed out"
            throw "$call timed out"
        }
        # A core started by this call can keep the pipes open after the call has ended: don't wait for ever.
        $text = ''
        foreach ($t in $out, $err) { if ($t.Wait(5000)) { $text += $t.Result } }
        $lines = @($text -split "`r?`n")
        if ($lines[-1] -eq '') { $lines = @($lines | Select-Object -SkipLast 1) }
        return [pscustomobject]@{ ExitCode = $p.ExitCode; Output = $lines }
    } finally { $p.Dispose() }
}

# The deadline: set when the pop-up shows, `capSec` less 30 s on. Every real key and click goes through
# a guard that calls Assert-InStretch, and so do the waits, so a step with no wait in it stops as well.
$script:stopAt = $null
function Start-Stretch([int]$capSec) { $script:stopAt = (Get-Date).AddSeconds($capSec - 30) }
function Assert-InStretch {
    if ($null -ne $script:stopAt -and (Get-Date) -gt $script:stopAt) { throw "out of the stretch's time" }
}

# UI Automation can throw for a whole walk while the tree moves: walk again, a few times.
function Retry-Walk([scriptblock]$walk) {
    for ($i = 0; ; ++$i) {
        try { return (& $walk) } catch {
            if ($i -ge 4) { throw }
            Write-Host "walk retry $($i + 1) of 4: $($_.Exception.Message)"
            Start-Sleep -Milliseconds 100
        }
    }
}

# What a dump holds of this PC's paths, masked: the temporary root, the repository and the profile
# folder, in each separator the app writes them in.
function Hide-Paths([string]$text) {
    foreach ($pair in @(@($root, '<root>'), @($Repo, '<repo>'), @($env:USERPROFILE, '%USERPROFILE%'))) {
        if (-not $pair[0]) { continue }
        foreach ($form in @($pair[0], $pair[0].Replace('\', '/'), $pair[0].Replace('\', '\\'))) {
            $text = [regex]::Replace($text, [regex]::Escape($form), $pair[1], [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
        }
    }
    return $text
}

# The files under `from` copied into the new folder `to`, each through Hide-Paths. The app has its log
# open for appending: it is read shared. Nothing is deleted, and nothing nests under a folder of its name.
function Copy-MaskedLogs([string]$from, [string]$to) {
    New-Item -ItemType Directory -Force $to | Out-Null
    $base = (Resolve-Path -LiteralPath $from).Path.TrimEnd('\')
    foreach ($f in Get-ChildItem -LiteralPath $from -Recurse -File -Force) {
        $dest = Join-Path $to $f.FullName.Substring($base.Length + 1)
        New-Item -ItemType Directory -Force (Split-Path -Parent $dest) | Out-Null
        $s = [IO.File]::Open($f.FullName, 'Open', 'Read', 'ReadWrite,Delete')
        try { $text = [IO.StreamReader]::new($s).ReadToEnd() } finally { $s.Dispose() }
        [IO.File]::WriteAllText($dest, (Hide-Paths $text), [Text.UTF8Encoding]::new($false))
    }
}

# A run's own folder name for what a failure leaves in -DiagDir.
$script:runStamp = (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmss')

# The pipes of a core or an app serving `channel`, from this deploy or another. They are listed, not
# asked: another deploy's core makes `lil --no-start status` fail as if there were none.
function Get-ChannelPipes([string]$channel) {
    return @([System.IO.Directory]::GetFiles('\\.\pipe\') | Where-Object { $_ -like "*-$channel" })
}
