import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { renderStatusLine, formatCycleModeSegment } = require('../../runtime/renderer.js');
const { getAnsiDisplayWidth, stripAnsi } = require('../../runtime/renderer/format.js');

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, '..', '..');

function runCliWithStdin(stdinPayload, envOverrides, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js')], {
      cwd,
      env: {
        ...process.env,
        ...envOverrides,
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
    child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
    child.on('error', reject);
    child.on('close', code => resolve({ code, stdout, stderr }));
    child.stdin.end(typeof stdinPayload === 'string' ? stdinPayload : JSON.stringify(stdinPayload));
  });
}

describe('renderer / cycle_mode badge', () => {
  test('formatCycleModeSegment renders all 4 known modes with fixed bold ANSI colors and trims/lowercases', () => {
    assert.equal(formatCycleModeSegment('auto'), '\x1b[1m\x1b[33mauto\x1b[0m');
    assert.equal(formatCycleModeSegment('accept-edits'), '\x1b[1m\x1b[32maccept-edits\x1b[0m');
    assert.equal(formatCycleModeSegment('plan'), '\x1b[1m\x1b[94mplan\x1b[0m');
    assert.equal(formatCycleModeSegment('bestn'), '\x1b[1m\x1b[35mbest-of-n\x1b[0m');

    assert.equal(formatCycleModeSegment('  AUTO  '), '\x1b[1m\x1b[33mauto\x1b[0m');
    assert.equal(formatCycleModeSegment(' Accept-Edits '), '\x1b[1m\x1b[32maccept-edits\x1b[0m');
    assert.equal(formatCycleModeSegment('\tPLAN\n'), '\x1b[1m\x1b[94mplan\x1b[0m');
    assert.equal(formatCycleModeSegment('  BestN  '), '\x1b[1m\x1b[35mbest-of-n\x1b[0m');
  });

  test('renderStatusLine places known cycle_mode badge right after plan tier on Line 1', () => {
    const cases = [
      ['auto', '\x1b[1m\x1b[33mauto\x1b[0m'],
      ['accept-edits', '\x1b[1m\x1b[32maccept-edits\x1b[0m'],
      ['plan', '\x1b[1m\x1b[94mplan\x1b[0m'],
      ['bestn', '\x1b[1m\x1b[35mbest-of-n\x1b[0m'],
    ];
    for (const [mode, expectedAnsi] of cases) {
      const output = renderStatusLine(
        { steps: 1, currentDir: 'proj' },
        { plan_tier: 'Pro', cycle_mode: mode },
        { display: { unicode: false, showModel: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
      );
      const line1 = output.split('\n')[0];
      assert.ok(line1.includes(expectedAnsi), `expected Line 1 to contain ${expectedAnsi} for ${mode}`);
      assert.equal(stripAnsi(line1), `proj | Pro | ${mode === 'bestn' ? 'best-of-n' : mode}`);
    }
  });

  test('empty, whitespace, non-string, and null agyData omit badge without extra dividers', () => {
    const emptyInputs = ['', '   ', '\t\n', undefined, null, 123, {}, ['plan']];
    for (const bad of emptyInputs) {
      assert.equal(formatCycleModeSegment(bad), '');
      const output = renderStatusLine(
        { steps: 1, currentDir: 'proj' },
        { plan_tier: 'Pro', cycle_mode: bad },
        { display: { unicode: false, showModel: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
      );
      const line1 = output.split('\n')[0];
      assert.equal(stripAnsi(line1), 'proj | Pro');
    }

    const nullAgyOutput = renderStatusLine(
      { steps: 1, currentDir: 'proj' },
      null,
      { display: { unicode: false, showModel: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
    );
    assert.equal(stripAnsi(nullAgyOutput.split('\n')[0]), 'proj | Free');
  });

  test('unknown non-empty modes render cyan and sanitize terminal escape sequences', () => {
    assert.equal(formatCycleModeSegment('review'), '\x1b[1m\x1b[36mreview\x1b[0m');
    assert.equal(formatCycleModeSegment('\x1b[31mplan\x1b[0m'), '\x1b[1m\x1b[36mplan\x1b[0m');
    assert.equal(formatCycleModeSegment('\x1b]0;title\x07custom'), '\x1b[1m\x1b[36mcustom\x1b[0m');
    assert.equal(formatCycleModeSegment('\x1b[31m   \x1b[0m'), '');

    const longMode = 'abcdefghijklmnopqrstuvwxyz012345';
    assert.equal(formatCycleModeSegment(longMode), '\x1b[1m\x1b[36mabcdefghijklmnopqrstuvwx\x1b[0m');

    const rendered = renderStatusLine(
      { steps: 1, currentDir: 'proj' },
      { plan_tier: 'Pro', cycle_mode: '\x1b]0;evil\x07review' },
      { display: { unicode: false, showModel: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
    );
    const line1 = rendered.split('\n')[0];
    assert.doesNotMatch(line1, /\x1b\]0;/);
    assert.ok(line1.includes('\x1b[1m\x1b[36mreview\x1b[0m'));
  });

  test('display.showCycleMode false hides badge while omitting it shows badge by default', () => {
    const hidden = renderStatusLine(
      { steps: 1, currentDir: 'proj' },
      { plan_tier: 'Pro', cycle_mode: 'plan' },
      { display: { showCycleMode: false, unicode: false, showModel: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
    );
    assert.equal(stripAnsi(hidden.split('\n')[0]), 'proj | Pro');

    const defaultShown = renderStatusLine(
      { steps: 1, currentDir: 'proj' },
      { plan_tier: 'Pro', cycle_mode: 'plan' },
      { display: { unicode: false, showModel: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
    );
    assert.equal(stripAnsi(defaultShown.split('\n')[0]), 'proj | Pro | plan');
  });

  test('cycle_mode colors remain fixed under Amber Gold theme preset', () => {
    const autoOut = renderStatusLine(
      { steps: 1, currentDir: 'proj' },
      { plan_tier: 'Pro', cycle_mode: 'auto' },
      { theme: 'Amber Gold', display: { unicode: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
    );
    assert.ok(autoOut.includes('\x1b[1m\x1b[33mauto\x1b[0m'));

    const acceptOut = renderStatusLine(
      { steps: 1, currentDir: 'proj' },
      { plan_tier: 'Pro', cycle_mode: 'accept-edits' },
      { theme: 'Amber Gold', display: { unicode: false, showContextMonitor: false, showModelCache: false, showSpeed: false } }
    );
    assert.ok(acceptOut.includes('\x1b[1m\x1b[32maccept-edits\x1b[0m'));
  });

  test('narrow terminal (80 cols) keeps Line 1 <= 80 cols and preserves cycle_mode badge', () => {
    for (const mode of ['plan', 'accept-edits']) {
      const output = renderStatusLine(
        {
          steps: 10,
          currentDir: 'very-long-workspace-directory-name-for-narrow-terminal-test',
          branch: 'feature/very-long-branch-name-that-should-degrade-first',
          username: 'developer@example.com',
          outputSpeed: 42.5,
          childModelSpeeds: [
            { model: 'Gemini 4 Hydrogen', avgSpeed: 88.2, avgTtftSec: 0.45 },
            { model: 'Gemini 4 Argon', avgSpeed: 64.1, avgTtftSec: 0.72 },
          ],
        },
        {
          plan_tier: 'Google AI Pro',
          cycle_mode: mode,
          terminal_width: 80,
          model: { display_name: 'Gemini 4 Carbon (High)' },
        },
        {
          display: {
            unicode: false,
            showUsername: true,
            showCurrentDir: true,
            showGitBranch: true,
            showModel: true,
            showSpeed: true,
            showContextMonitor: false,
            showModelCache: false,
          },
        },
        [],
        'Google AI Pro',
        { updateAvailable: true, latestVersion: '9.9.9' }
      );
      const line1 = output.split('\n')[0];
      assert.ok(getAnsiDisplayWidth(line1) <= 80, `Line 1 width ${getAnsiDisplayWidth(line1)} exceeded 80`);
      assert.ok(stripAnsi(line1).includes(mode), `expected Line 1 to preserve "${mode}", got: ${stripAnsi(line1)}`);
    }
  });

  test('CLI stdin pipe renders plan badge on Line 1 in isolated temp environment', async () => {
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-cli-cycle-mode-'));
    try {
      const home = path.join(tmp, 'home');
      const dataDir = path.join(tmp, 'data');
      const globalConfigDir = path.join(tmp, 'global-config');
      const workspace = path.join(tmp, 'workspace');
      fs.mkdirSync(home, { recursive: true });
      fs.mkdirSync(dataDir, { recursive: true });
      fs.mkdirSync(globalConfigDir, { recursive: true });
      fs.mkdirSync(workspace, { recursive: true });
      fs.writeFileSync(path.join(dataDir, 'agy-pulse-update-status.json'), JSON.stringify({ lastCheck: Date.now() }));

      const result = await runCliWithStdin(
        {
          cwd: workspace,
          cycle_mode: 'plan',
          plan_tier: 'Pro',
          terminal_width: 120,
        },
        {
          HOME: home,
          USERPROFILE: home,
          APPDATA: path.join(tmp, 'appdata'),
          LOCALAPPDATA: path.join(tmp, 'localappdata'),
          XDG_DATA_HOME: path.join(tmp, 'xdg'),
          AGY_PULSE_DATA_DIR: dataDir,
          AGY_PULSE_GLOBAL_CONFIG_DIR: globalConfigDir,
          AGY_PULSE_FORCE_ASCII: '1',
        },
        workspace
      );

      assert.equal(result.code, 0);
      assert.equal(result.stderr, '');
      const firstLine = result.stdout.split('\n')[0];
      assert.ok(firstLine.includes('\x1b[1m\x1b[94mplan\x1b[0m'), `expected bold bright-blue plan badge in: ${JSON.stringify(firstLine)}`);
      assert.ok(stripAnsi(firstLine).includes('Pro | plan'));
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  });
});
