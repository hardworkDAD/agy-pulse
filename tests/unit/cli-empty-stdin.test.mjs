import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, '..', '..');
const require = createRequire(import.meta.url);

function runStatusLineWithStdin(stdinText, envOverrides, cwd = projectRoot) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js')], {
      cwd,
      env: {
        ...process.env,
        ...envOverrides,
        AGY_PULSE_FORCE_ASCII: '1',
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
    child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
    child.on('error', reject);
    child.on('close', code => resolve({ code, stdout, stderr }));
    child.stdin.end(stdinText);
  });
}

function runStatusLineWithEmptyStdin(envOverrides) {
  return runStatusLineWithStdin('', envOverrides);
}

test('CLI renders a baseline status line when agy sends empty stdin', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-empty-stdin-'));
  try {
    const home = path.join(tmp, 'home');
    const appData = path.join(tmp, 'appdata');
    const localAppData = path.join(tmp, 'localappdata');
    fs.mkdirSync(path.join(home, '.gemini', 'antigravity-cli'), { recursive: true });

    const result = await runStatusLineWithEmptyStdin({
      HOME: home,
      USERPROFILE: home,
      APPDATA: appData,
      LOCALAPPDATA: localAppData,
      XDG_DATA_HOME: path.join(tmp, 'xdg'),
    });

    assert.equal(result.code, 0);
    assert.equal(result.stderr, '');
    assert.match(result.stdout, /Free/);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('CLI outputs nothing and exits 0 when config.enabled is false', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-disabled-'));
  try {
    const dataDir = path.join(tmp, 'data');
    const workspace = path.join(tmp, 'workspace');
    fs.mkdirSync(dataDir, { recursive: true });
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(workspace, 'agy-pulse.config.json'), JSON.stringify({ enabled: false }));
    fs.writeFileSync(path.join(dataDir, 'agy-pulse-update-status.json'), JSON.stringify({ lastCheck: Date.now() }));

    const result = await runStatusLineWithStdin(
      JSON.stringify({ cwd: workspace }),
      { AGY_PULSE_DATA_DIR: dataDir },
      workspace
    );

    assert.equal(result.code, 0);
    assert.equal(result.stderr, '');
    assert.equal(result.stdout, '');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('mergeProviderQuota merges windows per provider without clobbering weekly', () => {
  const { mergeProviderQuota } = require('../../runtime/bin/agy-pulse.js');
  const merged = mergeProviderQuota(
    {
      google: {
        fiveHour: { remainingFraction: 0.9, resetTime: '2026-11-18T12:00:00Z' },
        weekly: { remainingFraction: 0.6, resetTime: '2026-11-24T00:00:00Z' },
      },
      claude: {
        weekly: { remainingFraction: 0.8, resetTime: '2026-11-24T00:00:00Z' },
      },
    },
    {
      google: {
        fiveHour: { remainingFraction: 0.5, resetTime: '2026-11-18T13:00:00Z' },
      },
    }
  );

  assert.equal(merged.google.fiveHour.remainingFraction, 0.5);
  assert.equal(merged.google.weekly.remainingFraction, 0.6);
  assert.equal(merged.claude.weekly.remainingFraction, 0.8);
});

test('CLI --diagnose outputs JSON auth diagnostic and exits 0', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-cli-diagnose-'));
  try {
    const dataDir = path.join(tmp, 'data');
    fs.mkdirSync(dataDir, { recursive: true });
    const result = await new Promise((resolve, reject) => {
      const child = spawn(
        process.execPath,
        [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js'), '--diagnose'],
        {
          cwd: projectRoot,
          env: { ...process.env, AGY_PULSE_DATA_DIR: dataDir },
          stdio: ['ignore', 'pipe', 'pipe'],
        }
      );
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
      child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
      child.on('error', reject);
      child.on('close', code => resolve({ code, stdout, stderr }));
    });

    assert.equal(result.code, 0, result.stderr);
    const parsed = JSON.parse(result.stdout);
    assert.equal(parsed.schemaVersion, 1);
    assert.ok(parsed.readToken && typeof parsed.readToken.found === 'boolean');
    assert.ok(parsed.quotaCache && typeof parsed.quotaCache.exists === 'boolean');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('deriveRepoRawFromBootstrapUrl handles query strings and arbitrary subpaths', () => {
  const { deriveRepoRawFromBootstrapUrl } = require('../../runtime/bin/agy-pulse.js');
  assert.equal(
    deriveRepoRawFromBootstrapUrl('https://raw.githubusercontent.com/user/repo/main/scripts/bootstrap.js?token=abc'),
    'https://raw.githubusercontent.com/user/repo/main'
  );
  assert.equal(
    deriveRepoRawFromBootstrapUrl('http://127.0.0.1:8080/mirror/sub/bootstrap.js?v=2'),
    'http://127.0.0.1:8080/mirror/sub'
  );
});

test('readUpdateInfo suppresses stale update status <= installed version', () => {
  const { readUpdateInfo } = require('../../runtime/bin/agy-pulse.js');
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-read-update-'));
  const prevDataDir = process.env.AGY_PULSE_DATA_DIR;
  try {
    process.env.AGY_PULSE_DATA_DIR = tmp;
    const statusPath = path.join(tmp, 'agy-pulse-update-status.json');

    fs.writeFileSync(statusPath, JSON.stringify({
      updateAvailable: true,
      latestVersion: '1.0.0',
    }));
    assert.equal(readUpdateInfo('1.0.0'), null);

    fs.writeFileSync(statusPath, JSON.stringify({
      updateAvailable: true,
      latestVersion: '0.9.5',
    }));
    assert.equal(readUpdateInfo('1.0.0'), null);

    fs.writeFileSync(statusPath, JSON.stringify({
      updateAvailable: true,
      latestVersion: '1.2.0',
    }));
    assert.equal(readUpdateInfo('1.0.0')?.latestVersion, '1.2.0');
  } finally {
    if (prevDataDir === undefined) delete process.env.AGY_PULSE_DATA_DIR;
    else process.env.AGY_PULSE_DATA_DIR = prevDataDir;
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('CLI pre-warms daily-cost.json from turn 1 even when showDailyCost/showWeeklyCost are false', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-daily-prewarm-'));
  try {
    const dataDir = path.join(tmp, 'data');
    const workspace = path.join(tmp, 'workspace');
    fs.mkdirSync(dataDir, { recursive: true });
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(dataDir, 'agy-pulse-update-status.json'), JSON.stringify({ lastCheck: Date.now() }));
    fs.writeFileSync(path.join(workspace, 'agy-pulse.config.json'), JSON.stringify({
      display: { showDailyCost: false, showWeeklyCost: false },
    }));

    const result = await runStatusLineWithStdin(
      JSON.stringify({
        conversation_id: 'prewarm-conv-1',
        cwd: workspace,
        cost: { total_cost_usd: 0.42 },
      }),
      { AGY_PULSE_DATA_DIR: dataDir },
      workspace
    );

    assert.equal(result.code, 0, result.stderr);
    const ledgerPath = path.join(dataDir, 'daily-cost.json');
    assert.equal(fs.existsSync(ledgerPath), true, 'daily-cost.json should be created from turn 1');
    const ledger = JSON.parse(fs.readFileSync(ledgerPath, 'utf8'));
    assert.equal(ledger.sessions['prewarm-conv-1']?.total, 0.42);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('CLI --update awaits bootstrap subprocess and forwards non-zero exit code on failure', async () => {
  const server = http.createServer((req, res) => {
    if (req.url.startsWith('/scripts/bootstrap.js')) {
      res.writeHead(200, { 'Content-Type': 'application/javascript' });
      res.end('console.error("simulated bootstrap failure"); process.exit(2);\n');
      return;
    }
    res.writeHead(404);
    res.end();
  });

  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-self-update-exit-'));
  try {
    const port = server.address().port;
    const result = await new Promise((resolve, reject) => {
      const child = spawn(
        process.execPath,
        [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js'), '--update'],
        {
          cwd: tmp,
          env: {
            ...process.env,
            AGY_PULSE_DATA_DIR: tmp,
            AGY_PULSE_BOOTSTRAP_URL: `http://127.0.0.1:${port}/scripts/bootstrap.js?ts=123`,
          },
          stdio: ['ignore', 'pipe', 'pipe'],
        }
      );
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
      child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
      child.on('error', reject);
      child.on('close', code => resolve({ code, stdout, stderr }));
    });

    assert.equal(result.code, 2);
    assert.match(result.stderr, /simulated bootstrap failure/);
  } finally {
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('detectLiveTerminalColumns respects explicit widths, preserves 80-col widths with 3-col margin above 82, and closes fd cleanly', () => {
  const { detectLiveTerminalColumns } = require('../../runtime/bin/agy-pulse.js');

  // 1. Explicit widths take precedence (returns null so caller does not overwrite)
  assert.equal(detectLiveTerminalColumns({ terminal_width: 100 }, null, { env: {} }), null);
  assert.equal(detectLiveTerminalColumns(null, { display: { terminalWidth: 90 } }, { env: {} }), null);
  assert.equal(detectLiveTerminalColumns(null, null, { env: { COLUMNS: '88' } }), null);

  // 2. Direct stdout/stderr TTY columns subtract 3-col margin only when > 82
  assert.equal(
    detectLiveTerminalColumns(null, null, { env: {}, stdout: { isTTY: true, columns: 100 }, stderr: { isTTY: false } }),
    97
  );
  assert.equal(
    detectLiveTerminalColumns(null, null, { env: {}, stdout: { isTTY: true, columns: 83 }, stderr: { isTTY: false } }),
    80
  );
  assert.equal(
    detectLiveTerminalColumns(null, null, { env: {}, stdout: { isTTY: true, columns: 82 }, stderr: { isTTY: false } }),
    82
  );
  assert.equal(
    detectLiveTerminalColumns(null, null, { env: {}, stdout: { isTTY: false }, stderr: { isTTY: true, columns: 42 } }),
    42
  );

  // 3. Pipe mode probes /dev/tty then /proc/<ppid>/fd/{1,2,0} with readlink validation and safe cleanup
  let closedFds = [];
  let destroyedStreams = 0;
  const mockFs = {
    readlinkSync(p) {
      if (p === '/proc/4321/fd/1') return 'pipe:[12345]'; // invalid pipe target -> must skip
      if (p === '/proc/4321/fd/2') return '/dev/pts/3';
      throw new Error('ENOENT');
    },
    openSync(p) {
      if (p === '/dev/tty') throw new Error('ENXIO');
      if (p === '/proc/4321/fd/2') return 77;
      throw new Error('ENOENT');
    },
    closeSync(fd) {
      closedFds.push(fd);
    },
  };
  const mockTty = {
    WriteStream: class {
      constructor(fd) {
        assert.equal(fd, 77);
        this.columns = 80;
      }
      on() {}
      destroy() {
        destroyedStreams += 1;
      }
    },
  };
  const probed = detectLiveTerminalColumns(null, null, {
    env: {},
    stdout: { isTTY: false },
    stderr: { isTTY: false },
    platform: 'linux',
    ppid: 4321,
    fs: mockFs,
    tty: mockTty,
  });
  assert.equal(probed, 80);
  assert.equal(destroyedStreams, 1);
  assert.deepEqual(closedFds, []); // ws.destroy() owns closing fd, no double closeSync

  // 4. Error in WriteStream constructor must close opened fd without leaking
  closedFds = [];
  const errFs = {
    readlinkSync: () => '/dev/pts/1',
    openSync: (p) => (p === '/dev/tty' ? 88 : (() => { throw new Error('ENOENT'); })()),
    closeSync: (fd) => { closedFds.push(fd); },
  };
  const errTty = {
    WriteStream: class {
      constructor() {
        throw new Error('not a tty');
      }
    },
  };
  const failed = detectLiveTerminalColumns(null, null, {
    env: {},
    stdout: { isTTY: false },
    stderr: { isTTY: false },
    platform: 'linux',
    ppid: 4321,
    fs: errFs,
    tty: errTty,
  });
  assert.equal(failed, null);
  assert.deepEqual(closedFds, [88]);
});

test('CLI --help, -h, and short subcommand help print agypl short usage and flags and exit 0', async () => {
  for (const flag of ['--help', '-h', 'help']) {
    const result = await new Promise((resolve, reject) => {
      const child = spawn(
        process.execPath,
        [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js'), flag],
        {
          cwd: projectRoot,
          env: process.env,
          stdio: ['ignore', 'pipe', 'pipe'],
        }
      );
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
      child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
      child.on('error', reject);
      child.on('close', code => resolve({ code, stdout, stderr }));
    });

    assert.equal(result.code, 0, result.stderr);
    assert.equal(result.stderr, '');
    assert.match(result.stdout, /agypl dashboard/);
    assert.match(result.stdout, /agypl stats/);
    assert.match(result.stdout, /agypl config/);
    assert.match(result.stdout, /agypl diagnose/);
    assert.match(result.stdout, /agypl update/);
    assert.match(result.stdout, /--stats/);
    assert.match(result.stdout, /--dashboard/);
    assert.match(result.stdout, /--config/);
    assert.match(result.stdout, /--diagnose/);
    assert.match(result.stdout, /--update/);
  }
});

test('CLI short subcommands diagnose and stats today --no-sync --json execute cleanly', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-short-subcmd-'));
  try {
    const dataDir = path.join(tmp, 'data');
    fs.mkdirSync(dataDir, { recursive: true });

    const diagRes = await new Promise((resolve, reject) => {
      const child = spawn(
        process.execPath,
        [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js'), 'diagnose'],
        {
          cwd: projectRoot,
          env: { ...process.env, AGY_PULSE_DATA_DIR: dataDir },
          stdio: ['ignore', 'pipe', 'pipe'],
        }
      );
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
      child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
      child.on('error', reject);
      child.on('close', code => resolve({ code, stdout, stderr }));
    });
    assert.equal(diagRes.code, 0, diagRes.stderr);
    assert.equal(JSON.parse(diagRes.stdout).schemaVersion, 1);

    const statsRes = await new Promise((resolve, reject) => {
      const child = spawn(
        process.execPath,
        [path.join(projectRoot, 'runtime', 'bin', 'agy-pulse.js'), 'stats', 'today', '--no-sync', '--json'],
        {
          cwd: projectRoot,
          env: { ...process.env, AGY_PULSE_DATA_DIR: dataDir },
          stdio: ['ignore', 'pipe', 'pipe'],
        }
      );
      let stdout = '';
      let stderr = '';
      child.stdout.on('data', chunk => { stdout += chunk.toString('utf8'); });
      child.stderr.on('data', chunk => { stderr += chunk.toString('utf8'); });
      child.on('error', reject);
      child.on('close', code => resolve({ code, stdout, stderr }));
    });
    assert.equal(statsRes.code, 0, statsRes.stderr);
    const parsedStats = JSON.parse(statsRes.stdout);
    assert.equal(parsedStats.meta.range, 'today');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('extractConversationId resolves conversation_id or /brain/<uuid>/ transcript path', () => {
  const { extractConversationId } = require('../../runtime/bin/agy-pulse.js');
  assert.equal(
    extractConversationId({ conversation_id: 'conv-123' }, '/tmp/other/transcript.jsonl'),
    'conv-123'
  );
  assert.equal(
    extractConversationId(null, '/home/user/.gemini/antigravity-cli/brain/abc-def-123/.system_generated/logs/transcript.jsonl'),
    'abc-def-123'
  );
  assert.equal(
    extractConversationId(null, 'C:\\Users\\u\\.gemini\\antigravity-cli\\brain\\win-uuid-1\\.system_generated\\logs\\transcript.jsonl'),
    'win-uuid-1'
  );
  assert.equal(extractConversationId(null, '/tmp/no-brain/transcript.jsonl'), null);
});
