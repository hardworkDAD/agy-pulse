#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');
const os = require('os');
const tty = require('tty');
const http = require('http');
const https = require('https');
const { spawn } = require('child_process');
const { getSessionState, parseAgyInput, parseAgyQuota } = require('../parser.js');
const { renderStatusLine } = require('../renderer.js');
const { loadConfig } = require('../config.js');
const { getQuota, getCachedTier } = require('../quota.js');
const { enrichModelsWithQuotaSummary } = require('../quota/cloud.js');
const { resolveAntigravityPath } = require('../paths.js');
const { PACKAGE_VERSION, resolveRepoSlug, getRawGitHubUrl } = require('../brand.js');
const { getCostTotals } = require('../daily-cost.js');
const { isNewerVersion } = require('../update-checker.js');

function downloadFile(url, destPath, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    const client = parsed.protocol === 'http:' ? http : https;
    const req = client.get(parsed, { timeout: 15000 }, response => {
      const status = response.statusCode || 0;
      if (status >= 300 && status < 400 && response.headers.location && redirectsLeft > 0) {
        response.resume();
        const nextUrl = new URL(response.headers.location, parsed);
        if (parsed.protocol === 'https:' && nextUrl.protocol === 'http:') {
          return reject(new Error(`Insecure redirect from https to http forbidden: ${url} -> ${nextUrl.toString()}`));
        }
        return resolve(downloadFile(nextUrl.toString(), destPath, redirectsLeft - 1));
      }
      if (status < 200 || status >= 300) {
        response.resume();
        return reject(new Error(`Failed to fetch bootstrap: ${status}`));
      }
      const fileStream = fs.createWriteStream(destPath, { mode: 0o600 });
      response.pipe(fileStream);
      fileStream.on('finish', () => {
        fileStream.close(err => {
          if (err) reject(err);
          else resolve();
        });
      });
      fileStream.on('error', err => {
        try { fs.unlinkSync(destPath); } catch {}
        reject(err);
      });
    });
    req.setTimeout(15000, () => {
      req.destroy(new Error('Bootstrap download timed out'));
    });
    req.on('error', err => {
      try { fs.unlinkSync(destPath); } catch {}
      reject(err);
    });
  });
}

function deriveRepoRawFromBootstrapUrl(bootstrapUrl) {
  if (!bootstrapUrl || typeof bootstrapUrl !== 'string') return null;
  try {
    const parsed = new URL(bootstrapUrl);
    const pathname = parsed.pathname.replace(/\/+$/, '');
    const suffix = '/scripts/bootstrap.js';
    const suffixIdx = pathname.lastIndexOf(suffix);
    if (suffixIdx !== -1 && suffixIdx === pathname.length - suffix.length) {
      return `${parsed.origin}${pathname.slice(0, suffixIdx)}`;
    }
    const dirPath = pathname.replace(/\/[^/]+$/, '');
    if (dirPath && dirPath !== pathname) {
      return `${parsed.origin}${dirPath}`;
    }
    return parsed.origin;
  } catch {
    return null;
  }
}

async function handleSelfUpdate() {
  console.log('Checking for latest installer...');
  const config = await loadConfig(process.cwd()).catch(() => ({}));
  let pkg = {};
  try {
    const pkgPath = path.join(__dirname, '..', '..', 'package.json');
    if (fs.existsSync(pkgPath)) {
      pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
    }
  } catch {}

  const explicitUrl = process.env.AGY_PULSE_BOOTSTRAP_URL;
  const repoSlug = resolveRepoSlug({ repository: config?.repository || pkg?.repository });
  const bootstrapUrl = explicitUrl || getRawGitHubUrl(repoSlug, 'main', 'scripts/bootstrap.js');

  if (!bootstrapUrl) {
    console.error('Update skipped: no remote repository configured. Set AGY_PULSE_REPO=owner/repo or repository in config.');
    process.exit(1);
    return;
  }

  const tempPath = path.join(os.tmpdir(), `agy-pulse-bootstrap-${process.pid}-${Date.now()}.js`);

  try {
    await downloadFile(bootstrapUrl, tempPath);

    console.log('Running update installer...');
    const childEnv = { ...process.env };
    if (repoSlug) {
      childEnv.AGY_PULSE_REPO = repoSlug;
    }
    if (explicitUrl) {
      childEnv.AGY_PULSE_BOOTSTRAP_URL = explicitUrl;
      if (!childEnv.AGY_PULSE_REPO_RAW) {
        const derivedRaw = deriveRepoRawFromBootstrapUrl(explicitUrl);
        if (derivedRaw) childEnv.AGY_PULSE_REPO_RAW = derivedRaw;
      }
    } else if (repoSlug && !childEnv.AGY_PULSE_REPO_RAW) {
      childEnv.AGY_PULSE_REPO_RAW = `https://raw.githubusercontent.com/${repoSlug}/main`;
    }

    const exitCode = await new Promise((resolve, reject) => {
      const child = spawn(process.execPath, [tempPath, '--cleanup-self'], {
        stdio: 'inherit',
        windowsHide: true,
        env: childEnv,
      });
      child.on('error', reject);
      child.on('close', (code, signal) => {
        if (typeof code === 'number') resolve(code);
        else resolve(signal ? 1 : 0);
      });
    });
    try { fs.unlinkSync(tempPath); } catch {}
    if (exitCode !== 0) {
      console.error(`Update failed with exit code ${exitCode}.`);
    }
    process.exit(exitCode);
  } catch (err) {
    try { fs.unlinkSync(tempPath); } catch {}
    console.error(`Update failed: ${err.message}`);
    process.exit(1);
  }
}

function maybeCheckUpdates(targetCwd = process.cwd()) {
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
    const tmpPath = `${updateStatusPath}.tmp.${process.pid}.${Date.now()}`;
    try {
      fs.mkdirSync(path.dirname(updateStatusPath), { recursive: true });
      fs.writeFileSync(tmpPath, JSON.stringify(status, null, 2), { mode: 0o600 });
      fs.renameSync(tmpPath, updateStatusPath);
    } catch {
      try { fs.unlinkSync(tmpPath); } catch {}
    }

    const checkerPath = path.join(__dirname, '..', 'update-checker.js');
    if (fs.existsSync(checkerPath)) {
      const checker = spawn(process.execPath, [checkerPath, targetCwd], {
        detached: true,
        stdio: 'ignore',
        windowsHide: true,
      });
      checker.unref();
    }
  }
}

function getInstalledVersion() {
  try {
    const pkgPath = path.join(__dirname, '..', '..', 'package.json');
    if (fs.existsSync(pkgPath)) {
      const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
      if (typeof pkg?.version === 'string' && pkg.version.trim()) {
        return pkg.version.trim();
      }
    }
  } catch {}
  return PACKAGE_VERSION;
}

function readUpdateInfo(installedVersion = getInstalledVersion()) {
  const candidate = resolveAntigravityPath('agy-pulse-update-status.json');
  try {
    if (fs.existsSync(candidate)) {
      const status = JSON.parse(fs.readFileSync(candidate, 'utf8'));
      if (!status || !status.updateAvailable || !status.latestVersion) {
        return null;
      }
      if (!isNewerVersion(status.latestVersion, installedVersion)) {
        return null;
      }
      return status;
    }
  } catch {}
  return null;
}

function adjustLiveTerminalColumns(rawCols) {
  const cols = Math.floor(Number(rawCols));
  if (!Number.isFinite(cols) || cols <= 0) return null;
  return cols > 82 ? cols - 3 : cols;
}

function detectLiveTerminalColumns(agyData = null, config = null, deps = {}) {
  const env = deps.env || process.env;
  if (
    Number(agyData?.terminal_width) > 0 ||
    Number(config?.display?.terminalWidth) > 0 ||
    Number(env?.COLUMNS) > 0
  ) {
    return null;
  }

  const stdout = deps.stdout !== undefined ? deps.stdout : process.stdout;
  const stderr = deps.stderr !== undefined ? deps.stderr : process.stderr;
  let rawCols = 0;
  if (stdout?.isTTY && Number(stdout.columns) > 0) {
    rawCols = Number(stdout.columns);
  } else if (stderr?.isTTY && Number(stderr.columns) > 0) {
    rawCols = Number(stderr.columns);
  }

  if (rawCols > 0) {
    return adjustLiveTerminalColumns(rawCols);
  }

  const platform = deps.platform || process.platform;
  if (platform === 'win32') {
    return null;
  }

  const fsMod = deps.fs || fs;
  const ttyMod = deps.tty || tty;
  const ppid = deps.ppid !== undefined ? deps.ppid : process.ppid;
  const candidates = ['/dev/tty'];
  if (Number.isInteger(ppid) && ppid > 0) {
    candidates.push(
      `/proc/${ppid}/fd/1`,
      `/proc/${ppid}/fd/2`,
      `/proc/${ppid}/fd/0`
    );
  }

  for (const ttyPath of candidates) {
    let fd = null;
    let ws = null;
    try {
      if (ttyPath.startsWith('/proc/')) {
        const target = fsMod.readlinkSync(ttyPath);
        if (!/^\/dev\/(pts\/\d+|tty\w*)$/.test(target)) {
          continue;
        }
      }
      fd = fsMod.openSync(ttyPath, 'r');
      ws = new ttyMod.WriteStream(fd);
      ws.on('error', () => {});
      const cols = Number(ws.columns);
      ws.destroy();
      ws = null;
      fd = null;
      if (Number.isFinite(cols) && cols > 0) {
        return adjustLiveTerminalColumns(cols);
      }
    } catch {
      if (ws) {
        try { ws.destroy(); } catch {}
        fd = null;
      } else if (fd !== null) {
        try { fsMod.closeSync(fd); } catch {}
      }
    }
  }

  return null;
}

function mergeProviderQuota(prev, next) {
  const merged = { ...(prev || {}) };
  for (const [provider, windows] of Object.entries(next || {})) {
    if (windows && typeof windows === 'object') {
      merged[provider] = { ...(merged[provider] || {}), ...windows };
    }
  }
  return merged;
}

function writeErrorLogSafely(err) {
  try {
    const errorLogPath = resolveAntigravityPath('agy-pulse-error.log');
    fs.mkdirSync(path.dirname(errorLogPath), { recursive: true, mode: 0o700 });
    try {
      const st = fs.lstatSync(errorLogPath);
      if (st.isSymbolicLink()) {
        fs.unlinkSync(errorLogPath);
      }
    } catch {}
    fs.writeFileSync(errorLogPath, err.stack || String(err), { mode: 0o600 });
  } catch {}
}

function buildCliHelpText() {
  const version = getInstalledVersion();
  return [
    `agy-pulse v${version} — Antigravity CLI status line & telemetry (CLI: agypl / agy-pulse)`,
    '',
    'Usage:',
    '  agypl dashboard [options]               Start local web dashboard (alias: dash, ui, --dashboard)',
    '  agypl stats [today|7d|30d|all]          Show CLI & IDE telemetry summary (alias: report, --stats)',
    '  agypl config                            Launch interactive configuration wizard (alias: --config)',
    '  agypl diagnose                          Output JSON auth & quota cache diagnostics (alias: --diagnose)',
    '  agypl update                            Check and install the latest release (alias: --update)',
    '  agypl help                              Show this help message (alias: --help, -h)',
    '  agy-pulse                               Render status line from stdin JSON (or use any subcommand above)',
    '',
    'Options:',
    '  stats:     --range, --from, --to, --granularity, --source, --model, --workspace, --json, --csv, --lang, --no-sync',
    '  dashboard: --port, --no-open, --lang, --no-sync',
    '',
  ].join('\n');
}

function extractConversationId(agyData, transcriptPath) {
  if (typeof agyData?.conversation_id === 'string' && agyData.conversation_id.trim()) {
    return agyData.conversation_id.trim();
  }
  if (typeof transcriptPath === 'string' && transcriptPath) {
    const normalized = transcriptPath.replace(/\\/g, '/');
    const match = normalized.match(/\/brain\/([^/]+)\//);
    if (match && match[1]) {
      return match[1];
    }
  }
  return null;
}

async function main() {
  const cliArgs = process.argv.slice(2);
  const firstArg = cliArgs[0];

  if (firstArg === 'dashboard' || firstArg === 'dash' || firstArg === 'ui') {
    const { startDashboardCli } = require('../dashboard.js');
    await startDashboardCli(process.argv.slice(3));
    return;
  }

  if (firstArg === 'stats' || firstArg === 'report') {
    const { runStatsCli } = require('../stats.js');
    const result = await runStatsCli(process.argv.slice(3));
    process.exit(typeof result?.exitCode === 'number' ? result.exitCode : 0);
    return;
  }

  if (firstArg === 'config') {
    const { startWizard } = require('../config-wizard.js');
    await startWizard();
    return;
  }

  if (firstArg === 'diagnose') {
    const { buildAuthDiagnostic } = require('../../scripts/diagnose-auth.js');
    process.stdout.write(`${JSON.stringify(buildAuthDiagnostic(), null, 2)}\n`);
    process.exit(0);
    return;
  }

  if (firstArg === 'update') {
    await handleSelfUpdate();
    return;
  }

  if (firstArg === 'help') {
    process.stdout.write(buildCliHelpText());
    process.exit(0);
    return;
  }

  if (cliArgs.includes('--stats')) {
    const { runStatsCli } = require('../stats.js');
    const result = await runStatsCli(cliArgs);
    process.exit(typeof result?.exitCode === 'number' ? result.exitCode : 0);
    return;
  }

  if (cliArgs.includes('--dashboard')) {
    const { startDashboardCli } = require('../dashboard.js');
    await startDashboardCli(cliArgs);
    return;
  }

  if (cliArgs.includes('--help') || cliArgs.includes('-h')) {
    process.stdout.write(buildCliHelpText());
    process.exit(0);
    return;
  }

  if (cliArgs.includes('--diagnose')) {
    const { buildAuthDiagnostic } = require('../../scripts/diagnose-auth.js');
    process.stdout.write(`${JSON.stringify(buildAuthDiagnostic(), null, 2)}\n`);
    process.exit(0);
    return;
  }

  if (cliArgs.includes('--update')) {
    await handleSelfUpdate();
    return;
  }

  if (cliArgs.includes('--config')) {
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

      if (config?.enabled === false) {
        maybeCheckUpdates(targetCwd);
        process.exit(0);
        return;
      }

      let effectiveQuotaData = quotaData;
      const stdinProviderQuota = parseAgyQuota(agyData?.quota);
      if (stdinProviderQuota && Array.isArray(effectiveQuotaData)) {
        const mergedProviderQuota = mergeProviderQuota(quotaData?.providerQuota, stdinProviderQuota);
        effectiveQuotaData = enrichModelsWithQuotaSummary(effectiveQuotaData, mergedProviderQuota);
        effectiveQuotaData.providerQuota = mergedProviderQuota;
        if (effectiveQuotaData.length === 0 && quotaData?.unavailableReason && !effectiveQuotaData.unavailableReason) {
          Object.defineProperty(effectiveQuotaData, 'unavailableReason', {
            value: quotaData.unavailableReason,
            enumerable: false,
            configurable: true,
          });
        }
      }

      const hasSessionIdentity = Boolean(
        agyData?.conversation_id ||
        agyData?.session_id ||
        (transcriptPath && fs.existsSync(transcriptPath))
      );
      if (hasSessionIdentity && state && typeof state === 'object') {
        let sevenDayResetAt = null;
        if (effectiveQuotaData?.providerQuota?.google?.weekly?.resetTime) {
          sevenDayResetAt = effectiveQuotaData.providerQuota.google.weekly.resetTime;
        } else if (Array.isArray(effectiveQuotaData)) {
          const weeklyQ = effectiveQuotaData.find(q => q.windows?.weekly?.resetTime);
          if (weeklyQ) sevenDayResetAt = weeklyQ.windows.weekly.resetTime;
        }
        const costTotals = getCostTotals(
          agyData,
          { allowRoutedCost: config?.display?.showRoutedCost, sevenDayResetAt },
          undefined,
          state
        );
        if (costTotals) {
          state.costTotals = costTotals;
        }
      }

      const liveCols = detectLiveTerminalColumns(agyData, config);
      const effectiveAgyData = liveCols ? { ...(agyData || {}), terminal_width: liveCols } : agyData;
      const statusLineOutput = renderStatusLine(state, effectiveAgyData, config, effectiveQuotaData, tierName, updateInfo);
      process.stdout.write(statusLineOutput);

      const convId = extractConversationId(agyData, transcriptPath);
      if (convId) {
        try {
          const { syncSingleConversationToLedger } = require('../ledger.js');
          const childConversationIds = Array.isArray(state?.childConversationIds) ? state.childConversationIds : [];
          syncSingleConversationToLedger(convId, { childConversationIds });
        } catch {}
      }

      maybeCheckUpdates(targetCwd);
    } catch (err) {
      writeErrorLogSafely(err);
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
  buildCliHelpText,
  extractConversationId,
  handleSelfUpdate,
  deriveRepoRawFromBootstrapUrl,
  readUpdateInfo,
  maybeCheckUpdates,
  mergeProviderQuota,
  detectLiveTerminalColumns,
  downloadFile,
};
