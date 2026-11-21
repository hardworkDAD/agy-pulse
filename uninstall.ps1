# Remove agy-pulse from this machine (Windows, zero Node.js dependency).
# Runs native `agypl.exe uninstall` / `agy-pulse.exe uninstall` (forwarding flags such as --purge-ledger),
# then cleans up the agy plugin registration & plugin directory.
$ErrorActionPreference = 'Continue'

$NativeCandidates = @()
if ($env:AGY_PULSE_BIN_DIR) {
    $NativeCandidates += (Join-Path $env:AGY_PULSE_BIN_DIR 'agypl.exe')
    $NativeCandidates += (Join-Path $env:AGY_PULSE_BIN_DIR 'agy-pulse.exe')
}
if ($env:LOCALAPPDATA) {
    $NativeCandidates += (Join-Path $env:LOCALAPPDATA 'agy-pulse\bin\agypl.exe')
    $NativeCandidates += (Join-Path $env:LOCALAPPDATA 'agy-pulse\bin\agy-pulse.exe')
}

$NativeBin = $NativeCandidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $NativeBin) {
    $Cmd = Get-Command agypl -ErrorAction SilentlyContinue
    if (-not $Cmd) { $Cmd = Get-Command agy-pulse -ErrorAction SilentlyContinue }
    if ($Cmd) { $NativeBin = $Cmd.Source }
}

$UninstallExit = 0
if ($NativeBin) {
    & $NativeBin uninstall @args
    $UninstallExit = $LASTEXITCODE
} else {
    Write-Error "agy-pulse uninstall: native binary not found - skipping settings & cache cleanup"
}

$PluginDirs = @()
if ($env:USERPROFILE) {
    $PluginDirs += (Join-Path $env:USERPROFILE '.gemini\config\plugins\agy-pulse')
}
if ($env:XDG_CONFIG_HOME) {
    $PluginDirs += (Join-Path $env:XDG_CONFIG_HOME 'gemini\plugins\agy-pulse')
}
if ($env:APPDATA) {
    $PluginDirs += (Join-Path $env:APPDATA 'gemini\plugins\agy-pulse')
}

if (Get-Command agy -ErrorAction SilentlyContinue) {
    agy plugin uninstall agy-pulse
    if ($LASTEXITCODE -ne 0) {
        Write-Error "agy-pulse uninstall: 'agy plugin uninstall agy-pulse' failed - removing plugin dir directly"
        foreach ($dir in $PluginDirs) {
            if ($dir -and (Test-Path $dir)) {
                Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
} else {
    Write-Error "agy-pulse uninstall: agy CLI not on PATH - removing plugin dir directly"
    foreach ($dir in $PluginDirs) {
        if ($dir -and (Test-Path $dir)) {
            Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

Write-Host "agy-pulse uninstall complete"
exit $UninstallExit
