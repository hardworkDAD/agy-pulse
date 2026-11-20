import { test } from 'node:test';
import assert from 'node:assert';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const previousDataDir = process.env.AGY_PULSE_DATA_DIR;
const testDataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-test-parser-'));
process.env.AGY_PULSE_DATA_DIR = testDataDir;

const require = createRequire(import.meta.url);
const { getSessionState, parseAgyQuota } = require('../../runtime/parser.js');

process.on('exit', () => {
  try {
    fs.rmSync(testDataDir, { recursive: true, force: true });
  } catch {}
  if (previousDataDir === undefined) delete process.env.AGY_PULSE_DATA_DIR;
  else process.env.AGY_PULSE_DATA_DIR = previousDataDir;
});

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, '..', '..');

// Env vars that hijack `git` subprocesses regardless of cwd. The git pre-push
// hook environment sets GIT_DIR/GIT_WORK_TREE for its own use; without
// scrubbing them, the parser's spawned `git rev-parse` returns the host
// repo's hooks path instead of the test's fixture, then writeFileSync of
// fixture data lands in the real .git/hooks/ — silent pollution.
const GIT_ENV_VARS = ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR'];

function withScrubbedGitEnv(fn) {
  const saved = {};
  for (const key of GIT_ENV_VARS) {
    saved[key] = process.env[key];
    delete process.env[key];
  }
  try {
    return fn();
  } finally {
    for (const key of GIT_ENV_VARS) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
  }
}

function scrubbedGitEnv(extra = {}) {
  const env = { ...process.env, ...extra };
  for (const key of GIT_ENV_VARS) delete env[key];
  return env;
}

test('getSessionState should initialize state properly even for missing files', async () => {
  assert.strictEqual(typeof getSessionState, 'function');
  const state = await getSessionState('missing-transcript-nonexistent.jsonl');
  assert.strictEqual(state.currentDir, path.basename(process.cwd()));
});

test('getSessionState reads the highest transcript step index', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-steps-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({ step_index: 1 }),
      'not-json',
      JSON.stringify({ step_index: 4 }),
      JSON.stringify({ step_index: 2 }),
      '',
    ].join('\n'));

    const state = await getSessionState(transcriptPath);

    assert.equal(state.steps, 4);
    assert.equal(typeof state.branch, 'string');
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState detects workspace config metadata correctly', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-metadata-'));
  const originalCwd = process.cwd;
  process.cwd = () => tempDir;

  try { await withScrubbedGitEnv(async () => {
    // 1. Create CLAUDE.md
    fs.writeFileSync(path.join(tempDir, 'CLAUDE.md'), '# rules');

    // 2. Create rules folders and md files
    const ruleDir = path.join(tempDir, '.claude', 'rules');
    fs.mkdirSync(ruleDir, { recursive: true });
    fs.writeFileSync(path.join(ruleDir, 'rule1.md'), 'content');
    fs.writeFileSync(path.join(ruleDir, 'rule2.md'), 'content');

    // 3. Create active git hooks
    const hooksDir = path.join(tempDir, '.git', 'hooks');
    fs.mkdirSync(hooksDir, { recursive: true });
    fs.writeFileSync(path.join(hooksDir, 'pre-commit'), 'hook content');
    fs.writeFileSync(path.join(hooksDir, 'pre-push.sample'), 'sample hook content');

    const state = await getSessionState('missing-transcript.jsonl');

    assert.equal(state.memoryFile, 'CLAUDE.md');
    assert.equal(state.rulesCount, 2);
    assert.equal(state.hooksCount, 1);
  }); } finally {
    process.cwd = originalCwd;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState prioritizes AGENTS.md over GEMINI.md and CLAUDE.md', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-agents-priority-'));
  const originalCwd = process.cwd;
  process.cwd = () => tempDir;

  try { await withScrubbedGitEnv(async () => {
    fs.writeFileSync(path.join(tempDir, 'AGENTS.md'), '# Agents');
    fs.writeFileSync(path.join(tempDir, 'GEMINI.md'), '# Gemini');
    fs.writeFileSync(path.join(tempDir, 'CLAUDE.md'), '# Claude');

    const state = await getSessionState('missing-transcript.jsonl');

    assert.equal(state.memoryFile, 'AGENTS.md');
  }); } finally {
    process.cwd = originalCwd;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState detects lowercase agents.md', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-agents-lowercase-'));
  const originalCwd = process.cwd;
  process.cwd = () => tempDir;

  try { await withScrubbedGitEnv(async () => {
    fs.writeFileSync(path.join(tempDir, 'agents.md'), '# agents');

    const state = await getSessionState('missing-transcript.jsonl');

    assert.equal(state.memoryFile, 'agents.md');
  }); } finally {
    process.cwd = originalCwd;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState captures context window token usage from transcript lines', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-usage-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({ step_index: 1 }),
      JSON.stringify({
        step_index: 2,
        context_window: {
          total_input_tokens: 138206000,
          total_output_tokens: 202000,
          current_usage: {
            input_tokens: 6000,
            cache_read_input_tokens: 138200000
          }
        }
      }),
      '',
    ].join('\n'));

    const state = await getSessionState(transcriptPath);

    assert.equal(state.transcriptPath, transcriptPath);
    assert.equal(state.usage.total_input_tokens, 138206000);
    assert.equal(state.usage.total_output_tokens, 202000);
    assert.equal(state.usage.current_usage.input_tokens, 6000);
    assert.equal(state.usage.current_usage.cache_read_input_tokens, 138200000);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState measures PLANNER_RESPONSE durations to next non-user step and excludes USER_INPUT idle time', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-timing-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({ step_index: 0, type: 'USER_INPUT', created_at: '2026-10-08T10:00:00Z' }),
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: '2026-10-08T10:00:00Z', output_tokens: 400 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: '2026-10-08T10:00:05Z' }), // Step 1 took 5000ms
      JSON.stringify({ step_index: 3, type: 'PLANNER_RESPONSE', created_at: '2026-10-08T10:00:15Z', output_tokens: 300 }),
      JSON.stringify({ step_index: 4, type: 'USER_INPUT', created_at: '2026-10-08T10:05:00Z' }), // User idle time excluded
      JSON.stringify({ step_index: 5, type: 'PLANNER_RESPONSE', created_at: '2026-10-08T10:05:00Z', output_tokens: 600 }),
      JSON.stringify({ step_index: 6, type: 'GENERIC', created_at: '2026-10-08T10:05:04Z' }), // Step 5 took 4000ms
      '',
    ].join('\n'));

    const state = await getSessionState(transcriptPath);
    assert.equal(state.totalApiDurationMs, 9000);
    assert.deepEqual(state.lastPlannerResponse, {
      startMs: Date.parse('2026-10-08T10:05:00Z'),
      endMs: Date.parse('2026-10-08T10:05:04Z'),
      outputTokens: 600,
    });
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState reads project memory from HOME without counting memory docs as rules', () => {
  const workspaceDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-home-workspace-')));
  const homeDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-home-'));
  const normalizedCwd = workspaceDir.replace(/\\/g, '/');
  const projectKey = normalizedCwd.replace(/:/g, '').replace(/\//g, '-');
  const projectMemoryDir = path.join(homeDir, '.claude', 'projects', projectKey, 'memory');
  const ruleDir = path.join(workspaceDir, '.claude', 'rules');

  try {
    fs.mkdirSync(projectMemoryDir, { recursive: true });
    fs.writeFileSync(path.join(projectMemoryDir, 'MEMORY.md'), '# memory');
    fs.writeFileSync(path.join(projectMemoryDir, 'feedback.md'), '# feedback');
    fs.mkdirSync(ruleDir, { recursive: true });
    fs.writeFileSync(path.join(ruleDir, 'rule.md'), '# rule');

    const script = `
      process.chdir(${JSON.stringify(workspaceDir)});
      const { getSessionState } = require(${JSON.stringify(path.join(projectRoot, 'runtime', 'parser.js'))});
      getSessionState('missing-transcript.jsonl')
        .then(state => process.stdout.write(JSON.stringify(state)));
    `;

    const env = { ...process.env, HOME: homeDir, USERPROFILE: homeDir };
    delete env.GIT_DIR;
    delete env.GIT_WORK_TREE;

    const result = spawnSync(process.execPath, ['-e', script], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });

    assert.equal(result.status, 0);
    assert.equal(result.stderr, '');
    const parsed = JSON.parse(result.stdout);
    assert.equal(parsed.memoryFile, 'MEMORY.md');
    assert.equal(parsed.rulesCount, 1);
  } finally {
    fs.rmSync(workspaceDir, { recursive: true, force: true });
    fs.rmSync(homeDir, { recursive: true, force: true });
  }
});

test('getSessionState counts git hooks from subdirectories', () => {
  const repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-hooks-'));
  const nestedDir = path.join(repoDir, 'nested');
  const env = scrubbedGitEnv();

  try {
    const init = spawnSync('git', ['init'], {
      cwd: repoDir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });
    assert.equal(init.status, 0, init.stderr);

    const hooksPathResult = spawnSync('git', ['rev-parse', '--git-path', 'hooks'], {
      cwd: repoDir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });
    assert.equal(hooksPathResult.status, 0, hooksPathResult.stderr);
    const hooksDir = path.resolve(repoDir, hooksPathResult.stdout.trim());
    fs.mkdirSync(hooksDir, { recursive: true });
    fs.writeFileSync(path.join(hooksDir, 'pre-commit'), 'hook');
    fs.writeFileSync(path.join(hooksDir, 'pre-push.sample'), 'sample');

    fs.mkdirSync(nestedDir, { recursive: true });
    const script = `
      process.chdir(${JSON.stringify(nestedDir)});
      const { getSessionState } = require(${JSON.stringify(path.join(projectRoot, 'runtime', 'parser.js'))});
      getSessionState('missing-transcript.jsonl')
        .then(state => process.stdout.write(JSON.stringify(state)));
    `;

    const result = spawnSync(process.execPath, ['-e', script], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      env,
    });

    assert.equal(result.status, 0);
    assert.equal(result.stderr, '');
    const parsed = JSON.parse(result.stdout);
    assert.equal(parsed.hooksCount, 1);
  } finally {
    fs.rmSync(repoDir, { recursive: true, force: true });
  }
});

test('getSessionState falls back outside git repositories without stderr noise', () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-non-git-'));
  const script = `
    const { getSessionState } = require(${JSON.stringify(path.join(projectRoot, 'runtime', 'parser.js'))});
    getSessionState('missing-transcript.jsonl')
      .then(state => process.stdout.write(JSON.stringify(state)));
  `;

  const env = { ...process.env };
  delete env.GIT_DIR;
  delete env.GIT_WORK_TREE;

  const result = spawnSync(process.execPath, ['-e', script], {
    cwd: tempDir,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    env,
  });

  assert.equal(result.status, 0);
  assert.equal(result.stderr, '');
  const parsed = JSON.parse(result.stdout);
  assert.equal(parsed.steps, 0);
  assert.equal(parsed.branch, null);
});

test('getSessionState resolves username from id_token in oauth_creds.json', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-username-'));
  const originalHome = process.env.HOME;
  const originalUserProfile = process.env.USERPROFILE;
  process.env.HOME = tempDir;
  process.env.USERPROFILE = tempDir;

  try {
    const geminiDir = path.join(tempDir, '.gemini');
    fs.mkdirSync(geminiDir, { recursive: true });

    // Mock an ID token with an email claim: shetterelland@gmail.com
    const header = Buffer.from(JSON.stringify({ alg: 'RS256' })).toString('base64').replace(/=/g, '').replace(/\+/g, '-').replace(/\//g, '_');
    const payload = Buffer.from(JSON.stringify({ email: 'shetterelland@gmail.com' })).toString('base64').replace(/=/g, '').replace(/\+/g, '-').replace(/\//g, '_');
    const signature = 'dummy-sig';
    const mockIdToken = `${header}.${payload}.${signature}`;

    fs.writeFileSync(path.join(geminiDir, 'oauth_creds.json'), JSON.stringify({
      id_token: mockIdToken
    }));

    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.strictEqual(state.username, 'shetterelland@gmail.com');
  } finally {
    process.env.HOME = originalHome;
    process.env.USERPROFILE = originalUserProfile;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState falls back to OS username if oauth_creds.json is invalid or missing', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-username-fallback-'));
  const originalHome = process.env.HOME;
  const originalUserProfile = process.env.USERPROFILE;
  process.env.HOME = tempDir;
  process.env.USERPROFILE = tempDir;

  try {
    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.strictEqual(typeof state.username, 'string');
    assert.ok(state.username.length > 0);
    assert.strictEqual(state.username, os.userInfo().username);
  } finally {
    process.env.HOME = originalHome;
    process.env.USERPROFILE = originalUserProfile;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

const b64url = (obj) => Buffer.from(JSON.stringify(obj)).toString('base64')
  .replace(/=/g, '').replace(/\+/g, '-').replace(/\//g, '_');
const mockIdToken = (payload) => `${b64url({ alg: 'RS256' })}.${b64url(payload)}.dummy-sig`;

const withTempHome = async (fn) => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-account-'));
  const originalHome = process.env.HOME;
  const originalUserProfile = process.env.USERPROFILE;
  process.env.HOME = tempDir;
  process.env.USERPROFILE = tempDir;
  const geminiDir = path.join(tempDir, '.gemini');
  fs.mkdirSync(geminiDir, { recursive: true });
  try {
    return await fn(geminiDir);
  } finally {
    process.env.HOME = originalHome;
    process.env.USERPROFILE = originalUserProfile;
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
};

test('getSessionState prefers google_accounts.json active over the oauth_creds id_token', async () => {
  await withTempHome(async (geminiDir) => {
    // Authoritative active account differs from the stale oauth_creds email.
    fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({
      active: 'active-user@gmail.com',
      old: ['stale-user@gmail.com']
    }));
    fs.writeFileSync(path.join(geminiDir, 'oauth_creds.json'), JSON.stringify({
      id_token: mockIdToken({ email: 'stale-user@gmail.com' })
    }));
    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.strictEqual(state.username, 'active-user@gmail.com');
  });
});

test('getSessionState prefers live active token email over stale google_accounts.json', async () => {
  await withTempHome(async (geminiDir) => {
    // google_accounts.json has stale user, but active antigravity token has live user.
    fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({
      active: 'stale-user@gmail.com',
      old: []
    }));
    const cliDir = path.join(geminiDir, 'antigravity-cli');
    fs.mkdirSync(cliDir, { recursive: true });
    fs.writeFileSync(path.join(cliDir, 'antigravity-oauth-token'), JSON.stringify({
      token: {
        access_token: 'live-access-token',
        expiry: '2026-10-30T20:00:00Z',
      },
      id_token: mockIdToken({ email: 'live-user@gmail.com' }),
    }));
    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.strictEqual(state.username, 'live-user@gmail.com');
  });
});

test('getSessionState returns the oauth_creds email even when the id_token is expired', async () => {
  // Real-world state: oauth_creds is the Gemini-CLI file agy never refreshes, so
  // its id_token is routinely expired while the email is still the right account.
  await withTempHome(async (geminiDir) => {
    fs.writeFileSync(path.join(geminiDir, 'oauth_creds.json'), JSON.stringify({
      id_token: mockIdToken({ email: 'shetterelland@gmail.com', exp: Math.floor((Date.now() - 10000) / 1000) }),
      expiry_date: Date.now() - 10000
    }));
    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.strictEqual(state.username, 'shetterelland@gmail.com');
  });
});

test('getSessionState falls back to oauth_creds email when google_accounts.json is malformed', async () => {
  await withTempHome(async (geminiDir) => {
    fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({ active: null }));
    fs.writeFileSync(path.join(geminiDir, 'oauth_creds.json'), JSON.stringify({
      id_token: mockIdToken({ email: 'shetterelland@gmail.com' })
    }));
    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.strictEqual(state.username, 'shetterelland@gmail.com');
  });
});

test('getSessionState parses image rate limit 429 errors from transcript', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-ratelimit-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');

    // entry1: quotaResetDelay in structured error object
    const entry1 = {
      step_index: 1,
      created_at: new Date(Date.now() - 5000).toISOString(),
      content: 'RESOURCE_EXHAUSTED',
      error: {
        code: 429,
        status: 'RESOURCE_EXHAUSTED',
        details: [{ quotaResetDelay: '3h14m20s' }]
      }
    };

    // entry2: quotaResetTimeStamp in structured error object (should win — further in future)
    const future4h = new Date(Date.now() + 4 * 3600 * 1000).toISOString();
    const entry2 = {
      step_index: 2,
      created_at: new Date(Date.now() - 1000).toISOString(),
      content: 'RESOURCE_EXHAUSTED',
      error: {
        code: 429,
        status: 'RESOURCE_EXHAUSTED',
        details: [{ quotaResetTimeStamp: future4h }]
      }
    };

    fs.writeFileSync(transcriptPath, [
      JSON.stringify(entry1),
      JSON.stringify(entry2),
      ''
    ].join('\n'));

    const state = await getSessionState(transcriptPath);

    assert.ok(state.imageExhausted);
    assert.strictEqual(state.imageExhausted.exhausted, true);
    // Should prefer the later timestamp (4h) over the earlier delay (3h14m)
    const resetTime = new Date(state.imageExhausted.resetTime).getTime();
    assert.ok(resetTime > Date.now() + 3.9 * 3600 * 1000);
    assert.ok(resetTime < Date.now() + 4.1 * 3600 * 1000);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState does NOT false-positive on entries that only contain "429" as a number', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-no-fp-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');

    // step_index happens to be 429, token count is 429 — must NOT set imageExhausted
    const entry = {
      step_index: 429,
      context_window: { total_input_tokens: 429, total_output_tokens: 100, used_percentage: 1 }
    };

    fs.writeFileSync(transcriptPath, JSON.stringify(entry) + '\n');
    const state = await getSessionState(transcriptPath);
    assert.strictEqual(state.imageExhausted, undefined);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState accepts an explicit cwd and resolves metadata from that directory', async () => {
  const tempWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-workspace-'));
  try {
    fs.writeFileSync(path.join(tempWorkspace, 'AGENTS.md'), '# Agents');
    const rulesDir = path.join(tempWorkspace, '.claude', 'rules');
    fs.mkdirSync(rulesDir, { recursive: true });
    fs.writeFileSync(path.join(rulesDir, 'rule.md'), 'rule content');

    const state = await getSessionState('missing-transcript.jsonl', tempWorkspace);
    assert.equal(state.currentDir, path.basename(tempWorkspace));
    assert.equal(state.memoryFile, 'AGENTS.md');
    assert.equal(state.rulesCount, 1);
  } finally {
    fs.rmSync(tempWorkspace, { recursive: true, force: true });
  }
});

test('parseAgyQuota parses stdin quota objects into providerQuota structure', () => {
  const stdinQuota = {
    'gemini-5h': {
      remaining_fraction: 0.9495,
      reset_time: '2026-09-04T23:36:33Z',
      reset_in_seconds: 17380,
    },
    'gemini-weekly': {
      remaining_fraction: 0.8252,
      reset_time: '2026-09-10T22:02:44Z',
      reset_in_seconds: 530151,
    },
    '3p-5h': {
      remaining_fraction: 1,
      reset_time: '2026-09-04T23:44:15Z',
      reset_in_seconds: 17842,
    },
    '3p-weekly': {
      remaining_fraction: 1,
      reset_time: '2026-09-11T18:44:15Z',
      reset_in_seconds: 604642,
    },
  };

  const parsed = parseAgyQuota(stdinQuota);
  assert.ok(parsed);
  assert.equal(parsed.google.fiveHour.remainingFraction, 0.9495);
  assert.equal(parsed.google.fiveHour.resetTime, '2026-09-04T23:36:33Z');
  assert.equal(parsed.google.weekly.remainingFraction, 0.8252);
  assert.equal(parsed.google.weekly.resetTime, '2026-09-10T22:02:44Z');
  assert.equal(parsed.claude.fiveHour.remainingFraction, 1);
  assert.equal(parsed.claude.weekly.remainingFraction, 1);
});

test('parseAgyQuota returns null for missing or invalid quota', () => {
  assert.equal(parseAgyQuota(null), null);
  assert.equal(parseAgyQuota({}), null);
  assert.equal(parseAgyQuota('invalid'), null);
});

test('getSessionState incrementally parses PLANNER_RESPONSE tokens, CHECKPOINT count, and handles partial/truncated files', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-incremental-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    const turn1 = {
      step_index: 1,
      type: 'PLANNER_RESPONSE',
      created_at: '2026-11-18T10:00:00Z',
      model: 'gemini-4-carbon-high',
      input_tokens: 100000,
      output_tokens: 1200,
      cache_read_tokens: 80000,
      cache_creation_tokens: 5000,
    };
    fs.writeFileSync(transcriptPath, JSON.stringify(turn1) + '\n');

    let state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.branch, null);
    assert.equal(state.compactionCount, 0);
    assert.equal(state.totalOutputTokens, 1200);
    assert.equal(state.maxHistoricalCache, 80000);
    assert.deepEqual(state.lastTurnUsage, {
      input_tokens: 100000,
      output_tokens: 1200,
      cache_read_input_tokens: 80000,
      cache_creation_input_tokens: 5000,
      model: 'gemini-4-carbon-high',
      cacheExclusive: true,
    });
    // inclusive: max(100000, 85000) + 1200 = 101200; exclusive: 100000 + 85000 + 1200 = 186200
    assert.deepEqual(state.turnHistoryMaxByMode, {
      inclusivePeak: 101200,
      exclusivePeak: 186200,
    });
    assert.equal(state.peakRawTurnTokens, 186200);
    assert.equal(state.usage.current_usage.input_tokens, 100000);
    assert.equal(state.usage.total_output_tokens, 1200);
    assert.equal(state.serverCompactionLimit, 512000);

    // Append CHECKPOINT + partial line WITHOUT trailing newline -> partial line must wait
    const ckpt = { step_index: 2, type: 'CHECKPOINT', created_at: '2026-11-18T10:01:00Z' };
    const turn2 = {
      step_index: 3,
      type: 'PLANNER_RESPONSE',
      created_at: '2026-11-18T10:02:00Z',
      input_tokens: 20000,
      output_tokens: 800,
      cache_read_tokens: 15000,
      cache_creation_tokens: 1000,
    };
    const turn2Str = JSON.stringify(turn2);
    const halfIdx = Math.floor(turn2Str.length / 2);
    fs.appendFileSync(transcriptPath, JSON.stringify(ckpt) + '\n' + turn2Str.slice(0, halfIdx));

    state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.steps, 2);
    assert.equal(state.compactionCount, 1);
    assert.equal(state.totalOutputTokens, 1200);

    // Complete the partial line with trailing newline -> now parsed cleanly
    fs.appendFileSync(transcriptPath, turn2Str.slice(halfIdx) + '\n');
    state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.steps, 3);
    assert.equal(state.compactionCount, 1);
    assert.equal(state.totalOutputTokens, 2000);
    assert.equal(state.lastTurnUsage.input_tokens, 20000);
    assert.equal(state.turnHistoryMaxByMode.inclusivePeak, 101200);

    // Truncate/reset transcript to smaller file -> resets incremental cache cleanly
    const resetTurn = {
      step_index: 1,
      type: 'PLANNER_RESPONSE',
      created_at: '2026-11-18T11:00:00Z',
      input_tokens: 500,
      output_tokens: 100,
      cache_read_tokens: 0,
      cache_creation_tokens: 0,
    };
    fs.writeFileSync(transcriptPath, JSON.stringify(resetTurn) + '\n');
    state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.steps, 1);
    assert.equal(state.compactionCount, 0);
    assert.equal(state.totalOutputTokens, 100);
    assert.deepEqual(state.turnHistoryMaxByMode, {
      inclusivePeak: 600,
      exclusivePeak: 600,
    });
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState clears pendingPlannerStartMs on SYSTEM_MESSAGE, source=SYSTEM, or missing timestamp', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-sys-timing-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: '2026-11-18T10:00:00Z', output_tokens: 200 }),
      JSON.stringify({ step_index: 2, type: 'SYSTEM_MESSAGE', created_at: '2026-11-18T10:05:00Z' }),
      JSON.stringify({ step_index: 3, type: 'GENERIC', created_at: '2026-11-18T10:05:01Z' }),
      JSON.stringify({ step_index: 4, type: 'PLANNER_RESPONSE', created_at: '2026-11-18T10:06:00Z', output_tokens: 300 }),
      JSON.stringify({ step_index: 5, type: 'GENERIC', source: 'SYSTEM', created_at: '2026-11-18T10:09:00Z' }),
      JSON.stringify({ step_index: 6, type: 'PLANNER_RESPONSE', created_at: '2026-11-18T10:10:00Z', output_tokens: 400 }),
      JSON.stringify({ step_index: 7, type: 'GENERIC' }), // missing timestamp clears pending start
      JSON.stringify({ step_index: 8, type: 'GENERIC', created_at: '2026-11-18T10:15:00Z' }),
      '',
    ].join('\n'));

    const state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.totalApiDurationMs, undefined);
    assert.deepEqual(state.lastPlannerResponse, {
      startMs: Date.parse('2026-11-18T10:10:00Z'),
      endMs: null,
      outputTokens: 400,
    });
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState ignores 429 text in non-error entries and resolves truncated_fields from transcript_full.jsonl', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-429-full-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    const fullPath = path.join(tempDir, 'transcript_full.jsonl');

    // Non-error PLANNER_RESPONSE mentioning HTTP 429 / RESOURCE_EXHAUSTED in code/text must NOT trigger imageExhausted
    const chattyPlanner = {
      step_index: 1,
      type: 'PLANNER_RESPONSE',
      created_at: new Date().toISOString(),
      content: 'Handling HTTP 429 RESOURCE_EXHAUSTED with quotaResetDelay in docs',
    };
    fs.writeFileSync(transcriptPath, JSON.stringify(chattyPlanner) + '\n');
    let state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.imageExhausted, undefined);

    // Structured ERROR entry with truncated content -> reads transcript_full.jsonl for step_index 2
    const futureTs = new Date(Date.now() + 2 * 3600 * 1000).toISOString();
    const truncatedErr = {
      step_index: 2,
      status: 'ERROR',
      created_at: new Date().toISOString(),
      content: 'Error truncated...',
      truncated_fields: ['content'],
    };
    const fullErr = {
      step_index: 2,
      status: 'ERROR',
      created_at: truncatedErr.created_at,
      content: 'HTTP 429 RESOURCE_EXHAUSTED',
      error: {
        code: 429,
        status: 'RESOURCE_EXHAUSTED',
        details: [{ quotaResetTimeStamp: futureTs }],
      },
    };
    fs.writeFileSync(fullPath, JSON.stringify(fullErr) + '\n');
    fs.appendFileSync(transcriptPath, JSON.stringify(truncatedErr) + '\n');

    state = await getSessionState(transcriptPath, tempDir);
    assert.ok(state.imageExhausted);
    assert.equal(state.imageExhausted.exhausted, true);
    assert.equal(state.imageExhausted.resetTime, new Date(futureTs).toISOString());
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState decodes JWT email with base64url characters (- and _)', async () => {
  await withTempHome(async (geminiDir) => {
    // 'a>?>?@example.com' encodes '-' and '_' in base64url
    const targetEmail = 'a>?>?@example.com';
    const b64u = (obj) => Buffer.from(JSON.stringify(obj)).toString('base64url');
    const token = `${b64u({ alg: 'RS256' })}.${b64u({ email: targetEmail })}.sig`;
    fs.writeFileSync(path.join(geminiDir, 'oauth_creds.json'), JSON.stringify({ id_token: token }));

    const state = await getSessionState('missing-transcript-nonexistent.jsonl');
    assert.equal(state.username, targetEmail);
  });
});

test('getSessionState parses ms, decimal seconds, and compound quotaResetDelay durations', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-duration-'));
  try {
    const baseMs = Date.now() + 60_000;
    const baseIso = new Date(baseMs).toISOString();

    for (const [idx, [delay, expectedMs]] of [
      ['500ms', 500],
      ['42.5s', 42500],
      ['1h30m', 5400000],
    ].entries()) {
      const tp = path.join(tempDir, `t-${idx}.jsonl`);
      fs.writeFileSync(tp, JSON.stringify({
        step_index: 1,
        status: 'ERROR',
        created_at: baseIso,
        error: { code: 429, details: [{ quotaResetDelay: delay }] },
      }) + '\n');
      const state = await getSessionState(tp, tempDir);
      assert.ok(state.imageExhausted);
      assert.equal(Date.parse(state.imageExhausted.resetTime) - baseMs, expectedMs);
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('parseAgyQuota validates/clamps remaining_fraction, validates reset_time, and supports argon', () => {
  const parsed = parseAgyQuota({
    'gemini-5h': { remaining_fraction: 0, reset_time: '2026-09-04T23:36:33Z' },
    'gemini-weekly': { remaining_fraction: 1.5, reset_time: 'not-a-date' },
    '3p-5h': { remaining_fraction: -0.25, reset_time: '2026-09-04T23:44:15Z' },
    '3p-weekly': { remaining_fraction: 'bad', reset_time: '2026-09-11T18:44:15Z' },
    'argon-5h': { remaining_fraction: 0.4, reset_time: '2026-09-04T22:00:00Z' },
    'argon-weekly': { remaining_fraction: 0.75, reset_time: '2026-09-10T22:00:00Z' },
  });

  assert.equal(parsed.google.fiveHour.remainingFraction, 0);
  assert.equal(parsed.google.fiveHour.resetTime, '2026-09-04T23:36:33Z');
  assert.equal(parsed.google.weekly.remainingFraction, 1);
  assert.equal(parsed.google.weekly.resetTime, null);
  assert.equal(parsed.claude.fiveHour.remainingFraction, 0);
  assert.equal(parsed.claude.weekly, undefined);
  assert.equal(parsed.argon.fiveHour.remainingFraction, 0.4);
  assert.equal(parsed.argon.weekly.remainingFraction, 0.75);

  const aliasedClaude = parseAgyQuota({
    'claude-5h': { remaining_fraction: 0.75, reset_time: '2026-09-04T23:44:15Z' },
    'anthropic-weekly': { remaining_fraction: 0.65, reset_time: '2026-09-11T18:44:15Z' },
  });
  assert.equal(aliasedClaude.claude.fiveHour.remainingFraction, 0.75);
  assert.equal(aliasedClaude.claude.weekly.remainingFraction, 0.65);
});

test('getSessionState accumulates totalCacheReadTokens/totalCacheCreationTokens and preserves context_window_size', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-cache-tokens-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({
        step_index: 1,
        type: 'PLANNER_RESPONSE',
        created_at: '2026-11-18T10:00:00Z',
        context_window: {
          context_window_size: 1000000,
          total_input_tokens: 10000,
          total_output_tokens: 100,
          current_usage: {
            input_tokens: 10000,
            output_tokens: 100,
            cache_read_input_tokens: 5000,
            cache_creation_input_tokens: 200,
          },
        },
      }),
      JSON.stringify({
        step_index: 2,
        type: 'PLANNER_RESPONSE',
        created_at: '2026-11-18T10:01:00Z',
        input_tokens: 2000,
        output_tokens: 150,
        cache_read_tokens: 6000,
        cache_creation_tokens: 300,
      }),
      '',
    ].join('\n'));

    const state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.totalCacheReadTokens, 11000);
    assert.equal(state.totalCacheCreationTokens, 500);
    assert.equal(state.usage.context_window_size, 1000000);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState invalidates incremental cache on same-inode overwrite via headHash and prunes cache dir to 32 files', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-headhash-'));
  try {
    const transcriptPath = path.join(tempDir, 'same-inode.jsonl');
    const lineA = JSON.stringify({
      step_index: 1,
      type: 'PLANNER_RESPONSE',
      input_tokens: 1000,
      output_tokens: 100,
      cache_read_tokens: 400,
    });
    fs.writeFileSync(transcriptPath, lineA + '\n');
    let state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.totalOutputTokens, 100);
    assert.equal(state.totalCacheReadTokens, 400);

    // Overwrite in-place preserving inode with same or larger byte size
    const lineB = JSON.stringify({
      step_index: 1,
      type: 'PLANNER_RESPONSE',
      input_tokens: 2000,
      output_tokens: 999,
      cache_read_tokens: 1500,
      note: 'padding-to-exceed-previous-file-size-on-same-inode-overwrite',
    });
    const fd = fs.openSync(transcriptPath, 'r+');
    try {
      fs.writeSync(fd, lineB + '\n', 0, 'utf8');
    } finally {
      fs.closeSync(fd);
    }
    state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.totalOutputTokens, 999);
    assert.equal(state.totalCacheReadTokens, 1500);

    // Populate > 32 cached transcripts and verify pruning
    for (let i = 0; i < 35; i += 1) {
      const p = path.join(tempDir, `bulk-${i}.jsonl`);
      fs.writeFileSync(p, JSON.stringify({ step_index: i + 1 }) + '\n');
      await getSessionState(p, tempDir);
    }
    const cacheDir = path.join(testDataDir, 'transcript-cache');
    const jsonFiles = fs.readdirSync(cacheDir).filter(f => f.endsWith('.json'));
    assert.ok(jsonFiles.length <= 32, `expected <= 32 cache files, got ${jsonFiles.length}`);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState marks interrupted=true when open PLANNER_RESPONSE is followed by USER_INPUT or SYSTEM_MESSAGE', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-interrupted-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: '2026-11-18T10:00:00Z', output_tokens: 200 }),
      JSON.stringify({ step_index: 2, type: 'USER_INPUT', created_at: '2026-11-18T10:01:00Z' }),
      '',
    ].join('\n'));

    let state = await getSessionState(transcriptPath, tempDir);
    assert.deepEqual(state.lastPlannerResponse, {
      startMs: Date.parse('2026-11-18T10:00:00Z'),
      endMs: null,
      outputTokens: 200,
      interrupted: true,
    });

    // Cached incremental read preserves interrupted: true and SYSTEM_MESSAGE also marks interrupted
    fs.appendFileSync(transcriptPath, [
      JSON.stringify({ step_index: 3, type: 'PLANNER_RESPONSE', created_at: '2026-11-18T10:02:00Z', output_tokens: 350 }),
      JSON.stringify({ step_index: 4, type: 'SYSTEM_MESSAGE', created_at: '2026-11-18T10:03:00Z', content: 'notice' }),
      '',
    ].join('\n'));

    state = await getSessionState(transcriptPath, tempDir);
    assert.deepEqual(state.lastPlannerResponse, {
      startMs: Date.parse('2026-11-18T10:02:00Z'),
      endMs: null,
      outputTokens: 350,
      interrupted: true,
    });
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState deduplicates active swarm/subagent speeds per model and rolls chronologically', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-child-speeds-'));
  try {
    const brainDir = path.join(tempDir, 'brain');
    const mainId = '11111111-1111-1111-1111-111111111111';
    const w1Id = 'aaaaaaaa-1111-1111-1111-111111111111';
    const w2Id = 'bbbbbbbb-1111-1111-1111-111111111111';
    const w3Id = 'cccccccc-1111-1111-1111-111111111111';
    const subId = 'dddddddd-1111-1111-1111-111111111111';

    const mainTranscript = path.join(brainDir, mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childPath = (id) => path.join(brainDir, id, '.system_generated', 'logs', 'transcript.jsonl');
    for (const p of [mainTranscript, childPath(w1Id), childPath(w2Id), childPath(w3Id), childPath(subId)]) {
      fs.mkdirSync(path.dirname(p), { recursive: true });
    }

    const baseNow = Date.now();
    const iso = (offsetMs) => new Date(baseNow + offsetMs).toISOString();

    // 3 Hydrogen swarm workers + 1 Flash invoke_subagent
    fs.writeFileSync(mainTranscript, [
      JSON.stringify({ step_index: 1, type: 'USER_INPUT', created_at: iso(-60000) }),
      JSON.stringify({
        step_index: 2,
        type: 'GENERIC',
        created_at: iso(-58000),
        content: JSON.stringify({ status: 'added', conversation_id: w1Id, name: 'w-1', model_display_name: 'Gemini 4 Hydrogen (High)' }),
      }),
      JSON.stringify({
        step_index: 3,
        type: 'GENERIC',
        created_at: iso(-57000),
        content: JSON.stringify({ status: 'added', conversation_id: w2Id, name: 'w-2', model_display_name: 'Gemini 4 Hydrogen (High)' }),
      }),
      JSON.stringify({
        step_index: 4,
        type: 'GENERIC',
        created_at: iso(-56000),
        content: JSON.stringify({ status: 'added', conversation_id: w3Id, name: 'w-3', model_display_name: 'Gemini 4 Hydrogen (High)' }),
      }),
      JSON.stringify({
        step_index: 5,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-55000),
        tool_calls: [{ name: 'invoke_subagent', arguments: { Model: 'flash', Role: 'finder' } }],
      }),
      JSON.stringify({
        step_index: 6,
        type: 'GENERIC',
        created_at: iso(-54000),
        content: `Subagent started {"conversationId":"${subId}"}`,
      }),
      '',
    ].join('\n'));

    // w-1 finishes at -30s: 240 tokens / 2s = 120 tok/s
    fs.writeFileSync(childPath(w1Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-32000), model: 'gemini-4-hydrogen-high', output_tokens: 240 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-30000) }),
      '',
    ].join('\n'));

    // w-2 finishes at -20s: 480 tokens / 2s = 240 tok/s
    fs.writeFileSync(childPath(w2Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-22000), model: 'gemini-4-hydrogen-high', output_tokens: 480 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-20000) }),
      '',
    ].join('\n'));

    // w-3 finishes latest at -10s: 360 tokens / 2s = 180 tok/s
    fs.writeFileSync(childPath(w3Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-12000), model: 'gemini-4-hydrogen-high', output_tokens: 360 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-10000) }),
      '',
    ].join('\n'));

    // Flash subagent finishes at -15s: 620 tokens / 2s = 310 tok/s
    fs.writeFileSync(childPath(subId), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-17000), output_tokens: 620 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-15000) }),
      '',
    ].join('\n'));

    let state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 180 },
      { model: 'Flash', speed: 310 },
    ]);

    // w-1 produces a newer turn at -5s: 520 tokens / 2s = 260 tok/s -> Hydro rolls to 260
    fs.appendFileSync(childPath(w1Id), [
      JSON.stringify({ step_index: 3, type: 'PLANNER_RESPONSE', created_at: iso(-7000), model: 'gemini-4-hydrogen-high', output_tokens: 520 }),
      JSON.stringify({ step_index: 4, type: 'GENERIC', created_at: iso(-5000) }),
      '',
    ].join('\n'));

    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 260 },
      { model: 'Flash', speed: 310 },
    ]);

    // Same-second eventMs tie-breaker via stat.mtimeMs
    fs.writeFileSync(childPath(w2Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-7000), model: 'gemini-4-hydrogen-high', output_tokens: 800 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-5000) }),
      '',
    ].join('\n'));
    const olderMtime = new Date(baseNow - 2000);
    const newerMtime = new Date(baseNow - 500);
    fs.utimesSync(childPath(w1Id), olderMtime, olderMtime);
    fs.utimesSync(childPath(w2Id), newerMtime, newerMtime);

    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 400 },
      { model: 'Flash', speed: 310 },
    ]);

    // Kill all 3 swarm workers -> Hydro and Flash both remain persisted
    fs.appendFileSync(mainTranscript, [
      JSON.stringify({
        step_index: 7,
        type: 'GENERIC',
        created_at: iso(-4000),
        content: JSON.stringify({ status: 'killed', names: ['w-1', 'w-2', 'w-3'] }),
      }),
      '',
    ].join('\n'));

    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 400 },
      { model: 'Flash', speed: 310 },
    ]);

    // A later Hydro worker w-4 produces a newer turn at -2s: 600 tokens / 2s = 300 tok/s
    const w4Id = 'eeeeeeee-1111-1111-1111-111111111111';
    fs.mkdirSync(path.dirname(childPath(w4Id)), { recursive: true });
    fs.writeFileSync(childPath(w4Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-4000), model: 'gemini-4-hydrogen-high', output_tokens: 600 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-2000) }),
      '',
    ].join('\n'));
    fs.appendFileSync(mainTranscript, [
      JSON.stringify({
        step_index: 8,
        type: 'GENERIC',
        created_at: iso(-4500),
        content: JSON.stringify({ status: 'added', conversation_id: w4Id, name: 'w-4', model_display_name: 'Gemini 4 Hydrogen (High)' }),
      }),
      '',
    ].join('\n'));

    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 300 },
      { model: 'Flash', speed: 310 },
    ]);

    // Deleting all child log directories still preserves Hydro & Flash via transcript-cache + disk speed cache
    for (const id of [w1Id, w2Id, w3Id, w4Id, subId]) {
      fs.rmSync(path.join(brainDir, id), { recursive: true, force: true });
    }
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 300 },
      { model: 'Flash', speed: 310 },
    ]);

    // Even if transcript-cache is also removed, _persisted_child_speeds in compaction-cache preserves Hydro and Flash via modelHint
    const tCacheDir = path.join(testDataDir, 'transcript-cache');
    fs.rmSync(tCacheDir, { recursive: true, force: true });
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 300 },
      { model: 'Flash', speed: 310 },
    ]);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState persists childModelSpeeds across SYSTEM_MESSAGE stop, USER_INPUT turns, manage_subagents kill/kill_all, and idle timeout', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-child-lifecycle-'));
  try {
    const brainDir = path.join(tempDir, 'brain');
    const mainId = '22222222-2222-2222-2222-222222222222';
    const w1Id = 'e1111111-1111-1111-1111-111111111111';
    const w2Id = 'e2222222-2222-2222-2222-222222222222';
    const subId = 'e3333333-3333-3333-3333-333333333333';

    const mainTranscript = path.join(brainDir, mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childPath = (id) => path.join(brainDir, id, '.system_generated', 'logs', 'transcript.jsonl');
    for (const p of [mainTranscript, childPath(w1Id), childPath(w2Id), childPath(subId)]) {
      fs.mkdirSync(path.dirname(p), { recursive: true });
    }

    const baseNow = Date.now();
    const iso = (offsetMs) => new Date(baseNow + offsetMs).toISOString();

    fs.writeFileSync(childPath(w1Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-50000), model: 'gemini-4-hydrogen-high', output_tokens: 400 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-48000) }),
      '',
    ].join('\n'));
    fs.writeFileSync(childPath(w2Id), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-45000), model: 'gemini-4-carbon-high', output_tokens: 300 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-43000) }),
      '',
    ].join('\n'));
    fs.writeFileSync(childPath(subId), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-42000), model: 'gemini-3.5-flash', output_tokens: 500 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-40000) }),
      '',
    ].join('\n'));

    // Register w-1 (swarm), w-2 (swarm, later killed), sub (subagent), then SYSTEM_MESSAGE stops all
    fs.writeFileSync(mainTranscript, [
      JSON.stringify({ step_index: 1, type: 'USER_INPUT', created_at: iso(-60000) }),
      JSON.stringify({
        step_index: 2,
        type: 'GENERIC',
        created_at: iso(-55000),
        content: JSON.stringify({
          status: 'added',
          members: [
            { conversation_id: w1Id, name: 'w-1', model_display_name: 'Gemini 4 Hydrogen (High)' },
            { conversation_id: w2Id, name: 'w-2', model_display_name: 'Gemini 4 Carbon (High)' },
          ],
        }),
      }),
      JSON.stringify({
        step_index: 3,
        type: 'GENERIC',
        created_at: iso(-54000),
        content: JSON.stringify({ status: 'killed', name: 'w-2' }),
      }),
      JSON.stringify({
        step_index: 4,
        type: 'GENERIC',
        created_at: iso(-53000),
        content: JSON.stringify({ conversationId: subId }),
      }),
      JSON.stringify({
        step_index: 5,
        type: 'SYSTEM_MESSAGE',
        created_at: iso(-35000),
        content: 'All your subagents and background tasks have been stopped.',
      }),
      '',
    ].join('\n'));

    let state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Carbon', speed: 150 },
      { model: 'Hydro', speed: 200 },
      { model: 'Flash', speed: 250 },
    ]);

    // post_message @w-1 / @all continues, all persisted child speeds stay visible
    fs.appendFileSync(mainTranscript, [
      JSON.stringify({
        step_index: 6,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-30000),
        tool_calls: [{ name: 'post_message', arguments: { text: '@all please continue @w-1' } }],
      }),
      '',
    ].join('\n'));
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Carbon', speed: 150 },
      { model: 'Hydro', speed: 200 },
      { model: 'Flash', speed: 250 },
    ]);

    // New USER_INPUT at -20s and send_message at -15s keep all prior models persisted
    fs.appendFileSync(mainTranscript, [
      JSON.stringify({ step_index: 7, type: 'USER_INPUT', created_at: iso(-20000) }),
      JSON.stringify({
        step_index: 8,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-15000),
        tool_calls: [{ name: 'send_message', arguments: { Recipient: subId, Message: 'resume' } }],
      }),
      '',
    ].join('\n'));
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Carbon', speed: 150 },
      { model: 'Hydro', speed: 200 },
      { model: 'Flash', speed: 250 },
    ]);

    // manage_subagents kill & kill_all also keep persisted speeds intact
    fs.appendFileSync(mainTranscript, [
      JSON.stringify({
        step_index: 9,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-10000),
        tool_calls: [
          { name: 'manage_subagents', arguments: { Action: 'kill', ConversationIds: [subId] } },
          { name: 'manage_subagents', arguments: { Action: 'kill_all' } },
        ],
      }),
      '',
    ].join('\n'));
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Carbon', speed: 150 },
      { model: 'Hydro', speed: 200 },
      { model: 'Flash', speed: 250 },
    ]);

    // Within <= 5 minutes idle window, persisted child speeds remain visible
    // and a newer turn from a child updates its model speed chronologically.
    // Beyond > 5 minutes idle window, expired child speeds are hidden and evicted.
    fs.appendFileSync(childPath(subId), [
      JSON.stringify({ step_index: 3, type: 'PLANNER_RESPONSE', created_at: iso(-7000), model: 'gemini-3.5-flash', output_tokens: 700 }),
      JSON.stringify({ step_index: 4, type: 'GENERIC', created_at: iso(-5000) }),
      '',
    ].join('\n'));
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow + 4 * 60 * 1000 });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Carbon', speed: 150 },
      { model: 'Hydro', speed: 200 },
      { model: 'Flash', speed: 350 },
    ]);

    // At +6 minutes: Carbon (last active -43s), Hydro (-30s), Flash (-5s) all exceed 5 min -> hidden
    state = await getSessionState(mainTranscript, tempDir, { now: baseNow + 6 * 60 * 1000 });
    assert.deepEqual(state.childModelSpeeds, []);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState accumulates totalInputTokens and computes per-turn costBreakdown across model switches, compaction, and killed swarm children', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-cost-tree-'));
  try {
    const brainDir = path.join(tempDir, 'brain');
    const mainId = '33333333-3333-3333-3333-333333333333';
    const killedWorkerId = 'f1111111-1111-1111-1111-111111111111';
    const activeWorkerId = 'f2222222-2222-2222-2222-222222222222';

    const mainTranscript = path.join(brainDir, mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childPath = (id) => path.join(brainDir, id, '.system_generated', 'logs', 'transcript.jsonl');
    for (const p of [mainTranscript, childPath(killedWorkerId), childPath(activeWorkerId)]) {
      fs.mkdirSync(path.dirname(p), { recursive: true });
    }

    // Killed worker used Gemini 4 Hydrogen: 100k in, 500k cacheRead, 20k out -> 0.15 + 0.0375 + 0.15 = $0.3375
    fs.writeFileSync(childPath(killedWorkerId), [
      JSON.stringify({
        step_index: 1,
        type: 'PLANNER_RESPONSE',
        model: 'gemini-4-hydrogen-high',
        input_tokens: 100_000,
        cache_read_tokens: 500_000,
        output_tokens: 20_000,
      }),
      '',
    ].join('\n'));

    // Active worker used Gemini 3.8 Flash: 200k in, 100k cacheRead, 10k out -> 0.15 + 0.0075 + 0.0375 = $0.195
    fs.writeFileSync(childPath(activeWorkerId), [
      JSON.stringify({
        step_index: 1,
        type: 'PLANNER_RESPONSE',
        model: 'gemini-3.8-flash',
        input_tokens: 200_000,
        cache_read_tokens: 100_000,
        output_tokens: 10_000,
      }),
      '',
    ].join('\n'));

    // Main session:
    // Turn 1 (Carbon, input < cacheRead): 2,339 in, 369,996 cacheRead, 3,623 out -> 0.004678 + 0.0369996 + 0.03623 = 0.0779076
    // CHECKPOINT compaction
    // Turn 2 (switches to Claude Opus 5.5): 50,000 in, 100,000 cacheRead, 5,000 out -> 0.20 + 0.02 + 0.10 = 0.32
    // Turn 3 (no model field, inherits lastKnownModel Opus 5.5): 10,000 in, 0 cacheRead, 1,000 out -> 0.04 + 0.02 = 0.06
    fs.writeFileSync(mainTranscript, [
      JSON.stringify({
        step_index: 1,
        type: 'GENERIC',
        content: JSON.stringify({
          status: 'added',
          members: [
            { conversation_id: killedWorkerId, name: 'w-killed', model_display_name: 'Gemini 4 Hydrogen (High)' },
            { conversation_id: activeWorkerId, name: 'w-active', model_display_name: 'Gemini 3.8 Flash (Medium)' },
          ],
        }),
      }),
      JSON.stringify({
        step_index: 2,
        type: 'PLANNER_RESPONSE',
        model: 'gemini-4-carbon-high',
        input_tokens: 2_339,
        cache_read_tokens: 369_996,
        output_tokens: 3_623,
      }),
      JSON.stringify({
        step_index: 3,
        type: 'GENERIC',
        content: JSON.stringify({ status: 'killed', name: 'w-killed' }),
      }),
      JSON.stringify({ step_index: 4, type: 'CHECKPOINT' }),
      JSON.stringify({
        step_index: 5,
        type: 'PLANNER_RESPONSE',
        model: 'claude-opus-5-5',
        input_tokens: 50_000,
        cache_read_tokens: 100_000,
        output_tokens: 5_000,
      }),
      JSON.stringify({
        step_index: 6,
        type: 'PLANNER_RESPONSE',
        input_tokens: 10_000,
        cache_read_tokens: 0,
        output_tokens: 1_000,
      }),
      '',
    ].join('\n'));

    const state = await getSessionState(mainTranscript, tempDir, { roots: [tempDir] });
    assert.equal(state.totalInputTokens, 62_339);
    assert.equal(state.totalOutputTokens, 9_623);
    assert.equal(state.totalCacheReadTokens, 469_996);
    assert.equal(state.usage.total_input_tokens, 62_339);

    assert.ok(state.costBreakdown);
    assert.equal(state.costBreakdown.source, 'transcript');
    assert.equal(state.costBreakdown.childSessionCount, 2);

    const expectedMain = 0.0779076 + 0.32 + 0.06;
    const expectedChildren = 0.3375 + 0.195;
    assert.ok(Math.abs(state.costBreakdown.mainCostUsd - expectedMain) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.childrenCostUsd - expectedChildren) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.totalCostUsd - (expectedMain + expectedChildren)) < 1e-9);

    assert.ok(Math.abs(state.costBreakdown.byModel.Carbon.costUsd - 0.0779076) < 1e-9);
    assert.equal(state.costBreakdown.byModel.Carbon.inputTokens, 2339);
    assert.ok(Math.abs(state.costBreakdown.byModel.Opus.costUsd - 0.38) < 1e-9);
    assert.equal(state.costBreakdown.byModel.Opus.steps, 2);
    assert.ok(Math.abs(state.costBreakdown.byModel.Hydro.costUsd - 0.3375) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.byModel.Flash.costUsd - 0.195) < 1e-9);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState prefers SQLite cost tree and catches up when main transcript has newer steps than DB', async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-db-cost-'));
  try {
    const mainId = '44444444-4444-4444-4444-444444444444';
    const childId = '55555555-5555-5555-5555-555555555555';
    const mainTranscript = path.join(tempRoot, 'brain', mainId, '.system_generated', 'logs', 'transcript.jsonl');
    fs.mkdirSync(path.dirname(mainTranscript), { recursive: true });
    fs.mkdirSync(path.join(tempRoot, 'conversations'), { recursive: true });

    const encodeVarint = (val) => {
      let v = BigInt(val);
      const out = [];
      while (v >= 0x80n) {
        out.push(Number((v & 0x7fn) | 0x80n));
        v >>= 7n;
      }
      out.push(Number(v));
      return Buffer.from(out);
    };
    const encVar = (fn, val) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 0n), encodeVarint(val)]);
    const encBytes = (fn, buf) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 2n), encodeVarint(buf.length), buf]);
    const encUsage = (inp, out, cRead) => encBytes(4, Buffer.concat([
      encVar(2, inp),
      encVar(3, out),
      encVar(5, cRead),
    ]));
    const encChat = (inp, out, cRead, displayName, extraKvs = []) => encBytes(1, Buffer.concat([
      encUsage(inp, out, cRead),
      encBytes(21, Buffer.from(displayName, 'utf8')),
      ...extraKvs.map(([k, v]) => encBytes(20, Buffer.concat([
        encBytes(1, Buffer.from(k, 'utf8')),
        encBytes(2, Buffer.from(v, 'utf8')),
      ]))),
    ]));

    const { DatabaseSync } = require('node:sqlite');
    const mainDb = new DatabaseSync(path.join(tempRoot, 'conversations', `${mainId}.db`));
    try {
      mainDb.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
      // 100k in, 10k out, 500k cacheRead Carbon -> 0.20 + 0.10 + 0.05 = $0.35
      mainDb.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(
        1,
        encChat(100_000, 10_000, 500_000, 'Gemini 4 Carbon (High)')
      );
    } finally {
      mainDb.close();
    }

    const childDb = new DatabaseSync(path.join(tempRoot, 'conversations', `${childId}.db`));
    try {
      childDb.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
      // 1M in, 0 out, 0 cacheRead Hydro -> $1.50
      childDb.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(
        1,
        encChat(1_000_000, 0, 0, 'Gemini 4 Hydrogen (High)', [['parent_cascade_id', mainId]])
      );
    } finally {
      childDb.close();
    }

    // Main transcript initially matches step 1 ($0.35), DB has $0.35 + $1.50 = $1.85
    fs.writeFileSync(mainTranscript, JSON.stringify({
      step_index: 1,
      type: 'PLANNER_RESPONSE',
      model: 'gemini-4-carbon-high',
      input_tokens: 100_000,
      output_tokens: 10_000,
      cache_read_tokens: 500_000,
    }) + '\n');

    let state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot] });
    assert.equal(state.costBreakdown.source, 'db');
    assert.equal(state.costBreakdown.childSessionCount, 1);
    assert.ok(Math.abs(state.costBreakdown.mainCostUsd - 0.35) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.childrenCostUsd - 1.50) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.totalCostUsd - 1.85) < 1e-9);

    // Append newer step 2 to transcript before DB flushes (+$0.20 Carbon) -> main catches up to $0.55, total $2.05
    fs.appendFileSync(mainTranscript, JSON.stringify({
      step_index: 2,
      type: 'PLANNER_RESPONSE',
      model: 'gemini-4-carbon-high',
      input_tokens: 100_000,
      output_tokens: 0,
      cache_read_tokens: 0,
    }) + '\n');

    state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot] });
  assert.ok(Math.abs(state.costBreakdown.mainCostUsd - 0.55) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.totalCostUsd - 2.05) < 1e-9);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test('getSessionState resets single-turn snapshot on CHECKPOINT (/compact) while preserving cumulative ledgers', async () => {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-compact-reset-'));
  try {
    const transcriptPath = path.join(tempDir, 'transcript.jsonl');
    fs.writeFileSync(transcriptPath, [
      JSON.stringify({
        step_index: 1,
        type: 'PLANNER_RESPONSE',
        created_at: '2026-11-18T10:00:00Z',
        model: 'gemini-4-carbon-high',
        context_window: {
          context_window_size: 512000,
          total_input_tokens: 420000,
          total_output_tokens: 5000,
          used_percentage: 82,
          current_usage: {
            input_tokens: 400000,
            output_tokens: 5000,
            cache_read_input_tokens: 350000,
            cache_creation_input_tokens: 10000,
          },
        },
      }),
      // Unclosed PLANNER_RESPONSE before CHECKPOINT -> CHECKPOINT interrupts it (does NOT add 300s idle)
      JSON.stringify({
        step_index: 2,
        type: 'CHECKPOINT',
        created_at: '2026-11-18T10:05:00Z',
      }),
      '',
    ].join('\n'));

    const state = await getSessionState(transcriptPath, tempDir);
    assert.equal(state.compactionCount, 1);
    assert.equal(state.lastTurnUsage, null);
    assert.equal(state.maxHistoricalCache, 0);
    assert.equal(state.usage.current_usage, undefined);
    assert.equal(state.usage.input_tokens, 0);
    assert.equal(state.usage.cache_read_input_tokens, 0);
    assert.equal(state.usage.cache_creation_input_tokens, 0);
    assert.equal(state.usage.context_window_size, 512000);
    assert.equal(state.totalInputTokens, 400000);
    assert.equal(state.totalOutputTokens, 5000);
    assert.equal(state.totalCacheReadTokens, 350000);
    assert.equal(state.totalApiDurationMs, undefined);
    assert.equal(state.lastPlannerResponse.interrupted, true);
    assert.equal(state.lastPlannerResponse.endMs, null);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test('getSessionState reconciles lagging child SQLite DB with newer child transcript cost', async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-child-lag-'));
  try {
    const mainId = '66666666-6666-6666-6666-666666666666';
    const childId = '77777777-7777-7777-7777-777777777777';
    const mainTranscript = path.join(tempRoot, 'brain', mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childTranscript = path.join(tempRoot, 'brain', childId, '.system_generated', 'logs', 'transcript.jsonl');
    fs.mkdirSync(path.dirname(mainTranscript), { recursive: true });
    fs.mkdirSync(path.dirname(childTranscript), { recursive: true });
    fs.mkdirSync(path.join(tempRoot, 'conversations'), { recursive: true });

    const encodeVarint = (val) => {
      let v = BigInt(val);
      const out = [];
      while (v >= 0x80n) {
        out.push(Number((v & 0x7fn) | 0x80n));
        v >>= 7n;
      }
      out.push(Number(v));
      return Buffer.from(out);
    };
    const encVar = (fn, val) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 0n), encodeVarint(val)]);
    const encBytes = (fn, buf) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 2n), encodeVarint(buf.length), buf]);
    const encChat = (inp, out, cRead, displayName, extraKvs = []) => encBytes(1, Buffer.concat([
      encBytes(4, Buffer.concat([encVar(2, inp), encVar(3, out), encVar(5, cRead)])),
      encBytes(21, Buffer.from(displayName, 'utf8')),
      ...extraKvs.map(([k, v]) => encBytes(20, Buffer.concat([
        encBytes(1, Buffer.from(k, 'utf8')),
        encBytes(2, Buffer.from(v, 'utf8')),
      ]))),
    ]));

    const { DatabaseSync } = require('node:sqlite');
    const childDb = new DatabaseSync(path.join(tempRoot, 'conversations', `${childId}.db`));
    try {
      childDb.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
      // DB only flushed 1M input Hydro -> $1.50
      childDb.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(
        1,
        encChat(1_000_000, 0, 0, 'Gemini 4 Hydrogen (High)', [['parent_cascade_id', mainId]])
      );
    } finally {
      childDb.close();
    }

    fs.writeFileSync(mainTranscript, JSON.stringify({
      step_index: 1,
      type: 'GENERIC',
      content: JSON.stringify({ status: 'added', conversation_id: childId, name: 'w-lag', model_display_name: 'Gemini 4 Hydrogen (High)' }),
    }) + '\n');

    // Child transcript already has 2M input Hydro -> $3.00 (ahead of DB's $1.50)
    fs.writeFileSync(childTranscript, [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', model: 'gemini-4-hydrogen-high', input_tokens: 1_000_000, output_tokens: 0, cache_read_tokens: 0 }),
      JSON.stringify({ step_index: 2, type: 'PLANNER_RESPONSE', model: 'gemini-4-hydrogen-high', input_tokens: 1_000_000, output_tokens: 0, cache_read_tokens: 0 }),
      '',
    ].join('\n'));

    const state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot] });
    assert.ok(Math.abs(state.costBreakdown.childrenCostUsd - 3.00) < 1e-9);
    assert.ok(Math.abs(state.costBreakdown.byModel.Hydro.costUsd - 3.00) < 1e-9);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test('readChildTailSpeed skips tool-call duration inflation and resolveChildModelSpeeds prefers modelPerf over stale persisted fallback', async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-child-tools-'));
  try {
    const mainId = '88888888-8888-8888-8888-888888888888';
    const childId = '99999999-9999-9999-9999-999999999999';
    const mainTranscript = path.join(tempRoot, 'brain', mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childTranscript = path.join(tempRoot, 'brain', childId, '.system_generated', 'logs', 'transcript.jsonl');
    fs.mkdirSync(path.dirname(mainTranscript), { recursive: true });
    fs.mkdirSync(path.dirname(childTranscript), { recursive: true });

    const baseNow = Date.now();
    const iso = (offsetMs) => new Date(baseNow + offsetMs).toISOString();

    fs.writeFileSync(mainTranscript, JSON.stringify({
      step_index: 1,
      type: 'GENERIC',
      created_at: iso(-20000),
      content: JSON.stringify({ status: 'added', conversation_id: childId, name: 'tool-worker', model_display_name: 'Gemini 4 Hydrogen (High)' }),
    }) + '\n');

    // Child only has PLANNER_RESPONSE with non-empty tool_calls taking 15s -> must NOT compute 400/15 = 26.7 tok/s!
    fs.writeFileSync(childTranscript, [
      JSON.stringify({
        step_index: 1,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-16000),
        model_display_name: 'Gemini 4 Hydrogen (High)',
        output_tokens: 400,
        tool_calls: [{ name: 'run_command', arguments: { CommandLine: 'sleep 15' } }],
      }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-1000) }),
      '',
    ].join('\n'));

    const noDbState = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot], now: baseNow });
    assert.deepEqual(noDbState.childModelSpeeds, []);

    // With SQLite modelPerf present, child uses pure streaming avgSpeed (240) instead of tool-inflated timing
    const { resolveChildModelSpeeds } = require('../../runtime/parser.js');
    const withPerf = resolveChildModelSpeeds(
      mainTranscript,
      {
        childAgents: {
          [childId]: { id: childId, kind: 'swarm', name: 'tool-worker', modelHint: 'Gemini 4 Hydrogen (High)', addedStep: 1, lastActiveMs: baseNow - 1000 },
        },
        persistedChildSpeeds: {
          Hydro: { model: 'Hydro', speed: 26.7, updatedAt: baseNow - 5000 },
        },
      },
      baseNow,
      { Hydro: { avgSpeed: 240, avgTtftSec: 2.0 } },
      { roots: [tempRoot] }
    );
    assert.deepEqual(withPerf, [{ model: 'Hydro', speed: 240 }]);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test('parseAgyQuota maps openai-*, codex-*, and gpt-* keys to result.openai', () => {
  const parsed = parseAgyQuota({
    'openai-5h': { remaining_fraction: 0.85, reset_time: '2026-11-19T18:00:00Z' },
    'openai-weekly': { remaining_fraction: 0.60, reset_time: '2026-11-25T18:00:00Z' },
  });
  assert.equal(parsed.openai.fiveHour.remainingFraction, 0.85);
  assert.equal(parsed.openai.weekly.remainingFraction, 0.60);

  const codexParsed = parseAgyQuota({
    'codex-5h': { remaining_fraction: 0.5 },
    'gpt-weekly': { remaining_fraction: 0.9 },
  });
  assert.equal(codexParsed.openai.fiveHour.remainingFraction, 0.5);
  assert.equal(codexParsed.openai.weekly.remainingFraction, 0.9);
});

test('getSessionState tracks userTurns and expires childModelSpeeds & persistedChildSpeeds after 30 idle turns', async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-turn-expiry-'));
  const prevDataDir = process.env.AGY_PULSE_DATA_DIR;
  try {
    const testDataDir = path.join(tempRoot, 'data');
    process.env.AGY_PULSE_DATA_DIR = testDataDir;
    const brainDir = path.join(tempRoot, 'brain');
    const mainId = 'aaaa1111-1111-1111-1111-111111111111';
    const flashChildId = 'bbbb2222-2222-2222-2222-222222222222';
    const hydroChildId = 'cccc3333-3333-3333-3333-333333333333';

    const mainTranscript = path.join(brainDir, mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childPath = (id) => path.join(brainDir, id, '.system_generated', 'logs', 'transcript.jsonl');
    for (const p of [mainTranscript, childPath(flashChildId), childPath(hydroChildId)]) {
      fs.mkdirSync(path.dirname(p), { recursive: true });
    }

    const baseNow = Date.now();
    const iso = (offsetMs) => new Date(baseNow + offsetMs).toISOString();

    fs.writeFileSync(childPath(flashChildId), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-50000), model: 'gemini-3.5-flash', output_tokens: 600 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-48000) }),
      '',
    ].join('\n'));
    fs.writeFileSync(childPath(hydroChildId), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-45000), model: 'gemini-4-hydrogen-high', output_tokens: 500 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-43000) }),
      '',
    ].join('\n'));

    // Turn 1: add both Flash and Hydro workers
    const lines = [
      JSON.stringify({ step_index: 1, type: 'USER_INPUT', created_at: iso(-60000) }),
      JSON.stringify({
        step_index: 2,
        type: 'GENERIC',
        created_at: iso(-55000),
        content: JSON.stringify({
          status: 'added',
          members: [
            { conversation_id: flashChildId, name: 'w-flash', model_display_name: 'Gemini 3.5 Flash' },
            { conversation_id: hydroChildId, name: 'w-hydro', model_display_name: 'Gemini 4 Hydrogen (High)' },
          ],
        }),
      }),
    ];

    // Advance 30 more USER_INPUT turns (total userTurns = 31 -> Flash turnsAgo = 30 <= 30)
    // Refresh Hydro via SYSTEM_MESSAGE sender=<hydroChildId> at turn 25
    let step = 3;
    for (let t = 2; t <= 31; t += 1) {
      lines.push(JSON.stringify({ step_index: step++, type: 'USER_INPUT', created_at: iso(-40000 + t * 500) }));
      if (t === 25) {
        lines.push(JSON.stringify({
          step_index: step++,
          type: 'SYSTEM_MESSAGE',
          created_at: iso(-40000 + t * 500 + 100),
          content: `[Message] sender=${hydroChildId} content=ping`,
        }));
      }
    }
    fs.writeFileSync(mainTranscript, lines.join('\n') + '\n');

    let state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot], now: baseNow });
    assert.equal(state.userTurns, 31);
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Flash', speed: 300 },
      { model: 'Hydro', speed: 250 },
    ]);
    assert.equal(state.childModelSpeeds[0].turnsAgo, 30);
    assert.equal(state.childModelSpeeds[1].turnsAgo, 6);

    // Turn 32: Flash turnsAgo becomes 31 (> 30) -> Flash must be hidden and evicted from persistedChildSpeeds!
    fs.appendFileSync(mainTranscript, JSON.stringify({ step_index: step++, type: 'USER_INPUT', created_at: iso(-1000) }) + '\n');
    state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot], now: baseNow });
    assert.equal(state.userTurns, 32);
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 250 },
    ]);
    assert.equal(state.childModelSpeeds[0].turnsAgo, 7);

    // Verify Flash was evicted from transcript cache persistedChildSpeeds
    // (removing child log dir and re-reading still keeps only Hydro)
    fs.rmSync(path.join(brainDir, flashChildId), { recursive: true, force: true });
    state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot], now: baseNow });
    assert.deepEqual(state.childModelSpeeds, [
      { model: 'Hydro', speed: 250 },
    ]);
  } finally {
    if (prevDataDir === undefined) delete process.env.AGY_PULSE_DATA_DIR;
    else process.env.AGY_PULSE_DATA_DIR = prevDataDir;
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test('getSessionState tracks modelLastActiveMs across main, child tool_calls, and evicts >5m expired child speeds', async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-parser-time-expiry-'));
  const prevDataDir = process.env.AGY_PULSE_DATA_DIR;
  try {
    process.env.AGY_PULSE_DATA_DIR = path.join(tempRoot, 'data');
    const brainDir = path.join(tempRoot, 'brain');
    const mainId = '10101010-1010-1010-1010-101010101010';
    const hydroChildId = '20202020-2020-2020-2020-202020202020';
    const flashChildId = '30303030-3030-3030-3030-303030303030';

    const mainTranscript = path.join(brainDir, mainId, '.system_generated', 'logs', 'transcript.jsonl');
    const childPath = (id) => path.join(brainDir, id, '.system_generated', 'logs', 'transcript.jsonl');
    for (const p of [mainTranscript, childPath(hydroChildId), childPath(flashChildId)]) {
      fs.mkdirSync(path.dirname(p), { recursive: true });
    }

    const baseNow = Date.now();
    const iso = (offsetMs) => new Date(baseNow + offsetMs).toISOString();

    // Hydro: closed speed turn at -7m, then tool_calls PLANNER_RESPONSE at -2m (must keep Hydro alive at baseNow + 2m!)
    fs.writeFileSync(childPath(hydroChildId), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-7 * 60 * 1000 - 2000), model: 'gemini-4-hydrogen-high', output_tokens: 400 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-7 * 60 * 1000) }),
      JSON.stringify({
        step_index: 3,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-2 * 60 * 1000),
        model: 'gemini-4-hydrogen-high',
        output_tokens: 150,
        tool_calls: [{ name: 'run_command', arguments: { CommandLine: 'echo hi' } }],
      }),
      '',
    ].join('\n'));

    // Flash: closed speed turn at -7m, no recent activity
    fs.writeFileSync(childPath(flashChildId), [
      JSON.stringify({ step_index: 1, type: 'PLANNER_RESPONSE', created_at: iso(-7 * 60 * 1000 - 2000), model: 'gemini-3.5-flash', output_tokens: 600 }),
      JSON.stringify({ step_index: 2, type: 'GENERIC', created_at: iso(-7 * 60 * 1000) }),
      '',
    ].join('\n'));

    fs.writeFileSync(mainTranscript, [
      JSON.stringify({ step_index: 1, type: 'USER_INPUT', created_at: iso(-8 * 60 * 1000) }),
      JSON.stringify({
        step_index: 2,
        type: 'PLANNER_RESPONSE',
        created_at: iso(-60 * 1000),
        model: 'gemini-4-carbon-high',
        output_tokens: 300,
      }),
      JSON.stringify({
        step_index: 3,
        type: 'GENERIC',
        created_at: iso(-8 * 60 * 1000 + 1000),
        content: JSON.stringify({
          status: 'added',
          members: [
            { conversation_id: hydroChildId, name: 'w-hydro', model_display_name: 'Gemini 4 Hydrogen (High)' },
            { conversation_id: flashChildId, name: 'w-flash', model_display_name: 'Gemini 3.5 Flash' },
          ],
        }),
      }),
      '',
    ].join('\n'));

    let state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot], now: baseNow });
    assert.equal(state.modelLastActiveMs.Carbon, baseNow - 60 * 1000);
    assert.equal(state.modelLastActiveMs.Hydro, baseNow - 2 * 60 * 1000);
    assert.equal(state.modelLastActiveMs.Flash, baseNow - 7 * 60 * 1000);
    assert.deepEqual(state.childModelSpeeds, [{ model: 'Hydro', speed: 200 }]);
    assert.equal(state.childModelSpeeds[0].lastActiveMs, baseNow - 2 * 60 * 1000);

    // At +4m from baseNow, Hydro is 6m idle -> also evicted from persistedChildSpeeds
    fs.rmSync(path.join(brainDir, hydroChildId), { recursive: true, force: true });
    state = await getSessionState(mainTranscript, tempRoot, { roots: [tempRoot], now: baseNow + 4 * 60 * 1000 });
    assert.deepEqual(state.childModelSpeeds, []);
  } finally {
    if (prevDataDir === undefined) delete process.env.AGY_PULSE_DATA_DIR;
    else process.env.AGY_PULSE_DATA_DIR = prevDataDir;
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});


