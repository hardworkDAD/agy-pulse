# One-command native installer for agy-pulse v2.0.0 (Windows PowerShell, zero Node.js dependency).
# Downloads agy-pulse-windows-x86_64.zip + SHA256SUMS, verifies SHA-256,
# installs agy-pulse.exe and agypl.exe into AGY_PULSE_BIN_DIR
# (default %LOCALAPPDATA%\agy-pulse\bin), registers host settings.json via
# `agy-pulse.exe install`, and syncs plugin manifest when `agy` is available.
#
# Run from a normal shell — NOT from inside an active `agy` session
# (agy rewrites settings.json from in-memory state on exit).
#
# Env:
#   AGY_PULSE_REPO              owner/repo slug (default: hardworkDAD/agy-pulse)
#   AGY_PULSE_REPO_URL          override git URL for `agy plugin install`
#   AGY_PULSE_RELEASE_BASE_URL  override release download base URL (mirrors / local test servers)
#   AGY_PULSE_BIN_DIR           override target bin directory (default: %LOCALAPPDATA%\agy-pulse\bin)
$ErrorActionPreference = 'Stop'

$DefaultRepoSlug = 'hardworkDAD/agy-pulse'
$RepoSlug = if ($env:AGY_PULSE_REPO) { $env:AGY_PULSE_REPO } else { $DefaultRepoSlug }
$RepoUrl = if ($env:AGY_PULSE_REPO_URL) { $env:AGY_PULSE_REPO_URL } else { "https://github.com/$RepoSlug.git" }
$ReleaseBaseUrl = if ($env:AGY_PULSE_RELEASE_BASE_URL) {
    $env:AGY_PULSE_RELEASE_BASE_URL.TrimEnd('/')
} else {
    "https://github.com/$RepoSlug/releases/latest/download"
}

$Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($Arch -ne 'X64' -and $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    Write-Error "agy-pulse: unsupported Windows CPU architecture: $Arch ($env:PROCESSOR_ARCHITECTURE). Only x86_64 (X64) is supported."
    exit 1
}

$BinDir = if ($env:AGY_PULSE_BIN_DIR) {
    $env:AGY_PULSE_BIN_DIR
} elseif ($env:LOCALAPPDATA) {
    Join-Path $env:LOCALAPPDATA 'agy-pulse\bin'
} else {
    Join-Path $env:USERPROFILE 'AppData\Local\agy-pulse\bin'
}

$ScriptDir = if ($PSScriptRoot) {
    $PSScriptRoot
} elseif ($MyInvocation.MyCommand.Path) {
    Split-Path -Parent $MyInvocation.MyCommand.Path
} else {
    ''
}
$RepoRoot = if ($ScriptDir) { Split-Path -Parent $ScriptDir } else { '' }
$LocalPluginJson = if ($RepoRoot) { Join-Path $RepoRoot 'plugin.json' } else { '' }
$LocalExtJson = if ($RepoRoot) { Join-Path $RepoRoot 'gemini-extension.json' } else { '' }
$IsLocalRepo = [bool]($LocalPluginJson -and $LocalExtJson -and (Test-Path $LocalPluginJson) -and (Test-Path $LocalExtJson))

$AssetName = 'agy-pulse-windows-x86_64.zip'
$ArchiveUrl = "$ReleaseBaseUrl/$AssetName"
$SumsUrl = "$ReleaseBaseUrl/SHA256SUMS"

$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("agy-pulse-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $TempDir | Out-Null

try {
    $SumsPath = Join-Path $TempDir 'SHA256SUMS'
    $ArchivePath = Join-Path $TempDir $AssetName
    $ExtractDir = Join-Path $TempDir 'extract'

    Write-Host "==> Downloading $AssetName and SHA256SUMS..."
    Invoke-WebRequest -Uri $SumsUrl -OutFile $SumsPath -UseBasicParsing
    Invoke-WebRequest -Uri $ArchiveUrl -OutFile $ArchivePath -UseBasicParsing

    $ExpectedHash = $null
    foreach ($line in Get-Content -Path $SumsPath) {
        $trimmed = $line.Trim()
        if (-not $trimmed -or $trimmed.StartsWith('#')) { continue }
        $parts = $trimmed -split '\s+', 2
        if ($parts.Count -lt 2) { continue }
        $hash = $parts[0].ToLowerInvariant()
        $filePart = $parts[1].TrimStart('*')
        if ($filePart.StartsWith('./')) { $filePart = $filePart.Substring(2) }
        $baseName = Split-Path -Leaf $filePart
        if (($filePart -eq $AssetName -or $baseName -eq $AssetName) -and ($hash -match '^[0-9a-f]{64}$')) {
            $ExpectedHash = $hash
            break
        }
    }

    if (-not $ExpectedHash) {
        throw "SHA256SUMS does not contain a valid entry for $AssetName"
    }

    $ActualHash = (Get-FileHash -Path $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualHash -ne $ExpectedHash) {
        throw "SHA-256 mismatch for $AssetName (expected $ExpectedHash, got $ActualHash)"
    }
    Write-Host "==> SHA-256 verified ($ActualHash)"

    Expand-Archive -Path $ArchivePath -DestinationPath $ExtractDir -Force

    $PrimarySrc = Get-ChildItem -Path $ExtractDir -Filter 'agy-pulse.exe' -Recurse -File | Select-Object -First 1
    $AliasSrc = Get-ChildItem -Path $ExtractDir -Filter 'agypl.exe' -Recurse -File | Select-Object -First 1
    if (-not $PrimarySrc) {
        throw "Extracted archive did not contain agy-pulse.exe"
    }

    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    $PrimaryDst = Join-Path $BinDir 'agy-pulse.exe'
    $AliasDst = Join-Path $BinDir 'agypl.exe'
    Copy-Item -Path $PrimarySrc.FullName -Destination $PrimaryDst -Force
    if ($AliasSrc) {
        Copy-Item -Path $AliasSrc.FullName -Destination $AliasDst -Force
    }

    Write-Host "==> Configuring host statusLine via agy-pulse.exe install..."
    & $PrimaryDst install --bin-dir $BinDir
    if ($LASTEXITCODE -ne 0) {
        throw "agy-pulse.exe install exited with code $LASTEXITCODE"
    }

    if (Get-Command agy -ErrorAction SilentlyContinue) {
        Write-Host "==> Syncing plugin manifest with agy CLI..."
        try { agy plugin uninstall agy-pulse 2>$null } catch {}
        if ($IsLocalRepo -and -not $env:AGY_PULSE_REPO -and -not $env:AGY_PULSE_REPO_URL) {
            agy plugin install $RepoRoot
        } else {
            agy plugin install $RepoUrl
        }
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "agy plugin install exited with code $LASTEXITCODE; native statusLine remains active."
        }
    }

    Write-Host ""
    Write-Host "==> agy-pulse installed to $BinDir (agy-pulse.exe, agypl.exe)."
    if (-not (($env:PATH -split ';') -contains $BinDir)) {
        Write-Host "    Add $BinDir to your user PATH to invoke agypl / agy-pulse from any terminal."
    }
    Write-Host "    Open a new agy session to see the agy-pulse status line."
} finally {
    Remove-Item -Path $TempDir -Recurse -Force -ErrorAction SilentlyContinue
}
