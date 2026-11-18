# Remove agy-pulse (and legacy agy-hud) from this machine (Windows).
# 1. Run runtime\uninstall.js (clears settings.statusLine, removes runtime dirs, tmp tokens)
# 2. Run `agy plugin uninstall agy-pulse` (and `agy-hud`)
$ErrorActionPreference = 'Continue'

$Runtime = Join-Path $env:USERPROFILE '.gemini\antigravity-cli\agy-pulse-runtime\runtime\uninstall.js'
$LegacyRuntime = Join-Path $env:USERPROFILE '.gemini\antigravity-cli\agy-hud-runtime\runtime\uninstall.js'
$PluginDir = Join-Path $env:USERPROFILE '.gemini\config\plugins\agy-pulse'
$LegacyPluginDir = Join-Path $env:USERPROFILE '.gemini\config\plugins\agy-hud'

if ((Get-Command node -ErrorAction SilentlyContinue) -and (Test-Path $Runtime)) {
    node $Runtime
} elseif ((Get-Command node -ErrorAction SilentlyContinue) -and (Test-Path $LegacyRuntime)) {
    node $LegacyRuntime
} else {
    Write-Error "agy-pulse uninstall: runtime not found - skipping settings cleanup"
}

if (Get-Command agy -ErrorAction SilentlyContinue) {
    agy plugin uninstall agy-pulse
    if ($LASTEXITCODE -ne 0) {
        Write-Error "agy-pulse uninstall: 'agy plugin uninstall agy-pulse' failed - removing plugin dir directly"
        Remove-Item $PluginDir -Recurse -Force -ErrorAction SilentlyContinue
    }
    try { agy plugin uninstall agy-hud 2>$null } catch {}
    if (Test-Path $LegacyPluginDir) {
        Remove-Item $LegacyPluginDir -Recurse -Force -ErrorAction SilentlyContinue
    }
} else {
    Write-Error "agy-pulse uninstall: agy CLI not on PATH - removing plugin dir directly"
    Remove-Item $PluginDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $LegacyPluginDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "agy-pulse uninstall complete"
