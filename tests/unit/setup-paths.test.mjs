import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import statuslineModule from '../../runtime/statusline-installer.js';

const {
  createStatusLineCommand,
  buildCmdShimContents,
  buildShShimContents,
  writeCmdShim,
  ensureWindowsShShim,
  getWindowsAgyBinDirs,
} = statuslineModule;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, '..', '..');

test('bootstrap.sh invokes bootstrap.js via curl + node', () => {
  const bootstrap = fs.readFileSync(path.join(projectRoot, 'scripts', 'bootstrap.sh'), 'utf8');

  assert.match(bootstrap, /bootstrap\.js/);
  assert.match(bootstrap, /curl/);
  assert.match(bootstrap, /exec node/);
});

test('bootstrap.js downloads runtime/* files and invokes configureStatusLine', () => {
  const bootstrap = fs.readFileSync(path.join(projectRoot, 'scripts', 'bootstrap.js'), 'utf8');
  assert.match(bootstrap, /runtime\/bin\/agy-pulse\.js/);
  assert.match(bootstrap, /runtime\/bin\/agy-hud\.js/);
  assert.match(bootstrap, /statusline-installer\.js/);
  assert.match(bootstrap, /configureStatusLine/);
});

test('Windows statusLine command points at the .cmd shim, not raw node invocation', () => {
  const command = createStatusLineCommand(
    'C:\\Users\\testuser\\.gemini\\antigravity-cli\\agy-hud-runtime\\runtime\\bin\\agy-hud.js',
    'C:\\Program Files\\nodejs\\node.exe',
    'win32'
  );

  assert.equal(
    command,
    'C:\\Users\\testuser\\.gemini\\antigravity-cli\\agy-hud-runtime\\runtime\\bin\\agy-hud.cmd'
  );
  // The shim path replaces .js with .cmd — no inline node invocation or outer quotes.
  assert.doesNotMatch(command, /^node /);
  assert.doesNotMatch(command, /\.js"$/);
  assert.doesNotMatch(command, /^"/);
});

test('Windows statusLine command returns unquoted shim path even with spaces', () => {
  const command = createStatusLineCommand(
    'C:\\Users\\John Doe\\.gemini\\antigravity-cli\\agy-hud-runtime\\runtime\\bin\\agy-hud.js',
    'C:\\Program Files\\nodejs\\node.exe',
    'win32'
  );

  assert.equal(
    command,
    'C:\\Users\\John Doe\\.gemini\\antigravity-cli\\agy-hud-runtime\\runtime\\bin\\agy-hud.cmd'
  );
});

test('Unix statusLine command uses absolute process.execPath', () => {
  const command = createStatusLineCommand(
    '/Users/me/.gemini/antigravity-cli/agy-hud-runtime/runtime/bin/agy-hud.js',
    '/usr/local/bin/node',
    'darwin'
  );
  assert.equal(command, '"/usr/local/bin/node" "/Users/me/.gemini/antigravity-cli/agy-hud-runtime/runtime/bin/agy-hud.js"');
});

test('buildCmdShimContents includes node-on-PATH first then Program Files fallback', () => {
  const body = buildCmdShimContents('C:\\runtime\\bin\\agy-hud.js');
  assert.match(body, /^@echo off/);
  assert.match(body, /node "%~dp0agy-hud\.js"/);
  assert.match(body, /%ProgramFiles%\\nodejs\\node\.exe/);
  // CRLF line endings for Windows .cmd files
  assert.match(body, /\r\n/);
});

test('buildShShimContents forwards agy sh -c calls to cmd.exe without broken quote manipulation', () => {
  const body = buildShShimContents();

  assert.match(body, /^@echo off/);
  assert.match(body, /"%~1"=="-c"/);
  assert.match(body, /set "CMDLINE=%~2"/);
  assert.doesNotMatch(body, /%CMDLINE:\\"="%/);
  assert.match(body, /cmd\.exe \/d \/c "%CMDLINE%"/);
  assert.match(body, /\r\n/);
});

test('getWindowsAgyBinDirs only returns LOCALAPPDATA agy bin and PATH dirs with agy.exe', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-bin-dirs-'));
  try {
    const localAppData = path.join(tmp, 'local');
    const appData = path.join(tmp, 'roaming');
    const userProfile = path.join(tmp, 'home');
    const pathAgyBin = path.join(tmp, 'path-bin');
    const pathOther = path.join(tmp, 'path-other');
    fs.mkdirSync(path.join(localAppData, 'agy', 'bin'), { recursive: true });
    fs.mkdirSync(pathAgyBin, { recursive: true });
    fs.mkdirSync(pathOther, { recursive: true });
    fs.writeFileSync(path.join(pathAgyBin, 'agy.exe'), '');

    const dirs = getWindowsAgyBinDirs({
      LOCALAPPDATA: localAppData,
      APPDATA: appData,
      USERPROFILE: userProfile,
      PATH: [pathAgyBin, pathOther].join(path.delimiter),
    });

    // Must include LOCALAPPDATA\agy\bin and the PATH dir that contains agy.exe.
    assert.deepEqual(dirs, [
      path.resolve(localAppData, 'agy', 'bin'),
      path.resolve(pathAgyBin),
    ]);
    // Must NOT include risky shared locations.
    assert.ok(!dirs.includes(path.resolve(localAppData, 'Microsoft', 'WindowsApps')));
    assert.ok(!dirs.includes(path.resolve(appData, 'npm')));
    assert.ok(!dirs.includes(path.resolve(userProfile, 'App')));
    // PATH dirs without agy.exe are skipped.
    assert.ok(!dirs.includes(path.resolve(pathOther)));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('ensureWindowsShShim writes sh.cmd into discovered agy bin dirs', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-sh-shim-'));
  try {
    const localAppData = path.join(tmp, 'local');
    const agyBin = path.join(localAppData, 'agy', 'bin');
    fs.mkdirSync(agyBin, { recursive: true });

    const written = ensureWindowsShShim('win32', {
      LOCALAPPDATA: localAppData,
      PATH: '',
    });

    assert.ok(written.includes(path.join(agyBin, 'sh.cmd')));
    assert.ok(written.includes(path.join(agyBin, 'sh.bat')));
    const body = fs.readFileSync(path.join(agyBin, 'sh.cmd'), 'utf8');
    assert.doesNotMatch(body, /%CMDLINE:\\"="%/);
    assert.match(body, /cmd\.exe \/d \/c "%CMDLINE%"/);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('ensureWindowsShShim does not shadow a real sh.exe in the same dir', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-sh-real-'));
  try {
    const localAppData = path.join(tmp, 'local');
    const agyBin = path.join(localAppData, 'agy', 'bin');
    fs.mkdirSync(agyBin, { recursive: true });
    fs.writeFileSync(path.join(agyBin, 'sh.exe'), 'real-sh-binary');

    const written = ensureWindowsShShim('win32', {
      LOCALAPPDATA: localAppData,
      PATH: '',
    });

    assert.deepEqual(written, []);
    assert.ok(!fs.existsSync(path.join(agyBin, 'sh.cmd')));
    assert.ok(!fs.existsSync(path.join(agyBin, 'sh.bat')));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('ensureWindowsShShim does not overwrite a third-party sh.cmd', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-sh-tp-'));
  try {
    const localAppData = path.join(tmp, 'local');
    const agyBin = path.join(localAppData, 'agy', 'bin');
    fs.mkdirSync(agyBin, { recursive: true });
    const thirdParty = '@echo off\r\necho some other sh shim\r\n';
    fs.writeFileSync(path.join(agyBin, 'sh.cmd'), thirdParty);

    const written = ensureWindowsShShim('win32', {
      LOCALAPPDATA: localAppData,
      PATH: '',
    });

    assert.ok(!written.includes(path.join(agyBin, 'sh.cmd')));
    assert.equal(fs.readFileSync(path.join(agyBin, 'sh.cmd'), 'utf8'), thirdParty);
    // sh.bat was missing, so it is safe to create.
    assert.ok(written.includes(path.join(agyBin, 'sh.bat')));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('ensureWindowsShShim skips writes when on-disk content already matches', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-sh-idem-'));
  try {
    const localAppData = path.join(tmp, 'local');
    const agyBin = path.join(localAppData, 'agy', 'bin');
    fs.mkdirSync(agyBin, { recursive: true });

    const first = ensureWindowsShShim('win32', { LOCALAPPDATA: localAppData, PATH: '' });
    assert.ok(first.length > 0);

    const second = ensureWindowsShShim('win32', { LOCALAPPDATA: localAppData, PATH: '' });
    assert.deepEqual(second, []);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('writeCmdShim is a no-op on non-Windows', () => {
  const result = writeCmdShim('/tmp/whatever/agy-pulse.js', 'linux');
  assert.equal(result, null);
});

test('configureStatusLine preserves enabled and stack_with_default and prefers agy-pulse.js', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-installer-'));
  try {
    const baseDir = path.join(tmp, 'runtime');
    fs.mkdirSync(path.join(baseDir, 'bin'), { recursive: true });
    fs.writeFileSync(path.join(baseDir, 'bin', 'agy-pulse.js'), '// entry');
    const settingsPath = path.join(tmp, 'settings.json');
    fs.writeFileSync(settingsPath, JSON.stringify({
      statusLine: {
        type: 'command',
        command: '/old/statusline-combined.sh',
        enabled: true,
        stack_with_default: true,
      },
    }));

    const res = statuslineModule.configureStatusLine(baseDir, { settingsPath });
    const updated = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));

    assert.match(res.command, /agy-pulse\.(?:js|cmd)/);
    assert.equal(updated.statusLine.enabled, true);
    assert.equal(updated.statusLine.stack_with_default, true);
    assert.equal(updated.statusLine.command, res.command);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('configureStatusLine overwrites legacy statusline-combined.sh with single-process exec shim', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-combined-shim-'));
  try {
    const baseDir = path.join(tmp, 'agy-pulse-runtime', 'runtime');
    fs.mkdirSync(path.join(baseDir, 'bin'), { recursive: true });
    const entryPath = path.join(baseDir, 'bin', 'agy-pulse.js');
    fs.writeFileSync(entryPath, '// entry');
    const settingsPath = path.join(tmp, 'settings.json');
    const combinedPath = path.join(tmp, 'statusline-combined.sh');
    fs.writeFileSync(
      combinedPath,
      '#!/usr/bin/env bash\n"$PY_BIN" "$AGY_DIR/agy-statusline/statusline.sh"\n',
      { mode: 0o644 }
    );

    statuslineModule.configureStatusLine(baseDir, { settingsPath });

    const shim = fs.readFileSync(combinedPath, 'utf8');
    assert.match(shim, /^#!\/usr\/bin\/env bash\nexec /);
    assert.ok(shim.includes(entryPath));
    assert.doesNotMatch(shim, /agy-statusline\/statusline\.sh/);
    if (process.platform !== 'win32') {
      const mode = fs.statSync(combinedPath).mode & 0o777;
      assert.equal(mode & 0o111, 0o111);
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('deploy-local.sh replaces legacy statusline-combined.sh, upgrades secondary settings, and emits single context line', { skip: process.platform === 'win32' }, () => {
  const tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-deploy-home-'));
  try {
    const agyDir = path.join(tmpHome, '.gemini', 'antigravity-cli');
    const configDir = path.join(tmpHome, '.gemini', 'config');
    fs.mkdirSync(agyDir, { recursive: true });
    fs.mkdirSync(configDir, { recursive: true });

    const combinedPath = path.join(agyDir, 'statusline-combined.sh');
    fs.writeFileSync(
      combinedPath,
      '#!/usr/bin/env bash\n"$NODE_BIN" "$AGY_DIR/agy-hud-runtime/runtime/bin/agy-hud.js"\n"$PY_BIN" "$AGY_DIR/agy-statusline/statusline.sh"\n',
      { mode: 0o755 }
    );

    const secondaryWithStatusLine = path.join(configDir, 'settings.json');
    fs.writeFileSync(secondaryWithStatusLine, JSON.stringify({
      theme: 'dark',
      statusLine: {
        type: 'command',
        command: `"/bin/bash" "${combinedPath}"`,
        enabled: true,
      },
    }));

    const secondaryWithoutStatusLine = path.join(tmpHome, '.gemini', 'settings.json');
    fs.writeFileSync(secondaryWithoutStatusLine, JSON.stringify({ other: 123 }));

    const cleanEnv = {
      ...process.env,
      HOME: tmpHome,
      USERPROFILE: tmpHome,
      XDG_CONFIG_HOME: '',
      AGY_PULSE_DATA_DIR: '',
      AGY_HUD_DATA_DIR: '',
    };

    const deployRes = spawnSync('/bin/bash', [path.join(projectRoot, 'scripts', 'deploy-local.sh')], {
      cwd: projectRoot,
      env: cleanEnv,
      encoding: 'utf8',
    });
    assert.equal(deployRes.status, 0, deployRes.stderr || deployRes.stdout);

    const shim = fs.readFileSync(combinedPath, 'utf8');
    assert.match(shim, /^#!\/usr\/bin\/env bash\nexec /);
    assert.match(shim, /agy-pulse-runtime\/runtime\/bin\/agy-pulse\.js/);
    assert.doesNotMatch(shim, /agy-statusline\/statusline\.sh/);
    assert.equal(fs.statSync(combinedPath).mode & 0o111, 0o111);

    const updatedSecondary = JSON.parse(fs.readFileSync(secondaryWithStatusLine, 'utf8'));
    assert.equal(updatedSecondary.theme, 'dark');
    assert.equal(updatedSecondary.statusLine.enabled, true);
    assert.match(updatedSecondary.statusLine.command, /agy-pulse-runtime\/runtime\/bin\/agy-pulse\.js/);

    const untouchedSecondary = JSON.parse(fs.readFileSync(secondaryWithoutStatusLine, 'utf8'));
    assert.equal(untouchedSecondary.statusLine, undefined);

    const payload = JSON.stringify({
      cwd: projectRoot,
      model: { display_name: 'Gemini 4 Carbon', effort: 'High' },
      context_window: {
        context_window_size: 512000,
        total_output_tokens: 1200,
        current_usage: {
          input_tokens: 10000,
          cache_creation_input_tokens: 0,
          cache_read_input_tokens: 9000,
          output_tokens: 1200,
        },
      },
      cost: { total_cost_usd: 0.12 },
    });

    const runRes = spawnSync('/bin/bash', [combinedPath], {
      cwd: projectRoot,
      env: {
        ...cleanEnv,
        AGY_STATUSLINE_NODE: process.execPath,
      },
      input: payload,
      encoding: 'utf8',
    });
    assert.equal(runRes.status, 0, runRes.stderr || runRes.stdout);

    const plain = runRes.stdout.replace(/\x1b\[[0-9;]*m/g, '');
    const monitorLines = plain
      .split(/\r?\n/)
      .filter(line => /^\s*[◈*] .*?(?:上下文|Ctx)(?=\s|$)/.test(line));
    assert.equal(monitorLines.length, 1, `Expected 1 context monitor line, got:\n${plain}`);
    const contextMatches = plain.match(/(?:上下文|Ctx)(?=\s|$)/g) || [];
    assert.equal(contextMatches.length, 1, `Expected 1 context label occurrence, got:\n${plain}`);
  } finally {
    fs.rmSync(tmpHome, { recursive: true, force: true });
  }
});
