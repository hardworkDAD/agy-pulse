# Remove agy-pulse from this machine (Windows).
# 1. Prefer native v2 binary (`agypl.exe uninstall` / `agy-pulse.exe uninstall`), forwarding args such as --purge-ledger.
# 2. Fallback to legacy v1 runtime\uninstall.js if no native binary is installed.
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

if ($NativeBin) {
    & $NativeBin uninstall @args
    exit $LASTEXITCODE
}

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
    node $Runtime @args
} else {
    Write-Error "agy-pulse uninstall: binary and legacy runtime not found - skipping settings cleanup"
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
