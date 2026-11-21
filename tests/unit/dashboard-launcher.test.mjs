import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const launcher = require('../../runtime/dashboard-launcher.js');

const {
  DASHBOARD_APP_ID,
  DASHBOARD_STATE_FILENAME,
  DASHBOARD_SHORTCUT_MARKER,
  DEFAULT_IDLE_TIMEOUT_SEC,
  DEFAULT_IDLE_TIMEOUT_MS,
  DASHBOARD_ICON_SVG,
  getDashboardStatePath,
  readDashboardState,
  writeDashboardState,
  clearDashboardStateIfOwner,
  probeDashboardHealth,
  findRunningDashboard,
  ensureBackgroundDashboard,
  findAppWindowBrowser,
  openDashboardWindow,
  stopBackgroundDashboard,
  getDesktopShortcutPaths,
  isOurShortcutContent,
  installDesktopShortcut,
  uninstallDesktopShortcut,
  getDesktopShortcutStatus,
} = launcher;

function makeTempDir(prefix = 'agy-pulse-launcher-test-') {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

test('dashboard-launcher exports expected constants and marked SVG icon', () => {
  assert.equal(DASHBOARD_APP_ID, 'agy-pulse-dashboard');
  assert.equal(DASHBOARD_STATE_FILENAME, 'agy-pulse-dashboard.json');
  assert.equal(DASHBOARD_SHORTCUT_MARKER, 'agy-pulse-dashboard-shortcut');
  assert.equal(DEFAULT_IDLE_TIMEOUT_SEC, 600);
  assert.equal(DEFAULT_IDLE_TIMEOUT_MS, 600000);
  assert.ok(DASHBOARD_ICON_SVG.includes('xmlns="http://www.w3.org/2000/svg"'));
  assert.ok(DASHBOARD_ICON_SVG.includes('viewBox="0 0 64 64"'));
  assert.ok(DASHBOARD_ICON_SVG.includes('<!-- agy-pulse-dashboard-icon -->'));
});

test('state file helpers read, write atomically, and clear only when owner PID matches', () => {
  const tmpDir = makeTempDir();
  try {
    const env = { AGY_PULSE_DATA_DIR: path.join(tmpDir, 'data-root') };
    const statePath = getDashboardStatePath({ env, homeDir: tmpDir });
    assert.equal(statePath, path.join(tmpDir, 'data-root', DASHBOARD_STATE_FILENAME));
    assert.equal(readDashboardState({ env, homeDir: tmpDir }), null);

    // Invalid JSON returns null safely
    fs.mkdirSync(path.dirname(statePath), { recursive: true });
    fs.writeFileSync(statePath, '{not-valid-json', 'utf8');
    assert.equal(readDashboardState({ env, homeDir: tmpDir }), null);

    writeDashboardState(
      {
        pid: 424242,
        port: 3799,
        host: '127.0.0.1',
        url: 'http://127.0.0.1:3799',
        idleTimeoutMs: 600000,
        startedAt: '2026-11-21T00:00:00.000Z',
      },
      { env, homeDir: tmpDir }
    );

    const readBack = readDashboardState({ env, homeDir: tmpDir });
    assert.deepEqual(readBack, {
      app: DASHBOARD_APP_ID,
      pid: 424242,
      port: 3799,
      host: '127.0.0.1',
      url: 'http://127.0.0.1:3799',
      idleTimeoutMs: 600000,
      startedAt: '2026-11-21T00:00:00.000Z',
    });

    // Wrong owner does not remove state file
    assert.equal(clearDashboardStateIfOwner(999999, { env, homeDir: tmpDir }), false);
    assert.equal(fs.existsSync(statePath), true);

    // Matching owner removes state file; second call returns false silently
    assert.equal(clearDashboardStateIfOwner(424242, { env, homeDir: tmpDir }), true);
    assert.equal(fs.existsSync(statePath), false);
    assert.equal(clearDashboardStateIfOwner(424242, { env, homeDir: tmpDir }), false);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('probeDashboardHealth distinguishes healthy dashboard, occupied port, and dead port', async () => {
  // 1. Healthy agy-pulse dashboard
  const goodServer = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(
      JSON.stringify({
        ok: true,
        app: DASHBOARD_APP_ID,
        pid: 12345,
        port: goodServer.address().port,
        idleTimeoutMs: 600000,
        startedAt: '2026-11-21T00:00:00.000Z',
      })
    );
  });
  await new Promise((resolve) => goodServer.listen(0, '127.0.0.1', resolve));

  // 2. Port occupied by another service
  const otherServer = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true, app: 'something-else' }));
  });
  await new Promise((resolve) => otherServer.listen(0, '127.0.0.1', resolve));

  // 3. Port returning 404
  const notFoundServer = http.createServer((req, res) => {
    res.writeHead(404);
    res.end('nope');
  });
  await new Promise((resolve) => notFoundServer.listen(0, '127.0.0.1', resolve));

  // 4. Unused ephemeral port
  const tempServer = http.createServer();
  await new Promise((resolve) => tempServer.listen(0, '127.0.0.1', resolve));
  const deadPort = tempServer.address().port;
  await new Promise((resolve) => tempServer.close(resolve));

  try {
    const goodPort = goodServer.address().port;
    const goodRes = await probeDashboardHealth({ host: '127.0.0.1', port: goodPort });
    assert.deepEqual(goodRes, {
      alive: true,
      occupiedByOther: false,
      url: `http://127.0.0.1:${goodPort}`,
      host: '127.0.0.1',
      port: goodPort,
      pid: 12345,
      idleTimeoutMs: 600000,
      startedAt: '2026-11-21T00:00:00.000Z',
    });

    const otherRes = await probeDashboardHealth({ host: '127.0.0.1', port: otherServer.address().port });
    assert.deepEqual(otherRes, { alive: false, occupiedByOther: true, statusCode: 200 });

    const nfRes = await probeDashboardHealth({ host: '127.0.0.1', port: notFoundServer.address().port });
    assert.deepEqual(nfRes, { alive: false, occupiedByOther: true, statusCode: 404 });

    const deadRes = await probeDashboardHealth({ host: '127.0.0.1', port: deadPort, timeoutMs: 250 });
    assert.deepEqual(deadRes, { alive: false, occupiedByOther: false });
  } finally {
    await new Promise((resolve) => goodServer.close(resolve));
    await new Promise((resolve) => otherServer.close(resolve));
    await new Promise((resolve) => notFoundServer.close(resolve));
  }
});

test('findRunningDashboard falls back to state file port when default port differs', async () => {
  const tmpDir = makeTempDir();
  try {
    const env = { AGY_PULSE_DATA_DIR: tmpDir };
    writeDashboardState(
      { pid: 777, port: 4100, host: '127.0.0.1', url: 'http://127.0.0.1:4100' },
      { env, homeDir: tmpDir }
    );

    const probed = [];
    const found = await findRunningDashboard({
      port: 3789,
      host: '127.0.0.1',
      env,
      homeDir: tmpDir,
      probeHealth: async ({ port }) => {
        probed.push(port);
        if (port === 4100) {
          return { alive: true, occupiedByOther: false, url: 'http://127.0.0.1:4100', host: '127.0.0.1', port: 4100, pid: 777 };
        }
        return { alive: false, occupiedByOther: false };
      },
    });

    assert.deepEqual(probed, [3789, 4100]);
    assert.equal(found.port, 4100);
    assert.equal(found.pid, 777);

    const none = await findRunningDashboard({
      port: 3789,
      host: '127.0.0.1',
      env,
      homeDir: tmpDir,
      probeHealth: async () => ({ alive: false, occupiedByOther: false }),
    });
    assert.equal(none, null);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('ensureBackgroundDashboard reuses existing instance or spawns detached background daemon', async () => {
  const tmpDir = makeTempDir();
  try {
    const env = { AGY_PULSE_DATA_DIR: tmpDir };
    let spawnCount = 0;

    // 1. Reuse existing
    const reused = await ensureBackgroundDashboard({
      port: 3789,
      env,
      homeDir: tmpDir,
      spawn: () => {
        spawnCount += 1;
        return { on() {}, unref() {} };
      },
      probeHealth: async () => ({
        alive: true,
        occupiedByOther: false,
        url: 'http://127.0.0.1:3789',
        host: '127.0.0.1',
        port: 3789,
        pid: 111,
      }),
    });
    assert.equal(reused.reused, true);
    assert.equal(reused.pid, 111);
    assert.equal(spawnCount, 0);

    // 2. Spawn detached when not running, become alive on 3rd probe
    let probeCalls = 0;
    const spawned = [];
    const started = await ensureBackgroundDashboard({
      port: 3790,
      idleTimeoutSec: 120,
      lang: 'zh',
      noSync: true,
      env,
      homeDir: tmpDir,
      execPath: '/usr/local/bin/node',
      runtimeEntry: '/app/runtime/bin/agy-pulse.js',
      pollIntervalMs: 5,
      waitTimeoutMs: 500,
      spawn: (cmd, args, opts) => {
        spawned.push({ cmd, args, opts });
        return { on() {}, unref() {} };
      },
      probeHealth: async () => {
        probeCalls += 1;
        if (probeCalls >= 3) {
          return {
            alive: true,
            occupiedByOther: false,
            url: 'http://127.0.0.1:3790',
            host: '127.0.0.1',
            port: 3790,
            pid: 222,
          };
        }
        return { alive: false, occupiedByOther: false };
      },
    });

    assert.equal(started.reused, false);
    assert.equal(started.pid, 222);
    assert.equal(spawned.length, 1);
    assert.equal(spawned[0].cmd, '/usr/local/bin/node');
    assert.deepEqual(spawned[0].args, [
      '/app/runtime/bin/agy-pulse.js',
      'dashboard',
      '--serve-background',
      '--no-open',
      '--port',
      '3790',
      '--idle-timeout',
      '120',
      '--lang',
      'zh',
      '--no-sync',
    ]);
    assert.equal(spawned[0].opts.detached, true);
    assert.equal(spawned[0].opts.stdio, 'ignore');

    // 3. Timeout throws informative error
    await assert.rejects(
      () =>
        ensureBackgroundDashboard({
          port: 3791,
          env,
          homeDir: tmpDir,
          pollIntervalMs: 5,
          waitTimeoutMs: 25,
          spawn: () => ({ on() {}, unref() {} }),
          probeHealth: async () => ({ alive: false, occupiedByOther: false }),
        }),
      /Timed out/i
    );
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('findAppWindowBrowser and openDashboardWindow detect Chromium app browsers across platforms and fallback', () => {
  // Linux via resolveExecutable
  const linuxFound = findAppWindowBrowser({
    platform: 'linux',
    resolveExecutable: (name) => (name === 'chromium' ? '/usr/local/bin/chromium' : null),
    existsSync: () => false,
  });
  assert.equal(linuxFound.name, 'chromium');
  assert.equal(linuxFound.command, '/usr/local/bin/chromium');
  assert.deepEqual(linuxFound.argsForUrl('http://127.0.0.1:3789'), [
    '--app=http://127.0.0.1:3789',
    `--class=${DASHBOARD_APP_ID}`,
    '--new-window',
  ]);

  // Linux fallback /opt/google/chrome/google-chrome
  const linuxOpt = findAppWindowBrowser({
    platform: 'linux',
    resolveExecutable: () => null,
    existsSync: (p) => p === '/opt/google/chrome/google-chrome',
  });
  assert.equal(linuxOpt.command, '/opt/google/chrome/google-chrome');

  // macOS
  const darwinFound = findAppWindowBrowser({
    platform: 'darwin',
    homeDir: '/Users/dev',
    existsSync: (p) => p === '/Users/dev/Applications/Microsoft Edge.app',
  });
  assert.equal(darwinFound.command, 'open');
  assert.deepEqual(darwinFound.argsForUrl('http://127.0.0.1:3789'), [
    '-na',
    '/Users/dev/Applications/Microsoft Edge.app',
    '--args',
    '--app=http://127.0.0.1:3789',
    '--new-window',
  ]);

  // Windows
  const winFound = findAppWindowBrowser({
    platform: 'win32',
    env: { PROGRAMFILES: 'C:\\Program Files', LOCALAPPDATA: 'C:\\Users\\dev\\AppData\\Local' },
    existsSync: (p) => p === 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  });
  assert.equal(winFound.command, 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe');
  assert.deepEqual(winFound.argsForUrl('http://127.0.0.1:3789'), [
    '--app=http://127.0.0.1:3789',
    '--new-window',
  ]);

  // openDashboardWindow app mode vs browser fallback
  const launched = [];
  const appRes = openDashboardWindow('http://127.0.0.1:3789', {
    appMode: true,
    platform: 'linux',
    resolveExecutable: (name) => (name === 'google-chrome' ? '/usr/bin/google-chrome' : null),
    existsSync: () => false,
    spawn: (cmd, args) => {
      launched.push({ cmd, args });
      return { on() {}, unref() {} };
    },
  });
  assert.deepEqual(appRes, { ok: true, mode: 'app', browser: 'google-chrome' });
  assert.equal(launched[0].cmd, '/usr/bin/google-chrome');

  let fallbackCalled = null;
  const fallbackRes = openDashboardWindow('http://127.0.0.1:3789', {
    appMode: true,
    platform: 'linux',
    resolveExecutable: () => null,
    existsSync: () => false,
    fallbackOpenBrowser: (url, platform) => {
      fallbackCalled = { url, platform };
      return true;
    },
  });
  assert.deepEqual(fallbackRes, { ok: true, mode: 'browser', browser: 'default' });
  assert.deepEqual(fallbackCalled, { url: 'http://127.0.0.1:3789', platform: 'linux' });
});

test('stopBackgroundDashboard terminates running instance and cleans stale state files', async () => {
  const tmpDir = makeTempDir();
  try {
    const env = { AGY_PULSE_DATA_DIR: tmpDir };
    const statePath = writeDashboardState({ pid: 55555, port: 3789, host: '127.0.0.1' }, { env, homeDir: tmpDir });

    // Not running + dead pid cleans stale file
    const notRunningRes = await stopBackgroundDashboard({
      port: 3789,
      env,
      homeDir: tmpDir,
      probeHealth: async () => ({ alive: false, occupiedByOther: false }),
      kill: () => {
        const err = new Error('ESRCH');
        err.code = 'ESRCH';
        throw err;
      },
    });
    assert.deepEqual(notRunningRes, { stopped: false, reason: 'not_running' });
    assert.equal(fs.existsSync(statePath), false);

    // Running instance: sends SIGTERM, polls down, clears state
    writeDashboardState({ pid: 66666, port: 3789, host: '127.0.0.1' }, { env, homeDir: tmpDir });
    const signals = [];
    let checks = 0;
    const stopRes = await stopBackgroundDashboard({
      port: 3789,
      env,
      homeDir: tmpDir,
      pollIntervalMs: 5,
      waitTimeoutMs: 200,
      kill: (pid, sig) => {
        signals.push({ pid, sig });
      },
      probeHealth: async () => {
        checks += 1;
        if (checks === 1) {
          return { alive: true, occupiedByOther: false, url: 'http://127.0.0.1:3789', host: '127.0.0.1', port: 3789, pid: 66666 };
        }
        return { alive: false, occupiedByOther: false };
      },
    });
    assert.deepEqual(signals, [{ pid: 66666, sig: 'SIGTERM' }]);
    assert.deepEqual(stopRes, { stopped: true, pid: 66666, port: 3789, url: 'http://127.0.0.1:3789' });
    assert.equal(fs.existsSync(statePath), false);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('desktop shortcut install/status/uninstall on Linux enforces marker fingerprint protection', () => {
  const tmpDir = makeTempDir();
  try {
    const xdgDataHome = path.join(tmpDir, 'xdg-data');
    const env = { XDG_DATA_HOME: xdgDataHome };
    const syncCalls = [];
    const spawnSync = (cmd, args) => {
      syncCalls.push({ cmd, args });
      return { status: 0 };
    };

    assert.equal(isOurShortcutContent(`# ${DASHBOARD_SHORTCUT_MARKER}`), true);
    assert.equal(isOurShortcutContent('unrelated file'), false);

    assert.deepEqual(getDesktopShortcutStatus({ platform: 'linux', env, homeDir: tmpDir }), {
      installed: false,
      path: null,
    });

    const installRes = installDesktopShortcut({
      platform: 'linux',
      env,
      homeDir: tmpDir,
      execPath: '/usr/bin/node',
      runtimeEntry: '/opt/agy-pulse/runtime/bin/agy-pulse.js',
      includeDesktop: true,
      spawnSync,
    });
    assert.equal(installRes.installed, true);
    assert.equal(installRes.platform, 'linux');

    const paths = getDesktopShortcutPaths({ platform: 'linux', env, homeDir: tmpDir, includeDesktop: true });
    assert.equal(fs.existsSync(paths.desktopEntryPath), true);
    assert.equal(fs.existsSync(paths.desktopCopyPath), true);
    assert.equal(fs.existsSync(paths.iconPath), true);

    const entryText = fs.readFileSync(paths.desktopEntryPath, 'utf8');
    assert.ok(entryText.includes(DASHBOARD_SHORTCUT_MARKER));
    assert.ok(entryText.includes('Exec="/usr/bin/node" "/opt/agy-pulse/runtime/bin/agy-pulse.js" dashboard open'));
    assert.ok(entryText.includes(`StartupWMClass=${DASHBOARD_APP_ID}`));

    assert.deepEqual(getDesktopShortcutStatus({ platform: 'linux', env, homeDir: tmpDir }), {
      installed: true,
      path: paths.desktopEntryPath,
    });

    // Tamper desktopCopyPath without marker -> uninstall must skip it
    fs.writeFileSync(paths.desktopCopyPath, '[Desktop Entry]\nName=User Custom\n', 'utf8');
    const uninstallRes = uninstallDesktopShortcut({ platform: 'linux', env, homeDir: tmpDir, spawnSync });
    assert.ok(uninstallRes.removed.includes(paths.desktopEntryPath));
    assert.ok(uninstallRes.removed.includes(paths.iconPath));
    assert.ok(uninstallRes.skipped.includes(paths.desktopCopyPath));
    assert.equal(fs.existsSync(paths.desktopEntryPath), false);
    assert.equal(fs.existsSync(paths.iconPath), false);
    assert.equal(fs.existsSync(paths.desktopCopyPath), true);
    assert.ok(syncCalls.some((c) => c.cmd === 'update-desktop-database'));
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});

test('desktop shortcut install/status/uninstall on macOS and Windows respect markers', () => {
  const tmpDir = makeTempDir();
  try {
    // macOS
    const macHome = path.join(tmpDir, 'mac-home');
    const macInstall = installDesktopShortcut({
      platform: 'darwin',
      homeDir: macHome,
      execPath: '/usr/local/bin/node',
      runtimeEntry: '/app/agy-pulse.js',
    });
    assert.equal(macInstall.installed, true);
    const macPaths = getDesktopShortcutPaths({ platform: 'darwin', homeDir: macHome });
    assert.equal(fs.existsSync(macPaths.infoPlistPath), true);
    assert.equal(fs.existsSync(macPaths.launcherScriptPath), true);
    assert.equal(fs.existsSync(macPaths.iconPath), true);
    assert.deepEqual(getDesktopShortcutStatus({ platform: 'darwin', homeDir: macHome }), {
      installed: true,
      path: macPaths.appBundleDir,
    });

    const macUninstall = uninstallDesktopShortcut({ platform: 'darwin', homeDir: macHome });
    assert.deepEqual(macUninstall, { removed: [macPaths.appBundleDir], skipped: [] });
    assert.equal(fs.existsSync(macPaths.appBundleDir), false);

    // macOS without marker is skipped
    fs.mkdirSync(path.dirname(macPaths.infoPlistPath), { recursive: true });
    fs.writeFileSync(macPaths.infoPlistPath, '<plist>user app</plist>', 'utf8');
    const macSkipped = uninstallDesktopShortcut({ platform: 'darwin', homeDir: macHome });
    assert.deepEqual(macSkipped, { removed: [], skipped: [macPaths.appBundleDir] });
    assert.equal(fs.existsSync(macPaths.appBundleDir), true);

    // Windows
    const winHome = path.join(tmpDir, 'win-home');
    const winEnv = {
      LOCALAPPDATA: path.join(tmpDir, 'win-local'),
      APPDATA: path.join(tmpDir, 'win-roaming'),
    };
    const psCalls = [];
    const winInstall = installDesktopShortcut({
      platform: 'win32',
      env: winEnv,
      homeDir: winHome,
      execPath: 'C:\\Program Files\\nodejs\\node.exe',
      runtimeEntry: 'C:\\agy-pulse\\runtime\\bin\\agy-pulse.js',
      includeDesktop: true,
      spawnSync: (cmd, args) => {
        psCalls.push({ cmd, args });
        return { status: 0 };
      },
    });
    assert.equal(winInstall.installed, true);
    assert.equal(psCalls.length, 1);
    assert.equal(psCalls[0].cmd, 'powershell.exe');

    const winPaths = getDesktopShortcutPaths({ platform: 'win32', env: winEnv, homeDir: winHome, includeDesktop: true });
    assert.equal(fs.existsSync(winPaths.vbsPath), true);
    assert.equal(fs.existsSync(winPaths.iconPath), true);
    assert.equal(getDesktopShortcutStatus({ platform: 'win32', env: winEnv, homeDir: winHome }).installed, true);

    // Simulate .lnk creation + marker removal
    fs.mkdirSync(path.dirname(winPaths.startMenuLnkPath), { recursive: true });
    fs.writeFileSync(winPaths.startMenuLnkPath, 'lnk-binary', 'utf8');
    const winUninstall = uninstallDesktopShortcut({ platform: 'win32', env: winEnv, homeDir: winHome });
    assert.ok(winUninstall.removed.includes(winPaths.vbsPath));
    assert.ok(winUninstall.removed.includes(winPaths.iconPath));
    assert.ok(winUninstall.removed.includes(winPaths.startMenuLnkPath));

    // Windows without marker is skipped
    fs.mkdirSync(path.dirname(winPaths.vbsPath), { recursive: true });
    fs.writeFileSync(winPaths.vbsPath, "' custom user vbs", 'utf8');
    const winSkipped = uninstallDesktopShortcut({ platform: 'win32', env: winEnv, homeDir: winHome });
    assert.deepEqual(winSkipped, { removed: [], skipped: [winPaths.vbsPath] });
    assert.equal(fs.existsSync(winPaths.vbsPath), true);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});
