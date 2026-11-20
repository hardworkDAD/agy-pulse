import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { DatabaseSync } = require('node:sqlite');
const {
  parseStatsArgs,
  renderStatsText,
  runStatsCli,
} = require('../../runtime/stats.js');
const { stripAnsi } = require('../../runtime/renderer/format.js');

function encodeVarint(value) {
  let v = typeof value === 'bigint' ? value : BigInt(value);
  const bytes = [];
  while (v >= 0x80n) {
    bytes.push(Number((v & 0x7fn) | 0x80n));
    v >>= 7n;
  }
  bytes.push(Number(v));
  return Buffer.from(bytes);
}

function encodeVarintField(fieldNum, value) {
  return Buffer.concat([
    encodeVarint((BigInt(fieldNum) << 3n) | 0n),
    encodeVarint(value),
  ]);
}

function encodeBytesField(fieldNum, payload) {
  return Buffer.concat([
    encodeVarint((BigInt(fieldNum) << 3n) | 2n),
    encodeVarint(payload.length),
    payload,
  ]);
}

function encodeStringField(fieldNum, str) {
  return encodeBytesField(fieldNum, Buffer.from(String(str), 'utf8'));
}

function buildStepTimestampBlob(ms) {
  const sec = Math.floor(ms / 1000);
  return encodeBytesField(1, encodeVarintField(1, sec));
}

function buildTrajectoryBlob(workspaceUri, branch, ms) {
  const ws = Buffer.concat([encodeStringField(1, workspaceUri), encodeStringField(4, branch)]);
  return Buffer.concat([
    encodeBytesField(1, ws),
    encodeBytesField(2, encodeVarintField(1, Math.floor(ms / 1000))),
  ]);
}

function buildGenBlob(stepIndex, displayName, inputTokens, cacheReadTokens, outputTokens, streamSec, ttftSec) {
  const usage = Buffer.concat([
    encodeVarintField(2, inputTokens),
    encodeVarintField(3, outputTokens),
    encodeVarintField(5, cacheReadTokens),
  ]);
  const f1 = Buffer.concat([
    encodeStringField(21, displayName),
    encodeBytesField(4, usage),
    encodeBytesField(11, encodeVarintField(1, ttftSec)),
    encodeBytesField(12, encodeVarintField(1, streamSec)),
  ]);
  return Buffer.concat([
    encodeBytesField(1, f1),
    encodeBytesField(2, encodeVarint(stepIndex)),
  ]);
}

let tempDir;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-stats-test-'));
});

afterEach(() => {
  fs.rmSync(tempDir, { recursive: true, force: true });
});

test('parseStatsArgs parses flags, inline values, shortcuts, and defaults accurately', () => {
  assert.deepEqual(parseStatsArgs([]), {
    range: '7d',
    from: null,
    to: null,
    granularity: 'auto',
    source: 'all',
    model: 'all',
    workspace: 'all',
    includeSubagents: true,
    format: 'table',
    csvKind: 'timeseries',
    lang: 'auto',
    noSync: false,
    help: false,
  });

  const parsed = parseStatsArgs([
    '--stats',
    '--range', '30d',
    '--granularity=day',
    '--source', 'IDE',
    '--model', 'Carbon',
    '--workspace=agy-pulse',
    '--no-subagents',
    '--csv=models',
    '--lang=zh',
    '--no-sync',
  ]);
  assert.deepEqual(parsed, {
    range: '30d',
    from: null,
    to: null,
    granularity: 'day',
    source: 'ide',
    model: 'Carbon',
    workspace: 'agy-pulse',
    includeSubagents: false,
    format: 'csv',
    csvKind: 'models',
    lang: 'zh',
    noSync: true,
    help: false,
  });

  assert.equal(parseStatsArgs(['--source=cli']).source, 'cli');
  assert.equal(parseStatsArgs(['--source', 'IDE']).source, 'ide');
  assert.equal(parseStatsArgs(['--source', 'invalid']).source, 'all');
  assert.equal(parseStatsArgs(['--source=unknown']).source, 'all');

  const customRange = parseStatsArgs(['--from', '2026-11-01', '--to=2026-11-10', '--json']);
  assert.equal(customRange.range, 'custom');
  assert.equal(customRange.from, '2026-11-01');
  assert.equal(customRange.to, '2026-11-10');
  assert.equal(customRange.format, 'json');

  assert.equal(parseStatsArgs(['-h']).help, true);
  assert.equal(parseStatsArgs(['--today']).range, 'today');
  assert.equal(parseStatsArgs(['--stats', 'all']).range, 'all');
  assert.equal(parseStatsArgs(['--stats', 'today']).range, 'today');
  assert.equal(parseStatsArgs(['--stats', '24h']).range, '24h');
  assert.equal(parseStatsArgs(['--stats', '30d']).range, '30d');
  assert.equal(parseStatsArgs(['--stats', 'month']).range, 'month');
  // '--model all' must NOT overwrite range with 'all'
  const modelAll = parseStatsArgs(['--stats', '--model', 'all']);
  assert.equal(modelAll.model, 'all');
  assert.equal(modelAll.range, '7d');

  // Short subcommands ('stats' / 'report') behave identically to '--stats'
  assert.deepEqual(parseStatsArgs(['stats', 'today', '--json']), parseStatsArgs(['--stats', 'today', '--json']));
  assert.deepEqual(parseStatsArgs(['report', '30d', '--model', 'stats']), parseStatsArgs(['--stats', '30d', '--model', 'stats']));
});

test('renderStatsText renders English, Chinese, ANSI/plain, and empty states cleanly', () => {
  const sampleResult = {
    meta: {
      range: '7d',
      fromMs: Date.UTC(2026, 10, 13),
      toMs: Date.UTC(2026, 10, 20),
      granularity: 'day',
      backend: 'sqlite',
      totalRecordedSteps: 12,
      totalRecordedSessions: 3,
    },
    summary: {
      totalCostUsd: 1.25,
      mainCostUsd: 1.0,
      subagentCostUsd: 0.25,
      totalSteps: 12,
      rootSessionCount: 2,
      subagentSessionCount: 1,
      inputTokens: 100_000,
      cacheReadTokens: 900_000,
      cacheWriteTokens: 0,
      outputTokens: 50_000,
      totalPromptTokens: 1_000_000,
      totalTokens: 1_050_000,
      cacheHitRate: 90,
      avgSpeed: 125.5,
      avgTtftSec: 1.4,
      streamSamples: 10,
      ttftSamples: 10,
    },
    byModel: [
      {
        model: 'Carbon',
        costUsd: 1.25,
        costSharePct: 100,
        steps: 12,
        inputTokens: 100_000,
        cacheReadTokens: 900_000,
        cacheWriteTokens: 0,
        outputTokens: 50_000,
        totalPromptTokens: 1_000_000,
        cacheHitRate: 90,
        avgSpeed: 125.5,
        avgTtftSec: 1.4,
      },
    ],
    timeseries: [
      {
        bucketKey: '2026-11-20',
        bucketStartMs: Date.UTC(2026, 10, 20),
        label: '11-20',
        costUsd: 1.25,
        steps: 12,
        cacheHitRate: 90,
      },
    ],
    sessions: [
      {
        rootConversationId: 'root-session-1234',
        workspace: '/home/jason/developing/agy-pulse',
        workspaceLabel: 'agy-pulse',
        branch: 'main',
        totalCostUsd: 1.25,
        subagentCount: 1,
        steps: 12,
        cacheHitRate: 90,
        models: ['Carbon'],
        lastActivityMs: Date.UTC(2026, 10, 20, 12, 0),
      },
    ],
  };

  const zhColored = renderStatsText(sampleResult, { lang: 'zh', color: true });
  assert.match(zhColored, /\x1b\[/);
  const zhPlain = stripAnsi(zhColored);
  assert.match(zhPlain, /agy-pulse 遥测统计报表/);
  assert.match(zhPlain, /核心指标总览/);
  assert.match(zhPlain, /模型明细/);
  assert.match(zhPlain, /Carbon/);
  assert.match(zhPlain, /agy-pulse/);
  assert.match(zhPlain, /90\.0%/);

  const enPlain = renderStatsText(sampleResult, { lang: 'en', color: false });
  assert.equal(enPlain.includes('\x1b['), false);
  assert.match(enPlain, /agy-pulse Telemetry Stats/);
  assert.match(enPlain, /Summary KPIs/);
  assert.match(enPlain, /Top 10 Sessions/);

  const emptyZh = renderStatsText({ meta: { range: 'today' }, summary: { totalSteps: 0 } }, { lang: 'zh', color: false });
  assert.match(emptyZh, /暂无遥测记录/);

  const multiSourceResult = {
    ...sampleResult,
    meta: { ...sampleResult.meta, source: 'all' },
    bySource: [
      { source: 'cli', costUsd: 0.75, costSharePct: 60.0, steps: 8, sessionCount: 1 },
      { source: 'ide', costUsd: 0.50, costSharePct: 40.0, steps: 4, sessionCount: 1 },
    ],
    sessions: [
      { ...sampleResult.sessions[0], source: 'cli' },
      {
        rootConversationId: 'ide-session-5678',
        source: 'ide',
        workspace: '/home/jason/developing/ide-proj',
        workspaceLabel: 'ide-proj',
        branch: 'feat/ide',
        totalCostUsd: 0.50,
        subagentCount: 0,
        steps: 4,
        cacheHitRate: 85,
        models: ['Hydro'],
        lastActivityMs: Date.UTC(2026, 10, 20, 11, 0),
      },
    ],
  };
  const multiEn = renderStatsText(multiSourceResult, { lang: 'en', color: false });
  assert.match(multiEn, /Sources\s*: CLI \$0\.750 \(60\.0%\) · IDE \$0\.500 \(40\.0%\)/);
  assert.match(multiEn, /\[CLI\] agy-pulse/);
  assert.match(multiEn, /\[IDE\] ide-proj/);

  const filteredIdeZh = renderStatsText({
    ...multiSourceResult,
    meta: { ...sampleResult.meta, source: 'ide' },
    bySource: [{ source: 'ide', costUsd: 0.50, costSharePct: 100, steps: 4, sessionCount: 1 }],
    sessions: [multiSourceResult.sessions[1]],
  }, { lang: 'zh', color: false });
  assert.match(filteredIdeZh, /来源: IDE/);
  assert.match(filteredIdeZh, /来源分布\s*: IDE \$0\.500 \(100\.0%\)/);
  assert.match(filteredIdeZh, /\[IDE\] ide-proj/);
});

test('runStatsCli syncs real conversation DB and outputs table, JSON, CSV, and help', async () => {
  const root = path.join(tempDir, 'agy-cli-root');
  const convDir = path.join(root, 'conversations');
  fs.mkdirSync(convDir, { recursive: true });
  const now = Date.now();
  const dbPath = path.join(convDir, 'cli-sess-1.db');
  const db = new DatabaseSync(dbPath);
  try {
    db.exec('CREATE TABLE steps (idx INTEGER PRIMARY KEY, metadata BLOB);');
    db.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    db.exec('CREATE TABLE trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);');
    db.prepare('INSERT INTO steps (idx, metadata) VALUES (?, ?)').run(1, buildStepTimestampBlob(now - 60_000));
    db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(
      1,
      buildGenBlob(1, 'Gemini 4 Carbon (High)', 10_000, 90_000, 2_000, 10, 2)
    );
    db.prepare("INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?)").run(
      buildTrajectoryBlob('file:///workspace/demo-app', 'feat/stats', now - 120_000)
    );
  } finally {
    db.close();
  }

  let outHelp = '';
  const helpRes = await runStatsCli(['--stats', '--help'], {
    lang: 'en',
    env: { LANG: 'en_US.UTF-8' },
    write: (s) => { outHelp += s; },
  });
  assert.equal(helpRes.exitCode, 0);
  assert.match(outHelp, /--stats/);
  assert.match(outHelp, /--range/);
  assert.match(outHelp, /--source/);

  let outTable = '';
  const tableRes = await runStatsCli(['--stats', '--range=7d', '--lang=en'], {
    roots: [root],
    color: false,
    now,
    write: (s) => { outTable += s; },
  });
  assert.equal(tableRes.exitCode, 0);
  assert.match(outTable, /Carbon/);
  assert.match(outTable, /demo-app/);
  assert.match(outTable, /90\.0%/);

  let outJson = '';
  await runStatsCli(['--stats', '--json', '--no-sync'], {
    roots: [root],
    now,
    write: (s) => { outJson += s; },
  });
  const parsed = JSON.parse(outJson);
  assert.equal(parsed.summary.totalSteps, 1);
  assert.equal(parsed.byModel[0].model, 'Carbon');

  let outCsv = '';
  await runStatsCli(['--stats', '--csv=models', '--no-sync'], {
    roots: [root],
    now,
    write: (s) => { outCsv += s; },
  });
  assert.match(outCsv, /^model,costUsd,costSharePct,/);
  assert.match(outCsv, /Carbon/);
});

test('runStatsCli merges CLI roots and IDE ideRoots and filters by --source all|cli|ide', async () => {
  const cliRoot = path.join(tempDir, 'agy-cli-root');
  const ideRoot = path.join(tempDir, 'agy-ide-root');
  fs.mkdirSync(path.join(cliRoot, 'conversations'), { recursive: true });
  fs.mkdirSync(path.join(ideRoot, 'conversations'), { recursive: true });
  const now = Date.now();

  const createConvDb = (dir, id, workspaceUri, modelName, stepIdx) => {
    const db = new DatabaseSync(path.join(dir, 'conversations', `${id}.db`));
    try {
      db.exec('CREATE TABLE steps (idx INTEGER PRIMARY KEY, metadata BLOB);');
      db.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
      db.exec('CREATE TABLE trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);');
      db.prepare('INSERT INTO steps (idx, metadata) VALUES (?, ?)').run(stepIdx, buildStepTimestampBlob(now - 60_000));
      db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(
        stepIdx,
        buildGenBlob(stepIdx, modelName, 10_000, 90_000, 2_000, 10, 2)
      );
      db.prepare("INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?)").run(
        buildTrajectoryBlob(workspaceUri, 'main', now - 120_000)
      );
    } finally {
      db.close();
    }
  };

  createConvDb(cliRoot, 'cli-conv-1', 'file:///workspace/cli-app', 'Gemini 4 Carbon (High)', 1);
  createConvDb(ideRoot, 'ide-conv-1', 'file:///workspace/ide-app', 'Gemini 4 Hydrogen (Medium)', 1);

  const ledgerOpts = { roots: [cliRoot], ideRoots: [ideRoot] };

  let outAllText = '';
  await runStatsCli(['--stats', '--range=7d', '--lang=en'], {
    ...ledgerOpts,
    color: false,
    now,
    write: (s) => { outAllText += s; },
  });
  assert.match(outAllText, /Sources\s*:/);
  assert.match(outAllText, /\[CLI\] cli-app/);
  assert.match(outAllText, /\[IDE\] ide-app/);

  let outIdeJson = '';
  await runStatsCli(['--stats', '--source=ide', '--json', '--no-sync'], {
    ...ledgerOpts,
    now,
    write: (s) => { outIdeJson += s; },
  });
  const parsedIde = JSON.parse(outIdeJson);
  assert.equal(parsedIde.meta.source, 'ide');
  assert.deepEqual(parsedIde.meta.availableSources, ['cli', 'ide']);
  assert.equal(parsedIde.summary.totalSteps, 1);
  assert.equal(parsedIde.sessions.length, 1);
  assert.equal(parsedIde.sessions[0].source, 'ide');
  assert.equal(parsedIde.sessions[0].workspaceLabel, 'ide-app');

  let outCliCsv = '';
  await runStatsCli(['--stats', '--source', 'cli', '--csv=sessions', '--no-sync'], {
    ...ledgerOpts,
    now,
    write: (s) => { outCliCsv += s; },
  });
  assert.match(outCliCsv, /^rootConversationId,source,workspace,/);
  assert.match(outCliCsv, /cli-conv-1,cli,/);
  assert.doesNotMatch(outCliCsv, /ide-conv-1/);
});
