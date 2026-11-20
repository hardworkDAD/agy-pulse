import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { CACHE_PATH } from './_helpers/quota-test-utils.mjs';

const testDataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-test-bootstrap-'));
process.env.AGY_PULSE_DATA_DIR = testDataDir;

try {
  fs.mkdirSync(path.dirname(CACHE_PATH), { recursive: true });
} catch {}

process.on('exit', () => {
  try {
    fs.rmSync(testDataDir, { recursive: true, force: true });
  } catch {}
});

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, '..', '..');
const require = createRequire(import.meta.url);

test('bootstrap installs runtime files and writes statusLine from a source directory', () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-setup-home-'));
  try {
    const result = spawnSync(process.execPath, ['scripts/bootstrap.js'], {
      cwd: projectRoot,
      env: {
        ...process.env,
        HOME: home,
        USERPROFILE: home,
        AGY_PULSE_SETUP_SOURCE_DIR: projectRoot,
        AGY_PULSE_DATA_DIR: '',
        APPDATA: '',
        LOCALAPPDATA: '',
        XDG_DATA_HOME: '',
      },
      encoding: 'utf8',
    });

    assert.equal(result.status, 0, result.stderr || result.stdout);

    const base = path.join(home, '.gemini', 'antigravity-cli');
    const runtime = path.join(base, 'agy-pulse-runtime');
    const settingsPath = path.join(base, 'settings.json');
    const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));

    assert.ok(fs.existsSync(path.join(runtime, 'package.json')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'brand.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'compaction.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'bin', 'agy-pulse.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'cost.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'daily-cost.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'dashboard.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'dashboard-ui.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'ledger.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'speed.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'stats.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'renderer', 'context-monitor.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'runtime', 'statusline-installer.js')));
    assert.ok(fs.existsSync(path.join(runtime, 'scripts', 'diagnose-auth.js')));
    assert.equal(settings.statusLine.type, 'command');
    assert.match(settings.statusLine.command, /agy-pulse-runtime/);
    assert.match(settings.statusLine.command, /runtime[/\\]bin[/\\]agy-pulse\.(?:js|cmd)/);
    assert.match(result.stdout, /agy-pulse bootstrap complete/);
    // Regression: command must point inside the isolated tmp HOME, not the real
    // user's antigravity-cli (configureStatusLine used to ignore homeDir).
    assert.ok(
      settings.statusLine.command.includes(home),
      `statusLine leaked outside tmp HOME: ${settings.statusLine.command}`
    );
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('bootstrap does not require git clone as its runtime source', () => {
  const script = fs.readFileSync(path.join(projectRoot, 'scripts', 'bootstrap.js'), 'utf8');

  assert.match(script, /RUNTIME_FILES/);
  assert.match(script, /AGY_PULSE_SETUP_SOURCE_DIR/);
  assert.doesNotMatch(script, /git clone/);
});

test('bootstrap refreshes quota cache during setup when a token is available', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-setup-quota-home-'));
  const cachePath = CACHE_PATH;
  const previousCache = fs.existsSync(cachePath) ? fs.readFileSync(cachePath, 'utf8') : null;
  const previousFetch = globalThis.fetch;
  try {
    const base = path.join(home, '.gemini', 'antigravity-cli');
    const tokenPath = path.join(base, 'antigravity-oauth-token');
    fs.mkdirSync(base, { recursive: true });
    fs.writeFileSync(tokenPath, JSON.stringify({
      token: {
        access_token: 'setup-token',
        expiry: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
      },
    }));

    globalThis.fetch = async (url) => {
      if (String(url).includes('loadCodeAssist')) {
        return {
          ok: true,
          status: 200,
          json: async () => ({
            paidTier: { id: 'g1-pro-tier', name: 'Google AI Pro' },
            allowedTiers: [{ id: 'standard-tier', name: 'Antigravity', isDefault: true }],
          }),
        };
      }
      return {
        ok: true,
        status: 200,
        json: async () => ({
          models: {
            'gemini-3-flash-agent': {
              displayName: 'Gemini 3.5 Flash (High)',
              quotaInfo: {
                remainingFraction: 0.64,
                resetTime: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
              },
            },
          },
        }),
      };
    };

    const { installRuntime } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    const result = await installRuntime({
      homeDir: home,
      sourceDir: projectRoot,
      platform: 'linux',
      keyringReader: () => null,
      env: {
        ...process.env,
        HOME: home,
        USERPROFILE: home,
        AGY_PULSE_DATA_DIR: '',
        XDG_DATA_HOME: '',
        APPDATA: '',
        LOCALAPPDATA: '',
      },
    });

    assert.deepEqual(result.quotaRefresh, { status: 'refreshed', count: 1, tier: 'Google AI Pro' });

    const quotaModule = require(path.join(result.runtimeDir, 'runtime', 'quota.js'));
    const cached = quotaModule.readCache({
      accessToken: 'rotated-setup-token',
      sourcePath: tokenPath,
    }, { roots: [base] });

    assert.equal(cached.length, 1);
    assert.equal(cached[0].id, 'gemini-3-flash-agent');
    assert.equal(cached[0].remainingFraction, 0.64);
    assert.equal(fs.existsSync(cachePath), false);
  } finally {
    globalThis.fetch = previousFetch;
    if (previousCache === null) fs.rmSync(cachePath, { force: true });
    else fs.writeFileSync(cachePath, previousCache);
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('bootstrap skips quota refresh when the available token is expired', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-setup-expired-home-'));
  const previousFetch = globalThis.fetch;
  try {
    const base = path.join(home, '.gemini', 'antigravity-cli');
    fs.mkdirSync(base, { recursive: true });
    fs.writeFileSync(path.join(base, 'antigravity-oauth-token'), JSON.stringify({
      token: {
        access_token: 'expired-setup-token',
        expiry: '2000-01-01T00:00:00.000Z',
      },
    }));

    let fetchCalls = 0;
    globalThis.fetch = async () => {
      fetchCalls += 1;
      return { ok: false, status: 401, json: async () => ({}) };
    };

    const { installRuntime } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    const result = await installRuntime({
      homeDir: home,
      sourceDir: projectRoot,
      platform: 'linux',
      keyringReader: () => null,
      env: {
        ...process.env,
        HOME: home,
        USERPROFILE: home,
        AGY_PULSE_DATA_DIR: '',
        XDG_DATA_HOME: '',
        APPDATA: '',
        LOCALAPPDATA: '',
      },
    });

    assert.deepEqual(result.quotaRefresh, { status: 'skipped', reason: 'expired_token' });
    assert.equal(fetchCalls, 0);
  } finally {
    globalThis.fetch = previousFetch;
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('bootstrap cleans stale plugin files from current agy config plugin root', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-plugin-root-'));
  try {
    const antigravityRoot = path.join(home, '.gemini', 'antigravity-cli');
    const configPluginDir = path.join(home, '.gemini', 'config', 'plugins', 'agy-pulse');
    fs.mkdirSync(configPluginDir, { recursive: true });
    const staleHook = path.join(configPluginDir, 'hooks.json');
    const staleAgents = path.join(configPluginDir, 'agents');
    fs.writeFileSync(staleHook, '{}');
    fs.mkdirSync(staleAgents);

    const { cleanStalePluginFiles } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    const removed = cleanStalePluginFiles(antigravityRoot, {
      env: {
        ...process.env,
        HOME: home,
        USERPROFILE: home,
      },
      homeDir: home,
    });

    assert.equal(fs.existsSync(staleHook), false);
    assert.equal(fs.existsSync(staleAgents), false);
    assert.deepEqual(
      removed.map(entry => path.relative(home, entry)).sort(),
      [
        path.join('.gemini', 'config', 'plugins', 'agy-pulse', 'agents'),
        path.join('.gemini', 'config', 'plugins', 'agy-pulse', 'hooks.json'),
      ].sort()
    );
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('bootstrap preserves existing runtime when replacement download fails', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-bootstrap-atomic-'));
  const source = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-incomplete-source-'));
  try {
    const antigravityRoot = path.join(home, '.gemini', 'antigravity-cli');
    const runtimeDir = path.join(antigravityRoot, 'agy-pulse-runtime');
    const sentinel = path.join(runtimeDir, 'runtime', 'bin', 'agy-pulse.js');
    fs.mkdirSync(path.dirname(sentinel), { recursive: true });
    fs.writeFileSync(sentinel, 'old working runtime');

    const { installRuntime } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    await assert.rejects(
      () => installRuntime({
        homeDir: home,
        sourceDir: source,
        env: {
          ...process.env,
          HOME: home,
          USERPROFILE: home,
          AGY_PULSE_DATA_DIR: '',
          XDG_DATA_HOME: '',
          APPDATA: '',
          LOCALAPPDATA: '',
        },
      }),
      /ENOENT/
    );

    assert.equal(fs.readFileSync(sentinel, 'utf8'), 'old working runtime');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(source, { recursive: true, force: true });
  }
});

test('bootstrap preserves saved global config across upgrade and supports resetConfig', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-bootstrap-cfg-'));
  try {
    const dataDir = path.join(home, 'custom-data');
    const runtimeConfigPath = path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'agy-pulse.config.json');
    fs.mkdirSync(path.dirname(runtimeConfigPath), { recursive: true });
    fs.writeFileSync(runtimeConfigPath, JSON.stringify({
      enabled: false,
      language: 'zh',
      repository: 'custom/agy-pulse',
      theme: 'Ocean Blue',
      display: { quotaStyle: 'compact', showCost: true },
      thresholds: { warning: 0.55 },
    }, null, 2));

    const { installRuntime } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    await installRuntime({
      homeDir: home,
      sourceDir: projectRoot,
      keyringReader: () => null,
      env: { ...process.env, HOME: home, USERPROFILE: home, AGY_PULSE_DATA_DIR: dataDir },
    });

    const preserved = JSON.parse(fs.readFileSync(runtimeConfigPath, 'utf8'));
    assert.equal(preserved.enabled, false);
    assert.equal(preserved.language, 'zh');
    assert.equal(preserved.repository, 'custom/agy-pulse');
    assert.equal(preserved.theme, 'Ocean Blue');
    assert.equal(preserved.display.quotaStyle, 'compact');
    assert.equal(preserved.display.showCost, true);
    assert.equal(preserved.display.showContextMonitor, true);
    assert.equal(preserved.thresholds.warning, 0.55);
    assert.equal(preserved.thresholds.critical, 0.9);

    await installRuntime({
      homeDir: home,
      sourceDir: projectRoot,
      resetConfig: true,
      keyringReader: () => null,
      env: { ...process.env, HOME: home, USERPROFILE: home, AGY_PULSE_DATA_DIR: dataDir },
    });

    const reset = JSON.parse(fs.readFileSync(runtimeConfigPath, 'utf8'));
    assert.equal(reset.language, 'auto');
    assert.equal(reset.display.showCost, false);
    assert.deepEqual(reset.thresholds, { warning: 0.7, critical: 0.9 });
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('bootstrap honors AGY_PULSE_DATA_DIR and cleans legacy skills/hud-config', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-datadir-stale-'));
  try {
    const customDataDir = path.join(tmp, 'data-root');
    const home = path.join(tmp, 'home');
    const staleSkillDir = path.join(home, '.gemini', 'config', 'plugins', 'agy-pulse', 'skills', 'hud-config');
    fs.mkdirSync(staleSkillDir, { recursive: true });
    fs.writeFileSync(path.join(staleSkillDir, 'SKILL.md'), 'old');

    const { getAntigravityRoots, cleanStalePluginFiles } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    assert.deepEqual(
      getAntigravityRoots({ AGY_PULSE_DATA_DIR: customDataDir }, home),
      [customDataDir]
    );

    const removed = cleanStalePluginFiles(customDataDir, { env: { HOME: home }, homeDir: home });
    assert.equal(fs.existsSync(staleSkillDir), false);
    assert.ok(removed.some(p => p.endsWith(path.join('skills', 'hud-config'))));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('bootstrap cleanupSelfIfRequested removes file only inside os.tmpdir()', () => {
  const tmpFile = path.join(os.tmpdir(), `agy-pulse-self-cleanup-${process.pid}-${Date.now()}.js`);
  const outsideDir = fs.mkdtempSync(path.join(projectRoot, '.tmp-cleanup-check-'));
  const outsideFile = path.join(outsideDir, 'keep.js');
  try {
    fs.writeFileSync(tmpFile, '// tmp');
    fs.writeFileSync(outsideFile, '// outside');
    const { cleanupSelfIfRequested } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));

    assert.equal(cleanupSelfIfRequested(['node', tmpFile, '--cleanup-self'], tmpFile, os.tmpdir()), true);
    assert.equal(fs.existsSync(tmpFile), false);

    assert.equal(cleanupSelfIfRequested(['node', outsideFile, '--cleanup-self'], outsideFile, os.tmpdir()), false);
    assert.equal(fs.existsSync(outsideFile), true);
  } finally {
    fs.rmSync(tmpFile, { force: true });
    fs.rmSync(outsideDir, { recursive: true, force: true });
  }
});

test('bootstrap resets stale agy-pulse-update-status.json on upgrade', async () => {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-bootstrap-update-reset-'));
  try {
    const dataDir = path.join(home, 'data');
    fs.mkdirSync(dataDir, { recursive: true });
    const statusPath = path.join(dataDir, 'agy-pulse-update-status.json');
    fs.writeFileSync(statusPath, JSON.stringify({
      lastCheck: 12345,
      updateAvailable: true,
      latestVersion: '1.0.0',
      localVersion: '0.9.0',
    }));

    const { installRuntime } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    const result = await installRuntime({
      homeDir: home,
      sourceDir: projectRoot,
      keyringReader: () => null,
      env: { ...process.env, HOME: home, USERPROFILE: home, AGY_PULSE_DATA_DIR: dataDir },
    });

    assert.ok(fs.existsSync(path.join(result.runtimeDir, 'scripts', 'diagnose-auth.js')));
    const statusAfter = JSON.parse(fs.readFileSync(statusPath, 'utf8'));
    assert.equal(statusAfter.updateAvailable, false);
    assert.equal(statusAfter.localVersion, '1.0.0');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('deploy-local.sh syncs diagnose-auth.js and cleans legacy agy-hud files, dirty speed-cache, and stale update status', () => {
  if (process.platform === 'win32') return;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-deploy-local-'));
  try {
    const home = path.join(tmp, 'home');
    const dataDir = path.join(tmp, 'data');
    const xdgConfig = path.join(tmp, 'xdg-config');
    fs.mkdirSync(path.join(dataDir, 'bin'), { recursive: true });
    fs.mkdirSync(path.join(dataDir, 'agy-hud-runtime'), { recursive: true });
    fs.mkdirSync(path.join(dataDir, 'speed-cache'), { recursive: true });

    // Legacy agy-hud artifacts (both top-level and inside agy-pulse-runtime/runtime/)
    fs.mkdirSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'bin'), { recursive: true });
    fs.writeFileSync(path.join(dataDir, 'agy-hud.config.json'), '{}');
    fs.writeFileSync(path.join(dataDir, 'bin', 'agy-hud.js'), '// legacy');
    fs.writeFileSync(path.join(dataDir, 'bin', 'agy-hud.cmd'), '@echo off');
    fs.writeFileSync(path.join(dataDir, 'agy-hud-quota-cache.json'), '{}');
    fs.writeFileSync(path.join(dataDir, 'agy-hud-update-status.json'), '{}');
    fs.writeFileSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'agy-hud.config.json'), '{}');
    fs.writeFileSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'bin', 'agy-hud.js'), '// legacy');
    fs.writeFileSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'bin', 'agy-hud.cmd'), '@echo off');

    // Dirty and valid speed-cache entries
    const nullSpeedFile = path.join(dataDir, 'speed-cache', 'null-speed.json');
    const highSpeedFile = path.join(dataDir, 'speed-cache', 'high-speed.json');
    const goodSpeedFile = path.join(dataDir, 'speed-cache', 'good-speed.json');
    fs.writeFileSync(nullSpeedFile, JSON.stringify({ speed: null, apiDurationMs: 100, usageKey: 'k' }));
    fs.writeFileSync(highSpeedFile, JSON.stringify({ speed: 9999, apiDurationMs: 100, usageKey: 'k' }));
    fs.writeFileSync(goodSpeedFile, JSON.stringify({ speed: 82.5, apiDurationMs: 1000, usageKey: 'k' }));

    // Stale update status
    const updateStatusPath = path.join(dataDir, 'agy-pulse-update-status.json');
    fs.writeFileSync(updateStatusPath, JSON.stringify({
      lastCheck: 1000,
      updateAvailable: true,
      latestVersion: '1.0.0',
      localVersion: '0.9.0',
    }));

    const binDir = path.join(tmp, 'bin');
    const res = spawnSync('bash', [path.join(projectRoot, 'scripts', 'deploy-local.sh')], {
      cwd: projectRoot,
      env: {
        ...process.env,
        HOME: home,
        AGY_PULSE_DATA_DIR: dataDir,
        AGY_PULSE_BIN_DIR: binDir,
        XDG_CONFIG_HOME: xdgConfig,
      },
      encoding: 'utf8',
    });

    assert.equal(res.status, 0, res.stderr || res.stdout);
    for (const cmdName of ['agypl', 'agy-pulse']) {
      const shimPath = path.join(binDir, cmdName);
      assert.ok(fs.existsSync(shimPath), `Expected global CLI shim ${shimPath}`);
      const shimContent = fs.readFileSync(shimPath, 'utf8');
      assert.match(shimContent, /# agy-pulse CLI shim/);
      assert.match(shimContent, /agy-pulse-runtime\/runtime\/bin\/agy-pulse\.js/);
      const mode = fs.statSync(shimPath).mode & 0o777;
      assert.equal(mode, 0o755);
    }
    assert.ok(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'scripts', 'diagnose-auth.js')));
    assert.ok(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'ledger.js')));
    assert.ok(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'stats.js')));
    assert.ok(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'dashboard.js')));
    assert.ok(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'dashboard-ui.js')));
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-hud.config.json')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'bin', 'agy-hud.js')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'bin', 'agy-hud.cmd')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-hud-quota-cache.json')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-hud-update-status.json')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-hud-runtime')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'agy-hud.config.json')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'bin', 'agy-hud.js')), false);
    assert.equal(fs.existsSync(path.join(dataDir, 'agy-pulse-runtime', 'runtime', 'bin', 'agy-hud.cmd')), false);
    assert.equal(fs.existsSync(nullSpeedFile), false);
    assert.equal(fs.existsSync(highSpeedFile), false);
    assert.equal(fs.existsSync(goodSpeedFile), true);
    const statusAfter = JSON.parse(fs.readFileSync(updateStatusPath, 'utf8'));
    assert.equal(statusAfter.updateAvailable, false);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('bootstrap RUNTIME_FILES registers ledger, stats, dashboard, and dashboard-ui', () => {
  const { RUNTIME_FILES } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
  for (const rel of [
    'runtime/ledger.js',
    'runtime/stats.js',
    'runtime/dashboard.js',
    'runtime/dashboard-ui.js',
  ]) {
    assert.ok(RUNTIME_FILES.includes(rel), `RUNTIME_FILES missing ${rel}`);
  }
});

test('bootstrap installCliShim registers agypl and agy-pulse shims under AGY_PULSE_BIN_DIR on Unix and Windows', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-cli-shim-'));
  try {
    const { installCliShim } = require(path.join(projectRoot, 'scripts', 'bootstrap.js'));
    const unixBin = path.join(tmp, 'unix-bin');
    const runtimeDir = path.join(tmp, 'data', 'agy-pulse-runtime');

    const unixRes = installCliShim(runtimeDir, {
      platform: 'linux',
      env: { AGY_PULSE_BIN_DIR: unixBin },
      nodeExecPath: '/usr/bin/node',
    });
    assert.equal(unixRes.shimPath, path.join(unixBin, 'agypl'));
    assert.deepEqual(unixRes.shimPaths, [path.join(unixBin, 'agypl'), path.join(unixBin, 'agy-pulse')]);
    for (const p of unixRes.shimPaths) {
      const content = fs.readFileSync(p, 'utf8');
      assert.match(content, /^#!\/bin\/sh/);
      assert.match(content, /# agy-pulse CLI shim/);
      assert.match(content, /exec "\/usr\/bin\/node" ".*agy-pulse-runtime\/runtime\/bin\/agy-pulse\.js" "\$@"/);
      if (process.platform !== 'win32') {
        assert.equal(fs.statSync(p).mode & 0o777, 0o755);
      }
    }

    const winBin = path.join(tmp, 'win-bin');
    const winRes = installCliShim(runtimeDir, {
      platform: 'win32',
      env: { AGY_PULSE_BIN_DIR: winBin },
      nodeExecPath: 'C:\\Program Files\\nodejs\\node.exe',
    });
    assert.deepEqual(winRes.shimPaths, [path.join(winBin, 'agypl.cmd'), path.join(winBin, 'agy-pulse.cmd')]);
    for (const p of winRes.shimPaths) {
      const content = fs.readFileSync(p, 'utf8');
      assert.match(content, /@echo off\r\nrem agy-pulse CLI shim\r\n/);
      assert.match(content, /agy-pulse-runtime/);
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
