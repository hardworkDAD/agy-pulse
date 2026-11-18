#!/usr/bin/env node
'use strict';

const fs = require('fs');
const http = require('http');
const https = require('https');
const os = require('os');
const path = require('path');

const DEFAULT_SOURCE_BASE = '';

const RUNTIME_FILES = [
  'package.json',
  'runtime/agy-hud.config.json',
  'runtime/brand.js',
  'runtime/bin/agy-pulse.js',
  'runtime/bin/agy-pulse.cmd',
  'runtime/bin/agy-hud.js',
  'runtime/bin/agy-hud.cmd',
  'runtime/compaction.js',
  'runtime/config-wizard.js',
  'runtime/config.js',
  'runtime/cost.js',
  'runtime/daily-cost.js',
  'runtime/encoding.js',
  'runtime/parser.js',
  'runtime/paths.js',
  'runtime/quota.js',
  'runtime/quota/cache.js',
  'runtime/quota/cloud.js',
  'runtime/quota/models.js',
  'runtime/quota/token.js',
  'runtime/renderer.js',
  'runtime/renderer/context-monitor.js',
  'runtime/renderer/format.js',
  'runtime/renderer/lang.js',
  'runtime/renderer/quota-render.js',
  'runtime/speed.js',
  'runtime/statusline-installer.js',
  'runtime/uninstall.js',
  'runtime/update-checker.js',
];

function getAntigravityRoots(env = process.env, homeDir = os.homedir()) {
  return [
    path.join(homeDir, '.gemini', 'antigravity-cli'),
    env.XDG_DATA_HOME ? path.join(env.XDG_DATA_HOME, 'antigravity-cli') : null,
    env.APPDATA ? path.join(env.APPDATA, 'antigravity-cli') : null,
    env.LOCALAPPDATA ? path.join(env.LOCALAPPDATA, 'antigravity-cli') : null,
  ].filter(Boolean);
}

function pickAntigravityRoot(env = process.env, homeDir = os.homedir()) {
  const roots = getAntigravityRoots(env, homeDir);
  for (const root of roots) {
    if (getPluginDirs(root, { env, homeDir }).some(pluginDir =>
      fs.existsSync(path.join(pluginDir, 'plugin.json'))
    )) {
      return root;
    }
  }
  for (const root of roots) {
    if (fs.existsSync(path.join(root, 'settings.json'))) return root;
  }
  return roots[0];
}

function sourceUrl(sourceBase, relativePath) {
  return `${sourceBase.replace(/\/+$/, '')}/${relativePath}`;
}

function resolveSourceBase(options = {}, env = process.env) {
  if (options.sourceBase) return options.sourceBase;
  const explicit =
    env.AGY_PULSE_REPO_RAW ||
    env.AGY_HUD_REPO_RAW ||
    env.AGY_PULSE_SETUP_SOURCE_BASE ||
    env.AGY_HUD_SETUP_SOURCE_BASE;
  if (explicit) return explicit;

  const repoSlug = env.AGY_PULSE_REPO || env.AGY_HUD_REPO;
  if (repoSlug && /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repoSlug.trim())) {
    return `https://raw.githubusercontent.com/${repoSlug.trim()}/main`;
  }
  return DEFAULT_SOURCE_BASE;
}

function requestBuffer(url, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    const client = parsed.protocol === 'http:' ? http : https;
    const req = client.get(parsed, response => {
      const status = response.statusCode || 0;
      if (status >= 300 && status < 400 && response.headers.location && redirectsLeft > 0) {
        response.resume();
        const nextUrl = new URL(response.headers.location, parsed);
        if (parsed.protocol === 'https:' && nextUrl.protocol === 'http:') {
          reject(new Error(`insecure redirect from https to http forbidden: ${url} -> ${nextUrl.toString()}`));
          return;
        }
        resolve(requestBuffer(nextUrl.toString(), redirectsLeft - 1));
        return;
      }
      if (status < 200 || status >= 300) {
        response.resume();
        reject(new Error(`download failed (${status}): ${url}`));
        return;
      }

      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.on('end', () => resolve(Buffer.concat(chunks)));
    });
    req.setTimeout(30_000, () => {
      req.destroy(new Error(`download timed out: ${url}`));
    });
    req.on('error', reject);
  });
}

async function requestBufferWithRetry(url, maxAttempts = 3) {
  let lastErr;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    try {
      return await requestBuffer(url);
    } catch (err) {
      lastErr = err;
      if (attempt < maxAttempts) {
        const delaySec = Math.pow(2, attempt - 1);
        process.stderr.write(`[agy-pulse] download attempt ${attempt} failed (${err.message}), retrying in ${delaySec}s...\n`);
        await new Promise(r => setTimeout(r, delaySec * 1000));
      }
    }
  }
  throw lastErr;
}

async function readRuntimeFile(relativePath, options) {
  if (options.sourceDir) {
    return fs.readFileSync(path.join(options.sourceDir, ...relativePath.split('/')));
  }
  if (!options.sourceBase) {
    throw new Error('No local sourceDir or remote repository configured. Set AGY_PULSE_REPO=owner/repo or AGY_PULSE_REPO_RAW=<url>.');
  }
  return requestBufferWithRetry(sourceUrl(options.sourceBase, relativePath));
}

const STALE_PLUGIN_FILES = ['hooks.json', 'mcp_config.json'];
const STALE_PLUGIN_DIRS = ['agents', 'commands', 'rules', 'extensions'];

function unique(values) {
  return [...new Set(values.filter(Boolean))];
}

function getPluginDirs(antigravityRoot, options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const pluginNames = ['agy-pulse', 'agy-hud'];
  const dirs = [];

  for (const name of pluginNames) {
    dirs.push(path.join(homeDir, '.gemini', 'config', 'plugins', name));
    dirs.push(path.join(antigravityRoot, 'plugins', name));
    if (env.XDG_CONFIG_HOME) {
      dirs.push(path.join(env.XDG_CONFIG_HOME, 'gemini', 'plugins', name));
    }
    if (env.APPDATA) {
      dirs.push(path.join(env.APPDATA, 'gemini', 'plugins', name));
    }
  }

  return unique(dirs.map(dir => path.resolve(dir)));
}

function cleanStalePluginFiles(antigravityRoot, options = {}) {
  const removed = [];
  for (const pluginDir of getPluginDirs(antigravityRoot, options)) {
    if (!fs.existsSync(pluginDir)) continue;
    for (const f of STALE_PLUGIN_FILES) {
      const p = path.join(pluginDir, f);
      if (fs.existsSync(p)) {
        fs.rmSync(p, { force: true });
        removed.push(p);
      }
    }
    for (const d of STALE_PLUGIN_DIRS) {
      const p = path.join(pluginDir, d);
      if (fs.existsSync(p)) {
        fs.rmSync(p, { recursive: true, force: true });
        removed.push(p);
      }
    }
  }
  return removed;
}

function replaceRuntimeDirAtomically(runtimeDir, tempRuntimeDir) {
  const backupDir = `${runtimeDir}.bak-${process.pid}-${Date.now()}`;
  let backedUp = false;

  try {
    if (fs.existsSync(runtimeDir)) {
      fs.renameSync(runtimeDir, backupDir);
      backedUp = true;
    }
    fs.renameSync(tempRuntimeDir, runtimeDir);
    if (backedUp) fs.rmSync(backupDir, { recursive: true, force: true });
  } catch (error) {
    if (backedUp && fs.existsSync(runtimeDir)) {
      fs.rmSync(runtimeDir, { recursive: true, force: true });
    }
    if (backedUp && fs.existsSync(backupDir)) {
      fs.renameSync(backupDir, runtimeDir);
    }
    throw error;
  }
}

async function installRuntime(options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const repoCandidate = path.resolve(__dirname, '..');
  const hasLocalRuntime =
    fs.existsSync(path.join(repoCandidate, 'runtime', 'bin', 'agy-pulse.js')) ||
    fs.existsSync(path.join(repoCandidate, 'runtime', 'bin', 'agy-hud.js'));
  const sourceDir =
    options.sourceDir ||
    env.AGY_PULSE_SETUP_SOURCE_DIR ||
    env.AGY_HUD_SETUP_SOURCE_DIR ||
    (hasLocalRuntime ? repoCandidate : '');
  const sourceBase = resolveSourceBase(options, env);
  const antigravityRoot = options.antigravityRoot || pickAntigravityRoot(env, homeDir);
  const runtimeDir = path.join(antigravityRoot, 'agy-pulse-runtime');
  const tempRuntimeDir = `${runtimeDir}.tmp-${process.pid}-${Date.now()}`;

  const stalePluginFiles = cleanStalePluginFiles(antigravityRoot, { env, homeDir });
  fs.rmSync(tempRuntimeDir, { recursive: true, force: true });

  if ((options.platform || process.platform) === 'darwin') {
    for (const legacyFile of ['agy-pulse-token.json', 'agy-hud-token.json']) {
      const legacyTempToken = path.join(antigravityRoot, legacyFile);
      if (fs.existsSync(legacyTempToken)) {
        try { fs.rmSync(legacyTempToken, { force: true }); } catch {}
      }
    }
  }

  try {
    for (const relativePath of RUNTIME_FILES) {
      const body = await readRuntimeFile(relativePath, { sourceDir, sourceBase });
      const target = path.join(tempRuntimeDir, ...relativePath.split('/'));
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, body);
    }
    replaceRuntimeDirAtomically(runtimeDir, tempRuntimeDir);
  } catch (error) {
    fs.rmSync(tempRuntimeDir, { recursive: true, force: true });
    throw error;
  }

  const installerPath = path.join(runtimeDir, 'runtime', 'statusline-installer.js');
  delete require.cache[require.resolve(installerPath)];
  const { configureStatusLine } = require(installerPath);
  const result = configureStatusLine(path.join(runtimeDir, 'runtime'), {
    settingsPath: path.join(antigravityRoot, 'settings.json'),
  });
  const quotaRefresh = await refreshQuotaCache(runtimeDir, {
    env,
    homeDir,
    platform: options.platform,
    keyringReader: options.keyringReader,
  });

  return {
    antigravityRoot,
    runtimeDir,
    settingsPath: result.settingsPath,
    command: result.command,
    files: RUNTIME_FILES,
    stalePluginFiles,
    quotaRefresh,
  };
}

async function refreshQuotaCache(runtimeDir, options = {}) {
  try {
    const quotaPath = path.join(runtimeDir, 'runtime', 'quota.js');
    delete require.cache[require.resolve(quotaPath)];
    const { readToken, fetchQuotaFromCloud, fetchTierFromCloud, writeCache, isTokenExpired } = require(quotaPath);
    const env = options.env || process.env;
    const roots = getAntigravityRoots(env, options.homeDir || os.homedir());
    const token = readToken({ roots, platform: options.platform || process.platform, keyringReader: options.keyringReader });
    if (!token) return { status: 'skipped', reason: 'not_logged_in' };
    if (isTokenExpired(token)) return { status: 'skipped', reason: 'expired_token' };

    const [quota, tier] = await Promise.all([
      fetchQuotaFromCloud(token.accessToken),
      fetchTierFromCloud(token.accessToken).catch(() => null),
    ]);
    if (!Array.isArray(quota) || quota.length === 0) {
      return {
        status: 'skipped',
        reason: quota && quota.unavailableReason ? quota.unavailableReason : 'empty_quota',
      };
    }

    writeCache(quota, token, tier);
    return { status: 'refreshed', count: quota.length, tier: tier || null };
  } catch (error) {
    return { status: 'failed', reason: error.message };
  }
}

if (require.main === module) {
  const sleep = (ms) => new Promise(resolve => setTimeout(resolve, ms));
  const run = async () => {
    if (process.argv.includes('--delay-start')) {
      await sleep(1000);
    }
    return installRuntime();
  };

  run()
    .then(result => {
      process.stdout.write(`agy-pulse bootstrap complete\n`);
      process.stdout.write(`runtime: ${result.runtimeDir}\n`);
      process.stdout.write(`settings: ${result.settingsPath}\n`);
      process.stdout.write(`statusLine: ${result.command}\n`);
      if (result.stalePluginFiles.length > 0) {
        process.stdout.write(`cleaned ${result.stalePluginFiles.length} stale plugin file(s) from prior install\n`);
      }
      if (result.quotaRefresh.status === 'refreshed') {
        process.stdout.write(`quota: refreshed ${result.quotaRefresh.count}\n`);
      } else {
        process.stdout.write(`quota: ${result.quotaRefresh.status} ${result.quotaRefresh.reason || ''}\n`);
      }
    })
    .catch(error => {
      process.stderr.write(`${error.stack || error.message}\n`);
      process.exit(1);
    });
}

module.exports = {
  DEFAULT_SOURCE_BASE,
  RUNTIME_FILES,
  STALE_PLUGIN_FILES,
  STALE_PLUGIN_DIRS,
  getPluginDirs,
  getAntigravityRoots,
  pickAntigravityRoot,
  cleanStalePluginFiles,
  replaceRuntimeDirAtomically,
  installRuntime,
  refreshQuotaCache,
};
