import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { DatabaseSync } = require('node:sqlite');
const {
  DEFAULT_LEDGER_FILENAME,
  FALLBACK_LEDGER_FILENAME,
  SYNC_THROTTLE_MS,
  normalizeSource,
  normalizeSourceFilter,
  resolveLedgerSourceRoots,
  listAllConversationDbs,
  resolveLedgerPath,
  resolveTimeRange,
  syncSingleConversationToLedger,
  syncAllConversationsToLedger,
  queryLedger,
  formatLedgerCsv,
  extractStepTimestampMs,
  extractTrajectoryMeta,
  extractStepIndicesFromGenMetadata,
} = require('../../runtime/ledger.js');

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

function encodeTimestampMsg(ms) {
  const sec = Math.floor(ms / 1000);
  const nanos = (ms % 1000) * 1_000_000;
  return Buffer.concat([
    encodeVarintField(1, sec),
    ...(nanos > 0 ? [encodeVarintField(2, nanos)] : []),
  ]);
}

function encodeDurationMsg(secFloat) {
  const wholeSec = Math.floor(secFloat);
  const nanos = Math.round((secFloat - wholeSec) * 1_000_000_000);
  return Buffer.concat([
    encodeVarintField(1, wholeSec),
    ...(nanos > 0 ? [encodeVarintField(2, nanos)] : []),
  ]);
}

function encodePackedVarints(values) {
  return Buffer.concat(values.map(v => encodeVarint(v)));
}

function buildStepMetadataBlob({ tsMs, botUuid = null } = {}) {
  const parts = [];
  if (tsMs) {
    parts.push(encodeBytesField(1, encodeTimestampMsg(tsMs)));
  }
  if (botUuid) {
    parts.push(encodeBytesField(9, encodeStringField(7, botUuid)));
  }
  return Buffer.concat(parts);
}

function buildTrajectoryMetaBlob({ workspaceUri = null, gitRootUri = null, branch = null, createdAtMs = null, cascadeId = null, fallbackPathUri = null } = {}) {
  const parts = [];
  if (workspaceUri || gitRootUri || branch) {
    const wsParts = [];
    if (workspaceUri) wsParts.push(encodeStringField(1, workspaceUri));
    if (gitRootUri) wsParts.push(encodeStringField(2, gitRootUri));
    if (branch) wsParts.push(encodeStringField(4, branch));
    parts.push(encodeBytesField(1, Buffer.concat(wsParts)));
  }
  if (createdAtMs) {
    parts.push(encodeBytesField(2, encodeTimestampMsg(createdAtMs)));
  }
  if (cascadeId) {
    parts.push(encodeStringField(6, cascadeId));
  }
  if (fallbackPathUri) {
    parts.push(encodeStringField(7, fallbackPathUri));
  }
  return Buffer.concat(parts);
}

function buildGenMetadataBlob({
  stepIndices = [],
  entries = [],
} = {}) {
  const topParts = [];
  for (const e of entries) {
    const f1Parts = [];
    if (e.displayName) f1Parts.push(encodeStringField(21, e.displayName));
    if (e.rawModel) f1Parts.push(encodeStringField(19, e.rawModel));

    const usageParts = [];
    if (e.inputTokens) usageParts.push(encodeVarintField(2, e.inputTokens));
    if (e.outputTokens) usageParts.push(encodeVarintField(3, e.outputTokens));
    if (e.cacheReadTokens) usageParts.push(encodeVarintField(5, e.cacheReadTokens));
    if (e.botUuid) usageParts.push(encodeStringField(7, e.botUuid));
    if (usageParts.length > 0) {
      f1Parts.push(encodeBytesField(4, Buffer.concat(usageParts)));
    }

    if (e.ttftSec !== undefined && e.ttftSec !== null) {
      f1Parts.push(encodeBytesField(11, encodeDurationMsg(e.ttftSec)));
    }
    if (e.streamSec !== undefined && e.streamSec !== null) {
      f1Parts.push(encodeBytesField(12, encodeDurationMsg(e.streamSec)));
    }

    const kvs = [];
    if (e.modelEnum) kvs.push(['model_enum', e.modelEnum]);
    if (e.parentCascadeId) kvs.push(['parent_cascade_id', e.parentCascadeId]);
    if (e.rootCascadeId) kvs.push(['root_cascade_id', e.rootCascadeId]);
    if (e.cascadeId) kvs.push(['cascade_id', e.cascadeId]);
    if (e.lastStepIndex !== undefined && e.lastStepIndex !== null) {
      kvs.push(['last_step_index', String(e.lastStepIndex)]);
    }
    for (const [k, v] of kvs) {
      const kv = Buffer.concat([encodeStringField(1, k), encodeStringField(2, v)]);
      f1Parts.push(encodeBytesField(20, kv));
    }
    topParts.push(encodeBytesField(1, Buffer.concat(f1Parts)));
  }

  if (stepIndices.length > 0) {
    topParts.push(encodeBytesField(2, encodePackedVarints(stepIndices)));
  }

  return Buffer.concat(topParts);
}

function createConvDb(dbPath, { steps = [], genRows = [], trajectoryBlob = null, walMode = false } = {}) {
  fs.mkdirSync(path.dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    if (walMode) db.exec('PRAGMA journal_mode = WAL;');
    db.exec('CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);');
    db.exec('CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    db.exec('CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);');

    const insStep = db.prepare('INSERT INTO steps (idx, metadata) VALUES (?, ?)');
    for (const s of steps) {
      insStep.run(s.idx, s.metadata);
    }

    const insGen = db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)');
    for (const g of genRows) {
      insGen.run(g.idx, g.data);
    }

    if (trajectoryBlob) {
      db.prepare("INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?)").run(trajectoryBlob);
    }
  } finally {
    db.close();
  }
}

let tempDir;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-ledger-test-'));
});

afterEach(() => {
  fs.rmSync(tempDir, { recursive: true, force: true });
});

test('protobuf decoders decode steps timestamp, trajectory metadata, and packed/unpacked step_indices', () => {
  const tsMs = Date.UTC(2026, 10, 20, 8, 30, 15, 456);
  const stepBuf = buildStepMetadataBlob({ tsMs, botUuid: 'bot-123' });
  assert.equal(extractStepTimestampMs(new Uint8Array(stepBuf)), tsMs);
  assert.equal(extractStepTimestampMs(Buffer.alloc(0)), null);
  assert.equal(extractStepTimestampMs(null), null);

  const trajBuf = buildTrajectoryMetaBlob({
    workspaceUri: 'file:///home/jason/my%20repo/agy-pulse/',
    branch: 'feat/agy-pulse-unified',
    createdAtMs: tsMs - 5000,
    cascadeId: 'conv-abc-123',
  });
  assert.deepEqual(extractTrajectoryMeta(trajBuf), {
    workspace: '/home/jason/my repo/agy-pulse',
    branch: 'feat/agy-pulse-unified',
    createdAtMs: tsMs - 5000,
    cascadeId: 'conv-abc-123',
  });

  // Fallback to top-level tag 7 when tag 1 is absent
  const trajFallback = buildTrajectoryMetaBlob({
    fallbackPathUri: 'file:///C:/Users/dev/project%20one',
  });
  assert.equal(extractTrajectoryMeta(trajFallback).workspace, 'C:/Users/dev/project one');

  // Packed varints matching real-world examples: [1,2,3,4] and [700,701,702]
  const packedSmall = Buffer.concat([encodeBytesField(2, Buffer.from('01020304', 'hex'))]);
  assert.deepEqual(extractStepIndicesFromGenMetadata(packedSmall), [1, 2, 3, 4]);

  const packedLarge = Buffer.concat([encodeBytesField(2, Buffer.from('bc05bd05be05', 'hex'))]);
  assert.deepEqual(extractStepIndicesFromGenMetadata(packedLarge), [700, 701, 702]);

  // Unpacked repeated tag 2 varints
  const unpacked = Buffer.concat([encodeVarintField(2, 10), encodeVarintField(2, 11)]);
  assert.deepEqual(extractStepIndicesFromGenMetadata(unpacked), [10, 11]);
});

test('resolveLedgerPath and resolveTimeRange honor env, backend fallback, and local day expansion', () => {
  const customRoot = path.join(tempDir, 'env-root');
  const env = { AGY_PULSE_DATA_DIR: customRoot };
  assert.equal(resolveLedgerPath({ env }), path.join(customRoot, DEFAULT_LEDGER_FILENAME));
  assert.equal(
    resolveLedgerPath({ env, sqliteModule: null }),
    path.join(customRoot, FALLBACK_LEDGER_FILENAME)
  );
  assert.equal(
    resolveLedgerPath({ ledgerPath: path.join(tempDir, 'x.sqlite'), sqliteModule: null }),
    path.join(tempDir, 'x.json')
  );

  const now = new Date(2026, 10, 20, 15, 30, 0, 0).getTime();
  const today = resolveTimeRange({ range: 'today', now });
  assert.equal(today.range, 'today');
  assert.equal(today.fromMs, new Date(2026, 10, 20, 0, 0, 0, 0).getTime());
  assert.equal(today.toMs, new Date(2026, 10, 20, 23, 59, 59, 999).getTime());

  const custom = resolveTimeRange({ from: '2026-11-01', to: '2026-11-05', now });
  assert.equal(custom.range, 'custom');
  assert.equal(custom.fromMs, new Date(2026, 10, 1, 0, 0, 0, 0).getTime());
  assert.equal(custom.toMs, new Date(2026, 10, 5, 23, 59, 59, 999).getTime());
});

test('syncSingleConversationToLedger throttles (15s), captures uncheckpointed WAL, and handles timestamp fallbacks', () => {
  const root = path.join(tempDir, 'agy-data');
  const convDir = path.join(root, 'conversations');
  fs.mkdirSync(convDir, { recursive: true });
  const convId = 'conv-wal-test';
  const dbPath = path.join(convDir, `${convId}.db`);

  const baseMs = new Date(2026, 10, 20, 10, 0, 0, 0).getTime();
  const step1Ts = baseMs + 10_000;
  const step2Ts = baseMs + 20_000;
  const step3Ts = baseMs + 30_000;
  const trajCreatedMs = baseMs + 1_000;

  // Create DB with WAL mode and 4 timestamp fallback paths
  createConvDb(dbPath, {
    walMode: true,
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///home/jason/developing/agy-pulse',
      branch: 'main',
      createdAtMs: trajCreatedMs,
      cascadeId: convId,
    }),
    steps: [
      { idx: 1, metadata: buildStepMetadataBlob({ tsMs: step1Ts }) },
      { idx: 2, metadata: buildStepMetadataBlob({ tsMs: step2Ts }) },
      { idx: 99, metadata: buildStepMetadataBlob({ tsMs: step3Ts, botUuid: 'bot-fallback-99' }) },
    ],
    genRows: [
      {
        idx: 1,
        // Resolves via tag 2 step_indices -> step 1
        data: buildGenMetadataBlob({
          stepIndices: [1],
          entries: [{
            displayName: 'Gemini 4 Carbon (High)',
            inputTokens: 2000,
            cacheReadTokens: 100000,
            outputTokens: 1000,
            streamSec: 5,
            ttftSec: 2,
          }],
        }),
      },
      {
        idx: 2,
        // Resolves via f1.tag 20['last_step_index'] -> step 2
        data: buildGenMetadataBlob({
          entries: [{
            displayName: 'Gemini 4 Hydrogen (High)',
            inputTokens: 1000,
            outputTokens: 500,
            streamSec: 2,
            ttftSec: 1,
            lastStepIndex: 2,
          }],
        }),
      },
      {
        idx: 3,
        // Resolves via botUuid -> step 99
        data: buildGenMetadataBlob({
          entries: [{
            displayName: 'Gemini 4 Neon (High)',
            inputTokens: 1000,
            outputTokens: 400,
            botUuid: 'bot-fallback-99',
          }],
        }),
      },
      {
        idx: 4,
        // Resolves via trajectoryCreatedAtMs
        data: buildGenMetadataBlob({
          entries: [{
            displayName: 'Claude Opus 5.5',
            inputTokens: 500,
            outputTokens: 200,
          }],
        }),
      },
    ],
  });

  // First sync at T0
  const sync1 = syncSingleConversationToLedger(convId, { roots: [root], now: baseMs + 60_000 });
  assert.equal(sync1.ok, true);
  assert.equal(sync1.skipped, false);
  assert.equal(sync1.syncedDbs, 1);
  assert.equal(sync1.stepsUpserted, 4);

  // Second sync within 15s with unchanged sig -> throttled skip
  const sync2 = syncSingleConversationToLedger(convId, { roots: [root], now: baseMs + 60_000 + SYNC_THROTTLE_MS - 1000 });
  assert.equal(sync2.ok, true);
  assert.equal(sync2.skipped, true);
  assert.equal(sync2.syncedDbs, 0);

  // Open an active connection, insert an un-checkpointed WAL row while keeping connection open!
  const writerDb = new DatabaseSync(dbPath);
  try {
    const step4Ts = baseMs + 40_000;
    writerDb.prepare('INSERT INTO steps (idx, metadata) VALUES (?, ?)').run(4, buildStepMetadataBlob({ tsMs: step4Ts }));
    writerDb.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(
      5,
      buildGenMetadataBlob({
        stepIndices: [4],
        entries: [{
          displayName: 'Gemini 4 Carbon (High)',
          inputTokens: 3000,
          outputTokens: 1000,
          streamSec: 4,
          ttftSec: 1.5,
        }],
      })
    );

    assert.ok(fs.existsSync(`${dbPath}-wal`) && fs.statSync(`${dbPath}-wal`).size > 0);

    // Because sig changed on disk (-wal grew), syncSingleConversationToLedger picks up the WAL row
    const sync3 = syncSingleConversationToLedger(convId, { roots: [root], now: baseMs + 65_000 });
    assert.equal(sync3.ok, true);
    assert.equal(sync3.skipped, false);
    assert.equal(sync3.stepsUpserted, 5);
  } finally {
    writerDb.close();
  }

  const res = queryLedger({ range: 'all', now: baseMs + 100_000 }, { roots: [root] });
  assert.equal(res.summary.totalSteps, 5);
  assert.equal(res.sessions.length, 1);
  assert.equal(res.sessions[0].workspace, '/home/jason/developing/agy-pulse');
  assert.equal(res.sessions[0].workspaceLabel, 'agy-pulse');
  assert.equal(res.sessions[0].branch, 'main');
  assert.equal(res.sessions[0].startedAtMs, trajCreatedMs);
  assert.equal(res.sessions[0].lastActivityMs, baseMs + 40_000);
});

test('syncAllConversationsToLedger is incremental, preserves history after source DB deletion, and reads immutable DBs without creating sidecars', () => {
  const root = path.join(tempDir, 'agy-persist');
  const convDir = path.join(root, 'conversations');
  const db1 = path.join(convDir, 'sess-1.db');
  const db2 = path.join(convDir, 'sess-2.db');
  const t0 = new Date(2026, 10, 18, 12, 0, 0, 0).getTime();

  createConvDb(db1, {
    trajectoryBlob: buildTrajectoryMetaBlob({ workspaceUri: 'file:///work/alpha', branch: 'main', createdAtMs: t0 }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: t0 }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Gemini 4 Carbon (High)', inputTokens: 1_000_000, outputTokens: 100_000, streamSec: 100, ttftSec: 2 }],
      }),
    }],
  });
  createConvDb(db2, {
    trajectoryBlob: buildTrajectoryMetaBlob({ workspaceUri: 'file:///work/beta', branch: 'dev', createdAtMs: t0 + HOUR_MS }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: t0 + HOUR_MS }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Gemini 4 Hydrogen (High)', inputTokens: 1_000_000, outputTokens: 100_000, streamSec: 50, ttftSec: 1 }],
      }),
    }],
  });

  const cold = syncAllConversationsToLedger({ roots: [root], now: t0 + 2 * HOUR_MS });
  assert.equal(cold.ok, true);
  assert.equal(cold.totalDbs, 2);
  assert.equal(cold.newlySyncedDbs, 2);
  assert.equal(cold.stepsUpserted, 2);
  // Immutable open must not leave -wal/-shm sidecars on source DBs
  assert.equal(fs.existsSync(`${db1}-wal`), false);
  assert.equal(fs.existsSync(`${db1}-shm`), false);

  const warm = syncAllConversationsToLedger({ roots: [root], now: t0 + 3 * HOUR_MS });
  assert.equal(warm.newlySyncedDbs, 0);
  assert.equal(warm.stepsUpserted, 0);

  // Delete source DB 1 completely -> ledger MUST retain all recorded data permanently!
  fs.unlinkSync(db1);
  const afterDeleteSync = syncAllConversationsToLedger({ roots: [root], now: t0 + 4 * HOUR_MS });
  assert.equal(afterDeleteSync.totalDbs, 1);

  const q = queryLedger({ range: 'all', now: t0 + 4 * HOUR_MS }, { roots: [root] });
  assert.equal(q.summary.totalSteps, 2);
  assert.equal(q.summary.rootSessionCount, 2);
  assert.equal(q.meta.availableWorkspaces.length, 2);
});

const HOUR_MS = 60 * 60 * 1000;
const DAY_MS = 24 * HOUR_MS;

test('queryLedger resolves 2-level subagent trees, spike filtering, granularities, model/workspace filters, JSON fallback, and CSV export', () => {
  const root = path.join(tempDir, 'agy-agg');
  const convDir = path.join(root, 'conversations');

  const now = new Date(2026, 10, 20, 14, 0, 0, 0).getTime();
  const mainId = 'root-001';
  const childId = 'child-002';
  const grandChildId = 'grandchild-003';

  // Root: 2 steps in different hours of today, 1 has a speed spike (>5000 tok/s) and valid TTFT
  createConvDb(path.join(convDir, `${mainId}.db`), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/agy-pulse',
      branch: 'feat/v8',
      createdAtMs: now - 3 * HOUR_MS,
    }),
    steps: [
      { idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 3 * HOUR_MS }) },
      { idx: 2, metadata: buildStepMetadataBlob({ tsMs: now - HOUR_MS }) },
    ],
    genRows: [
      {
        idx: 1,
        data: buildGenMetadataBlob({
          stepIndices: [1],
          entries: [{
            displayName: 'Gemini 4 Carbon (High)',
            inputTokens: 100_000,
            cacheReadTokens: 400_000,
            outputTokens: 1_000,
            streamSec: 10, // 100 tok/s (valid)
            ttftSec: 2,
          }],
        }),
      },
      {
        idx: 2,
        data: buildGenMetadataBlob({
          stepIndices: [2],
          entries: [{
            displayName: 'Gemini 4 Carbon (High)',
            inputTokens: 0,
            cacheReadTokens: 500_000,
            outputTokens: 5_000,
            streamSec: 0.1, // 50,000 tok/s -> MUST be filtered from avgSpeed!
            ttftSec: 700,   // > 600s -> MUST be filtered from avgTtftSec!
          }],
        }),
      },
    ],
  });

  // Child under rootId
  createConvDb(path.join(convDir, `${childId}.db`), {
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 2 * HOUR_MS }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{
          displayName: 'Gemini 4 Hydrogen (High)',
          inputTokens: 1_000_000,
          outputTokens: 100_000,
          streamSec: 500, // 200 tok/s
          ttftSec: 4,
          parentCascadeId: mainId,
          rootCascadeId: mainId,
        }],
      }),
    }],
  });

  // Grandchild pointing ONLY to childId via parentCascadeId
  createConvDb(path.join(convDir, `${grandChildId}.db`), {
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 30 * 60 * 1000 }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{
          displayName: 'Gemini 4 Neon (High)',
          inputTokens: 100_000,
          outputTokens: 10_000,
          parentCascadeId: childId,
        }],
      }),
    }],
  });

  // Also create an older session 100 days ago in another workspace
  const oldTs = now - 100 * DAY_MS;
  createConvDb(path.join(convDir, 'old-other.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/other,proj',
      branch: 'main',
      createdAtMs: oldTs,
    }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: oldTs }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Claude Opus 5.5', inputTokens: 10_000, outputTokens: 1_000 }],
      }),
    }],
  });

  syncAllConversationsToLedger({ roots: [root], now });

  // 1. 24h auto-granularity -> 'hour'
  const q24 = queryLedger({ range: '24h', now }, { roots: [root] });
  assert.equal(q24.meta.backend, 'sqlite');
  assert.equal(q24.meta.granularity, 'hour');
  assert.equal(q24.summary.rootSessionCount, 1);
  assert.equal(q24.summary.subagentSessionCount, 2);
  assert.equal(q24.summary.totalSteps, 4);

  // Carbon speed only uses step 1 (1000 / 10 = 100 tok/s) and TTFT only uses step 1 (2s)
  const carbon = q24.byModel.find(m => m.model === 'Carbon');
  assert.ok(carbon);
  assert.equal(carbon.steps, 2);
  assert.equal(carbon.streamSamples, 1);
  assert.equal(carbon.ttftSamples, 1);
  assert.ok(Math.abs(carbon.avgSpeed - 100) < 1e-9);
  assert.ok(Math.abs(carbon.avgTtftSec - 2) < 1e-9);
  // Carbon cacheHitRate: (400k + 500k) / (100k + 400k + 500k) = 90%
  assert.ok(Math.abs(carbon.cacheHitRate - 90) < 1e-9);

  // Verify 2-level child & grandchild merged under root-001
  assert.equal(q24.sessions.length, 1);
  assert.equal(q24.sessions[0].rootConversationId, mainId);
  assert.equal(q24.sessions[0].subagentCount, 2);
  assert.equal(q24.sessions[0].children.length, 2);
  assert.ok(q24.sessions[0].mainCostUsd > 0);
  assert.ok(q24.sessions[0].subagentCostUsd > 0);
  assert.ok(Math.abs(q24.sessions[0].totalCostUsd - (q24.sessions[0].mainCostUsd + q24.sessions[0].subagentCostUsd)) < 1e-9);

  // 2. includeSubagents: false excludes child & grandchild
  const qMainOnly = queryLedger({ range: '24h', includeSubagents: false, now }, { roots: [root] });
  assert.equal(qMainOnly.summary.totalSteps, 2);
  assert.equal(qMainOnly.summary.subagentSessionCount, 0);
  assert.equal(qMainOnly.summary.subagentCostUsd, 0);
  assert.equal(qMainOnly.sessions[0].children.length, 0);

  // 3. Model & Workspace filtering
  const qModel = queryLedger({ range: '24h', model: 'Neon', now }, { roots: [root] });
  assert.equal(qModel.summary.totalSteps, 1);
  assert.equal(qModel.byModel.length, 1);
  assert.equal(qModel.byModel[0].model, 'Neon');

  const qWs = queryLedger({ range: 'all', workspace: 'agy-pulse', now }, { roots: [root] });
  assert.equal(qWs.summary.totalSteps, 4);

  // 4. Range 'all' (>90d span) auto-granularity -> 'week', and explicit 'day'
  const qAllAuto = queryLedger({ range: 'all', now }, { roots: [root] });
  assert.equal(qAllAuto.meta.granularity, 'week');
  assert.equal(qAllAuto.summary.totalSteps, 5);
  const qAllDay = queryLedger({ range: 'all', granularity: 'day', now }, { roots: [root] });
  assert.equal(qAllDay.meta.granularity, 'day');

  // 5. CSV formatting (timeseries, models, sessions with comma-quoted workspace)
  const csvTs = formatLedgerCsv(qAllAuto, 'timeseries');
  assert.match(csvTs, /^bucketKey,bucketStartMs,label,costUsd,/);
  const csvModels = formatLedgerCsv(qAllAuto, 'models');
  assert.match(csvModels, /^model,costUsd,costSharePct,/);
  assert.match(csvModels, /Carbon/);
  const csvSessions = formatLedgerCsv(qAllAuto, 'sessions');
  assert.match(csvSessions, /^rootConversationId,source,workspace,workspaceLabel,/);
  assert.match(csvSessions, /"\/repos\/other,proj"/);

  // 6. JSON fallback backend produces identical totals
  const jsonSync = syncAllConversationsToLedger({ roots: [root], backend: 'json', now });
  assert.equal(jsonSync.ok, true);
  assert.ok(jsonSync.ledgerPath.endsWith(FALLBACK_LEDGER_FILENAME));
  const jsonQuery = queryLedger({ range: '24h', now }, { roots: [root], backend: 'json' });
  assert.equal(jsonQuery.meta.backend, 'json');
  assert.equal(jsonQuery.summary.totalSteps, q24.summary.totalSteps);
  assert.ok(Math.abs(jsonQuery.summary.totalCostUsd - q24.summary.totalCostUsd) < 1e-9);
});

test('filters out 0-token ghost steps (e.g. MODEL_UNSPECIFIED) at ingestion and query time', () => {
  const root = path.join(tempDir, 'agy-ghost-filter');
  const convDir = path.join(root, 'conversations');
  const now = new Date(2026, 10, 20, 14, 0, 0, 0).getTime();

  createConvDb(path.join(convDir, 'ghost-sess.db'), {
    steps: [
      { idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 20_000 }) },
      { idx: 2, metadata: buildStepMetadataBlob({ tsMs: now - 10_000 }) },
    ],
    genRows: [
      {
        idx: 1,
        // Ghost 0-token entry with ttftSec = 0 and MODEL_UNSPECIFIED
        data: buildGenMetadataBlob({
          stepIndices: [1],
          entries: [{
            rawModel: 'MODEL_UNSPECIFIED',
            inputTokens: 0,
            cacheReadTokens: 0,
            outputTokens: 0,
            ttftSec: 0,
          }],
        }),
      },
      {
        idx: 2,
        data: buildGenMetadataBlob({
          stepIndices: [2],
          entries: [{
            displayName: 'Gemini 4 Carbon (High)',
            inputTokens: 1000,
            outputTokens: 500,
          }],
        }),
      },
    ],
  });

  syncAllConversationsToLedger({ roots: [root], now });

  // Also inject a legacy 0-token ghost row directly into ledger SQLite to verify query-time purification
  const ledgerPath = resolveLedgerPath({ roots: [root] });
  const ldb = new DatabaseSync(ledgerPath);
  try {
    ldb.prepare(`
      INSERT INTO telemetry_steps (
        step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
        model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens, cost_usd
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 0)
    `).run('legacy-ghost:1:0', 'ghost-sess', 99, 0, 99, now - 5_000, 'MODEL_', 'MODEL_UNSPECIFIED');
  } finally {
    ldb.close();
  }

  const res = queryLedger({ range: 'all', now }, { roots: [root] });
  assert.equal(res.summary.totalSteps, 1);
  assert.equal(res.meta.totalRecordedSteps, 1);
  assert.deepEqual(res.meta.availableModels, ['Carbon']);
  assert.equal(res.byModel.length, 1);
  assert.equal(res.byModel[0].model, 'Carbon');
});

test('dual-source sync (CLI + IDE) deduplicates by conversationId, reports cliDbs/ideDbs, and filters/aggregates by source', () => {
  assert.equal(normalizeSource('ide'), 'ide');
  assert.equal(normalizeSource(' IDE '), 'ide');
  assert.equal(normalizeSource('cli'), 'cli');
  assert.equal(normalizeSource('unknown'), 'cli');
  assert.equal(normalizeSourceFilter('IDE'), 'ide');
  assert.equal(normalizeSourceFilter('cli'), 'cli');
  assert.equal(normalizeSourceFilter('all'), 'all');
  assert.equal(normalizeSourceFilter('invalid'), 'all');

  const cliRoot = path.join(tempDir, 'cli-root');
  const ideRoot = path.join(tempDir, 'ide-root');
  const now = new Date(2026, 10, 20, 14, 0, 0, 0).getTime();

  // Verify resolveLedgerSourceRoots isolation and dedup
  assert.deepEqual(resolveLedgerSourceRoots({ roots: [cliRoot] }), [
    { root: cliRoot, source: 'cli' },
  ]);
  assert.deepEqual(resolveLedgerSourceRoots({ roots: [cliRoot], ideRoots: [ideRoot, cliRoot] }), [
    { root: cliRoot, source: 'cli' },
    { root: ideRoot, source: 'ide' },
  ]);

  // Empty ledger query -> availableSources: [], bySource: []
  const emptyRes = queryLedger({ range: 'all', now }, { roots: [cliRoot] });
  assert.equal(emptyRes.meta.source, 'all');
  assert.deepEqual(emptyRes.meta.availableSources, []);
  assert.deepEqual(emptyRes.bySource, []);

  // CLI session + subagent
  createConvDb(path.join(cliRoot, 'conversations', 'cli-root-1.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/cli-proj',
      branch: 'main',
      createdAtMs: now - 3 * HOUR_MS,
    }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 3 * HOUR_MS }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{
          displayName: 'Gemini 4 Carbon (High)',
          inputTokens: 1_000_000,
          outputTokens: 100_000,
          streamSec: 100,
          ttftSec: 2,
        }],
      }),
    }],
  });

  createConvDb(path.join(cliRoot, 'conversations', 'cli-child-1.db'), {
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 2 * HOUR_MS }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{
          displayName: 'Gemini 4 Hydrogen (High)',
          inputTokens: 500_000,
          outputTokens: 50_000,
          streamSec: 50,
          ttftSec: 1,
          parentCascadeId: 'cli-root-1',
          rootCascadeId: 'cli-root-1',
        }],
      }),
    }],
  });

  // Duplicate conversationId in IDE root (must be deduped in favor of CLI root)
  createConvDb(path.join(ideRoot, 'conversations', 'cli-root-1.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/should-skip-dup',
      createdAtMs: now - 3 * HOUR_MS,
    }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 3 * HOUR_MS }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Claude Opus 5.5', inputTokens: 10_000_000, outputTokens: 1_000_000 }],
      }),
    }],
  });

  // Unique IDE session
  createConvDb(path.join(ideRoot, 'conversations', 'ide-sess-1.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/ide-app',
      branch: 'feat/ide',
      createdAtMs: now - HOUR_MS,
    }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - HOUR_MS }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{
          displayName: 'Gemini 4 Carbon (High)',
          inputTokens: 200_000,
          cacheReadTokens: 800_000,
          outputTokens: 50_000,
          streamSec: 25,
          ttftSec: 1.5,
        }],
      }),
    }],
  });

  const listed = listAllConversationDbs([
    { root: cliRoot, source: 'cli' },
    { root: ideRoot, source: 'ide' },
  ]);
  assert.equal(listed.length, 3);
  assert.equal(listed.find(d => d.conversationId === 'cli-root-1')?.source, 'cli');
  assert.equal(listed.find(d => d.conversationId === 'ide-sess-1')?.source, 'ide');

  const syncRes = syncAllConversationsToLedger({
    roots: [cliRoot],
    ideRoots: [ideRoot],
    now,
  });
  assert.equal(syncRes.ok, true);
  assert.equal(syncRes.totalDbs, 3);
  assert.equal(syncRes.cliDbs, 2);
  assert.equal(syncRes.ideDbs, 1);
  assert.equal(syncRes.newlySyncedDbs, 3);
  assert.equal(syncRes.stepsUpserted, 3);
  assert.equal(syncRes.ledgerPath, path.join(cliRoot, DEFAULT_LEDGER_FILENAME));

  // Query all sources
  const qAll = queryLedger({ range: 'all', source: 'all', now }, { roots: [cliRoot] });
  assert.equal(qAll.meta.source, 'all');
  assert.deepEqual(qAll.meta.availableSources, ['cli', 'ide']);
  assert.equal(qAll.summary.totalSteps, 3);
  assert.equal(qAll.summary.rootSessionCount, 2);
  assert.equal(qAll.summary.subagentSessionCount, 1);
  assert.equal(qAll.bySource.length, 2);
  const cliSrc = qAll.bySource.find(s => s.source === 'cli');
  const ideSrc = qAll.bySource.find(s => s.source === 'ide');
  assert.ok(cliSrc && ideSrc);
  assert.equal(cliSrc.steps, 2);
  assert.equal(cliSrc.sessionCount, 1);
  assert.equal(ideSrc.steps, 1);
  assert.equal(ideSrc.sessionCount, 1);
  assert.ok(Math.abs((cliSrc.costSharePct + ideSrc.costSharePct) - 100) < 1e-6);
  assert.ok(qAll.bySource[0].costUsd >= qAll.bySource[1].costUsd);

  const ideSession = qAll.sessions.find(s => s.rootConversationId === 'ide-sess-1');
  const cliSession = qAll.sessions.find(s => s.rootConversationId === 'cli-root-1');
  assert.equal(ideSession?.source, 'ide');
  assert.equal(cliSession?.source, 'cli');
  assert.equal(cliSession?.children[0]?.source, 'cli');

  // Query source: 'ide'
  const qIde = queryLedger({ range: 'all', source: 'ide', now }, { roots: [cliRoot] });
  assert.equal(qIde.meta.source, 'ide');
  assert.deepEqual(qIde.meta.availableSources, ['cli', 'ide']);
  assert.equal(qIde.summary.totalSteps, 1);
  assert.equal(qIde.summary.rootSessionCount, 1);
  assert.equal(qIde.sessions[0].rootConversationId, 'ide-sess-1');
  assert.equal(qIde.bySource.length, 1);
  assert.equal(qIde.bySource[0].source, 'ide');
  assert.ok(Math.abs(qIde.bySource[0].costSharePct - 100) < 1e-6);

  // Query source: 'cli'
  const qCli = queryLedger({ range: 'all', source: 'cli', now }, { roots: [cliRoot] });
  assert.equal(qCli.meta.source, 'cli');
  assert.equal(qCli.summary.totalSteps, 2);
  assert.equal(qCli.summary.rootSessionCount, 1);
  assert.equal(qCli.sessions[0].rootConversationId, 'cli-root-1');
  assert.equal(qCli.bySource.length, 1);
  assert.equal(qCli.bySource[0].source, 'cli');

  // CSV exports: sessions adds source column; models & timeseries stay unchanged
  const csvSessions = formatLedgerCsv(qAll, 'sessions');
  const sessionLines = csvSessions.trim().split('\n');
  assert.match(sessionLines[0], /^rootConversationId,source,workspace,workspaceLabel,/);
  assert.ok(sessionLines.some(l => l.startsWith('ide-sess-1,ide,')));
  assert.ok(sessionLines.some(l => l.startsWith('cli-root-1,cli,')));
  const csvModels = formatLedgerCsv(qAll, 'models');
  assert.match(csvModels, /^model,costUsd,costSharePct,steps,/);
  const csvTs = formatLedgerCsv(qAll, 'timeseries');
  assert.match(csvTs, /^bucketKey,bucketStartMs,label,costUsd,/);
});

test('SQLite v1 ledger auto-migrates idempotently to v2 with default cli source and ingests new IDE sessions', () => {
  const cliRoot = path.join(tempDir, 'v1-cli');
  const ideRoot = path.join(tempDir, 'v1-ide');
  fs.mkdirSync(cliRoot, { recursive: true });
  const ledgerPath = path.join(cliRoot, DEFAULT_LEDGER_FILENAME);
  const now = new Date(2026, 10, 20, 15, 0, 0, 0).getTime();

  // Seed legacy v1 SQLite database WITHOUT source column
  const legacyDb = new DatabaseSync(ledgerPath);
  try {
    legacyDb.exec(`
      CREATE TABLE ledger_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      INSERT INTO ledger_meta (key, value) VALUES ('schema_version', '1');
      CREATE TABLE ingested_dbs (
        conversation_id TEXT PRIMARY KEY,
        db_path TEXT NOT NULL,
        sig TEXT NOT NULL,
        last_synced_ms INTEGER NOT NULL,
        workspace TEXT,
        branch TEXT,
        created_at_ms INTEGER,
        cascade_id TEXT,
        parent_conversation_id TEXT,
        root_conversation_id TEXT,
        db_mtime_ms INTEGER NOT NULL
      );
      CREATE TABLE telemetry_steps (
        step_uid TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL,
        gen_idx INTEGER NOT NULL,
        entry_idx INTEGER NOT NULL,
        step_index INTEGER,
        ts_ms INTEGER NOT NULL,
        model TEXT NOT NULL,
        raw_model TEXT,
        input_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd REAL NOT NULL DEFAULT 0,
        stream_sec REAL,
        ttft_sec REAL,
        valid_stream INTEGER NOT NULL DEFAULT 0,
        valid_ttft INTEGER NOT NULL DEFAULT 0,
        parent_conversation_id TEXT,
        root_conversation_id TEXT,
        workspace TEXT,
        branch TEXT
      );
    `);
    legacyDb.prepare(`
      INSERT INTO ingested_dbs (
        conversation_id, db_path, sig, last_synced_ms, workspace, branch,
        created_at_ms, cascade_id, parent_conversation_id, root_conversation_id, db_mtime_ms
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)
    `).run('legacy-cli-1', '/tmp/legacy-cli-1.db', 'sig-1', now - HOUR_MS, '/repos/legacy', 'main', now - 2 * HOUR_MS, 'legacy-cli-1', now - HOUR_MS);
    legacyDb.prepare(`
      INSERT INTO telemetry_steps (
        step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
        model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
        cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
        parent_conversation_id, root_conversation_id, workspace, branch
      ) VALUES (?, ?, 0, 0, 1, ?, ?, ?, 10000, 0, 0, 1000, 0.05, 5, 1, 1, 1, NULL, NULL, ?, ?)
    `).run('legacy-cli-1:0:0', 'legacy-cli-1', now - HOUR_MS, 'Carbon', 'Gemini 4 Carbon (High)', '/repos/legacy', 'main');
  } finally {
    legacyDb.close();
  }

  // Add a new IDE conversation DB
  createConvDb(path.join(ideRoot, 'conversations', 'new-ide-1.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/ide-migrated',
      branch: 'dev',
      createdAtMs: now - 10_000,
    }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 10_000 }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Gemini 4 Hydrogen (High)', inputTokens: 20_000, outputTokens: 2_000 }],
      }),
    }],
  });

  // First sync migrates v1 -> v2 and ingests new IDE DB
  const sync1 = syncAllConversationsToLedger({ roots: [cliRoot], ideRoots: [ideRoot], now });
  assert.equal(sync1.ok, true);
  assert.equal(sync1.ideDbs, 1);
  assert.equal(sync1.newlySyncedDbs, 1);

  // Second sync must remain idempotent
  const sync2 = syncAllConversationsToLedger({ roots: [cliRoot], ideRoots: [ideRoot], now: now + 5000 });
  assert.equal(sync2.ok, true);
  assert.equal(sync2.newlySyncedDbs, 0);

  // Verify SQLite schema & indexes upgraded to v2
  const checkDb = new DatabaseSync(ledgerPath, { readOnly: true });
  try {
    const ver = checkDb.prepare("SELECT value FROM ledger_meta WHERE key = 'schema_version'").get();
    assert.equal(ver?.value, '2');
    const dbCols = checkDb.prepare('PRAGMA table_info(ingested_dbs)').all().map(c => c.name);
    const stepCols = checkDb.prepare('PRAGMA table_info(telemetry_steps)').all().map(c => c.name);
    assert.ok(dbCols.includes('source'));
    assert.ok(stepCols.includes('source'));
    const indexes = checkDb.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'telemetry_steps'").all().map(i => i.name);
    assert.ok(indexes.includes('idx_steps_source'));
  } finally {
    checkDb.close();
  }

  const qAll = queryLedger({ range: 'all', now }, { roots: [cliRoot] });
  assert.deepEqual(qAll.meta.availableSources, ['cli', 'ide']);
  assert.equal(qAll.summary.totalSteps, 2);
  assert.equal(qAll.sessions.find(s => s.rootConversationId === 'legacy-cli-1')?.source, 'cli');
  assert.equal(qAll.sessions.find(s => s.rootConversationId === 'new-ide-1')?.source, 'ide');
});

test('JSON fallback backend (forceJson: true) handles legacy records without source, IDE ingestion, and source filtering', () => {
  const cliRoot = path.join(tempDir, 'json-cli');
  const ideRoot = path.join(tempDir, 'json-ide');
  fs.mkdirSync(cliRoot, { recursive: true });
  const jsonPath = path.join(cliRoot, FALLBACK_LEDGER_FILENAME);
  const now = new Date(2026, 10, 20, 16, 0, 0, 0).getTime();

  // Seed legacy JSON ledger WITHOUT source fields
  fs.writeFileSync(jsonPath, JSON.stringify({
    schemaVer: 1,
    ingestedDbs: {
      'old-json-cli': {
        conversationId: 'old-json-cli',
        dbPath: '/tmp/old-json-cli.db',
        sig: 'old-sig',
        lastSyncedMs: now - HOUR_MS,
        workspace: '/repos/json-legacy',
        branch: 'main',
      },
    },
    steps: {
      'old-json-cli:0:0': {
        stepUid: 'old-json-cli:0:0',
        conversationId: 'old-json-cli',
        genIdx: 0,
        entryIdx: 0,
        stepIndex: 1,
        tsMs: now - HOUR_MS,
        model: 'Carbon',
        rawModel: 'Gemini 4 Carbon (High)',
        inputTokens: 100_000,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        outputTokens: 5_000,
        costUsd: 0.12,
        validStream: 0,
        validTtft: 0,
        workspace: '/repos/json-legacy',
        branch: 'main',
      },
    },
  }));

  createConvDb(path.join(ideRoot, 'conversations', 'json-ide-1.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({
      workspaceUri: 'file:///repos/json-ide',
      branch: 'feat/json',
      createdAtMs: now - 10_000,
    }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 10_000 }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Gemini 4 Neon (High)', inputTokens: 50_000, outputTokens: 2_000 }],
      }),
    }],
  });

  const syncJson = syncAllConversationsToLedger({
    roots: [cliRoot],
    ideRoots: [ideRoot],
    forceJson: true,
    now,
  });
  assert.equal(syncJson.ok, true);
  assert.equal(syncJson.ledgerPath, jsonPath);
  assert.equal(syncJson.ideDbs, 1);

  const persisted = JSON.parse(fs.readFileSync(jsonPath, 'utf8'));
  assert.equal(persisted.ingestedDbs['old-json-cli'].source, 'cli');
  assert.equal(persisted.ingestedDbs['json-ide-1'].source, 'ide');
  assert.equal(persisted.steps['json-ide-1:1:0'].source, 'ide');

  const qAll = queryLedger({ range: 'all', now }, { roots: [cliRoot], forceJson: true });
  assert.equal(qAll.meta.backend, 'json');
  assert.deepEqual(qAll.meta.availableSources, ['cli', 'ide']);
  assert.equal(qAll.summary.totalSteps, 2);

  const qIde = queryLedger({ range: 'all', source: 'ide', now }, { roots: [cliRoot], forceJson: true });
  assert.equal(qIde.summary.totalSteps, 1);
  assert.equal(qIde.sessions[0].rootConversationId, 'json-ide-1');
  assert.equal(qIde.sessions[0].source, 'ide');

  const qCli = queryLedger({ range: 'all', source: 'cli', now }, { roots: [cliRoot], forceJson: true });
  assert.equal(qCli.summary.totalSteps, 1);
  assert.equal(qCli.sessions[0].rootConversationId, 'old-json-cli');
  assert.equal(qCli.sessions[0].source, 'cli');
});

test('syncSingleConversationToLedger scans CLI roots only, tags cli source, and ignores IDE roots', () => {
  const cliRoot = path.join(tempDir, 'single-cli');
  const ideRoot = path.join(tempDir, 'single-ide');
  const now = new Date(2026, 10, 20, 17, 0, 0, 0).getTime();

  createConvDb(path.join(cliRoot, 'conversations', 'only-cli-sess.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({ workspaceUri: 'file:///repos/cli-only', createdAtMs: now - 20_000 }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 20_000 }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Gemini 4 Carbon (High)', inputTokens: 5_000, outputTokens: 500 }],
      }),
    }],
  });

  createConvDb(path.join(ideRoot, 'conversations', 'only-ide-sess.db'), {
    trajectoryBlob: buildTrajectoryMetaBlob({ workspaceUri: 'file:///repos/ide-only', createdAtMs: now - 10_000 }),
    steps: [{ idx: 1, metadata: buildStepMetadataBlob({ tsMs: now - 10_000 }) }],
    genRows: [{
      idx: 1,
      data: buildGenMetadataBlob({
        stepIndices: [1],
        entries: [{ displayName: 'Gemini 4 Carbon (High)', inputTokens: 8_000, outputTokens: 800 }],
      }),
    }],
  });

  const ideAttempt = syncSingleConversationToLedger('only-ide-sess', {
    roots: [cliRoot],
    ideRoots: [ideRoot],
    env: { AGY_PULSE_DATA_DIR: cliRoot, AGY_PULSE_IDE_DATA_DIR: ideRoot },
    now,
  });
  assert.equal(ideAttempt.ok, true);
  assert.equal(ideAttempt.skipped, true);
  assert.equal(ideAttempt.syncedDbs, 0);

  const cliSync = syncSingleConversationToLedger('only-cli-sess', {
    roots: [cliRoot],
    ideRoots: [ideRoot],
    env: { AGY_PULSE_DATA_DIR: cliRoot, AGY_PULSE_IDE_DATA_DIR: ideRoot },
    now,
  });
  assert.equal(cliSync.ok, true);
  assert.equal(cliSync.skipped, false);
  assert.equal(cliSync.syncedDbs, 1);

  const q = queryLedger({ range: 'all', now }, { roots: [cliRoot] });
  assert.deepEqual(q.meta.availableSources, ['cli']);
  assert.equal(q.summary.totalSteps, 1);
  assert.equal(q.sessions[0].rootConversationId, 'only-cli-sess');
  assert.equal(q.sessions[0].source, 'cli');
});
