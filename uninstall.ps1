# Remove agy-pulse from this machine (Windows).
# 1. Run runtime\uninstall.js (clears settings.statusLine, removes runtime dirs, tmp tokens)
# 2. Run `agy plugin uninstall agy-pulse`
$ErrorActionPreference = 'Continue'

$ScriptDir = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $MyInvocation.MyCommand.Path }
$RuntimeCandidates = @()
if ($env:AGY_PULSE_DATA_DIR) {
    $RuntimeCandidates += (Join-Path $env:AGY_PULSE_DATA_DIR 'agy-pulse-runtime\runtime\uninstall.js')
}
if ($env:USERPROFILE) {
    $RuntimeCandidates += (Join-Path $env:USERPROFILE '.gemini\antigravity-cli\agy-pulse-runtime\runtime\uninstall.js')
}
if ($env:APPDATA) {
    $RuntimeCandidates += (Join-Path $env:APPDATA 'antigravity-cli\agy-pulse-runtime\runtime\uninstall.js')
}
if ($env:LOCALAPPDATA) {
    $RuntimeCandidates += (Join-Path $env:LOCALAPPDATA 'antigravity-cli\agy-pulse-runtime\runtime\uninstall.js')
}
$RuntimeCandidates += (Join-Path $ScriptDir 'runtime\uninstall.js')

$Runtime = $RuntimeCandidates | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
$PluginDir = Join-Path $env:USERPROFILE '.gemini\config\plugins\agy-pulse'

if ((Get-Command node -ErrorAction SilentlyContinue) -and $Runtime) {
    node $Runtime
} else {
    Write-Error "agy-pulse uninstall: runtime not found - skipping settings cleanup"
}

if (Get-Command agy -ErrorAction SilentlyContinue) {
    agy plugin uninstall agy-pulse
    if ($LASTEXITCODE -ne 0) {
        Write-Error "agy-pulse uninstall: 'agy plugin uninstall agy-pulse' failed - removing plugin dir directly"
        Remove-Item $PluginDir -Recurse -Force -ErrorAction SilentlyContinue
    }
} else {
    Write-Error "agy-pulse uninstall: agy CLI not on PATH - removing plugin dir directly"
    Remove-Item $PluginDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "agy-pulse uninstall complete"
