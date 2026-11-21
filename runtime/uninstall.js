#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { resolveAntigravityPath, getAntigravityRoots } = require('./paths.js');
const { RUNTIME_DIR_NAME } = require('./brand.js');

function isOurStatusLineCommand(command, runtimeDirOrDirs) {
  if (!command || !runtimeDirOrDirs) return false;
  const dirs = Array.isArray(runtimeDirOrDirs) ? runtimeDirOrDirs : [runtimeDirOrDirs];
  return dirs.some(dir => Boolean(dir) && command.includes(dir));
}

function clearStatusLine(settingsPath, runtimeDirOrDirs) {
  if (!fs.existsSync(settingsPath)) {
    return { changed: false, reason: 'no_settings' };
  }
  let settings;
  try {
    settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
  } catch {
    return { changed: false, reason: 'parse_error' };
  }
  const current = settings && settings.statusLine && settings.statusLine.command;
  if (!current) return { changed: false, reason: 'no_status_line' };
  if (!isOurStatusLineCommand(current, runtimeDirOrDirs)) {
    return { changed: false, reason: 'foreign_status_line' };
  }
  fs.copyFileSync(settingsPath, settingsPath + '.bak');
  delete settings.statusLine;
  fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2), 'utf8');
  return { changed: true };
}

function removeRuntimeDir(runtimeDir) {
  if (!fs.existsSync(runtimeDir)) return { removed: false, reason: 'not_present' };
  fs.rmSync(runtimeDir, { recursive: true, force: true });
  return { removed: true };
}

function removeTokenMirrors() {
  const removed = [];
  const candidates = [
    path.join(os.tmpdir(), 'agy-pulse-token.json'),
    path.join(os.tmpdir(), 'agy-pulse-quota-cache.json'),
  ];
  for (const file of candidates) {
    if (fs.existsSync(file)) {
      try {
        fs.unlinkSync(file);
        removed.push(file);
      } catch {
        // best-effort
      }
    }
  }
  return removed;
}

function loadDashboardLauncher() {
  try {
    return require('./dashboard-launcher.js');
  } catch (err) {
    if (err && err.code === 'MODULE_NOT_FOUND') {
      return null;
    }
    throw err;
  }
}

/**
 * Remove extra files written by the runtime/bootstrap.
 *
 * @param {string} root  antigravity-cli data root
 * @returns {string[]}   list of paths actually removed
 */
function removeExtraFiles(root) {
  const removed = [];
  const launcher = loadDashboardLauncher();
  const dashboardStateFilename = (launcher && launcher.DASHBOARD_STATE_FILENAME) || 'agy-pulse-dashboard.json';
  const targets = [
    path.join(root, 'agy-pulse-payload.json'),
    path.join(root, 'agy-pulse-token.json'),
    path.join(root, 'agy-pulse-quota-cache.json'),
    path.join(root, 'agy-pulse-error.log'),
    path.join(root, 'agy-pulse-update-status.json'),
    path.join(root, 'agy-pulse-compaction-cache.json'),
    path.join(root, dashboardStateFilename),
    path.join(root, 'daily-cost.json'),
    path.join(root, 'speed-cache'),
    path.join(root, 'transcript-cache'),
    path.join(root, 'hud'),
    path.join(root, 'statusline-combined.sh'),
    path.join(root, 'agy-statusline'),
    path.join(root, 'plugin_data', 'agy-pulse'),
  ];
  for (const target of targets) {
    if (fs.existsSync(target)) {
      try {
        fs.rmSync(target, { recursive: true, force: true });
        removed.push(target);
      } catch {
        // best-effort
      }
    }
  }
  return removed;
}

function removeWindowsShShims(platform = process.platform, env = process.env) {
  if (platform !== 'win32') return [];
  try {
    const installer = require('./statusline-installer.js');
    const body = installer.buildShShimContents();
    const dirs = installer.getWindowsAgyBinDirs(env);
    const removed = [];
    for (const dir of dirs) {
      for (const name of ['sh.cmd', 'sh.bat']) {
        const target = path.join(dir, name);
        try {
          if (fs.existsSync(target)) {
            const contents = fs.readFileSync(target, 'utf8');
            if (contents === body) {
              fs.unlinkSync(target);
              removed.push(target);
            }
          }
        } catch {}
      }
    }
    return removed;
  } catch {
    return [];
  }
}

function getCliShimCandidateDirs(options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const platform = options.platform || process.platform;
  if (options.binDir) {
    return [path.resolve(options.binDir)];
  }
  const dirs = [];
  if (env.AGY_PULSE_BIN_DIR) {
    dirs.push(env.AGY_PULSE_BIN_DIR);
  }
  dirs.push(path.join(homeDir, '.local', 'bin'));
  if (platform === 'win32' && env.LOCALAPPDATA) {
    dirs.push(path.join(env.LOCALAPPDATA, 'agy', 'bin'));
  }
  return [...new Set(dirs.map(dir => path.resolve(dir)))];
}

function isOurCliShimContent(contents) {
  if (typeof contents !== 'string' || !contents) return false;
  return (
    contents.includes('agy-pulse-runtime') ||
    contents.includes('# agy-pulse CLI shim') ||
    contents.includes('rem agy-pulse CLI shim')
  );
}

function removeCliShims(options = {}) {
  const removed = [];
  for (const dir of getCliShimCandidateDirs(options)) {
    for (const name of ['agypl', 'agypl.cmd', 'agy-pulse', 'agy-pulse.cmd']) {
      const target = path.join(dir, name);
      try {
        const stat = fs.lstatSync(target);
        if (!stat.isFile()) continue;
        const contents = fs.readFileSync(target, 'utf8');
        if (isOurCliShimContent(contents)) {
          fs.unlinkSync(target);
          removed.push(target);
        }
      } catch {
        // best-effort
      }
    }
  }
  return removed;
}

function removeTempArtifacts(tmpDir = os.tmpdir()) {
  const removed = [];
  try {
    for (const entry of fs.readdirSync(tmpDir)) {
      if (entry.startsWith('agy-pulse-db-') || entry.startsWith('agy-pulse-bootstrap-')) {
        const target = path.join(tmpDir, entry);
        try {
          fs.rmSync(target, { recursive: true, force: true });
          removed.push(target);
        } catch {
          // best-effort
        }
      }
    }
  } catch {
    // best-effort
  }
  return removed;
}

const LEDGER_PRIMARY_FILENAMES = [
  'agy-pulse-ledger.sqlite',
  'agy-pulse-ledger.json',
];

const LEDGER_ALL_FILENAMES = [
  ...LEDGER_PRIMARY_FILENAMES,
  'agy-pulse-ledger.sqlite-wal',
  'agy-pulse-ledger.sqlite-shm',
  'agy-pulse-ledger.sqlite-journal',
];

function handleLedgerFiles(root, purgeLedger = false) {
  const preserved = [];
  const purged = [];
  if (!purgeLedger) {
    for (const filename of LEDGER_PRIMARY_FILENAMES) {
      const target = path.join(root, filename);
      if (fs.existsSync(target)) {
        preserved.push(target);
      }
    }
    return { preserved, purged };
  }

  for (const filename of LEDGER_ALL_FILENAMES) {
    const target = path.join(root, filename);
    if (fs.existsSync(target)) {
      try {
        fs.rmSync(target, { force: true });
        purged.push(target);
      } catch {
        // best-effort
      }
    }
  }
  return { preserved, purged };
}

function removeDashboardShortcuts(options = {}) {
  const launcher = loadDashboardLauncher();
  if (!launcher || typeof launcher.uninstallDesktopShortcut !== 'function') {
    return { removed: [], skipped: [] };
  }
  const callOptions = {
    platform: options.platform || process.platform,
    env: options.env || process.env,
    homeDir: options.homeDir || os.homedir(),
  };
  if (options.spawnSync !== undefined) {
    callOptions.spawnSync = options.spawnSync;
  }
  return launcher.uninstallDesktopShortcut(callOptions);
}

function uninstall(options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const roots = options.roots || getAntigravityRoots(env, homeDir);
  const settingsPath = options.settingsPath || resolveAntigravityPath('settings.json', env, homeDir);
  const purgeLedger = Boolean(options.purgeLedger);

  const results = {
    settingsPath,
    statusLine: null,
    secondarySettings: [],
    runtimes: [],
    tokenMirrors: [],
    extraFiles: [],
    tempArtifacts: [],
    cliShims: [],
    dashboardShortcuts: { removed: [], skipped: [] },
    preservedLedgers: [],
    purgedLedgers: [],
  };

  const seenLedgers = new Set();

  const ourCommandMatchers = [];
  for (const root of roots) {
    ourCommandMatchers.push(
      path.join(root, RUNTIME_DIR_NAME),
      path.join(root, 'statusline-combined.sh')
    );
  }
  const activeBaseDir = path.dirname(settingsPath);
  ourCommandMatchers.push(
    path.join(activeBaseDir, RUNTIME_DIR_NAME),
    path.join(activeBaseDir, 'statusline-combined.sh')
  );

  const clearedSettingsPaths = new Set();

  for (const root of roots) {
    const pulseRuntimeDir = path.join(root, RUNTIME_DIR_NAME);
    const runtimeResult = removeRuntimeDir(pulseRuntimeDir);
    results.runtimes.push({ runtimeDir: pulseRuntimeDir, ...runtimeResult });
    const rootSettings = path.join(root, 'settings.json');
    if (path.resolve(rootSettings) !== path.resolve(settingsPath)) {
      clearedSettingsPaths.add(path.resolve(rootSettings));
      results.runtimes.push({
        runtimeDir: rootSettings,
        ...clearStatusLine(rootSettings, [
          pulseRuntimeDir,
          path.join(root, 'statusline-combined.sh'),
        ]),
      });
    }
    results.extraFiles.push(...removeExtraFiles(root));

    const ledgerRes = handleLedgerFiles(root, purgeLedger);
    for (const p of ledgerRes.preserved) {
      const resolved = path.resolve(p);
      if (!seenLedgers.has(resolved)) {
        seenLedgers.add(resolved);
        results.preservedLedgers.push(p);
      }
    }
    for (const p of ledgerRes.purged) {
      const resolved = path.resolve(p);
      if (!seenLedgers.has(resolved)) {
        seenLedgers.add(resolved);
        results.purgedLedgers.push(p);
      }
    }
  }

  clearedSettingsPaths.add(path.resolve(settingsPath));
  results.statusLine = clearStatusLine(settingsPath, [
    path.join(activeBaseDir, RUNTIME_DIR_NAME),
    path.join(activeBaseDir, 'statusline-combined.sh'),
  ]);

  const secondaryCandidates = [
    path.join(homeDir, '.gemini', 'config', 'settings.json'),
    path.join(homeDir, '.gemini', 'settings.json'),
  ];
  if (env.XDG_CONFIG_HOME) {
    secondaryCandidates.push(path.join(env.XDG_CONFIG_HOME, 'gemini', 'settings.json'));
  }
  for (const candidate of secondaryCandidates) {
    const resolved = path.resolve(candidate);
    if (clearedSettingsPaths.has(resolved)) continue;
    clearedSettingsPaths.add(resolved);
    results.secondarySettings.push({
      settingsPath: candidate,
      ...clearStatusLine(candidate, ourCommandMatchers),
    });
  }

  results.tokenMirrors = removeTokenMirrors();
  results.tempArtifacts = removeTempArtifacts(options.tmpDir || os.tmpdir());
  results.shShims = removeWindowsShShims(process.platform, env);
  results.cliShims = removeCliShims({
    env,
    homeDir,
    platform: options.platform || process.platform,
    binDir: options.binDir,
  });
  results.dashboardShortcuts = removeDashboardShortcuts({
    platform: options.platform || process.platform,
    env,
    homeDir,
    spawnSync: options.spawnSync,
  });

  return results;
}

if (require.main === module) {
  const purgeLedger = process.argv.includes('--purge-ledger') || process.argv.includes('--purge');
  const result = uninstall({ purgeLedger });
  process.stdout.write(`agy-pulse uninstall complete\n`);
  process.stdout.write(`settings: ${result.settingsPath} (${result.statusLine.changed ? 'cleared' : result.statusLine.reason})\n`);
  for (const r of result.runtimes) {
    if (r.removed) process.stdout.write(`runtime removed: ${r.runtimeDir}\n`);
  }
  for (const f of result.tokenMirrors) {
    process.stdout.write(`token mirror removed: ${f}\n`);
  }
  for (const f of result.extraFiles) {
    process.stdout.write(`extra file removed: ${f}\n`);
  }
  for (const f of result.shShims) {
    process.stdout.write(`Windows sh shim removed: ${f}\n`);
  }
  for (const f of result.cliShims) {
    process.stdout.write(`CLI shim removed: ${f}\n`);
  }
  for (const f of (result.dashboardShortcuts && result.dashboardShortcuts.removed) || []) {
    process.stdout.write(`dashboard shortcut removed: ${f}\n`);
  }
  for (const f of result.purgedLedgers) {
    process.stdout.write(`ledger removed: ${f}\n`);
  }
  for (const f of result.preservedLedgers) {
    process.stdout.write(`ledger preserved: ${f} (pass --purge-ledger to remove)\n`);
  }
}

module.exports = {
  LEDGER_PRIMARY_FILENAMES,
  LEDGER_ALL_FILENAMES,
  isOurStatusLineCommand,
  clearStatusLine,
  removeRuntimeDir,
  removeTokenMirrors,
  removeTempArtifacts,
  removeExtraFiles,
  removeWindowsShShims,
  isOurCliShimContent,
  removeCliShims,
  removeDashboardShortcuts,
  handleLedgerFiles,
  uninstall,
};
