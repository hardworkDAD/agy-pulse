#!/usr/bin/env bash
# agy-pulse Official Rust Release Script (zero Node.js dependency)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_ROOT"

if [ "${ALLOW_DIRTY:-0}" != "1" ] && [ -n "$(git status --porcelain)" ]; then
  echo "Working tree has uncommitted changes. Commit/stash them or set ALLOW_DIRTY=1 to proceed." >&2
  exit 1
fi

VERSION="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' Cargo.toml)"
if [ -z "$VERSION" ]; then
  echo "Failed to determine package version from Cargo.toml" >&2
  exit 1
fi

REPO_SLUG="${AGY_PULSE_REPO:-hardworkDAD/agy-pulse}"
REPO_URL="${AGY_PULSE_REPO_URL:-https://github.com/$REPO_SLUG}"
REPO_URL="${REPO_URL%.git}"

OFFLINE_FLAGS=()
if [ "${CARGO_OFFLINE:-0}" = "1" ]; then
  OFFLINE_FLAGS+=(--offline)
fi

LOCAL_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --local) LOCAL_ONLY=1 ;;
    --offline) OFFLINE_FLAGS+=(--offline) ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Usage: $0 [--local] [--offline]" >&2
      exit 1
      ;;
  esac
done

echo "Preparing release v$VERSION..."

echo "==> Running clippy & test suite..."
cargo clippy "${OFFLINE_FLAGS[@]}" --all-targets -- -D warnings
cargo test "${OFFLINE_FLAGS[@]}"

echo "==> Building release binaries..."
cargo build "${OFFLINE_FLAGS[@]}" --release

detect_asset_name() {
  local os_tag arch_tag
  case "$(uname -s)" in
    Linux) os_tag="linux" ;;
    Darwin) os_tag="macos" ;;
    *)
      echo "Unsupported host OS for local release packaging: $(uname -s)" >&2
      exit 1
      ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch_tag="x86_64" ;;
    arm64|aarch64) arch_tag="aarch64" ;;
    *)
      echo "Unsupported host arch for local release packaging: $(uname -m)" >&2
      exit 1
      ;;
  esac
  echo "agy-pulse-${os_tag}-${arch_tag}.tar.gz"
}

ASSET_NAME="$(detect_asset_name)"
DIST_DIR="$REPO_ROOT/dist"
STAGE_DIR="$DIST_DIR/stage"
rm -rf "$DIST_DIR"
mkdir -p "$STAGE_DIR"

cp -f "$REPO_ROOT/target/release/agy-pulse" "$STAGE_DIR/agy-pulse"
cp -f "$REPO_ROOT/target/release/agypl" "$STAGE_DIR/agypl"
chmod 0755 "$STAGE_DIR/agy-pulse" "$STAGE_DIR/agypl"
tar -czf "$DIST_DIR/$ASSET_NAME" -C "$STAGE_DIR" agy-pulse agypl
rm -rf "$STAGE_DIR"

(
  cd "$DIST_DIR"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$ASSET_NAME" > SHA256SUMS
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$ASSET_NAME" > SHA256SUMS
  else
    echo "sha256sum or shasum is required to generate SHA256SUMS" >&2
    exit 1
  fi
)

echo "Packaged: $DIST_DIR/$ASSET_NAME"
cat "$DIST_DIR/SHA256SUMS"

if [ "$LOCAL_ONLY" -eq 1 ] || [ "${SKIP_GH_RELEASE:-}" = "true" ]; then
  echo "Skipping GitHub Release upload (local build only)."
  exit 0
fi

echo "Tagging and pushing v$VERSION to trigger 5-platform GitHub Release workflow..."
git tag -f "v$VERSION"
git push origin "refs/tags/v$VERSION"
echo "Release workflow triggered for v$VERSION at $REPO_URL/releases/tag/v$VERSION"
