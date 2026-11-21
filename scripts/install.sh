#!/usr/bin/env bash
# One-command native installer for agy-pulse v2.0.0 (zero Node.js dependency).
# Downloads the prebuilt release archive matching the current OS/CPU, verifies
# SHA256SUMS, installs `agy-pulse` and `agypl` into AGY_PULSE_BIN_DIR
# (default ~/.local/bin), registers host settings.json via `agy-pulse install`,
# and syncs plugin manifests when `agy` is available.
#
# Run from a normal shell — NOT from inside an active `agy` session
# (agy rewrites settings.json from in-memory state on exit).
#
# Env:
#   AGY_PULSE_REPO              owner/repo slug (default: hardworkDAD/agy-pulse)
#   AGY_PULSE_REPO_URL          override git URL for `agy plugin install`
#   AGY_PULSE_RELEASE_BASE_URL  override release download base URL (mirrors / local test servers)
#   AGY_PULSE_BIN_DIR           override target bin directory (default: ~/.local/bin)
set -euo pipefail

DEFAULT_REPO_SLUG="hardworkDAD/agy-pulse"
REPO_SLUG="${AGY_PULSE_REPO:-$DEFAULT_REPO_SLUG}"
REPO_URL="${AGY_PULSE_REPO_URL:-https://github.com/$REPO_SLUG.git}"
RELEASE_BASE_URL="${AGY_PULSE_RELEASE_BASE_URL:-https://github.com/$REPO_SLUG/releases/latest/download}"
RELEASE_BASE_URL="${RELEASE_BASE_URL%/}"
BIN_DIR="${AGY_PULSE_BIN_DIR:-$HOME/.local/bin}"

if [ -n "${BASH_SOURCE[0]:-}" ]; then
  SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
else
  SCRIPT_DIR=""
  REPO_ROOT=""
fi

detect_os() {
  case "$(uname -s)" in
    Linux) echo "linux" ;;
    Darwin) echo "macos" ;;
    *)
      echo "agy-pulse: unsupported operating system: $(uname -s)" >&2
      exit 1
      ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *)
      echo "agy-pulse: unsupported CPU architecture: $(uname -m)" >&2
      exit 1
      ;;
  esac
}

download_file() {
  local url="$1"
  local dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$dest"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$dest" "$url"
  else
    echo "agy-pulse: curl or wget is required to download release archives." >&2
    exit 1
  fi
}

compute_sha256() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print tolower($1)}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$file" | awk '{print tolower($NF)}'
  else
    echo "agy-pulse: sha256sum, shasum, or openssl is required for SHA-256 verification." >&2
    exit 1
  fi
}

extract_expected_sha256() {
  local sums_file="$1"
  local asset_name="$2"
  awk -v target="$asset_name" '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
    {
      hash = tolower($1)
      name = $2
      sub(/^\*/, "", name)
      sub(/^\.\//, "", name)
      n = split(name, parts, "/")
      base = parts[n]
      if (name == target || base == target) {
        if (hash ~ /^[0-9a-f]{64}$/) {
          print hash
          exit
        }
      }
    }
  ' "$sums_file"
}

OS_TAG="$(detect_os)"
ARCH_TAG="$(detect_arch)"
ASSET_NAME="agy-pulse-${OS_TAG}-${ARCH_TAG}.tar.gz"
ARCHIVE_URL="${RELEASE_BASE_URL}/${ASSET_NAME}"
SUMS_URL="${RELEASE_BASE_URL}/SHA256SUMS"

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "==> Downloading ${ASSET_NAME} and SHA256SUMS..."
download_file "$SUMS_URL" "$TMP_DIR/SHA256SUMS"
download_file "$ARCHIVE_URL" "$TMP_DIR/$ASSET_NAME"

EXPECTED_SHA="$(extract_expected_sha256 "$TMP_DIR/SHA256SUMS" "$ASSET_NAME")"
if [ -z "$EXPECTED_SHA" ]; then
  echo "agy-pulse: SHA256SUMS missing valid entry for ${ASSET_NAME}" >&2
  exit 1
fi

ACTUAL_SHA="$(compute_sha256 "$TMP_DIR/$ASSET_NAME")"
if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
  echo "agy-pulse: SHA-256 mismatch for ${ASSET_NAME} (expected ${EXPECTED_SHA}, got ${ACTUAL_SHA})" >&2
  exit 1
fi
echo "==> SHA-256 verified (${ACTUAL_SHA})"

EXTRACT_DIR="$TMP_DIR/extract"
mkdir -p "$EXTRACT_DIR"
tar -xzf "$TMP_DIR/$ASSET_NAME" -C "$EXTRACT_DIR"

PRIMARY_SRC="$(find "$EXTRACT_DIR" -type f -name agy-pulse | head -n 1 || true)"
ALIAS_SRC="$(find "$EXTRACT_DIR" -type f -name agypl | head -n 1 || true)"
if [ -z "$PRIMARY_SRC" ]; then
  echo "agy-pulse: extracted archive did not contain 'agy-pulse' binary" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
cp -f "$PRIMARY_SRC" "$BIN_DIR/agy-pulse"
chmod 0755 "$BIN_DIR/agy-pulse"
if [ -n "$ALIAS_SRC" ]; then
  cp -f "$ALIAS_SRC" "$BIN_DIR/agypl"
  chmod 0755 "$BIN_DIR/agypl"
fi

echo "==> Configuring host statusLine via agy-pulse install..."
"$BIN_DIR/agy-pulse" install --bin-dir "$BIN_DIR"

if command -v agy >/dev/null 2>&1; then
  echo "==> Syncing plugin manifest with agy CLI..."
  agy plugin uninstall agy-pulse >/dev/null 2>&1 || true
  if [ -n "$REPO_ROOT" ] && [ -f "$REPO_ROOT/plugin.json" ] && [ -f "$REPO_ROOT/gemini-extension.json" ] && [ -z "${AGY_PULSE_REPO_URL:-}" ] && [ -z "${AGY_PULSE_REPO:-}" ]; then
    agy plugin install "$REPO_ROOT" || echo "agy-pulse: warning: 'agy plugin install' failed; native statusLine remains active." >&2
  else
    agy plugin install "$REPO_URL" || echo "agy-pulse: warning: 'agy plugin install' failed; native statusLine remains active." >&2
  fi
fi

echo
echo "==> agy-pulse installed to $BIN_DIR (agy-pulse, agypl)."
case ":${PATH:-}:" in
  *":$BIN_DIR:"*) ;;
  *)
    echo "    Add $BIN_DIR to your PATH:"
    echo "      export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac
echo "    Open a new agy session to see the agy-pulse status line."
