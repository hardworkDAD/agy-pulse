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
  decodeVarint,
  pbGetSub,
  pbGetAllSub,
  pbGetVarint,
  extractCompactionLimitFromGenMetadata,
  extractCompactionLimitFromExecutorMetadata,
  querySqliteCompactionLimit,
  resolveServerCompactionLimit,
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
