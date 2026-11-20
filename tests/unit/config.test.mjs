import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  FULL_DEFAULT_CONFIG,
  mergeConfig,
  loadConfig,
  getLocalConfigPath,
  getGlobalConfigPath,
  saveConfig,
} from '../../runtime/config.js';

function withCwd(cwd, fn) {
  const previous = process.cwd();
  process.chdir(cwd);
  try {
    return fn();
  } finally {
    process.chdir(previous);
  }
}

test('loadConfig reads project-local config and deep-merges defaults', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-local-'));
  try {
    fs.writeFileSync(path.join(tmp, 'agy-pulse.config.json'), JSON.stringify({
      display: { columnWidth: 45 },
      enabled: false,
      customField: 'kept',
    }));

    const config = await loadConfig(tmp);

    assert.equal(config.enabled, false);
    assert.equal(config.customField, 'kept');
    assert.equal(config.display.columnWidth, 45);
    assert.equal(config.display.showContextMonitor, true);
    assert.equal(config.display.showModelCache, true);
    assert.equal(config.thresholds.warning, 0.7);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('loadConfig falls back to bundled plugin config when local config is absent', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-plugin-'));
  try {
    const config = await withCwd(tmp, () => loadConfig());

    assert.equal(config.enabled, true);
    assert.equal(config.display.showContextMonitor, FULL_DEFAULT_CONFIG.display.showContextMonitor);
    assert.equal(config.display.showSpeed, true);
    assert.equal(config.display.showModel, false);
    assert.equal(config.thresholds.warning, 0.7);
    assert.equal(config.thresholds.critical, 0.9);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('bundled plugin config leaves display.unicode undefined for auto detection', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-unicode-'));
  try {
    const config = await withCwd(tmp, () => loadConfig());
    assert.equal(config.display.unicode, undefined);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('saveConfig writes config to local path', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-save-'));
  try {
    await withCwd(tmp, async () => {
      const localPath = getLocalConfigPath();
      assert.equal(fs.realpathSync(path.dirname(localPath)), fs.realpathSync(tmp));

      const newConfig = { theme: 'yellow', display: { useNerdFonts: true } };
      await saveConfig(newConfig, false);

      const read = JSON.parse(fs.readFileSync(localPath, 'utf8'));
      assert.deepEqual(read, newConfig);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('saveConfig writes config to global path', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-global-'));
  const prevGlobalDir = process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
  process.env.AGY_PULSE_GLOBAL_CONFIG_DIR = tmp;
  try {
    const globalPath = getGlobalConfigPath();
    assert.equal(path.dirname(globalPath), tmp);

    const newConfig = { display: { quotaStyle: 'compact' } };
    await saveConfig(newConfig, true);

    const read = JSON.parse(fs.readFileSync(globalPath, 'utf8'));
    assert.deepEqual(read, newConfig);
  } finally {
    if (prevGlobalDir === undefined) delete process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
    else process.env.AGY_PULSE_GLOBAL_CONFIG_DIR = prevGlobalDir;
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('saveConfig throws on write failure', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-err-'));
  try {
    await withCwd(tmp, async () => {
      const localPath = getLocalConfigPath();
      fs.mkdirSync(localPath);
      await assert.rejects(() => saveConfig({ enabled: true }, false));
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('saveConfig restricts file permissions to owner read/write (0600)', async () => {
  if (process.platform === 'win32') return;
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-perm-'));
  try {
    await withCwd(tmp, async () => {
      const localPath = getLocalConfigPath();
      await saveConfig({ enabled: true }, false);
      const mode = fs.statSync(localPath).mode & 0o777;
      assert.equal(mode & 0o177, 0);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('mergeConfig shallow-merges icons and display.cacheHitThresholds without overwriting sibling keys', () => {
  const merged = mergeConfig(
    {
      icons: { branch: 'B ', model: 'M ' },
      display: {
        showSpeed: true,
        cacheHitThresholds: { high: 80, mid: 50 },
      },
    },
    {
      icons: { model: 'CustomM ' },
      display: {
        cacheHitThresholds: { high: 90 },
      },
    }
  );

  assert.deepEqual(merged.icons, { branch: 'B ', model: 'CustomM ' });
  assert.deepEqual(merged.display.cacheHitThresholds, { high: 90, mid: 50 });
  assert.equal(merged.display.showSpeed, true);
});

test('showCycleMode defaults to true in FULL_DEFAULT_CONFIG and bundled runtime config', async () => {
  assert.equal(FULL_DEFAULT_CONFIG.display.showCycleMode, true);
  const bundledPath = fileURLToPath(new URL('../../runtime/agy-pulse.config.json', import.meta.url));
  const bundledJson = JSON.parse(fs.readFileSync(bundledPath, 'utf8'));
  assert.equal(bundledJson.display.showCycleMode, true);

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-config-cycle-mode-'));
  const prevGlobalDir = process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
  const isolatedGlobalDir = path.join(tmp, 'global');
  const isolatedCwd = path.join(tmp, 'cwd');
  fs.mkdirSync(isolatedGlobalDir, { recursive: true });
  fs.mkdirSync(isolatedCwd, { recursive: true });
  process.env.AGY_PULSE_GLOBAL_CONFIG_DIR = isolatedGlobalDir;
  try {
    const config = await loadConfig(isolatedCwd);
    assert.equal(config.display.showCycleMode, true);
  } finally {
    if (prevGlobalDir === undefined) delete process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
    else process.env.AGY_PULSE_GLOBAL_CONFIG_DIR = prevGlobalDir;
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
