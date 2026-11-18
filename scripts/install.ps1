# One-command installer for agy-pulse (Windows PowerShell).
# Equivalent of install.sh for Unix — does:
#   1. agy plugin install (or upgrade) from AGY_PULSE_REPO_URL / AGY_PULSE_REPO
#   2. bootstrap runtime + configure settings.json statusLine
#
# Run from a normal shell — NOT from inside an active `agy` session
# (agy rewrites settings.json from in-memory state on exit).
#
# Env:
#   AGY_PULSE_REPO      owner/repo slug (e.g. yourname/agy-pulse)
#   AGY_PULSE_REPO_RAW  override raw GitHub base URL (forks / mirrors)
#   AGY_PULSE_REPO_URL  override git URL for `agy plugin install`
$ErrorActionPreference = 'Stop'

$RepoSlug = if ($env:AGY_PULSE_REPO) { $env:AGY_PULSE_REPO } elseif ($env:AGY_HUD_REPO) { $env:AGY_HUD_REPO } else { '' }
$RepoRaw = if ($env:AGY_PULSE_REPO_RAW) { $env:AGY_PULSE_REPO_RAW } elseif ($env:AGY_HUD_REPO_RAW) { $env:AGY_HUD_REPO_RAW } elseif ($RepoSlug) { "https://raw.githubusercontent.com/$RepoSlug/main" } else { '' }
$RepoUrl = if ($env:AGY_PULSE_REPO_URL) { $env:AGY_PULSE_REPO_URL } elseif ($env:AGY_HUD_REPO_URL) { $env:AGY_HUD_REPO_URL } elseif ($RepoSlug) { "https://github.com/$RepoSlug.git" } else { '' }

if (-not (Get-Command agy -ErrorAction SilentlyContinue)) {
    Write-Error "agy-pulse: agy CLI not on PATH. Install it first:`n  irm https://antigravity.google/cli/install.ps1 | iex"
    exit 1
}
if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    Write-Error "agy-pulse: node is required but not on PATH."
    exit 1
}
if (-not $RepoUrl -or -not $RepoRaw) {
    Write-Error "agy-pulse: set `$env:AGY_PULSE_REPO = 'owner/agy-pulse' (or `$env:AGY_PULSE_REPO_URL and `$env:AGY_PULSE_REPO_RAW) before running remote install."
    exit 1
}

# Step 1: stage the plugin. Uninstall first so stale files don't linger.
Write-Host "==> Removing any existing agy-pulse / agy-hud plugin install..."
try { agy plugin uninstall agy-pulse 2>$null } catch {}
try { agy plugin uninstall agy-hud 2>$null } catch {}

Write-Host "==> Installing plugin from $RepoUrl..."
agy plugin install $RepoUrl
if ($LASTEXITCODE -ne 0) { throw "agy plugin install exited with code $LASTEXITCODE" }

# Step 2: download bootstrap.js to temp, run with node
Write-Host ""
Write-Host "==> Bootstrapping runtime + statusLine config..."
$BootstrapUrl = "$RepoRaw/scripts/bootstrap.js"
$TempBootstrap = Join-Path ([System.IO.Path]::GetTempPath()) "agy-pulse-bootstrap-$PID.js"
try {
    Invoke-WebRequest -Uri $BootstrapUrl -OutFile $TempBootstrap -UseBasicParsing
    $env:AGY_PULSE_REPO_RAW = $RepoRaw
    node $TempBootstrap
    if ($LASTEXITCODE -ne 0) { throw "bootstrap.js exited with code $LASTEXITCODE" }
} finally {
    Remove-Item $TempBootstrap -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "==> agy-pulse installed."
Write-Host "    Open a new agy session in your terminal to see the HUD."
Write-Host "    Repair anytime by re-running this script."
