#!/bin/bash

# agy-pulse Official Release Script
set -e

if [ "${ALLOW_DIRTY:-0}" != "1" ] && [ -n "$(git status --porcelain)" ]; then
  echo "Working tree has uncommitted changes. Commit/stash them or set ALLOW_DIRTY=1 to proceed." >&2
  exit 1
fi

VERSION=$(node -e "console.log(require('./package.json').version)")
REPO_URL=$(node -e "
  if (process.env.AGY_PULSE_REPO_URL) {
    console.log(process.env.AGY_PULSE_REPO_URL.replace(/\.git$/, ''));
    process.exit(0);
  }
  const slug = process.env.AGY_PULSE_REPO;
  if (slug) {
    console.log('https://github.com/' + slug.replace(/^git\+/, '').replace(/\.git$/, ''));
    process.exit(0);
  }
  const r = require('./package.json').repository;
  const rawUrl = typeof r === 'string' ? r : (r && r.url);
  if (!rawUrl) { console.error('Configure package.json repository or AGY_PULSE_REPO'); process.exit(1); }
  console.log(rawUrl.replace(/^git\+/, '').replace(/\.git$/, ''));
")
echo "Preparing release v$VERSION..."

# 1. Run tests
npm test

# 2. Create standardized flattened .zip package (once, reused by E2E gate and release)
echo "Creating flattened .zip package..."
rm -rf release_tmp
mkdir -p release_tmp
cp plugin.json gemini-extension.json package.json README.md README_zh.md LICENSE uninstall.sh uninstall.ps1 release_tmp/
[ -d runtime ] && cp -r runtime release_tmp/ || true
[ -d scripts ] && cp -r scripts release_tmp/ || true
[ -d skills ] && cp -r skills release_tmp/ || true

cd release_tmp
rm -f ../agy-pulse.zip
zip -qr ../agy-pulse.zip .
cd ..
rm -rf release_tmp

# 3. E2E gate
if [ "${SKIP_E2E:-0}" != "1" ]; then
  echo "Running E2E verify-display..."
  AGY_PULSE_SKIP_BUILD=1 node scripts/verify-display.js --observe-timeout-ms=30000 > /tmp/release-e2e.json 2>&1
  E2E_RC=$?
  if [ $E2E_RC -ne 0 ]; then
    echo "E2E failed (exit $E2E_RC). Full report at /tmp/release-e2e.json"
    tail -40 /tmp/release-e2e.json
    exit 1
  fi
  E2E_OK=$(node -e "console.log(require('/tmp/release-e2e.json').ok)")
  if [ "$E2E_OK" != "true" ]; then
    echo "E2E report.ok=$E2E_OK. Full report at /tmp/release-e2e.json"
    tail -40 /tmp/release-e2e.json
    exit 1
  fi
  echo "E2E green: agy-pulse status line rendered in a live agy session"
else
  echo "SKIP_E2E=1 set — E2E gate bypassed. Verify on real device before declaring live."
fi

# 4. Create GitHub Release
if [ "${1:-}" == "--local" ] || [ "${SKIP_GH_RELEASE:-}" == "true" ]; then
  echo "Skipping GitHub Release upload (local build only)."
  exit 0
fi

echo "Uploading to GitHub..."
gh release delete "v$VERSION" --yes || true
git tag -d "v$VERSION" || true
git push origin :refs/tags/v$VERSION || true

gh release create "v$VERSION" agy-pulse.zip --title "Release v$VERSION" --notes "Official agy-pulse plugin release in flattened .zip format."

echo "Release v$VERSION is now live!"
echo "Permanent 'Latest' Install URL:"
echo "agy plugin install $REPO_URL/releases/latest/download/agy-pulse.zip"
echo "Versioned Install URL:"
echo "agy plugin install $REPO_URL/releases/download/v$VERSION/agy-pulse.zip"
