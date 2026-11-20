import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {
  getAntigravityRoots,
  getAntigravityIdeRoots,
  resolveAntigravityPath,
  resolveSafeExecutable,
} from '../../runtime/paths.js';

function withEnv(overrides, fn) {
  const snapshot = {};
  for (const key of Object.keys(overrides)) {
    snapshot[key] = process.env[key];
    if (overrides[key] === undefined) delete process.env[key];
    else process.env[key] = overrides[key];
  }
  try {
    return fn();
  } finally {
    for (const key of Object.keys(snapshot)) {
      if (snapshot[key] === undefined) delete process.env[key];
      else process.env[key] = snapshot[key];
    }
  }
}

test('getAntigravityRoots starts with home/.gemini/antigravity-cli', () => {
  withEnv({ XDG_DATA_HOME: undefined, APPDATA: undefined, LOCALAPPDATA: undefined }, () => {
    const roots = getAntigravityRoots();
    assert.equal(roots[0], path.join(os.homedir(), '.gemini', 'antigravity-cli'));
  });
});

test('getAntigravityRoots includes XDG/APPDATA/LOCALAPPDATA when set', () => {
  withEnv({
    XDG_DATA_HOME: '/xdg/home',
    APPDATA: 'C:\\AppData\\Roaming',
    LOCALAPPDATA: 'C:\\AppData\\Local',
  }, () => {
    const roots = getAntigravityRoots();
    assert.ok(roots.includes(path.join('/xdg/home', 'antigravity-cli')));
    assert.ok(roots.includes(path.join('C:\\AppData\\Roaming', 'antigravity-cli')));
    assert.ok(roots.includes(path.join('C:\\AppData\\Local', 'antigravity-cli')));
  });
});

test('getAntigravityRoots filters out unset env candidates', () => {
  withEnv({ XDG_DATA_HOME: undefined, APPDATA: undefined, LOCALAPPDATA: undefined }, () => {
    const roots = getAntigravityRoots();
    assert.equal(roots.length, 1);
  });
});

test('resolveAntigravityPath returns first existing candidate', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'paths-test-'));
  try {
    const fakeAppdata = path.join(tmp, 'Roaming');
    const fakeRoot = path.join(fakeAppdata, 'antigravity-cli');
    fs.mkdirSync(fakeRoot, { recursive: true });
    fs.writeFileSync(path.join(fakeRoot, 'settings.json'), '{}');

    withEnv({
      XDG_DATA_HOME: undefined,
      APPDATA: fakeAppdata,
      LOCALAPPDATA: undefined,
      HOME: path.join(tmp, 'nonexistent-home'),
      USERPROFILE: path.join(tmp, 'nonexistent-home'),
    }, () => {
      const resolved = resolveAntigravityPath('settings.json');
      assert.equal(resolved, path.join(fakeRoot, 'settings.json'));
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('resolveAntigravityPath falls back to first candidate when none exist', () => {
  withEnv({
    XDG_DATA_HOME: undefined,
    APPDATA: undefined,
    LOCALAPPDATA: undefined,
  }, () => {
    const resolved = resolveAntigravityPath('does-not-exist.json');
    assert.equal(
      resolved,
      path.join(os.homedir(), '.gemini', 'antigravity-cli', 'does-not-exist.json')
    );
  });
});

test('resolveAntigravityPath joins multi-segment relative paths', () => {
  withEnv({
    XDG_DATA_HOME: undefined,
    APPDATA: undefined,
    LOCALAPPDATA: undefined,
  }, () => {
    const resolved = resolveAntigravityPath(path.join('brain', 'abc', 'log.jsonl'));
    assert.equal(
      resolved,
      path.join(os.homedir(), '.gemini', 'antigravity-cli', 'brain', 'abc', 'log.jsonl')
    );
  });
});

test('resolveSafeExecutable resolves absolute path and skips relative directories', () => {
  const nodePath = resolveSafeExecutable('node');
  assert.ok(nodePath);
  assert.ok(path.isAbsolute(nodePath));

  withEnv({
    PATH: ['.', 'relative_dir', path.dirname(nodePath)].join(path.delimiter)
  }, () => {
    const resolved = resolveSafeExecutable('node');
    assert.equal(resolved, nodePath);
  });
});

test('getAntigravityRoots honors AGY_PULSE_DATA_DIR and injected env/homeDir', () => {
  const rootsPulse = getAntigravityRoots({
    AGY_PULSE_DATA_DIR: '/pulse/data',
  }, '/custom/home');
  assert.deepEqual(rootsPulse, ['/pulse/data']);

  const injectedRoots = getAntigravityRoots({
    XDG_DATA_HOME: '/custom/xdg',
  }, '/injected/home');
  assert.deepEqual(injectedRoots, [
    path.join('/injected/home', '.gemini', 'antigravity-cli'),
    path.join('/custom/xdg', 'antigravity-cli'),
  ]);
});

test('resolveAntigravityPath and resolveSafeExecutable respect injected env/homeDir', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'paths-inject-test-'));
  try {
    const customHome = path.join(tmp, 'home');
    const customRoot = path.join(customHome, '.gemini', 'antigravity-cli');
    fs.mkdirSync(customRoot, { recursive: true });
    fs.writeFileSync(path.join(customRoot, 'settings.json'), '{}');

    const resolved = resolveAntigravityPath('settings.json', {}, customHome);
    assert.equal(resolved, path.join(customRoot, 'settings.json'));

    const binDir = path.join(tmp, 'bin');
    fs.mkdirSync(binDir, { recursive: true });
    const exeName = process.platform === 'win32' ? 'custom-tool.exe' : 'custom-tool';
    const exePath = path.join(binDir, exeName);
    fs.writeFileSync(exePath, '');
    assert.equal(resolveSafeExecutable('custom-tool', { PATH: binDir }), exePath);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('getAntigravityIdeRoots resolves default, XDG/APPDATA/LOCALAPPDATA, and deduplicates resolved paths', () => {
  const customHome = path.join(os.tmpdir(), 'ide-home-test');
  const defaultRoots = getAntigravityIdeRoots({
    AGY_PULSE_DATA_DIR: undefined,
    AGY_PULSE_IDE_DATA_DIR: undefined,
    XDG_DATA_HOME: undefined,
    APPDATA: undefined,
    LOCALAPPDATA: undefined,
  }, customHome);
  assert.deepEqual(defaultRoots, [path.join(customHome, '.gemini', 'antigravity')]);

  const sharedWinDir = 'C:\\Users\\dev\\AppData\\Shared';
  const multiRoots = getAntigravityIdeRoots({
    XDG_DATA_HOME: '/xdg/ide',
    APPDATA: sharedWinDir,
    LOCALAPPDATA: sharedWinDir,
  }, customHome);
  assert.deepEqual(multiRoots, [
    path.join(customHome, '.gemini', 'antigravity'),
    path.join('/xdg/ide', 'antigravity'),
    path.join(sharedWinDir, 'antigravity'),
  ]);

  const dupXdgRoots = getAntigravityIdeRoots({
    XDG_DATA_HOME: path.join(customHome, '.gemini') + path.sep,
  }, customHome);
  assert.deepEqual(dupXdgRoots, [path.join(customHome, '.gemini', 'antigravity')]);
});

test('getAntigravityIdeRoots honors AGY_PULSE_IDE_DATA_DIR and isolates on AGY_PULSE_DATA_DIR without changing getAntigravityRoots', () => {
  const customHome = '/custom/ide-home';
  const cliOnlyEnv = {
    AGY_PULSE_DATA_DIR: '/pulse/cli-only',
    XDG_DATA_HOME: '/xdg/should-ignore',
  };
  assert.deepEqual(getAntigravityIdeRoots(cliOnlyEnv, customHome), []);
  assert.deepEqual(getAntigravityRoots(cliOnlyEnv, customHome), ['/pulse/cli-only']);

  const overrideEnv = {
    AGY_PULSE_DATA_DIR: '/pulse/cli-only',
    AGY_PULSE_IDE_DATA_DIR: '/pulse/ide-override',
  };
  assert.deepEqual(getAntigravityIdeRoots(overrideEnv, customHome), ['/pulse/ide-override']);
  assert.deepEqual(getAntigravityRoots(overrideEnv, customHome), ['/pulse/cli-only']);
});
