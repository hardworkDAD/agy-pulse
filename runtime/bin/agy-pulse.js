#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');
const { spawn } = require('child_process');
const { getSessionState, parseAgyInput, parseAgyQuota } = require('../parser.js');
const { renderHUD } = require('../renderer.js');
const { loadConfig } = require('../config.js');
const { getQuota, getCachedTier } = require('../quota.js');
const { enrichModelsWithQuotaSummary } = require('../quota/cloud.js');
const { resolveAntigravityPath } = require('../paths.js');
const { resolveRepoSlug, getRawGitHubUrl } = require('../brand.js');

async function handleSelfUpdate() {
  console.log('Checking for latest installer...');
  const config = await loadConfig().catch(() => ({}));
  let pkg = {};
  try {
    const pkgPath = path.join(__dirname, '..', '..', 'package.json');
    if (fs.existsSync(pkgPath)) {
      pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
    }
  } catch {}

  const explicitUrl = process.env.AGY_PULSE_BOOTSTRAP_URL || process.env.AGY_HUD_BOOTSTRAP_URL;
  const repoSlug = resolveRepoSlug({ repository: config?.repository || pkg?.repository });
  const bootstrapUrl = explicitUrl || getRawGitHubUrl(repoSlug, 'main', 'scripts/bootstrap.js');

  if (!bootstrapUrl) {
    console.error('Update skipped: no remote repository configured. Set AGY_PULSE_REPO=owner/repo or repository in config.');
    process.exit(1);
    return;
  }

  const tempPath = path.join(os.tmpdir(), `agy-pulse-bootstrap-${Date.now()}.js`);

  try {
    await new Promise((resolve, reject) => {
      const req = https.get(bootstrapUrl, { timeout: 15000 }, response => {
        if (response.statusCode !== 200) {
          response.resume();
          return reject(new Error(`Failed to fetch bootstrap: ${response.statusCode}`));
        }
        const fileStream = fs.createWriteStream(tempPath);
        response.pipe(fileStream);
        fileStream.on('finish', () => {
          fileStream.close();
          resolve();
        });
        fileStream.on('error', reject);
      });
      req.setTimeout(15000, () => {
        req.destroy(new Error('Bootstrap download timed out'));
      });
      req.on('error', reject);
    });

    console.log('Spawning update process in background...');
    const child = spawn(process.execPath, [tempPath, '--delay-start'], {
      detached: true,
      stdio: 'inherit',
      windowsHide: true,
    });
    child.unref();
    console.log('Update initiated. Exiting current process...');
    process.exit(0);
  } catch (err) {
    console.error(`Update failed: ${err.message}`);
    process.exit(1);
  }
}

function maybeCheckUpdates() {
  const updateStatusPath = resolveAntigravityPath('agy-pulse-update-status.json');
  let lastCheck = 0;
  let status = {};
  try {
    if (fs.existsSync(updateStatusPath)) {
      status = JSON.parse(fs.readFileSync(updateStatusPath, 'utf8'));
      lastCheck = status.lastCheck || 0;
    }
  } catch {}

  const ONE_DAY_MS = 24 * 60 * 60 * 1000;
  if (Date.now() - lastCheck > ONE_DAY_MS) {
    status.lastCheck = Date.now();
    try {
      fs.mkdirSync(path.dirname(updateStatusPath), { recursive: true });
      fs.writeFileSync(updateStatusPath, JSON.stringify(status, null, 2));
    } catch {}

    const checkerPath = path.join(__dirname, '..', 'update-checker.js');
    if (fs.existsSync(checkerPath)) {
      const checker = spawn(process.execPath, [checkerPath], {
        detached: true,
        stdio: 'ignore',
        windowsHide: true,
      });
      checker.unref();
    }
  }
}

function readUpdateInfo() {
  const candidates = [
    resolveAntigravityPath('agy-pulse-update-status.json'),
    resolveAntigravityPath('agy-hud-update-status.json'),
  ];
  for (const candidate of candidates) {
    try {
      if (fs.existsSync(candidate)) {
        return JSON.parse(fs.readFileSync(candidate, 'utf8'));
      }
    } catch {}
  }
  return null;
}

async function main() {
  if (process.argv.includes('--update')) {
    await handleSelfUpdate();
    return;
  }

  if (process.argv.includes('--config')) {
    const { startWizard } = require('../config-wizard.js');
    await startWizard();
    return;
  }

  const stdinData = [];
  let hasHandled = false;

  // Hard cap: if neither 'end' nor enough data arrives within 1.5s, give up.
  const timeout = setTimeout(() => handleInputAndRender(), 1500);

  async function handleInputAndRender() {
    if (hasHandled) return;
    hasHandled = true;
    clearTimeout(timeout);

    const inputStr = Buffer.concat(stdinData).toString();
    const agyData = inputStr.trim() ? parseAgyInput(inputStr) : null;

    const targetCwd = agyData?.workspace?.current_dir || agyData?.cwd || process.cwd();

    let transcriptPath = agyData?.transcript_path;
    if (!transcriptPath || !fs.existsSync(transcriptPath)) {
      const candidate = resolveAntigravityPath(path.join(
        'brain',
        agyData?.conversation_id || '',
        '.system_generated',
        'logs',
        'transcript.jsonl'
      ));
      if (fs.existsSync(candidate) || !transcriptPath) {
        transcriptPath = candidate;
      }
    }

    try {
      const updateInfo = readUpdateInfo();

      const [state, config, quotaData, tierName] = await Promise.all([
        getSessionState(transcriptPath, targetCwd),
        loadConfig(targetCwd),
        getQuota({ fast: true }).catch(() => []),
        getCachedTier(),
      ]);

      let effectiveQuotaData = quotaData;
      const stdinProviderQuota = parseAgyQuota(agyData?.quota);
      if (stdinProviderQuota && Array.isArray(effectiveQuotaData)) {
        effectiveQuotaData = enrichModelsWithQuotaSummary(effectiveQuotaData, stdinProviderQuota);
        effectiveQuotaData.providerQuota = { ...(quotaData.providerQuota || {}), ...stdinProviderQuota };
      }

      const hudOutput = renderHUD(state, agyData, config, effectiveQuotaData, tierName, updateInfo);
      process.stdout.write(hudOutput);

      maybeCheckUpdates();
    } catch (err) {
      try {
        const errorLogPath = resolveAntigravityPath('agy-pulse-error.log');
        fs.mkdirSync(path.dirname(errorLogPath), { recursive: true });
        fs.writeFileSync(errorLogPath, err.stack || String(err), { mode: 0o600 });
      } catch {}
    }
    process.exit(0);
  }

  process.stdin.on('data', chunk => stdinData.push(chunk));
  process.stdin.on('end', handleInputAndRender);
}

if (require.main === module) {
  main();
}

module.exports = {
  main,
  handleSelfUpdate,
  maybeCheckUpdates,
};
