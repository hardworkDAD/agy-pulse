#!/usr/bin/env node
'use strict';

const fs = require('fs');
const http = require('http');
const https = require('https');
const os = require('os');
const path = require('path');

const DEFAULT_SOURCE_BASE = 'https://raw.githubusercontent.com/hardworkDAD/agy-pulse/main';

const RUNTIME_FILES = [
  'package.json',
  'runtime/agy-pulse.config.json',
  'runtime/brand.js',
  'runtime/bin/agy-pulse.js',
  'runtime/bin/agy-pulse.cmd',
  'runtime/compaction.js',
  'runtime/config-wizard.js',
  'runtime/config.js',
  'runtime/cost.js',
  'runtime/daily-cost.js',
  'runtime/dashboard-launcher.js',
  'runtime/dashboard-ui.js',
  'runtime/dashboard.js',
  'runtime/encoding.js',
  'runtime/ledger.js',
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
  'runtime/stats.js',
  'runtime/statusline-installer.js',
  'runtime/uninstall.js',
  'runtime/update-checker.js',
  'scripts/diagnose-auth.js',
];

function getAntigravityRoots(env = process.env, homeDir = os.homedir()) {
  if (env && env.AGY_PULSE_DATA_DIR) {
    return [env.AGY_PULSE_DATA_DIR];
  }
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
    env.AGY_PULSE_SETUP_SOURCE_BASE;
  if (explicit) return explicit;

  const repoSlug = env.AGY_PULSE_REPO;
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
const STALE_PLUGIN_DIRS = ['agents', 'commands', 'rules', 'extensions', 'skills/hud-config'];

function unique(values) {
  return [...new Set(values.filter(Boolean))];
}

function getPluginDirs(antigravityRoot, options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const pluginNames = ['agy-pulse'];
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
      const p = path.join(pluginDir, ...d.split('/'));
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

function isPlainObject(value) {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value));
}

function mergePreservedConfig(defaults, existing) {
  const merged = { ...defaults, ...existing };
  if (typeof existing.theme === 'string') {
    merged.theme = existing.theme;
  } else if (isPlainObject(defaults.theme) || isPlainObject(existing.theme)) {
    merged.theme = {
      ...(isPlainObject(defaults.theme) ? defaults.theme : {}),
      ...(isPlainObject(existing.theme) ? existing.theme : {}),
    };
  }
  if (isPlainObject(defaults.display) || isPlainObject(existing.display)) {
    const mergedDisplay = {
      ...(isPlainObject(defaults.display) ? defaults.display : {}),
      ...(isPlainObject(existing.display) ? existing.display : {}),
    };
    if (isPlainObject(defaults.display?.cacheHitThresholds) || isPlainObject(existing.display?.cacheHitThresholds)) {
      mergedDisplay.cacheHitThresholds = {
        ...(isPlainObject(defaults.display?.cacheHitThresholds) ? defaults.display.cacheHitThresholds : {}),
        ...(isPlainObject(existing.display?.cacheHitThresholds) ? existing.display.cacheHitThresholds : {}),
      };
    }
    merged.display = mergedDisplay;
  }
  if (isPlainObject(defaults.icons) || isPlainObject(existing.icons)) {
    merged.icons = {
      ...(isPlainObject(defaults.icons) ? defaults.icons : {}),
      ...(isPlainObject(existing.icons) ? existing.icons : {}),
    };
  }
  if (isPlainObject(defaults.thresholds) || isPlainObject(existing.thresholds)) {
    merged.thresholds = {
      ...(isPlainObject(defaults.thresholds) ? defaults.thresholds : {}),
      ...(isPlainObject(existing.thresholds) ? existing.thresholds : {}),
    };
  }
  return merged;
}

function preserveExistingConfig(runtimeDir, tempRuntimeDir, options = {}) {
  const resetConfig = options.resetConfig === true || process.argv.includes('--reset-config');
  if (resetConfig) return false;

  const existingConfigPath = path.join(runtimeDir, 'runtime', 'agy-pulse.config.json');
  const nextConfigPath = path.join(tempRuntimeDir, 'runtime', 'agy-pulse.config.json');
  if (!fs.existsSync(existingConfigPath) || !fs.existsSync(nextConfigPath)) {
    return false;
  }

  try {
    const existingConfig = JSON.parse(fs.readFileSync(existingConfigPath, 'utf8'));
    const nextDefaultConfig = JSON.parse(fs.readFileSync(nextConfigPath, 'utf8'));
    if (!isPlainObject(existingConfig) || !isPlainObject(nextDefaultConfig)) {
      return false;
    }
    const mergedConfig = mergePreservedConfig(nextDefaultConfig, existingConfig);
    fs.writeFileSync(nextConfigPath, JSON.stringify(mergedConfig, null, 2) + '\n', 'utf8');
    return true;
  } catch {
    return false;
  }
}

function clearStaleUpdateStatus(antigravityRoot, runtimeDir) {
  const statusPath = path.join(antigravityRoot, 'agy-pulse-update-status.json');
  if (!fs.existsSync(statusPath)) return false;

  let installedVersion = '0.0.0';
  try {
    const pkgPath = path.join(runtimeDir, 'package.json');
    if (fs.existsSync(pkgPath)) {
      const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
      if (typeof pkg?.version === 'string' && pkg.version.trim()) {
        installedVersion = pkg.version.trim();
      }
    }
  } catch {}

  try {
    const status = JSON.parse(fs.readFileSync(statusPath, 'utf8'));
    if (!status || typeof status !== 'object') {
      fs.rmSync(statusPath, { force: true });
      return true;
    }
    const checkerPath = path.join(runtimeDir, 'runtime', 'update-checker.js');
    delete require.cache[require.resolve(checkerPath)];
    const { isNewerVersion } = require(checkerPath);
    if (!status.latestVersion || !isNewerVersion(status.latestVersion, installedVersion)) {
      const nextStatus = {
        ...status,
        localVersion: installedVersion,
        updateAvailable: false,
      };
      const tmpPath = `${statusPath}.tmp.${process.pid}.${Date.now()}`;
      fs.writeFileSync(tmpPath, JSON.stringify(nextStatus, null, 2), { mode: 0o600 });
      fs.renameSync(tmpPath, statusPath);
      return true;
    }
    return false;
  } catch {
    try { fs.rmSync(statusPath, { force: true }); } catch {}
    return true;
  }
}

function resolveCliBinDir(options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const platform = options.platform || process.platform;
  if (options.binDir) return options.binDir;
  if (env.AGY_PULSE_BIN_DIR) return env.AGY_PULSE_BIN_DIR;
  if (platform === 'win32' && env.LOCALAPPDATA) {
    return path.join(env.LOCALAPPDATA, 'agy', 'bin');
  }
  return path.join(homeDir, '.local', 'bin');
}

function installCliShim(runtimeDir, options = {}) {
  const platform = options.platform || process.platform;
  const binDir = resolveCliBinDir(options);
  const nodeExecPath = options.nodeExecPath || process.execPath || 'node';
  const entryPath = path.join(runtimeDir, 'runtime', 'bin', 'agy-pulse.js');
  const commandNames = ['agypl', 'agy-pulse'];

  fs.mkdirSync(binDir, { recursive: true });

  if (platform === 'win32') {
    const body = [
      '@echo off',
      'rem agy-pulse CLI shim',
      'setlocal',
      `node "${entryPath}" %* 2>nul`,
      'if %ERRORLEVEL%==0 exit /b 0',
      `if exist "${nodeExecPath}" "${nodeExecPath}" "${entryPath}" %*`,
      '',
    ].join('\r\n');
    const shimPaths = commandNames.map(name => path.join(binDir, `${name}.cmd`));
    for (const shimPath of shimPaths) {
      try { fs.rmSync(shimPath, { force: true }); } catch {}
      fs.writeFileSync(shimPath, body, 'utf8');
    }
    return { binDir, shimPath: shimPaths[0], shimPaths, platform };
  }

  const body = [
    '#!/bin/sh',
    '# agy-pulse CLI shim',
    `exec "${nodeExecPath}" "${entryPath}" "$@"`,
    '',
  ].join('\n');
  const shimPaths = commandNames.map(name => path.join(binDir, name));
  for (const shimPath of shimPaths) {
    try { fs.rmSync(shimPath, { force: true }); } catch {}
    fs.writeFileSync(shimPath, body, { mode: 0o755 });
    fs.chmodSync(shimPath, 0o755);
  }
  return { binDir, shimPath: shimPaths[0], shimPaths, platform };
}

async function installRuntime(options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const repoCandidate = path.resolve(__dirname, '..');
  const hasLocalRuntime =
    fs.existsSync(path.join(repoCandidate, 'runtime', 'bin', 'agy-pulse.js'));
  const sourceDir =
    options.sourceDir ||
    env.AGY_PULSE_SETUP_SOURCE_DIR ||
    (hasLocalRuntime ? repoCandidate : '');
  const sourceBase = resolveSourceBase(options, env);
  const antigravityRoot = options.antigravityRoot || pickAntigravityRoot(env, homeDir);
  const runtimeDir = path.join(antigravityRoot, 'agy-pulse-runtime');
  const tempRuntimeDir = `${runtimeDir}.tmp-${process.pid}-${Date.now()}`;

  const stalePluginFiles = cleanStalePluginFiles(antigravityRoot, { env, homeDir });
  fs.rmSync(tempRuntimeDir, { recursive: true, force: true });

  if ((options.platform || process.platform) === 'darwin') {
    const tempToken = path.join(antigravityRoot, 'agy-pulse-token.json');
    if (fs.existsSync(tempToken)) {
      try { fs.rmSync(tempToken, { force: true }); } catch {}
    }
  }

  try {
    for (const relativePath of RUNTIME_FILES) {
      const body = await readRuntimeFile(relativePath, { sourceDir, sourceBase });
      const target = path.join(tempRuntimeDir, ...relativePath.split('/'));
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, body);
    }
    preserveExistingConfig(runtimeDir, tempRuntimeDir, options);
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
  clearStaleUpdateStatus(antigravityRoot, runtimeDir);
  const cliShim = installCliShim(runtimeDir, {
    env,
    homeDir,
    platform: options.platform,
    binDir: options.binDir,
    nodeExecPath: options.nodeExecPath,
  });

  return {
    antigravityRoot,
    runtimeDir,
    settingsPath: result.settingsPath,
    command: result.command,
    cliShim,
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

    writeCache(quota, token, tier, null, quota.providerQuota || null, { roots });
    return { status: 'refreshed', count: quota.length, tier: tier || null };
  } catch (error) {
    return { status: 'failed', reason: error.message };
  }
}

function isPathInsideDir(targetPath, parentDir) {
  let resolvedParent;
  let resolvedTarget;
  try {
    resolvedParent = fs.realpathSync(parentDir);
  } catch {
    resolvedParent = path.resolve(parentDir);
  }
  try {
    resolvedTarget = fs.realpathSync(targetPath);
  } catch {
    resolvedTarget = path.resolve(targetPath);
  }
  const rel = path.relative(resolvedParent, resolvedTarget);
  return Boolean(rel && !rel.startsWith('..') && !path.isAbsolute(rel));
}

function cleanupSelfIfRequested(argv = process.argv, selfPath = __filename, tmpDir = os.tmpdir()) {
  if (!argv.includes('--cleanup-self')) return false;
  if (!isPathInsideDir(selfPath, tmpDir)) return false;
  try {
    fs.rmSync(selfPath, { force: true });
    return true;
  } catch {
    return false;
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

  let exitCode = 0;
  run()
    .then(result => {
      process.stdout.write(`agy-pulse bootstrap complete\n`);
      process.stdout.write(`runtime: ${result.runtimeDir}\n`);
      process.stdout.write(`settings: ${result.settingsPath}\n`);
      process.stdout.write(`statusLine: ${result.command}\n`);
      if (result.cliShim && Array.isArray(result.cliShim.shimPaths) && result.cliShim.shimPaths.length > 0) {
        process.stdout.write(`cli: ${result.cliShim.shimPaths.join(', ')}\n`);
      } else if (result.cliShim && result.cliShim.shimPath) {
        process.stdout.write(`cli: ${result.cliShim.shimPath}\n`);
      }
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
      exitCode = 1;
    })
    .finally(() => {
      cleanupSelfIfRequested();
      if (exitCode !== 0) process.exit(exitCode);
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
  preserveExistingConfig,
  cleanupSelfIfRequested,
  resolveCliBinDir,
  installCliShim,
  installRuntime,
  refreshQuotaCache,
};
