import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
  DEFAULT_COMPACTION_LIMIT,
  MAX_DB_CACHE_ENTRIES,
  MAX_PERF_RECENT_DBS,
  MAX_PERF_DB_ENTRIES,
  decodeVarint,
  pbGetSub,
  pbGetAllSub,
  pbGetVarint,
  pbGetString,
  pbGetDurationSec,
  extractCompactionLimitFromGenMetadata,
  extractCompactionLimitFromExecutorMetadata,
  extractGenMetadataTelemetry,
  extractModelPerfFromGenMetadata,
  querySqliteCompactionLimit,
  querySqliteModelTelemetry,
  querySqliteModelPerf,
  resolveServerCompactionLimit,
  resolveModelPerfStats,
  resolveSessionCostTree,
} = require('../../runtime/compaction.js');

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

function encodeFixed64Field(fieldNum, fillByte = 0xaa) {
  const payload = Buffer.alloc(8, fillByte);
  return Buffer.concat([
    encodeVarint((BigInt(fieldNum) << 3n) | 1n),
    payload,
  ]);
}

function encodeFixed32Field(fieldNum, fillByte = 0xbb) {
  const payload = Buffer.alloc(4, fillByte);
  return Buffer.concat([
    encodeVarint((BigInt(fieldNum) << 3n) | 5n),
    payload,
  ]);
}

function buildGenMetadataBlob(limit) {
  const f10 = encodeVarintField(4, limit);
  const f9 = encodeBytesField(10, f10);
  const f1 = encodeBytesField(9, f9);
  return encodeBytesField(1, f1);
}

function buildExecutorMetadataBlob(limit) {
  const f2 = encodeVarintField(5, limit);
  const f10 = encodeBytesField(2, f2);
  return encodeBytesField(10, f10);
}

function createConvSqliteDb(dbPath, { genBlob = null, execBlob = null } = {}) {
  const { DatabaseSync } = require('node:sqlite');
  fs.mkdirSync(path.dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec('CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    db.exec('CREATE TABLE IF NOT EXISTS executor_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    if (genBlob) {
      db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(1, genBlob);
    }
    if (execBlob) {
      db.prepare('INSERT INTO executor_metadata (idx, data) VALUES (?, ?)').run(1, execBlob);
    }
  } finally {
    db.close();
  }
}

let tempDir;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-compaction-test-'));
});

afterEach(() => {
  fs.rmSync(tempDir, { recursive: true, force: true });
});

test('decodeVarint handles small, multi-byte, >31-bit BigInt values, and invalid inputs', () => {
  assert.deepEqual(decodeVarint(Buffer.from([0x00])), { value: 0, nextOffset: 1 });
  assert.deepEqual(decodeVarint(Buffer.from([0x7f])), { value: 127, nextOffset: 1 });
  assert.deepEqual(decodeVarint(Buffer.from([0xac, 0x02])), { value: 300, nextOffset: 2 });

  // Value > 2^31 (would overflow 32-bit signed bitwise << in JS)
  const bigVal = 34359738491n; // 2^35 + 123
  const bigBuf = encodeVarint(bigVal);
  const decodedBig = decodeVarint(bigBuf, 0);
  assert.equal(decodedBig.value, Number(bigVal));
  assert.equal(decodedBig.nextOffset, bigBuf.length);

  // Truncated / empty / out-of-range
  assert.deepEqual(decodeVarint(Buffer.alloc(0)), { value: null, nextOffset: 0 });
  assert.deepEqual(decodeVarint(Buffer.from([0x80])), { value: null, nextOffset: 1 });
  assert.deepEqual(decodeVarint(Buffer.from([0x01]), 5), { value: null, nextOffset: 5 });
});

test('pbGetSub, pbGetAllSub, and pbGetVarint decode mixed wire types and Uint8Array inputs', () => {
  const subA = encodeVarintField(1, 42);
  const subB = encodeVarintField(1, 99);
  const msg = Buffer.concat([
    encodeFixed64Field(2),
    encodeFixed32Field(3),
    encodeVarintField(4, 123456),
    encodeBytesField(5, subA),
    encodeBytesField(5, subB),
  ]);

  const asUint8 = new Uint8Array(msg);
  assert.equal(pbGetVarint(asUint8, 4), 123456);
  assert.equal(pbGetVarint(asUint8, 99), null);

  const firstSub = pbGetSub(asUint8, 5);
  assert.ok(Buffer.isBuffer(firstSub));
  assert.equal(pbGetVarint(firstSub, 1), 42);

  const allSubs = pbGetAllSub(asUint8, 5);
  assert.equal(allSubs.length, 2);
  assert.equal(pbGetVarint(allSubs[0], 1), 42);
  assert.equal(pbGetVarint(allSubs[1], 1), 99);

  // Truncated length-delimited field should safely stop without throwing
  const truncated = Buffer.concat([encodeVarint((5n << 3n) | 2n), encodeVarint(50), Buffer.from([1, 2])]);
  assert.equal(pbGetSub(truncated, 5), null);
  assert.deepEqual(pbGetAllSub(truncated, 5), []);
});

test('extractCompactionLimitFromGenMetadata and ExecutorMetadata validate paths and range [16000, 10000000]', () => {
  assert.equal(extractCompactionLimitFromGenMetadata(buildGenMetadataBlob(512000)), 512000);
  assert.equal(extractCompactionLimitFromGenMetadata(buildGenMetadataBlob(16000)), 16000);
  assert.equal(extractCompactionLimitFromGenMetadata(buildGenMetadataBlob(10000000)), 10000000);
  assert.equal(extractCompactionLimitFromGenMetadata(buildGenMetadataBlob(15999)), null);
  assert.equal(extractCompactionLimitFromGenMetadata(buildGenMetadataBlob(10000001)), null);

  assert.equal(extractCompactionLimitFromExecutorMetadata(buildExecutorMetadataBlob(256000)), 256000);
  assert.equal(extractCompactionLimitFromExecutorMetadata(buildExecutorMetadataBlob(12000)), null);

  // Repeated sub-message where the first f9 lacks f10->f4 and second f9 contains it
  const multiF1 = Buffer.concat([
    encodeBytesField(9, encodeVarintField(1, 1)),
    encodeBytesField(9, encodeBytesField(10, encodeVarintField(4, 768000))),
  ]);
  const multiGen = encodeBytesField(1, multiF1);
  assert.equal(extractCompactionLimitFromGenMetadata(multiGen), 768000);
});

test('querySqliteCompactionLimit reads gen_metadata first, falls back to executor_metadata, and handles errors', () => {
  const db1 = path.join(tempDir, 'both.db');
  createConvSqliteDb(db1, {
    genBlob: buildGenMetadataBlob(640000),
    execBlob: buildExecutorMetadataBlob(256000),
  });
  assert.equal(querySqliteCompactionLimit(db1), 640000);

  const db2 = path.join(tempDir, 'exec-only.db');
  createConvSqliteDb(db2, {
    execBlob: buildExecutorMetadataBlob(320000),
  });
  assert.equal(querySqliteCompactionLimit(db2), 320000);

  assert.equal(querySqliteCompactionLimit(path.join(tempDir, 'missing.db')), null);
  assert.equal(querySqliteCompactionLimit(''), null);
});

test('querySqliteCompactionLimit falls back to temp copy on SQLITE_CANTOPEN and sqlite3 CLI when node:sqlite unavailable', () => {
  const dbPath = path.join(tempDir, 'locked.db');
  createConvSqliteDb(dbPath, { genBlob: buildGenMetadataBlob(400000) });
  fs.writeFileSync(`${dbPath}-wal`, Buffer.from('wal-bytes'));

  let readOnlyAttempts = 0;
  let writableCopyAttempts = 0;
  const { DatabaseSync: RealDatabaseSync } = require('node:sqlite');
  class MockSqliteSync {
    constructor(target, opts) {
      if (opts && opts.readOnly) {
        readOnlyAttempts += 1;
        const err = new Error('SQLITE_CANTOPEN: unable to open database file');
        err.code = 'SQLITE_CANTOPEN';
        throw err;
      }
      writableCopyAttempts += 1;
      assert.notEqual(target, dbPath);
      assert.ok(fs.existsSync(`${target}-wal`));
      this.inner = new RealDatabaseSync(target, opts);
    }
    prepare(sql) {
      return this.inner.prepare(sql);
    }
    close() {
      return this.inner.close();
    }
  }

  assert.equal(
    querySqliteCompactionLimit(dbPath, { sqliteModule: { DatabaseSync: MockSqliteSync } }),
    400000
  );
  assert.equal(readOnlyAttempts, 1);
  assert.equal(writableCopyAttempts, 1);

  // Mock sqlite3 CLI script when sqliteModule is null
  const hexPayload = buildGenMetadataBlob(520000).toString('hex');
  const fakeCli = path.join(tempDir, 'fake-sqlite3.sh');
  fs.writeFileSync(fakeCli, `#!/bin/sh\necho "${hexPayload}"\n`, { mode: 0o755 });
  assert.equal(
    querySqliteCompactionLimit(dbPath, { sqliteModule: null, sqlite3Path: fakeCli }),
    520000
  );
});

test('resolveServerCompactionLimit resolves active db, caches by size+mtime+wal sig, and falls back on cold start', () => {
  const root = path.join(tempDir, 'agy-root');
  const convId = 'conv-abc-123';
  const dbPath = path.join(root, 'conversations', `${convId}.db`);
  const transcriptPath = path.join(root, 'brain', convId, '.system_generated', 'logs', 'transcript.jsonl');
  fs.mkdirSync(path.dirname(transcriptPath), { recursive: true });
  fs.writeFileSync(transcriptPath, '');

  // 1. Cold start with no DBs and no cache -> fallback
  const coldFallback = resolveServerCompactionLimit({ transcriptPath, roots: [root] });
  assert.deepEqual(coldFallback, { serverLimit: DEFAULT_COMPACTION_LIMIT, source: 'fallback' });

  // 2. Active DB present -> source: 'db'
  createConvSqliteDb(dbPath, { genBlob: buildGenMetadataBlob(700000) });
  const firstRead = resolveServerCompactionLimit({ transcriptPath, roots: [root] });
  assert.deepEqual(firstRead, { serverLimit: 700000, source: 'db' });

  // 3. Unchanged DB -> source: 'cache'
  const secondRead = resolveServerCompactionLimit({ transcriptPath, roots: [root] });
  assert.deepEqual(secondRead, { serverLimit: 700000, source: 'cache' });

  // 4. WAL change invalidates signature cache even if main DB size didn't change
  fs.writeFileSync(`${dbPath}-wal`, Buffer.from('updated-wal-1234'));
  const afterWal = resolveServerCompactionLimit({ transcriptPath, roots: [root] });
  assert.deepEqual(afterWal, { serverLimit: 700000, source: 'db' });

  // 5. Cold start for brand-new session without DB yet -> uses _last_known_limit from cache
  const newTranscript = path.join(root, 'brain', 'conv-new-456', '.system_generated', 'logs', 'transcript.jsonl');
  const fromLastKnown = resolveServerCompactionLimit({ transcriptPath: newTranscript, roots: [root] });
  assert.deepEqual(fromLastKnown, { serverLimit: 700000, source: 'cache' });
});

test('resolveServerCompactionLimit scans top 5 recent DBs by mtimeMs when cache is cold and caps cache at 24 entries', () => {
  const root = path.join(tempDir, 'agy-cold-recent');
  const convDir = path.join(root, 'conversations');
  fs.mkdirSync(convDir, { recursive: true });

  // Create 7 DBs: oldest 6 have 256000, newest 1 has 880000
  const baseSec = Math.floor(Date.now() / 1000) - 100;
  for (let i = 0; i < 7; i += 1) {
    const p = path.join(convDir, `conv-${i}.db`);
    createConvSqliteDb(p, { genBlob: buildGenMetadataBlob(i === 6 ? 880000 : 256000) });
    const ts = baseSec + i * 10;
    fs.utimesSync(p, ts, ts);
  }

  const newTranscript = path.join(root, 'brain', 'brand-new', '.system_generated', 'logs', 'transcript.jsonl');
  const resolved = resolveServerCompactionLimit({ transcriptPath: newTranscript, roots: [root] });
  assert.deepEqual(resolved, { serverLimit: 880000, source: 'recent_db' });

  // Verify _db_limits pruning to MAX_DB_CACHE_ENTRIES (24)
  for (let i = 0; i < 28; i += 1) {
    const id = `prune-${i}`;
    const p = path.join(convDir, `${id}.db`);
    createConvSqliteDb(p, { genBlob: buildGenMetadataBlob(300000 + i * 1000) });
    resolveServerCompactionLimit({ conversationId: id, roots: [root] });
  }
  const cacheFile = path.join(root, 'agy-pulse-compaction-cache.json');
  const cacheData = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
  assert.ok(Object.keys(cacheData._db_limits).length <= MAX_DB_CACHE_ENTRIES);
});

test('querySqliteCompactionLimit omits -readonly on sqlite3 temp copy and handles SQLITE_LOCKED/IOERR', () => {
  const dbPath = path.join(tempDir, 'wal-cli.db');
  createConvSqliteDb(dbPath, { genBlob: buildGenMetadataBlob(444000) });

  // Fake sqlite3 CLI: fails when -readonly is passed (simulating WAL without -shm), succeeds without -readonly
  const hexPayload = buildGenMetadataBlob(444000).toString('hex');
  const fakeCli = path.join(tempDir, 'wal-sqlite3.sh');
  fs.writeFileSync(
    fakeCli,
    `#!/bin/sh\nif [ "$1" = "-readonly" ]; then exit 1; fi\necho "${hexPayload}"\n`,
    { mode: 0o755 }
  );
  assert.equal(
    querySqliteCompactionLimit(dbPath, { sqliteModule: null, sqlite3Path: fakeCli }),
    444000
  );

  // SQLITE_LOCKED triggers temp-copy fallback
  const { DatabaseSync: RealDatabaseSync } = require('node:sqlite');
  let lockedFallbackCalled = false;
  class LockedMockSqlite {
    constructor(target, opts) {
      if (opts && opts.readOnly) {
        const err = new Error('SQLITE_LOCKED: database table is locked');
        err.code = 'SQLITE_LOCKED';
        throw err;
      }
      lockedFallbackCalled = true;
      this.inner = new RealDatabaseSync(target, opts);
    }
    prepare(sql) {
      return this.inner.prepare(sql);
    }
    close() {
      return this.inner.close();
    }
  }
  assert.equal(
    querySqliteCompactionLimit(dbPath, { sqliteModule: { DatabaseSync: LockedMockSqlite } }),
    444000
  );
  assert.equal(lockedFallbackCalled, true);
});

test('resolveServerCompactionLimit uses lastIndexOf(/brain/) and prefers newer recent_db over stale _last_known_limit', () => {
  const root = path.join(tempDir, 'user', 'brain', 'workspace', '.gemini', 'antigravity');
  const convOld = 'conv-old-1';
  const convNew = 'conv-new-2';
  const oldDb = path.join(root, 'conversations', `${convOld}.db`);
  const newDb = path.join(root, 'conversations', `${convNew}.db`);
  createConvSqliteDb(oldDb, { genBlob: buildGenMetadataBlob(500000) });
  const t1 = Math.floor(Date.now() / 1000) - 60;
  fs.utimesSync(oldDb, t1, t1);

  const oldTranscript = path.join(root, 'brain', convOld, '.system_generated', 'logs', 'transcript.jsonl');
  assert.deepEqual(resolveServerCompactionLimit({ transcriptPath: oldTranscript, roots: [root] }), {
    serverLimit: 500000,
    source: 'db',
  });

  // Create a newer conversation DB on disk (mtimeMs > _last_known_mtime)
  createConvSqliteDb(newDb, { genBlob: buildGenMetadataBlob(900000) });
  const t2 = t1 + 30;
  fs.utimesSync(newDb, t2, t2);

  // Brand-new session without its own DB must pick newer recent_db (900000) instead of stale _last_known_limit (500000)
  const brandNewTranscript = path.join(root, 'brain', 'conv-brand-new', '.system_generated', 'logs', 'transcript.jsonl');
  assert.deepEqual(resolveServerCompactionLimit({ transcriptPath: brandNewTranscript, roots: [root] }), {
    serverLimit: 900000,
    source: 'recent_db',
  });
});

function encodeStringField(fieldNum, str) {
  return encodeBytesField(fieldNum, Buffer.from(str, 'utf8'));
}

function encodeDuration(sec, nanos) {
  const parts = [];
  if (sec !== undefined && sec !== null) parts.push(encodeVarintField(1, sec));
  if (nanos !== undefined && nanos !== null) parts.push(encodeVarintField(2, nanos));
  return Buffer.concat(parts);
}

function buildModelPerfChatEntry({
  inputTokens = null,
  outputTokens,
  cacheReadTokens = null,
  ttftSec = null,
  ttftNanos = null,
  streamSec = null,
  streamNanos = null,
  rawModel = null,
  displayName = null,
  modelEnum = null,
  parentCascadeId = null,
  rootCascadeId = null,
  limit = null,
}) {
  const f1Parts = [];
  const f4Parts = [];
  if (inputTokens !== undefined && inputTokens !== null) {
    f4Parts.push(encodeVarintField(2, inputTokens));
  }
  if (outputTokens !== undefined && outputTokens !== null) {
    f4Parts.push(encodeVarintField(3, outputTokens));
  }
  if (cacheReadTokens !== undefined && cacheReadTokens !== null) {
    f4Parts.push(encodeVarintField(5, cacheReadTokens));
  }
  if (f4Parts.length > 0) {
    f1Parts.push(encodeBytesField(4, Buffer.concat(f4Parts)));
  }
  if (limit !== null) {
    f1Parts.push(encodeBytesField(9, encodeBytesField(10, encodeVarintField(4, limit))));
  }
  if (ttftSec !== null || ttftNanos !== null) {
    f1Parts.push(encodeBytesField(11, encodeDuration(ttftSec ?? 0, ttftNanos ?? 0)));
  }
  if (streamSec !== null || streamNanos !== null) {
    f1Parts.push(encodeBytesField(12, encodeDuration(streamSec ?? 0, streamNanos ?? 0)));
  }
  if (rawModel) {
    f1Parts.push(encodeStringField(19, rawModel));
  }
  if (modelEnum) {
    const kv = Buffer.concat([encodeStringField(1, 'model_enum'), encodeStringField(2, modelEnum)]);
    f1Parts.push(encodeBytesField(20, kv));
  }
  if (parentCascadeId) {
    const kv = Buffer.concat([encodeStringField(1, 'parent_cascade_id'), encodeStringField(2, parentCascadeId)]);
    f1Parts.push(encodeBytesField(20, kv));
  }
  if (rootCascadeId) {
    const kv = Buffer.concat([encodeStringField(1, 'root_cascade_id'), encodeStringField(2, rootCascadeId)]);
    f1Parts.push(encodeBytesField(20, kv));
  }
  if (displayName) {
    f1Parts.push(encodeStringField(21, displayName));
  }
  return encodeBytesField(1, Buffer.concat(f1Parts));
}

function createPerfSqliteDb(dbPath, blobs = []) {
  const { DatabaseSync } = require('node:sqlite');
  fs.mkdirSync(path.dirname(dbPath), { recursive: true });
  const db = new DatabaseSync(dbPath);
  try {
    db.exec('CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    const stmt = db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)');
    blobs.forEach((blob, i) => {
      stmt.run(i + 1, blob);
    });
  } finally {
    db.close();
  }
}

test('pbGetString, pbGetDurationSec, and extractModelPerfFromGenMetadata decode nanosecond durations and model priority', () => {
  const durMsg = encodeBytesField(11, encodeDuration(4, 370500000));
  assert.ok(Math.abs(pbGetDurationSec(durMsg, 11) - 4.3705) < 1e-9);
  assert.equal(pbGetDurationSec(durMsg, 99), null);

  // Empty Duration submessage decodes to 0s
  assert.equal(pbGetDurationSec(encodeBytesField(11, Buffer.alloc(0)), 11), 0);

  // Negative/overflow sec or out-of-range nanos rejected
  const negDur = encodeBytesField(11, encodeVarintField(1, 0xffffffffffffffffn));
  assert.equal(pbGetDurationSec(negDur, 11), null);
  const badNanos = encodeBytesField(11, encodeDuration(1, 1000000000));
  assert.equal(pbGetDurationSec(badNanos, 11), null);

  assert.equal(pbGetString(encodeStringField(21, 'Gemini 4 Carbon (High)'), 21), 'Gemini 4 Carbon (High)');
  assert.equal(pbGetString(encodeStringField(21, 'abc'), 19), null);

  const blob = Buffer.concat([
    // Entry 1: f21 preferred over f19
    buildModelPerfChatEntry({
      outputTokens: 1200,
      ttftSec: 5,
      ttftNanos: 250000000,
      streamSec: 10,
      streamNanos: 500000000,
      rawModel: 'gemini-4-hydrogen',
      displayName: 'Gemini 4 Carbon (High)',
    }),
    // Entry 2: f19 fallback
    buildModelPerfChatEntry({
      outputTokens: 400,
      ttftSec: 0,
      ttftNanos: 420000000,
      streamSec: 1,
      streamNanos: 600000000,
      rawModel: 'gemini-4-hydrogen',
    }),
    // Entry 3: f20 model_enum fallback
    buildModelPerfChatEntry({
      outputTokens: 300,
      ttftSec: 2,
      ttftNanos: 0,
      streamSec: 3,
      streamNanos: 0,
      modelEnum: 'claude-4-sonnet',
    }),
    // Entry 4: config-only record with no tokens/duration -> skipped
    buildGenMetadataBlob(512000),
  ]);

  const extracted = extractModelPerfFromGenMetadata(blob);
  assert.equal(extracted.length, 3);
  assert.equal(extracted[0].modelKey, 'Carbon');
  assert.equal(extracted[0].outputTokens, 1200);
  assert.ok(Math.abs(extracted[0].ttftSec - 5.25) < 1e-9);
  assert.ok(Math.abs(extracted[0].streamSec - 10.5) < 1e-9);
  assert.equal(extracted[1].modelKey, 'Hydro');
  assert.ok(Math.abs(extracted[1].ttftSec - 0.42) < 1e-9);
  assert.equal(extracted[2].modelKey, 'Sonnet');
});

test('querySqliteModelPerf computes weighted streaming speed, filters <0.05s noise & >600s TTFT, and distinguishes empty vs error', () => {
  const dbPath = path.join(tempDir, 'perf-weighted.db');
  createPerfSqliteDb(dbPath, [
    // Short burst (0.15s, 250 tok) + long stream (10.0s, 2000 tok) => weighted (2250 / 10.15 = 221.67 tok/s), NOT inflated arithmetic mean (1033 tok/s)
    buildModelPerfChatEntry({
      outputTokens: 250,
      ttftSec: 2,
      ttftNanos: 0,
      streamSec: 0,
      streamNanos: 150000000,
      displayName: 'Gemini 4 Hydrogen (High)',
    }),
    buildModelPerfChatEntry({
      outputTokens: 2000,
      ttftSec: 4,
      ttftNanos: 0,
      streamSec: 10,
      streamNanos: 0,
      displayName: 'Gemini 4 Hydrogen (High)',
    }),
    // < 0.05s stream duration filtered out, while valid TTFT (1.5s) is still recorded
    buildModelPerfChatEntry({
      outputTokens: 100,
      ttftSec: 1,
      ttftNanos: 500000000,
      streamSec: 0,
      streamNanos: 40000000,
      displayName: 'Gemini 4 Hydrogen (High)',
    }),
    // TTFT > 600s filtered out while valid stream is kept
    buildModelPerfChatEntry({
      outputTokens: 500,
      ttftSec: 601,
      ttftNanos: 0,
      streamSec: 5,
      streamNanos: 0,
      displayName: 'Gemini 4 Carbon (High)',
    }),
  ]);

  const perf = querySqliteModelPerf(dbPath);
  assert.ok(perf);
  assert.equal(perf.Hydro.streamSamples, 2);
  assert.equal(perf.Hydro.sumOut, 2250);
  assert.ok(Math.abs(perf.Hydro.sumStreamSec - 10.15) < 1e-6);
  assert.equal(perf.Hydro.ttftSamples, 3);
  assert.ok(Math.abs(perf.Hydro.sumTtftSec - 7.5) < 1e-6);

  assert.equal(perf.Carbon.streamSamples, 1);
  assert.equal(perf.Carbon.sumOut, 500);
  assert.equal(perf.Carbon.ttftSamples, 0);

  // Valid empty DB (no gen_metadata table) returns {}, whereas missing/non-db returns null
  const emptyDb = path.join(tempDir, 'empty-no-table.db');
  createConvSqliteDb(emptyDb);
  assert.deepEqual(querySqliteModelPerf(emptyDb), {});
  assert.equal(querySqliteModelPerf(path.join(tempDir, 'missing.db')), null);
});

test('resolveModelPerfStats aggregates across 30 recent DBs, hits sig cache, preserves _db_limits, and bounds _db_perf to 64', () => {
  const root = path.join(tempDir, 'agy-perf-window');
  const convDir = path.join(root, 'conversations');
  fs.mkdirSync(convDir, { recursive: true });

  // Pre-populate compaction limit to verify _db_limits & _last_known_limit remain untouched
  const seedId = 'conv-34';
  createPerfSqliteDb(path.join(convDir, `${seedId}.db`), [
    buildModelPerfChatEntry({
      outputTokens: 1000,
      ttftSec: 10,
      streamSec: 10,
      displayName: 'Gemini 4 Carbon (High)',
      limit: 650000,
    }),
  ]);
  const limitRes = resolveServerCompactionLimit({ conversationId: seedId, roots: [root] });
  assert.equal(limitRes.serverLimit, 650000);

  // Create 35 DBs (conv-0 oldest .. conv-34 newest). Window is 30 -> conv-5..conv-34
  const baseSec = Math.floor(Date.now() / 1000) - 500;
  for (let i = 0; i < 35; i += 1) {
    const dbPath = path.join(convDir, `conv-${i}.db`);
    if (i < 5) {
      // Oldest 5 outside 30-DB window have distinct model 'Argon' that must NOT appear in window
      createPerfSqliteDb(dbPath, [
        buildModelPerfChatEntry({
          outputTokens: 9999,
          ttftSec: 99,
          streamSec: 10,
          displayName: 'Gemini 4 Argon (High)',
        }),
      ]);
    } else if (i !== 34) {
      createPerfSqliteDb(dbPath, [
        buildModelPerfChatEntry({
          outputTokens: 500,
          ttftSec: 5,
          streamSec: 2,
          displayName: 'Gemini 4 Hydrogen (High)',
        }),
      ]);
    }
    const ts = baseSec + i * 10;
    fs.utimesSync(dbPath, ts, ts);
  }

  const res1 = resolveModelPerfStats({ roots: [root] });
  assert.equal(res1.dbCount, MAX_PERF_RECENT_DBS);
  assert.equal(res1.models.Argon, undefined);
  assert.equal(res1.models.Carbon.streamSamples, 1);
  assert.ok(Math.abs(res1.models.Carbon.avgSpeed - 100) < 1e-6);
  assert.ok(Math.abs(res1.models.Carbon.avgTtftSec - 10) < 1e-6);
  assert.equal(res1.models.Hydro.streamSamples, 29);
  assert.ok(Math.abs(res1.models.Hydro.avgSpeed - 250) < 1e-6);
  assert.ok(Math.abs(res1.models.Hydro.avgTtftSec - 5) < 1e-6);

  // Second call with sqliteModule: null and sqlite3Path: null must hit _db_perf cache 100%
  const resCached = resolveModelPerfStats({ roots: [root], sqliteModule: null, sqlite3Path: null });
  assert.equal(resCached.dbCount, MAX_PERF_RECENT_DBS);
  assert.ok(Math.abs(resCached.models.Hydro.avgSpeed - 250) < 1e-6);

  // Active old DB outside top-30 replaces the 30th item in the window
  const resWithActiveOld = resolveModelPerfStats({
    conversationId: 'conv-0',
    roots: [root],
  });
  assert.equal(resWithActiveOld.dbCount, MAX_PERF_RECENT_DBS);
  assert.ok(resWithActiveOld.models.Argon);
  assert.equal(resWithActiveOld.models.Hydro.streamSamples, 28);

  // Create 40 more newer DBs (total 75 > 64) to verify pruning to MAX_PERF_DB_ENTRIES (64)
  for (let j = 35; j < 75; j += 1) {
    const dbPath = path.join(convDir, `conv-${j}.db`);
    createPerfSqliteDb(dbPath, [
      buildModelPerfChatEntry({
        outputTokens: 600,
        ttftSec: 3,
        streamSec: 3,
        displayName: 'Gemini 4 Hydrogen (High)',
      }),
    ]);
    const ts = baseSec + j * 10;
    fs.utimesSync(dbPath, ts, ts);
  }
  resolveModelPerfStats({ roots: [root], maxPerfDbEntries: 64 });

  const cacheFile = path.join(root, 'agy-pulse-compaction-cache.json');
  const cacheData = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
  assert.equal(Object.keys(cacheData._db_perf).length, 64);
  assert.ok(Object.keys(cacheData._db_limits).length <= MAX_DB_CACHE_ENTRIES);
  assert.equal(cacheData._last_known_limit, 650000);
});

test('querySqliteModelPerf falls back to temp copy on SQLITE_CANTOPEN and multi-row sqlite3 CLI', () => {
  const dbPath = path.join(tempDir, 'perf-fallback.db');
  const row1 = buildModelPerfChatEntry({
    outputTokens: 600,
    ttftSec: 2,
    streamSec: 3,
    displayName: 'Gemini 4 Carbon (High)',
  });
  const row2 = buildModelPerfChatEntry({
    outputTokens: 1200,
    ttftSec: 4,
    streamSec: 3,
    displayName: 'Gemini 4 Carbon (High)',
  });
  createPerfSqliteDb(dbPath, [row1, row2]);
  fs.writeFileSync(`${dbPath}-wal`, Buffer.from('wal-bytes'));

  const { DatabaseSync: RealDatabaseSync } = require('node:sqlite');
  let readOnlyAttempts = 0;
  let tempCopyAttempts = 0;
  class MockPerfSqlite {
    constructor(target, opts) {
      if (opts && opts.readOnly) {
        readOnlyAttempts += 1;
        const err = new Error('SQLITE_CANTOPEN: unable to open database file');
        err.code = 'SQLITE_CANTOPEN';
        throw err;
      }
      tempCopyAttempts += 1;
      assert.notEqual(target, dbPath);
      assert.ok(fs.existsSync(`${target}-wal`));
      this.inner = new RealDatabaseSync(target, opts);
    }
    prepare(sql) {
      return this.inner.prepare(sql);
    }
    close() {
      return this.inner.close();
    }
  }

  const viaTemp = querySqliteModelPerf(dbPath, { sqliteModule: { DatabaseSync: MockPerfSqlite } });
  assert.equal(readOnlyAttempts, 1);
  assert.equal(tempCopyAttempts, 1);
  assert.equal(viaTemp.Carbon.streamSamples, 2);
  assert.equal(viaTemp.Carbon.sumOut, 1800);
  assert.equal(viaTemp.Carbon.sumStreamSec, 6);
  assert.equal(viaTemp.Carbon.sumTtftSec, 6);

  // Multi-row sqlite3 CLI fallback (omitting -readonly on temp copy)
  const fakeCli = path.join(tempDir, 'perf-sqlite3.sh');
  fs.writeFileSync(
    fakeCli,
    `#!/bin/sh\nif [ "$1" = "-readonly" ]; then exit 1; fi\nprintf '%s\\n%s\\n' "${row1.toString('hex')}" "${row2.toString('hex')}"\n`,
    { mode: 0o755 }
  );
  const viaCli = querySqliteModelPerf(dbPath, { sqliteModule: null, sqlite3Path: fakeCli });
  assert.equal(viaCli.Carbon.streamSamples, 2);
  assert.equal(viaCli.Carbon.sumOut, 1800);
});

test('extractGenMetadataTelemetry & resolveSessionCostTree compute per-model cost ledgers across main + child DBs and upgrade v1 cache', () => {
  const root = path.join(tempDir, 'agy-cost-tree');
  const convDir = path.join(root, 'conversations');
  fs.mkdirSync(convDir, { recursive: true });

  const mainId = 'main-conv-001';
  const childByLineageId = 'child-lineage-002';
  const childByListId = 'child-listed-003';
  const unrelatedId = 'unrelated-004';

  // Main DB: Carbon step (2000 in, 100k cacheRead, 1000 out) + Opus 5.5 step without durations (500 in, 200 out)
  const mainBlob = Buffer.concat([
    buildModelPerfChatEntry({
      inputTokens: 2_000,
      outputTokens: 1_000,
      cacheReadTokens: 100_000,
      ttftSec: 2,
      streamSec: 5,
      displayName: 'Gemini 4 Carbon (High)',
    }),
    buildModelPerfChatEntry({
      inputTokens: 500,
      outputTokens: 200,
      cacheReadTokens: 0,
      displayName: 'Claude Opus 5.5',
    }),
  ]);
  createPerfSqliteDb(path.join(convDir, `${mainId}.db`), [mainBlob]);

  // Child 1 discovered via root_cascade_id / parent_cascade_id lineage (no childConversationIds needed)
  createPerfSqliteDb(path.join(convDir, `${childByLineageId}.db`), [
    buildModelPerfChatEntry({
      inputTokens: 1_000_000,
      outputTokens: 100_000,
      cacheReadTokens: 2_000_000,
      ttftSec: 1,
      streamSec: 4,
      displayName: 'Gemini 4 Hydrogen (High)',
      parentCascadeId: mainId,
      rootCascadeId: mainId,
    }),
  ]);

  // Child 2 discovered via childConversationIds (even without lineage KV)
  createPerfSqliteDb(path.join(convDir, `${childByListId}.db`), [
    buildModelPerfChatEntry({
      inputTokens: 100_000,
      outputTokens: 10_000,
      cacheReadTokens: 0,
      displayName: 'Gemini 3.8 Flash (Medium)',
    }),
  ]);

  // Unrelated DB must NOT be included in sessionCostTree
  createPerfSqliteDb(path.join(convDir, `${unrelatedId}.db`), [
    buildModelPerfChatEntry({
      inputTokens: 5_000_000,
      outputTokens: 500_000,
      displayName: 'Claude Opus 4.6',
      rootCascadeId: 'someone-else',
    }),
  ]);

  // Seed legacy schemaVer=1 cache entry for mainId without costUsd -> must auto-rescan
  const cachePath = path.join(root, 'agy-pulse-compaction-cache.json');
  const mainDbPath = path.join(convDir, `${mainId}.db`);
  const st = fs.statSync(mainDbPath);
  const sig = `${st.size}:${Math.trunc(st.mtimeMs)}:0:0`;
  fs.writeFileSync(cachePath, JSON.stringify({
    _db_limits: {},
    _db_perf: {
      [mainDbPath]: {
        sig,
        models: { Carbon: { streamSamples: 1, sumOut: 1000, sumStreamSec: 5, ttftSamples: 1, sumTtftSec: 2 } },
        updatedAt: Date.now(),
      },
    },
  }));

  const decoded = extractGenMetadataTelemetry(mainBlob);
  assert.equal(decoded.entries.length, 2);
  assert.equal(decoded.entries[0].inputTokens, 2000);
  assert.equal(decoded.entries[0].cacheReadTokens, 100000);

  const telemetry = querySqliteModelTelemetry(path.join(convDir, `${childByLineageId}.db`));
  assert.equal(telemetry.parentCascadeId, mainId);
  assert.equal(telemetry.rootCascadeId, mainId);

  const tree = resolveSessionCostTree({
    conversationId: mainId,
    childConversationIds: [childByListId],
    roots: [root],
  });
  assert.ok(tree);
  assert.equal(tree.childSessionCount, 2);

  // Expected costs:
  // Carbon: 2k*2/1M + 1k*10/1M + 100k*0.10/1M = 0.004 + 0.01 + 0.01 = 0.024
  // Opus 5.5: 500*4/1M + 200*20/1M = 0.002 + 0.004 = 0.006
  // Hydro: 1M*1.5 + 100k*7.5/1M + 2M*0.075/1M = 1.5 + 0.75 + 0.15 = 2.40
  // Flash (3.8): 100k*0.75/1M + 10k*3.75/1M = 0.075 + 0.0375 = 0.1125
  assert.ok(Math.abs(tree.mainCostUsd - 0.030) < 1e-9);
  assert.ok(Math.abs(tree.childrenCostUsd - 2.5125) < 1e-9);
  assert.ok(Math.abs(tree.totalCostUsd - 2.5425) < 1e-9);

  assert.equal(tree.byModel.Carbon.steps, 1);
  assert.equal(tree.byModel.Carbon.inputTokens, 2000);
  assert.equal(tree.byModel.Carbon.cacheReadTokens, 100000);
  assert.ok(Math.abs(tree.byModel.Carbon.costUsd - 0.024) < 1e-9);
  assert.ok(Math.abs(tree.byModel.Opus.costUsd - 0.006) < 1e-9);
  assert.ok(Math.abs(tree.byModel.Hydro.costUsd - 2.40) < 1e-9);
  assert.ok(Math.abs(tree.byModel.Flash.costUsd - 0.1125) < 1e-9);
  assert.equal(tree.byModel['Opus 4.6'], undefined);

  // Verify cache upgraded to schemaVer 3 and second call hits cache with sqliteModule: null
  const updatedCache = JSON.parse(fs.readFileSync(cachePath, 'utf8'));
  assert.equal(updatedCache._db_perf[mainDbPath].schemaVer, 3);
  const cachedTree = resolveSessionCostTree({
    conversationId: mainId,
    childConversationIds: [childByListId],
    roots: [root],
    sqliteModule: null,
    sqlite3Path: null,
  });
  assert.ok(Math.abs(cachedTree.totalCostUsd - 2.5425) < 1e-9);
});

test('querySqliteCompactionLimit scans LIMIT 16 rows in gen_metadata and executor_metadata', () => {
  const { DatabaseSync } = require('node:sqlite');
  const dbPath = path.join(tempDir, 'limit16.db');
  const db = new DatabaseSync(dbPath);
  try {
    db.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    db.exec('CREATE TABLE executor_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    // Insert valid limit at idx=1, followed by 10 rows without limit (idx=2..11)
    db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(1, buildGenMetadataBlob(768000));
    const emptyBlob = encodeBytesField(1, encodeStringField(19, 'gemini-4-carbon-high'));
    for (let i = 2; i <= 11; i += 1) {
      db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)').run(i, emptyBlob);
    }
  } finally {
    db.close();
  }
  assert.equal(querySqliteCompactionLimit(dbPath), 768000);

  const execDbPath = path.join(tempDir, 'exec-limit16.db');
  const execDb = new DatabaseSync(execDbPath);
  try {
    execDb.exec('CREATE TABLE executor_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
    execDb.prepare('INSERT INTO executor_metadata (idx, data) VALUES (?, ?)').run(1, buildExecutorMetadataBlob(600000));
    const emptyExec = encodeBytesField(1, encodeVarintField(1, 1));
    for (let i = 2; i <= 8; i += 1) {
      execDb.prepare('INSERT INTO executor_metadata (idx, data) VALUES (?, ?)').run(i, emptyExec);
    }
  } finally {
    execDb.close();
  }
  assert.equal(querySqliteCompactionLimit(execDbPath), 600000);
});

test('resolveModelPerfStats filters >600s & >5000 tok/s spikes, skips empty DBs in 30-DB window, protects lineage & uses true LRU (256 cap)', () => {
  assert.equal(MAX_PERF_DB_ENTRIES, 256);

  // 1. Spike filtering: streamSec > 600 and output/streamSec > 5000 are rejected
  const spikeDb = path.join(tempDir, 'spike.db');
  createPerfSqliteDb(spikeDb, [
    buildModelPerfChatEntry({ outputTokens: 1000, streamSec: 601, displayName: 'Gemini 4 Carbon (High)' }),
    buildModelPerfChatEntry({ outputTokens: 5000, streamSec: 0, streamNanos: 100_000_000, displayName: 'Gemini 4 Carbon (High)' }), // 50000 tok/s > 5000
    buildModelPerfChatEntry({ outputTokens: 500, streamSec: 5, displayName: 'Gemini 4 Carbon (High)' }), // valid 100 tok/s
  ]);
  const spikePerf = querySqliteModelPerf(spikeDb);
  assert.equal(spikePerf.Carbon.streamSamples, 1);
  assert.equal(spikePerf.Carbon.sumOut, 500);

  // 2. Non-empty 30-DB window skips empty DBs
  const root = path.join(tempDir, 'agy-non-empty-window');
  const convDir = path.join(root, 'conversations');
  fs.mkdirSync(convDir, { recursive: true });
  const baseSec = Math.floor(Date.now() / 1000) - 500;

  // Create 30 non-empty DBs (ne-0..ne-29) and 5 newer empty DBs (empty-0..empty-4)
  for (let i = 0; i < 30; i += 1) {
    const p = path.join(convDir, `ne-${i}.db`);
    createPerfSqliteDb(p, [
      buildModelPerfChatEntry({ outputTokens: 400, streamSec: 2, ttftSec: 1, displayName: 'Gemini 4 Hydrogen (High)' }),
    ]);
    fs.utimesSync(p, baseSec + i * 5, baseSec + i * 5);
  }
  for (let i = 0; i < 5; i += 1) {
    const p = path.join(convDir, `empty-${i}.db`);
    createConvSqliteDb(p);
    fs.utimesSync(p, baseSec + 300 + i * 5, baseSec + 300 + i * 5);
  }

  const windowRes = resolveModelPerfStats({ roots: [root] });
  assert.equal(windowRes.dbCount, 30);
  assert.equal(windowRes.models.Hydro.streamSamples, 30);

  // 3. True LRU + Grandchild Lineage Protection + matchedCostById / matchedModelsById
  const lruRoot = path.join(tempDir, 'agy-lru-lineage');
  const lruConvDir = path.join(lruRoot, 'conversations');
  fs.mkdirSync(lruConvDir, { recursive: true });

  const mainId = 'main-lru';
  const childId = 'child-lru';
  const grandchildId = 'grandchild-lru';
  createPerfSqliteDb(path.join(lruConvDir, `${mainId}.db`), [
    buildModelPerfChatEntry({ inputTokens: 100_000, outputTokens: 10_000, displayName: 'Gemini 4 Carbon (High)' }),
  ]);
  createPerfSqliteDb(path.join(lruConvDir, `${childId}.db`), [
    buildModelPerfChatEntry({ inputTokens: 100_000, outputTokens: 10_000, displayName: 'Gemini 4 Hydrogen (High)', parentCascadeId: mainId }),
  ]);
  createPerfSqliteDb(path.join(lruConvDir, `${grandchildId}.db`), [
    buildModelPerfChatEntry({ inputTokens: 100_000, outputTokens: 10_000, displayName: 'Gemini 3.8 Flash (Medium)', parentCascadeId: childId }),
  ]);
  // Seed 4 unrelated DBs
  for (let i = 0; i < 4; i += 1) {
    createPerfSqliteDb(path.join(lruConvDir, `other-${i}.db`), [
      buildModelPerfChatEntry({ outputTokens: 200, streamSec: 2, displayName: 'Gemini 4 Argon (High)' }),
    ]);
  }
  const treeRes = resolveModelPerfStats({ conversationId: mainId, roots: [lruRoot], maxPerfDbEntries: 5 });
  assert.ok(treeRes.sessionCostTree);
  assert.equal(treeRes.sessionCostTree.childSessionCount, 2);
  assert.ok(treeRes.sessionCostTree.matchedCostById[childId] > 0);
  assert.ok(treeRes.sessionCostTree.matchedCostById[grandchildId] > 0);
  assert.ok(treeRes.sessionCostTree.matchedModelsById[grandchildId].Flash);

  const lruCache = JSON.parse(fs.readFileSync(path.join(lruRoot, 'agy-pulse-compaction-cache.json'), 'utf8'));
  const cachedKeys = Object.keys(lruCache._db_perf);
  assert.equal(cachedKeys.length, 5);
  assert.ok(cachedKeys.some(k => k.endsWith(`${mainId}.db`)));
  assert.ok(cachedKeys.some(k => k.endsWith(`${childId}.db`)));
  assert.ok(cachedKeys.some(k => k.endsWith(`${grandchildId}.db`)));
});

test('zero-byte .db-wal does not pollute DB mtime ordering, and resolveModelPerfStats computes turnsAgo across sessions', () => {
  const root = path.join(tempDir, 'agy-zero-wal-turns');
  const convDir = path.join(root, 'conversations');
  const brainDir = path.join(root, 'brain');
  fs.mkdirSync(convDir, { recursive: true });
  fs.mkdirSync(brainDir, { recursive: true });

  const nowSec = Math.floor(Date.now() / 1000);
  // Old DB from 8 days ago with Argon, plus a fresh 0-byte -wal file
  const oldArgonDb = path.join(convDir, 'old-argon.db');
  createPerfSqliteDb(oldArgonDb, [
    buildModelPerfChatEntry({ outputTokens: 500, streamSec: 5, ttftSec: 2, displayName: 'Gemini 4 Argon (High)' }),
  ]);
  const eightDaysAgo = nowSec - 8 * 86400;
  fs.utimesSync(oldArgonDb, eightDaysAgo, eightDaysAgo);
  fs.writeFileSync(`${oldArgonDb}-wal`, Buffer.alloc(0)); // 0-byte WAL touched today!

  // 30 newer Hydro DBs (each 2 USER_INPUT turns -> total 60 turns newer than old-argon)
  for (let i = 0; i < 30; i += 1) {
    const id = `recent-${i}`;
    const dbPath = path.join(convDir, `${id}.db`);
    createPerfSqliteDb(dbPath, [
      buildModelPerfChatEntry({ outputTokens: 800, streamSec: 4, ttftSec: 2, displayName: 'Gemini 4 Hydrogen (High)' }),
    ]);
    const t = nowSec - 1000 + i * 10;
    fs.utimesSync(dbPath, t, t);
    const trPath = path.join(brainDir, id, '.system_generated', 'logs', 'transcript.jsonl');
    fs.mkdirSync(path.dirname(trPath), { recursive: true });
    fs.writeFileSync(trPath, [
      JSON.stringify({ step_index: 1, type: 'USER_INPUT' }),
      JSON.stringify({ step_index: 2, type: 'USER_INPUT' }),
      '',
    ].join('\n'));
  }

  // Because 0-byte -wal is ignored, old-argon must NOT enter top-30 perf window!
  const res30 = resolveModelPerfStats({ roots: [root] });
  assert.equal(res30.models.Argon, undefined);
  assert.equal(res30.models.Hydro.turnsAgo, 0);

  // When fewer DBs exist (say current + 2 historical roots totaling 35 turns), Argon enters window with turnsAgo = 35
  const root2 = path.join(tempDir, 'agy-turns-ago');
  const convDir2 = path.join(root2, 'conversations');
  const brainDir2 = path.join(root2, 'brain');
  fs.mkdirSync(convDir2, { recursive: true });
  fs.mkdirSync(brainDir2, { recursive: true });

  const curId = 'cur-session';
  const hist1Id = 'hist-1';
  const hist1ChildId = 'hist-1-child';
  const hist2Id = 'hist-2-flash';

  createPerfSqliteDb(path.join(convDir2, `${curId}.db`), [
    buildModelPerfChatEntry({ outputTokens: 1000, streamSec: 5, displayName: 'Gemini 4 Carbon (High)' }),
  ]);
  createPerfSqliteDb(path.join(convDir2, `${hist1Id}.db`), [
    buildModelPerfChatEntry({ outputTokens: 1000, streamSec: 4, displayName: 'Gemini 4 Hydrogen (High)' }),
  ]);
  // Child DB of hist-1 should merge into hist-1's turn position (0 extra turns)
  createPerfSqliteDb(path.join(convDir2, `${hist1ChildId}.db`), [
    buildModelPerfChatEntry({ outputTokens: 500, streamSec: 2, displayName: 'Claude Opus 4.6', parentCascadeId: hist1Id }),
  ]);
  createPerfSqliteDb(path.join(convDir2, `${hist2Id}.db`), [
    buildModelPerfChatEntry({ outputTokens: 600, streamSec: 2, displayName: 'Gemini 3.8 Flash (Medium)' }),
  ]);

  fs.utimesSync(path.join(convDir2, `${hist2Id}.db`), nowSec - 300, nowSec - 300);
  fs.utimesSync(path.join(convDir2, `${hist1Id}.db`), nowSec - 100, nowSec - 100);
  fs.utimesSync(path.join(convDir2, `${hist1ChildId}.db`), nowSec - 90, nowSec - 90);
  fs.utimesSync(path.join(convDir2, `${curId}.db`), nowSec - 10, nowSec - 10);

  // hist-1 has 30 USER_INPUT turns
  const hist1Tr = path.join(brainDir2, hist1Id, '.system_generated', 'logs', 'transcript.jsonl');
  fs.mkdirSync(path.dirname(hist1Tr), { recursive: true });
  fs.writeFileSync(
    hist1Tr,
    Array.from({ length: 30 }, (_, idx) => JSON.stringify({ step_index: idx + 1, type: 'USER_INPUT' })).join('\n') + '\n'
  );

  const turnsRes = resolveModelPerfStats({
    conversationId: curId,
    currentSessionTurns: 5,
    currentSessionModelTurnsAgo: { Carbon: 0 },
    roots: [root2],
  });
  assert.equal(turnsRes.models.Carbon.turnsAgo, 0);
  assert.equal(turnsRes.models.Hydro.turnsAgo, 5);
  assert.equal(turnsRes.models.Opus.turnsAgo, 5);
  assert.equal(turnsRes.models.Flash.turnsAgo, 35);
});

test('querySqliteModelPerf uses -uri ?immutable=1 when no non-empty -wal exists and falls back to -readonly', () => {
  const dbPath = path.join(tempDir, 'uri-fallback.db');
  const row = buildModelPerfChatEntry({
    outputTokens: 400,
    ttftSec: 1,
    streamSec: 2,
    displayName: 'Gemini 4 Hydrogen (High)',
  });
  createPerfSqliteDb(dbPath, [row]);

  const argsLog = path.join(tempDir, 'cli-args.log');
  const fakeCli = path.join(tempDir, 'uri-sqlite3.sh');
  fs.writeFileSync(
    fakeCli,
    `#!/bin/sh\necho "$*" >> "${argsLog}"\nif [ "$1" = "-uri" ]; then exit 1; fi\necho "${row.toString('hex')}"\n`,
    { mode: 0o755 }
  );

  const perf = querySqliteModelPerf(dbPath, { sqliteModule: null, sqlite3Path: fakeCli });
  assert.equal(perf.Hydro.sumOut, 400);
  const logged = fs.readFileSync(argsLog, 'utf8');
  assert.match(logged, /-uri file:.*\?immutable=1/);
  assert.match(logged, /-readonly /);
});
