import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const uninstallModule = require('../../runtime/uninstall.js');
const { removeExtraFiles, removeTempArtifacts, removeWindowsShShims, removeDashboardShortcuts, clearStatusLine, uninstall } = uninstallModule;

test('removeExtraFiles removes agy-pulse-payload.json and the legacy status line directory', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-extra-test-'));
  try {
    const payloadFile = path.join(tmp, 'agy-pulse-payload.json');
    const tokenFile = path.join(tmp, 'agy-pulse-token.json');
    const cacheFile = path.join(tmp, 'agy-pulse-quota-cache.json');
    const errorFile = path.join(tmp, 'agy-pulse-error.log');
    const dailyCostFile = path.join(tmp, 'daily-cost.json');
    const speedCacheDir = path.join(tmp, 'speed-cache');
    const legacyStatusLineDir = path.join(tmp, 'hud');
    const otherFile = path.join(tmp, 'other.json');

    fs.writeFileSync(payloadFile, '{}');
    fs.writeFileSync(tokenFile, '{}');
    fs.writeFileSync(cacheFile, '{}');
    fs.writeFileSync(errorFile, '{}');
    fs.writeFileSync(dailyCostFile, '{}');
    fs.mkdirSync(speedCacheDir, { recursive: true });
    fs.writeFileSync(path.join(speedCacheDir, 'cache.json'), '{}');
    fs.mkdirSync(legacyStatusLineDir, { recursive: true });
    fs.writeFileSync(path.join(legacyStatusLineDir, 'file.txt'), 'hello');
    fs.writeFileSync(otherFile, '{}');

    const removed = removeExtraFiles(tmp);

    assert.ok(removed.includes(payloadFile));
    assert.ok(removed.includes(tokenFile));
    assert.ok(removed.includes(cacheFile));
    assert.ok(removed.includes(errorFile));
    assert.ok(removed.includes(dailyCostFile));
    assert.ok(removed.includes(speedCacheDir));
    assert.ok(removed.includes(legacyStatusLineDir));
    assert.ok(!fs.existsSync(payloadFile));
    assert.ok(!fs.existsSync(tokenFile));
    assert.ok(!fs.existsSync(cacheFile));
    assert.ok(!fs.existsSync(errorFile));
    assert.ok(!fs.existsSync(dailyCostFile));
    assert.ok(!fs.existsSync(speedCacheDir));
    assert.ok(!fs.existsSync(legacyStatusLineDir));
    assert.ok(fs.existsSync(otherFile));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('clearStatusLine removes statusLine field if it points to our runtime', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-settings-test-'));
  try {
    const settingsPath = path.join(tmp, 'settings.json');
    const runtimeDir = path.join(tmp, 'agy-pulse-runtime');

    const settingsObj = {
      statusLine: {
        type: 'command',
        command: `node "${runtimeDir}/runtime/bin/agy-pulse.js"`,
        enabled: true
      },
      otherField: 'value'
    };
    fs.writeFileSync(settingsPath, JSON.stringify(settingsObj, null, 2));

    const result = clearStatusLine(settingsPath, runtimeDir);
    assert.equal(result.changed, true);

    const updatedSettings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    assert.equal(updatedSettings.statusLine, undefined);
    assert.equal(updatedSettings.otherField, 'value');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('clearStatusLine leaves foreign statusLine commands untouched', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-foreign-test-'));
  try {
    const settingsPath = path.join(tmp, 'settings.json');
    const runtimeDir = path.join(tmp, 'agy-pulse-runtime');

    const settingsObj = {
      statusLine: {
        type: 'command',
        command: 'node "/other/path/bin/script.js"',
        enabled: true
      }
    };
    fs.writeFileSync(settingsPath, JSON.stringify(settingsObj, null, 2));

    const result = clearStatusLine(settingsPath, runtimeDir);
    assert.equal(result.changed, false);
    assert.equal(result.reason, 'foreign_status_line');

    const updatedSettings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    assert.equal(updatedSettings.statusLine.command, 'node "/other/path/bin/script.js"');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('removeWindowsShShims cleans up only our shims on Windows', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-shims-test-'));
  try {
    const installer = require('../../runtime/statusline-installer.js');
    const shimContents = installer.buildShShimContents();

    const ourShim = path.join(tmp, 'sh.cmd');
    const foreignShim = path.join(tmp, 'sh.bat');

    // Create our shim matching exact contents
    fs.writeFileSync(ourShim, shimContents, 'utf8');
    // Create foreign shim with different contents
    fs.writeFileSync(foreignShim, 'different contents', 'utf8');

    // Mock getWindowsAgyBinDirs to return our temp directory
    const originalGetDirs = installer.getWindowsAgyBinDirs;
    installer.getWindowsAgyBinDirs = () => [tmp];

    try {
      const removed = removeWindowsShShims('win32', {});
      assert.ok(removed.includes(ourShim));
      assert.ok(!removed.includes(foreignShim));
      assert.ok(!fs.existsSync(ourShim));
      assert.ok(fs.existsSync(foreignShim));
    } finally {
      installer.getWindowsAgyBinDirs = originalGetDirs;
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('uninstall removes agy-pulse-runtime using injected env/homeDir', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-pulse-runtime-'));
  try {
    const home = path.join(tmp, 'home');
    const root = path.join(home, '.gemini', 'antigravity-cli');
    const pulseRuntime = path.join(root, 'agy-pulse-runtime');
    const settingsPath = path.join(root, 'settings.json');
    fs.mkdirSync(pulseRuntime, { recursive: true });
    fs.writeFileSync(path.join(root, 'agy-pulse-compaction-cache.json'), '{}');
    fs.writeFileSync(settingsPath, JSON.stringify({
      statusLine: { type: 'command', command: `node "${pulseRuntime}/runtime/bin/agy-pulse.js"` },
    }));

    const res = uninstall({
      homeDir: home,
      env: { HOME: home, USERPROFILE: home },
      settingsPath,
    });

    assert.equal(res.statusLine.changed, true);
    assert.equal(fs.existsSync(pulseRuntime), false);
    assert.equal(fs.existsSync(path.join(root, 'agy-pulse-compaction-cache.json')), false);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('uninstall cleans plugin_data, secondary settings statusLine, and temp artifacts', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-full-cleanup-'));
  try {
    const home = path.join(tmp, 'home');
    const tmpDir = path.join(tmp, 'tmp');
    const root = path.join(home, '.gemini', 'antigravity-cli');
    const pulseRuntime = path.join(root, 'agy-pulse-runtime');
    const primarySettings = path.join(root, 'settings.json');
    const configSettings = path.join(home, '.gemini', 'config', 'settings.json');
    const rootGeminiSettings = path.join(home, '.gemini', 'settings.json');
    const pluginDataDir = path.join(root, 'plugin_data', 'agy-pulse');
    const dbTmp = path.join(tmpDir, 'agy-pulse-db-123');
    const bootstrapTmp = path.join(tmpDir, 'agy-pulse-bootstrap-456.js');
    const unrelatedTmp = path.join(tmpDir, 'unrelated-file.txt');

    fs.mkdirSync(pulseRuntime, { recursive: true });
    fs.mkdirSync(pluginDataDir, { recursive: true });
    fs.mkdirSync(path.dirname(configSettings), { recursive: true });
    fs.mkdirSync(tmpDir, { recursive: true });
    fs.writeFileSync(path.join(pluginDataDir, 'state.json'), '{}');
    fs.writeFileSync(primarySettings, JSON.stringify({}));
    fs.writeFileSync(configSettings, JSON.stringify({
      statusLine: { type: 'command', command: `node "${pulseRuntime}/runtime/bin/agy-pulse.js"` },
      keepMe: true,
    }));
    fs.writeFileSync(rootGeminiSettings, JSON.stringify({
      statusLine: { type: 'command', command: 'node "/other/plugin.js"' },
    }));
    fs.mkdirSync(dbTmp, { recursive: true });
    fs.writeFileSync(bootstrapTmp, '// tmp');
    fs.writeFileSync(unrelatedTmp, 'keep');

    const res = uninstall({
      homeDir: home,
      env: { HOME: home, USERPROFILE: home },
      settingsPath: primarySettings,
      tmpDir,
    });

    assert.equal(fs.existsSync(pluginDataDir), false);
    assert.equal(fs.existsSync(dbTmp), false);
    assert.equal(fs.existsSync(bootstrapTmp), false);
    assert.equal(fs.existsSync(unrelatedTmp), true);

    const updatedConfigSettings = JSON.parse(fs.readFileSync(configSettings, 'utf8'));
    assert.equal(updatedConfigSettings.statusLine, undefined);
    assert.equal(updatedConfigSettings.keepMe, true);

    const untouchedGeminiSettings = JSON.parse(fs.readFileSync(rootGeminiSettings, 'utf8'));
    assert.equal(untouchedGeminiSettings.statusLine.command, 'node "/other/plugin.js"');
    assert.ok(res.tempArtifacts.includes(dbTmp));
    assert.ok(res.tempArtifacts.includes(bootstrapTmp));
    assert.equal(typeof removeTempArtifacts, 'function');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('uninstall removes only agy-pulse CLI shims (agypl / agy-pulse / .cmd) and leaves unrelated user scripts untouched', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-cli-shims-'));
  try {
    const home = path.join(tmp, 'home');
    const customBin = path.join(tmp, 'custom-bin');
    const defaultBin = path.join(home, '.local', 'bin');
    fs.mkdirSync(customBin, { recursive: true });
    fs.mkdirSync(defaultBin, { recursive: true });

    const ourAgypl = path.join(customBin, 'agypl');
    const ourAlias = path.join(customBin, 'agy-pulse');
    const ourWinCmd = path.join(customBin, 'agypl.cmd');
    const foreignDefault = path.join(defaultBin, 'agypl');
    const foreignAlias = path.join(defaultBin, 'agy-pulse');

    fs.writeFileSync(ourAgypl, '#!/bin/sh\n# agy-pulse CLI shim\nexec node "/x/agy-pulse-runtime/runtime/bin/agy-pulse.js" "$@"\n');
    fs.writeFileSync(ourAlias, '#!/bin/sh\nexec node "/x/agy-pulse-runtime/runtime/bin/agy-pulse.js" "$@"\n');
    fs.writeFileSync(ourWinCmd, '@echo off\r\nrem agy-pulse CLI shim\r\nnode "C:\\x\\agy-pulse-runtime\\runtime\\bin\\agy-pulse.js" %*\r\n');
    fs.writeFileSync(foreignDefault, '#!/bin/sh\necho "user custom script"\n');
    fs.writeFileSync(foreignAlias, '#!/bin/sh\necho "another tool"\n');

    const res = uninstall({
      homeDir: home,
      env: { HOME: home, USERPROFILE: home, AGY_PULSE_BIN_DIR: customBin },
      settingsPath: path.join(home, '.gemini', 'antigravity-cli', 'settings.json'),
      tmpDir: path.join(tmp, 'tmp'),
    });

    assert.equal(fs.existsSync(ourAgypl), false);
    assert.equal(fs.existsSync(ourAlias), false);
    assert.equal(fs.existsSync(ourWinCmd), false);
    assert.equal(fs.existsSync(foreignDefault), true);
    assert.equal(fs.existsSync(foreignAlias), true);
    assert.ok(res.cliShims.includes(ourAgypl));
    assert.ok(res.cliShims.includes(ourAlias));
    assert.ok(res.cliShims.includes(ourWinCmd));
    assert.ok(!res.cliShims.includes(foreignDefault));
    assert.ok(!res.cliShims.includes(foreignAlias));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('removeExtraFiles removes agy-pulse-dashboard.json', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-dash-state-'));
  try {
    const dashboardStateFile = path.join(tmp, 'agy-pulse-dashboard.json');
    const keepFile = path.join(tmp, 'keep.json');
    fs.writeFileSync(dashboardStateFile, JSON.stringify({ pid: 12345, port: 3789 }), 'utf8');
    fs.writeFileSync(keepFile, '{}', 'utf8');

    const removed = removeExtraFiles(tmp);

    assert.ok(removed.includes(dashboardStateFile));
    assert.equal(fs.existsSync(dashboardStateFile), false);
    assert.equal(fs.existsSync(keepFile), true);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('removeDashboardShortcuts removes marked shortcuts and protects unmarked foreign files', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-dash-shortcuts-'));
  try {
    const { DASHBOARD_SHORTCUT_MARKER } = require('../../runtime/dashboard-launcher.js');
    const home = path.join(tmp, 'home');
    const xdgData = path.join(tmp, 'xdg-data');
    const env = { HOME: home, USERPROFILE: home, XDG_DATA_HOME: xdgData };

    const desktopEntry = path.join(xdgData, 'applications', 'agy-pulse-dashboard.desktop');
    const desktopCopy = path.join(home, 'Desktop', 'agy-pulse-dashboard.desktop');
    const iconFile = path.join(xdgData, 'icons', 'hicolor', 'scalable', 'apps', 'agy-pulse-dashboard.svg');
    fs.mkdirSync(path.dirname(desktopEntry), { recursive: true });
    fs.mkdirSync(path.dirname(desktopCopy), { recursive: true });
    fs.mkdirSync(path.dirname(iconFile), { recursive: true });

    // Case 1: marked shortcuts & icon should be removed
    fs.writeFileSync(desktopEntry, `[Desktop Entry]\n# ${DASHBOARD_SHORTCUT_MARKER}\nName=agy-pulse Dashboard\n`, 'utf8');
    fs.writeFileSync(desktopCopy, `[Desktop Entry]\n# ${DASHBOARD_SHORTCUT_MARKER}\nName=agy-pulse Dashboard\n`, 'utf8');
    fs.writeFileSync(iconFile, '<svg><!-- agy-pulse-dashboard-icon --></svg>', 'utf8');

    const spawnCalls = [];
    const removedRes = removeDashboardShortcuts({
      platform: 'linux',
      env,
      homeDir: home,
      spawnSync: (cmd, args) => {
        spawnCalls.push({ cmd, args });
        return { status: 0 };
      },
    });

    assert.ok(removedRes.removed.includes(desktopEntry));
    assert.ok(removedRes.removed.includes(desktopCopy));
    assert.ok(removedRes.removed.includes(iconFile));
    assert.equal(fs.existsSync(desktopEntry), false);
    assert.equal(fs.existsSync(desktopCopy), false);
    assert.equal(fs.existsSync(iconFile), false);
    assert.ok(spawnCalls.some(c => c.cmd === 'update-desktop-database'));

    // Case 2: foreign unmarked files at the same paths must be preserved
    fs.writeFileSync(desktopEntry, '[Desktop Entry]\nName=Foreign App\n', 'utf8');
    fs.writeFileSync(desktopCopy, '[Desktop Entry]\nName=Foreign Desktop Copy\n', 'utf8');
    fs.writeFileSync(iconFile, '<svg>foreign-icon</svg>', 'utf8');

    const foreignRes = removeDashboardShortcuts({
      platform: 'linux',
      env,
      homeDir: home,
      spawnSync: () => ({ status: 0 }),
    });

    assert.deepEqual(foreignRes.removed, []);
    assert.ok(foreignRes.skipped.includes(desktopEntry));
    assert.ok(foreignRes.skipped.includes(desktopCopy));
    assert.equal(fs.existsSync(desktopEntry), true);
    assert.equal(fs.existsSync(desktopCopy), true);
    assert.equal(fs.existsSync(iconFile), true);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('uninstall cleans agy-pulse-dashboard.json and marked dashboard shortcuts while preserving unmarked files', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'uninstall-dash-integration-'));
  try {
    const { DASHBOARD_SHORTCUT_MARKER } = require('../../runtime/dashboard-launcher.js');
    const home = path.join(tmp, 'home');
    const xdgData = path.join(tmp, 'xdg-data');
    const root = path.join(home, '.gemini', 'antigravity-cli');
    const settingsPath = path.join(root, 'settings.json');
    const dashState = path.join(root, 'agy-pulse-dashboard.json');
    const desktopEntry = path.join(xdgData, 'applications', 'agy-pulse-dashboard.desktop');
    const foreignDesktopCopy = path.join(home, 'Desktop', 'agy-pulse-dashboard.desktop');

    fs.mkdirSync(root, { recursive: true });
    fs.mkdirSync(path.dirname(desktopEntry), { recursive: true });
    fs.mkdirSync(path.dirname(foreignDesktopCopy), { recursive: true });
    fs.writeFileSync(settingsPath, JSON.stringify({}), 'utf8');
    fs.writeFileSync(dashState, JSON.stringify({ pid: 99999, port: 3789 }), 'utf8');
    fs.writeFileSync(desktopEntry, `[Desktop Entry]\n# ${DASHBOARD_SHORTCUT_MARKER}\n`, 'utf8');
    fs.writeFileSync(foreignDesktopCopy, '[Desktop Entry]\nName=User Custom\n', 'utf8');

    const res = uninstall({
      homeDir: home,
      env: { HOME: home, USERPROFILE: home, XDG_DATA_HOME: xdgData },
      settingsPath,
      platform: 'linux',
      spawnSync: () => ({ status: 0 }),
      tmpDir: path.join(tmp, 'tmp'),
    });

    assert.ok(res.extraFiles.includes(dashState));
    assert.equal(fs.existsSync(dashState), false);
    assert.ok(res.dashboardShortcuts.removed.includes(desktopEntry));
    assert.equal(fs.existsSync(desktopEntry), false);
    assert.ok(res.dashboardShortcuts.skipped.includes(foreignDesktopCopy));
    assert.equal(fs.existsSync(foreignDesktopCopy), true);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
