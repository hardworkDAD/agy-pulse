'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');
const {
  getAntigravityRoots,
  resolveAntigravityPath,
  resolveSafeExecutable,
} = require('./paths.js');
const { compactModelName } = require('./renderer/format.js');
const { calculateTokenCost } = require('./cost.js');

const DEFAULT_COMPACTION_LIMIT = 512000;
const MIN_COMPACTION_LIMIT = 16000;
const MAX_COMPACTION_LIMIT = 10000000;
const MAX_DB_CACHE_ENTRIES = 24;
const MAX_RECENT_DBS = 5;
const MAX_PERF_RECENT_DBS = 30;
const MAX_PERF_DB_ENTRIES = 256;
const MAX_PERSISTED_CHILD_SPEEDS = 32;
const PERF_DB_SCHEMA_VERSION = 3;
const MIN_VALID_STREAM_SEC = 0.05;
const MAX_VALID_STREAM_SEC = 600;
const MAX_VALID_STREAM_SPEED = 5000;
const MAX_VALID_TTFT_SEC = 600;
const COMPACTION_CACHE_FILENAME = 'agy-pulse-compaction-cache.json';

function toBuffer(buf) {
  if (!buf) return null;
  if (Buffer.isBuffer(buf)) return buf;
  if (buf instanceof Uint8Array) {
    return Buffer.from(buf.buffer, buf.byteOffset, buf.byteLength);
  }
  return null;
}

function isValidCompactionLimit(val) {
  return typeof val === 'number' && Number.isFinite(val) && val >= MIN_COMPACTION_LIMIT && val <= MAX_COMPACTION_LIMIT;
}

/**
 * Decodes an unsigned protobuf Base-128 varint using BigInt shifts so values
 * exceeding 31 bits do not overflow signed 32-bit bitwise operators in JS.
 *
 * @param {Buffer|Uint8Array} buf
 * @param {number} [offset=0]
 * @returns {{ value: number|null, nextOffset: number }}
 */
function decodeVarint(buf, offset = 0) {
  const b = toBuffer(buf);
  if (!b || !Number.isInteger(offset) || offset < 0 || offset >= b.length) {
    return { value: null, nextOffset: offset };
  }

  let res = 0n;
  let shift = 0n;
  let o = offset;
  const n = b.length;

  while (o < n) {
    const byte = b[o++];
    res |= BigInt(byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) {
      return { value: Number(res), nextOffset: o };
    }
    shift += 7n;
    if (shift > 63n) {
      return { value: null, nextOffset: o };
    }
  }

  return { value: null, nextOffset: o };
}

/**
 * Returns the first length-delimited (wire type 2) sub-message for `fieldNum`.
 *
 * @param {Buffer|Uint8Array} buf
 * @param {number} fieldNum
 * @returns {Buffer|null}
 */
function pbGetSub(buf, fieldNum) {
  const b = toBuffer(buf);
  if (!b || !Number.isInteger(fieldNum) || fieldNum <= 0) return null;

  let o = 0;
  const n = b.length;
  while (o < n) {
    const tagDec = decodeVarint(b, o);
    if (tagDec.value === null) break;
    o = tagDec.nextOffset;

    const fn = Math.floor(tagDec.value / 8);
    const wire = tagDec.value % 8;

    if (wire === 0) {
      const vDec = decodeVarint(b, o);
      if (vDec.value === null) break;
      o = vDec.nextOffset;
    } else if (wire === 2) {
      const lenDec = decodeVarint(b, o);
      if (lenDec.value === null || lenDec.value < 0 || lenDec.nextOffset + lenDec.value > n) break;
      o = lenDec.nextOffset;
      if (fn === fieldNum) {
        return b.subarray(o, o + lenDec.value);
      }
      o += lenDec.value;
    } else if (wire === 1) {
      if (o + 8 > n) break;
      o += 8;
    } else if (wire === 5) {
      if (o + 4 > n) break;
      o += 4;
    } else {
      break;
    }
  }
  return null;
}

/**
 * Returns all length-delimited (wire type 2) sub-messages for `fieldNum`.
 *
 * @param {Buffer|Uint8Array} buf
 * @param {number} fieldNum
 * @returns {Buffer[]}
 */
function pbGetAllSub(buf, fieldNum) {
  const b = toBuffer(buf);
  const results = [];
  if (!b || !Number.isInteger(fieldNum) || fieldNum <= 0) return results;

  let o = 0;
  const n = b.length;
  while (o < n) {
    const tagDec = decodeVarint(b, o);
    if (tagDec.value === null) break;
    o = tagDec.nextOffset;

    const fn = Math.floor(tagDec.value / 8);
    const wire = tagDec.value % 8;

    if (wire === 0) {
      const vDec = decodeVarint(b, o);
      if (vDec.value === null) break;
      o = vDec.nextOffset;
    } else if (wire === 2) {
      const lenDec = decodeVarint(b, o);
      if (lenDec.value === null || lenDec.value < 0 || lenDec.nextOffset + lenDec.value > n) break;
      o = lenDec.nextOffset;
      if (fn === fieldNum) {
        results.push(b.subarray(o, o + lenDec.value));
      }
      o += lenDec.value;
    } else if (wire === 1) {
      if (o + 8 > n) break;
      o += 8;
    } else if (wire === 5) {
      if (o + 4 > n) break;
      o += 4;
    } else {
      break;
    }
  }
  return results;
}

/**
 * Returns the first varint (wire type 0) value for `fieldNum`.
 *
 * @param {Buffer|Uint8Array} buf
 * @param {number} fieldNum
 * @returns {number|null}
 */
function pbGetVarint(buf, fieldNum) {
  const b = toBuffer(buf);
  if (!b || !Number.isInteger(fieldNum) || fieldNum <= 0) return null;

  let o = 0;
  const n = b.length;
  while (o < n) {
    const tagDec = decodeVarint(b, o);
    if (tagDec.value === null) break;
    o = tagDec.nextOffset;

    const fn = Math.floor(tagDec.value / 8);
    const wire = tagDec.value % 8;

    if (wire === 0) {
      const vDec = decodeVarint(b, o);
      if (vDec.value === null) break;
      o = vDec.nextOffset;
      if (fn === fieldNum) {
        return vDec.value;
      }
    } else if (wire === 2) {
      const lenDec = decodeVarint(b, o);
      if (lenDec.value === null || lenDec.value < 0 || lenDec.nextOffset + lenDec.value > n) break;
      o = lenDec.nextOffset + lenDec.value;
    } else if (wire === 1) {
      if (o + 8 > n) break;
      o += 8;
    } else if (wire === 5) {
      if (o + 4 > n) break;
      o += 4;
    } else {
      break;
    }
  }
  return null;
}

/**
 * Returns the first UTF-8 string (wire type 2) for `fieldNum`.
 *
 * @param {Buffer|Uint8Array} buf
 * @param {number} fieldNum
 * @returns {string|null}
 */
function pbGetString(buf, fieldNum) {
  const sub = pbGetSub(buf, fieldNum);
  return sub ? sub.toString('utf8') : null;
}

/**
 * Decodes a `google.protobuf.Duration` sub-message at `fieldNum` into seconds.
 *
 * @param {Buffer|Uint8Array} buf
 * @param {number} fieldNum
 * @returns {number|null}
 */
function pbGetDurationSec(buf, fieldNum) {
  const dur = pbGetSub(buf, fieldNum);
  if (!dur) return null;
  const sec = pbGetVarint(dur, 1) ?? 0;
  const nanos = pbGetVarint(dur, 2) ?? 0;
  if (
    typeof sec !== 'number' || !Number.isFinite(sec) || sec < 0 || sec > 1e12 ||
    typeof nanos !== 'number' || !Number.isFinite(nanos) || nanos < 0 || nanos >= 1e9
  ) {
    return null;
  }
  const total = sec + nanos / 1e9;
  return Number.isFinite(total) && total >= 0 ? total : null;
}

/**
 * Extracts per-generation model performance, token usage, and session lineage
 * from a gen_metadata blob:
 * f1[] -> usage (f4.f2 input, f4.f3 output, f4.f5 cacheRead), TTFT (f11),
 * streamingDuration (f12), model identifier (f21 || f19 || f20[model_enum]),
 * and lineage keys (f20[parent_cascade_id], f20[root_cascade_id]).
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {{ entries: Array<Object>, parentCascadeId: string|null, rootCascadeId: string|null }}
 */
function extractGenMetadataTelemetry(buf) {
  const b = toBuffer(buf);
  if (!b) return { entries: [], parentCascadeId: null, rootCascadeId: null };

  const entries = [];
  let parentCascadeId = null;
  let rootCascadeId = null;

  for (const f1 of pbGetAllSub(b, 1)) {
    const modelDisplayName = pbGetString(f1, 21);
    const rawModel = pbGetString(f1, 19);
    const trimmedDisplay = modelDisplayName ? modelDisplayName.trim() : '';
    const trimmedRaw = rawModel ? rawModel.trim() : '';
    let trimmedEnum = '';

    for (const kv of pbGetAllSub(f1, 20)) {
      const k = pbGetString(kv, 1);
      if (!k) continue;
      const v = pbGetString(kv, 2);
      const trimmedVal = v ? v.trim() : '';
      if (!trimmedVal) continue;
      if (k === 'model_enum' && !trimmedEnum) {
        trimmedEnum = trimmedVal;
      } else if (k === 'parent_cascade_id' && !parentCascadeId) {
        parentCascadeId = trimmedVal;
      } else if (k === 'root_cascade_id' && !rootCascadeId) {
        rootCascadeId = trimmedVal;
      }
    }

    const stepModel = trimmedDisplay || trimmedRaw || trimmedEnum;
    const modelKey = compactModelName(stepModel);
    if (!modelKey) continue;

    const usage = pbGetSub(f1, 4);
    const inputTokens = usage ? (pbGetVarint(usage, 2) ?? 0) : 0;
    const outputTokens = usage ? (pbGetVarint(usage, 3) ?? 0) : 0;
    const cacheReadTokens = usage ? (pbGetVarint(usage, 5) ?? 0) : 0;
    const cacheWriteTokens = 0;

    const ttftSec = pbGetDurationSec(f1, 11);
    const streamSec = pbGetDurationSec(f1, 12);

    const hasRawStream = typeof outputTokens === 'number' && Number.isFinite(outputTokens) && outputTokens > 0 &&
      streamSec !== null && Number.isFinite(streamSec) && streamSec > 0;
    const hasRawTtft = ttftSec !== null && Number.isFinite(ttftSec) && ttftSec >= 0;
    const hasAnyToken = inputTokens > 0 || outputTokens > 0 || cacheReadTokens > 0 || cacheWriteTokens > 0;
    if (!hasRawStream && !hasRawTtft && !hasAnyToken) continue;

    entries.push({
      modelKey,
      stepModel,
      inputTokens,
      cacheReadTokens,
      cacheWriteTokens,
      outputTokens,
      streamSec,
      ttftSec,
    });
  }
  return { entries, parentCascadeId, rootCascadeId };
}

/**
 * Extracts per-generation model performance & token usage entries from a gen_metadata blob.
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {Array<{ modelKey: string, stepModel: string, inputTokens: number, cacheReadTokens: number, cacheWriteTokens: number, outputTokens: number, streamSec: number|null, ttftSec: number|null }>}
 */
function extractModelPerfFromGenMetadata(buf) {
  return extractGenMetadataTelemetry(buf).entries;
}

/**
 * Extracts CheckpointConfig.max_context_tokens from gen_metadata blob:
 * f1 -> f9 -> f10 -> f4, clamped to [16000, 10000000].
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {number|null}
 */
function extractCompactionLimitFromGenMetadata(buf) {
  const b = toBuffer(buf);
  if (!b) return null;

  for (const f1 of pbGetAllSub(b, 1)) {
    for (const f9 of pbGetAllSub(f1, 9)) {
      for (const f10 of pbGetAllSub(f9, 10)) {
        const lim = pbGetVarint(f10, 4);
        if (isValidCompactionLimit(lim)) {
          return Math.trunc(lim);
        }
      }
    }
  }
  return null;
}

/**
 * Extracts CheckpointConfig.max_context_tokens from executor_metadata blob:
 * f10 -> f2 -> f5, clamped to [16000, 10000000].
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {number|null}
 */
function extractCompactionLimitFromExecutorMetadata(buf) {
  const b = toBuffer(buf);
  if (!b) return null;

  for (const f10 of pbGetAllSub(b, 10)) {
    for (const f2 of pbGetAllSub(f10, 2)) {
      const lim = pbGetVarint(f2, 5);
      if (isValidCompactionLimit(lim)) {
        return Math.trunc(lim);
      }
    }
  }
  return null;
}

function isSqliteOpenOrAccessError(err) {
  if (!err) return false;
  const code = String(err.code || err.errcode || '');
  const msg = String(err.message || '');
  return (
    code.includes('SQLITE_CANTOPEN') ||
    code.includes('SQLITE_BUSY') ||
    code.includes('SQLITE_LOCKED') ||
    code.includes('SQLITE_IOERR') ||
    code.includes('SQLITE_PERM') ||
    code.includes('SQLITE_READONLY') ||
    code === 'EACCES' ||
    code === 'EPERM' ||
    /unable to open|SQLITE_CANTOPEN|SQLITE_LOCKED|SQLITE_IOERR|readonly|permission|attempt to write|locked/i.test(msg)
  );
}

function queryDatabaseSync(DatabaseSync, targetPath, readOnly) {
  let db = null;
  try {
    db = new DatabaseSync(targetPath, { open: true, readOnly });
    try {
      const rows = db.prepare('SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 16').all();
      for (const row of rows) {
        if (row && row.data) {
          const lim = extractCompactionLimitFromGenMetadata(row.data);
          if (lim !== null) return { ok: true, limit: lim };
        }
      }
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
    }

    try {
      const erows = db.prepare('SELECT data FROM executor_metadata ORDER BY idx DESC LIMIT 16').all();
      for (const erow of erows) {
        if (erow && erow.data) {
          const lim = extractCompactionLimitFromExecutorMetadata(erow.data);
          if (lim !== null) return { ok: true, limit: lim };
        }
      }
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
    }

    return { ok: true, limit: null };
  } finally {
    if (db) {
      try {
        db.close();
      } catch {
        // Ignore close errors
      }
    }
  }
}

function createTempDbCopy(dbPath) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-db-'));
  const baseName = path.basename(dbPath);
  const tmpDbPath = path.join(tmpDir, baseName);
  fs.copyFileSync(dbPath, tmpDbPath);

  const walPath = `${dbPath}-wal`;
  if (fs.existsSync(walPath)) {
    fs.copyFileSync(walPath, `${tmpDbPath}-wal`);
  }

  return { tmpDir, tmpDbPath };
}

function queryWithTempCopy(DatabaseSync, dbPath) {
  let tmpDir = null;
  try {
    const copy = createTempDbCopy(dbPath);
    tmpDir = copy.tmpDir;
    const res = queryDatabaseSync(DatabaseSync, copy.tmpDbPath, false);
    return res.limit;
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // Ignore cleanup failure
      }
    }
  }
}

function queryWithSqlite3Cli(sqlite3Path, dbPath) {
  const tryTarget = (target, readOnly = true) => {
    const genRes = runSqliteCliHexRowsQuery(
      sqlite3Path,
      target,
      'SELECT hex(data) FROM gen_metadata ORDER BY idx DESC LIMIT 16;',
      { readOnly }
    );
    if (genRes.ok && Array.isArray(genRes.buffers)) {
      for (const genBuf of genRes.buffers) {
        const lim = extractCompactionLimitFromGenMetadata(genBuf);
        if (lim !== null) return lim;
      }
    }

    const execRes = runSqliteCliHexRowsQuery(
      sqlite3Path,
      target,
      'SELECT hex(data) FROM executor_metadata ORDER BY idx DESC LIMIT 16;',
      { readOnly }
    );
    if (execRes.ok && Array.isArray(execRes.buffers)) {
      for (const execBuf of execRes.buffers) {
        const lim = extractCompactionLimitFromExecutorMetadata(execBuf);
        if (lim !== null) return lim;
      }
    }
    return null;
  };

  const direct = tryTarget(dbPath, true);
  if (direct !== null) return direct;

  let tmpDir = null;
  try {
    const copy = createTempDbCopy(dbPath);
    tmpDir = copy.tmpDir;
    return tryTarget(copy.tmpDbPath, false);
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // Ignore cleanup failure
      }
    }
  }
}

function loadNodeSqlite(options = {}) {
  if (Object.prototype.hasOwnProperty.call(options, 'sqliteModule')) {
    return options.sqliteModule;
  }
  try {
    return require('node:sqlite');
  } catch {
    return null;
  }
}

/**
 * Reads server-configured CheckpointConfig.max_context_tokens from a conversation
 * SQLite database. Prefers built-in `node:sqlite` (readOnly), falling back to a
 * temporary `.db`/`.db-wal` copy or `sqlite3` CLI if WAL/permissions block open.
 *
 * @param {string} dbPath
 * @param {Object} [options]
 * @returns {number|null}
 */
function querySqliteCompactionLimit(dbPath, options = {}) {
  if (!dbPath || typeof dbPath !== 'string') return null;
  try {
    if (!fs.existsSync(dbPath) || !fs.statSync(dbPath).isFile()) return null;
  } catch {
    return null;
  }

  const sqliteMod = loadNodeSqlite(options);
  if (sqliteMod && typeof sqliteMod.DatabaseSync === 'function') {
    try {
      const res = queryDatabaseSync(sqliteMod.DatabaseSync, dbPath, true);
      if (res.limit !== null) return res.limit;
      // Direct read-only open succeeded without finding a valid limit in either table.
      return null;
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) {
        const copiedLimit = queryWithTempCopy(sqliteMod.DatabaseSync, dbPath);
        if (copiedLimit !== null) return copiedLimit;
      }
    }
  }

  const sqlite3Path = Object.prototype.hasOwnProperty.call(options, 'sqlite3Path')
    ? options.sqlite3Path
    : resolveSafeExecutable('sqlite3', options.env);
  if (sqlite3Path) {
    return queryWithSqlite3Cli(sqlite3Path, dbPath);
  }

  return null;
}

function isNoSuchTableError(err) {
  return Boolean(err && /no such table/i.test(String(err.message || '')));
}

function createEmptyTelemetryState() {
  return {
    models: {},
    parentCascadeId: null,
    rootCascadeId: null,
    totalCostUsd: 0,
  };
}

function accumulateModelTelemetry(state, buf) {
  const decoded = extractGenMetadataTelemetry(buf);
  if (decoded.parentCascadeId && !state.parentCascadeId) {
    state.parentCascadeId = decoded.parentCascadeId;
  }
  if (decoded.rootCascadeId && !state.rootCascadeId) {
    state.rootCascadeId = decoded.rootCascadeId;
  }

  for (const item of decoded.entries) {
    const {
      modelKey,
      stepModel,
      inputTokens,
      cacheReadTokens,
      cacheWriteTokens,
      outputTokens,
      streamSec,
      ttftSec,
    } = item;
    const validStream = (
      typeof outputTokens === 'number' && Number.isFinite(outputTokens) && outputTokens > 0 &&
      streamSec !== null && Number.isFinite(streamSec) &&
      streamSec >= MIN_VALID_STREAM_SEC && streamSec <= MAX_VALID_STREAM_SEC &&
      (outputTokens / streamSec) <= MAX_VALID_STREAM_SPEED
    );
    const validTtft = (
      ttftSec !== null && Number.isFinite(ttftSec) && ttftSec >= 0 && ttftSec <= MAX_VALID_TTFT_SEC
    );
    const hasAnyToken = inputTokens > 0 || outputTokens > 0 || cacheReadTokens > 0 || cacheWriteTokens > 0;
    if (!validStream && !validTtft && !hasAnyToken) continue;

    let acc = state.models[modelKey];
    if (!acc) {
      acc = {
        streamSamples: 0,
        sumOut: 0,
        sumStreamSec: 0,
        ttftSamples: 0,
        sumTtftSec: 0,
        inputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        outputTokens: 0,
        costUsd: 0,
        steps: 0,
      };
      state.models[modelKey] = acc;
    }
    if (validStream) {
      acc.streamSamples += 1;
      acc.sumOut += outputTokens;
      acc.sumStreamSec += streamSec;
    }
    if (validTtft) {
      acc.ttftSamples += 1;
      acc.sumTtftSec += ttftSec;
    }
    if (hasAnyToken) {
      // gen_metadata f4.f2 (inputTokens) and f4.f5 (cacheReadTokens) are strictly mutually exclusive
      const stepCost = calculateTokenCost(stepModel, {
        input_tokens: inputTokens,
        output_tokens: outputTokens,
        cache_read_input_tokens: cacheReadTokens,
        cache_creation_input_tokens: cacheWriteTokens,
      }, { cacheInclusive: false }) || 0;
      acc.inputTokens += inputTokens;
      acc.cacheReadTokens += cacheReadTokens;
      acc.cacheWriteTokens += cacheWriteTokens;
      acc.outputTokens += outputTokens;
      acc.costUsd += stepCost;
      acc.steps += 1;
      state.totalCostUsd += stepCost;
    }
  }
}

function queryDatabasePerfSync(DatabaseSync, targetPath, readOnly) {
  let db = null;
  try {
    db = new DatabaseSync(targetPath, { open: true, readOnly });
    try {
      const rows = db.prepare('SELECT data FROM gen_metadata ORDER BY idx ASC').all();
      const telemetry = createEmptyTelemetryState();
      for (const row of rows) {
        if (row && row.data) {
          accumulateModelTelemetry(telemetry, row.data);
        }
      }
      return { ok: true, telemetry };
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
      if (isNoSuchTableError(err)) return { ok: true, telemetry: createEmptyTelemetryState() };
      return { ok: false, telemetry: null };
    }
  } finally {
    if (db) {
      try {
        db.close();
      } catch {
        // Ignore close errors
      }
    }
  }
}

function queryPerfWithTempCopy(DatabaseSync, dbPath) {
  let tmpDir = null;
  try {
    const copy = createTempDbCopy(dbPath);
    tmpDir = copy.tmpDir;
    const res = queryDatabasePerfSync(DatabaseSync, copy.tmpDbPath, false);
    return res.ok ? res.telemetry : null;
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // Ignore cleanup failure
      }
    }
  }
}

function hasNonEmptyWal(dbPath) {
  try {
    const st = fs.statSync(`${dbPath}-wal`);
    return st.isFile() && st.size > 0;
  } catch {
    return false;
  }
}

function toImmutableSqliteUri(dbPath) {
  const resolved = path.resolve(dbPath).replace(/\\/g, '/');
  const encoded = encodeURI(resolved).replace(/#/g, '%23').replace(/\?/g, '%3f');
  return resolved.startsWith('/') ? `file:${encoded}?immutable=1` : `file:///${encoded}?immutable=1`;
}

function execSqliteCliQuery(sqlite3Path, args) {
  return execFileSync(sqlite3Path, args, {
    encoding: 'utf8',
    timeout: 2000,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
}

function parseSqliteCliHexLines(out) {
  const lines = String(out || '').split(/\r?\n/).map(s => s.trim()).filter(Boolean);
  const buffers = [];
  for (const hex of lines) {
    if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length % 2 !== 0) continue;
    buffers.push(Buffer.from(hex, 'hex'));
  }
  return buffers;
}

function runSqliteCliHexRowsQuery(sqlite3Path, dbPath, sql, options = {}) {
  const readOnly = options.readOnly !== false;
  const attempts = [];
  if (readOnly && !hasNonEmptyWal(dbPath)) {
    attempts.push(['-uri', toImmutableSqliteUri(dbPath), sql]);
  }
  attempts.push(readOnly ? ['-readonly', dbPath, sql] : [dbPath, sql]);

  for (const args of attempts) {
    try {
      const out = execSqliteCliQuery(sqlite3Path, args);
      return { ok: true, buffers: parseSqliteCliHexLines(out) };
    } catch (err) {
      const stderr = String(err?.stderr || '');
      if (/no such table/i.test(stderr)) {
        return { ok: true, buffers: [] };
      }
    }
  }
  return { ok: false, buffers: null };
}

function queryPerfWithSqlite3Cli(sqlite3Path, dbPath) {
  const sql = 'SELECT hex(data) FROM gen_metadata ORDER BY idx ASC;';
  const direct = runSqliteCliHexRowsQuery(sqlite3Path, dbPath, sql, { readOnly: true });
  if (direct.ok) {
    const telemetry = createEmptyTelemetryState();
    for (const buf of direct.buffers) {
      accumulateModelTelemetry(telemetry, buf);
    }
    return telemetry;
  }

  let tmpDir = null;
  try {
    const copy = createTempDbCopy(dbPath);
    tmpDir = copy.tmpDir;
    const copied = runSqliteCliHexRowsQuery(sqlite3Path, copy.tmpDbPath, sql, { readOnly: false });
    if (!copied.ok) return null;
    const telemetry = createEmptyTelemetryState();
    for (const buf of copied.buffers) {
      accumulateModelTelemetry(telemetry, buf);
    }
    return telemetry;
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // Ignore cleanup failure
      }
    }
  }
}

function findTranscriptForDb(dbPath, options = {}) {
  const convId = path.basename(dbPath, '.db');
  if (!convId) return null;
  const directCandidate = path.resolve(path.dirname(dbPath), '..', 'brain', convId, '.system_generated', 'logs', 'transcript.jsonl');
  try {
    if (fs.existsSync(directCandidate) && fs.statSync(directCandidate).isFile()) {
      return directCandidate;
    }
  } catch {}
  if (Array.isArray(options.roots)) {
    for (const root of options.roots) {
      if (!root) continue;
      const candidate = path.join(root, 'brain', convId, '.system_generated', 'logs', 'transcript.jsonl');
      try {
        if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
          return candidate;
        }
      } catch {}
    }
  }
  return null;
}

function countUserInputsInTranscriptFile(transcriptPath) {
  let fd = null;
  try {
    const chunkSize = 64 * 1024;
    const buf = Buffer.allocUnsafe(chunkSize);
    let carry = Buffer.alloc(0);
    let count = 0;
    fd = fs.openSync(transcriptPath, 'r');
    let bytesRead;

    const countLine = (lineBuf) => {
      if (!lineBuf.includes('"USER_INPUT"')) return;
      const str = lineBuf.toString('utf8').trim();
      if (!str) return;
      try {
        const obj = JSON.parse(str);
        if (obj && obj.type === 'USER_INPUT') count += 1;
      } catch {
        if (/"type"\s*:\s*"USER_INPUT"/.test(str)) count += 1;
      }
    };

    while ((bytesRead = fs.readSync(fd, buf, 0, chunkSize, null)) > 0) {
      const slice = buf.subarray(0, bytesRead);
      let start = 0;
      while (start < slice.length) {
        const nlIdx = slice.indexOf(0x0a, start);
        if (nlIdx === -1) {
          const tail = slice.subarray(start);
          if (carry.length + tail.length <= 4 * 1024 * 1024) {
            carry = carry.length > 0 ? Buffer.concat([carry, tail]) : Buffer.from(tail);
          }
          break;
        }
        const part = slice.subarray(start, nlIdx);
        const lineBuf = carry.length > 0 ? Buffer.concat([carry, part]) : part;
        carry = Buffer.alloc(0);
        countLine(lineBuf);
        start = nlIdx + 1;
      }
    }
    if (carry.length > 0) {
      countLine(carry);
    }
    return count;
  } catch {
    return null;
  } finally {
    if (fd !== null) {
      try { fs.closeSync(fd); } catch {}
    }
  }
}

function estimateDbUserTurns(dbPath, telemetry, options = {}) {
  const convId = path.basename(dbPath, '.db');
  const isChildDb = Boolean(
    telemetry.parentCascadeId ||
    (telemetry.rootCascadeId && telemetry.rootCascadeId !== convId)
  );
  let totalSteps = 0;
  for (const stats of Object.values(telemetry.models || {})) {
    if (stats && Number.isFinite(stats.steps) && stats.steps > 0) {
      totalSteps += stats.steps;
    }
  }
  const hasPerf = hasValidModelPerfSamples(telemetry.models);
  const hasActivity = totalSteps > 0 || hasPerf;

  const transcriptPath = findTranscriptForDb(dbPath, options);
  if (transcriptPath) {
    const counted = countUserInputsInTranscriptFile(transcriptPath);
    if (counted !== null) {
      if (counted > 0) return counted;
      return hasActivity ? (isChildDb ? 0 : 1) : 0;
    }
  }

  if (!isChildDb && hasActivity) {
    return Math.max(1, Math.ceil(totalSteps / 4));
  }
  return 0;
}

/**
 * Scans all rows in `gen_metadata` of a conversation SQLite database and
 * aggregates per-model streaming stats, per-model cost/token ledgers, session lineage,
 * and estimated user turns.
 *
 * @param {string} dbPath
 * @param {Object} [options]
 * @returns {{ models: Record<string, Object>, parentCascadeId: string|null, rootCascadeId: string|null, totalCostUsd: number, userTurns: number }|null}
 */
function querySqliteModelTelemetry(dbPath, options = {}) {
  if (!dbPath || typeof dbPath !== 'string') return null;
  try {
    if (!fs.existsSync(dbPath) || !fs.statSync(dbPath).isFile()) return null;
  } catch {
    return null;
  }

  let telemetry = null;
  const sqliteMod = loadNodeSqlite(options);
  if (sqliteMod && typeof sqliteMod.DatabaseSync === 'function') {
    try {
      const res = queryDatabasePerfSync(sqliteMod.DatabaseSync, dbPath, true);
      if (res.ok) telemetry = res.telemetry;
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) {
        const copiedTelemetry = queryPerfWithTempCopy(sqliteMod.DatabaseSync, dbPath);
        if (copiedTelemetry !== null) telemetry = copiedTelemetry;
      }
    }
  }

  if (telemetry === null) {
    const sqlite3Path = Object.prototype.hasOwnProperty.call(options, 'sqlite3Path')
      ? options.sqlite3Path
      : resolveSafeExecutable('sqlite3', options.env);
    if (sqlite3Path) {
      telemetry = queryPerfWithSqlite3Cli(sqlite3Path, dbPath);
    }
  }

  if (telemetry === null) return null;
  telemetry.userTurns = estimateDbUserTurns(dbPath, telemetry, options);
  return telemetry;
}

/**
 * Scans all rows in `gen_metadata` of a conversation SQLite database and
 * aggregates per-model streaming token/duration and TTFT sufficient statistics
 * along with per-model cost and token totals.
 * Returns `{}` for a valid database without records/table, or `null` on query error.
 *
 * @param {string} dbPath
 * @param {Object} [options]
 * @returns {Record<string, Object>|null}
 */
function querySqliteModelPerf(dbPath, options = {}) {
  const telemetry = querySqliteModelTelemetry(dbPath, options);
  return telemetry ? telemetry.models : null;
}

function extractTranscriptContext(transcriptPath) {
  if (!transcriptPath || typeof transcriptPath !== 'string') {
    return { baseDir: null, conversationId: null };
  }
  const normalized = transcriptPath.replace(/\\/g, '/');
  const marker = '/brain/';
  const pos = normalized.lastIndexOf(marker);
  if (pos === -1) {
    return { baseDir: null, conversationId: null };
  }
  const baseDir = pos > 0 ? path.normalize(normalized.slice(0, pos)) : null;
  const rest = normalized.slice(pos + marker.length);
  const firstSegment = rest.split('/')[0] || null;
  return { baseDir, conversationId: firstSegment };
}

function getDbMaxMtimeMs(dbPath) {
  let maxMtime = 0;
  try {
    const st = fs.statSync(dbPath);
    if (st.isFile()) maxMtime = Math.max(maxMtime, st.mtimeMs);
  } catch {
    // Ignore missing/unreadable DB file
  }
  try {
    const walSt = fs.statSync(`${dbPath}-wal`);
    if (walSt.isFile() && walSt.size > 0) maxMtime = Math.max(maxMtime, walSt.mtimeMs);
  } catch {
    // Ignore missing/unreadable WAL file
  }
  return maxMtime;
}

function getDbSignature(dbPath) {
  try {
    const dbStat = fs.statSync(dbPath);
    if (!dbStat.isFile()) return null;
    let walStat = null;
    try {
      const candidateWal = fs.statSync(`${dbPath}-wal`);
      if (candidateWal.isFile()) walStat = candidateWal;
    } catch {
      walStat = null;
    }
    return `${dbStat.size}:${Math.trunc(dbStat.mtimeMs)}:${walStat ? walStat.size : 0}:${walStat ? Math.trunc(walStat.mtimeMs) : 0}`;
  } catch {
    return null;
  }
}

function resolveCompactionCachePath(options = {}) {
  if (Array.isArray(options.roots) && options.roots.length > 0) {
    for (const root of options.roots) {
      if (!root) continue;
      const candidate = path.join(root, COMPACTION_CACHE_FILENAME);
      if (fs.existsSync(candidate)) return candidate;
    }
    const firstValid = options.roots.find(Boolean);
    if (firstValid) return path.join(firstValid, COMPACTION_CACHE_FILENAME);
  }
  return resolveAntigravityPath(COMPACTION_CACHE_FILENAME, options.env, options.homeDir);
}

function sanitizePersistedChildSpeeds(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return {};
  const result = {};
  for (const [key, entry] of Object.entries(raw)) {
    if (!entry || typeof entry !== 'object') continue;
    const model = typeof entry.model === 'string' && entry.model.trim()
      ? entry.model.trim()
      : String(key || '').trim();
    const speed = Number(entry.speed);
    if (!model || !Number.isFinite(speed) || speed <= 0 || speed > MAX_VALID_STREAM_SPEED) continue;
    const updatedAt = Number.isFinite(entry.updatedAt) && entry.updatedAt >= 0 ? entry.updatedAt : 0;
    result[model] = { model, speed, updatedAt };
  }
  return result;
}

function readCompactionCache(cachePath) {
  try {
    if (!cachePath || !fs.existsSync(cachePath)) {
      return { _db_limits: {}, _db_perf: {}, _persisted_child_speeds: {} };
    }
    const parsed = JSON.parse(fs.readFileSync(cachePath, 'utf8'));
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return { _db_limits: {}, _db_perf: {}, _persisted_child_speeds: {} };
    }
    const dbLimits = parsed._db_limits && typeof parsed._db_limits === 'object' && !Array.isArray(parsed._db_limits)
      ? parsed._db_limits
      : {};
    const dbPerf = parsed._db_perf && typeof parsed._db_perf === 'object' && !Array.isArray(parsed._db_perf)
      ? parsed._db_perf
      : {};
    const persistedChildSpeeds = sanitizePersistedChildSpeeds(parsed._persisted_child_speeds);
    return {
      ...parsed,
      _db_limits: dbLimits,
      _db_perf: dbPerf,
      _persisted_child_speeds: persistedChildSpeeds,
    };
  } catch {
    return { _db_limits: {}, _db_perf: {}, _persisted_child_speeds: {} };
  }
}

function getCacheEntryLruTime(entry) {
  if (!entry || typeof entry !== 'object') return 0;
  if (typeof entry.lastAccessAt === 'number' && Number.isFinite(entry.lastAccessAt)) {
    return entry.lastAccessAt;
  }
  if (typeof entry.updatedAt === 'number' && Number.isFinite(entry.updatedAt)) {
    return entry.updatedAt;
  }
  return 0;
}

function pruneCacheEntries(map, maxEntries, protectedKeys = null) {
  const entries = Object.entries(map || {});
  if (entries.length <= maxEntries) {
    return { ...map };
  }
  const byLruAsc = (a, b) => getCacheEntryLruTime(a[1]) - getCacheEntryLruTime(b[1]);
  if (protectedKeys instanceof Set && protectedKeys.size > 0) {
    const protectedEntries = [];
    const evictableEntries = [];
    for (const entry of entries) {
      if (protectedKeys.has(entry[0])) {
        protectedEntries.push(entry);
      } else {
        evictableEntries.push(entry);
      }
    }
    protectedEntries.sort(byLruAsc);
    evictableEntries.sort(byLruAsc);
    const keptProtected = protectedEntries.slice(Math.max(0, protectedEntries.length - maxEntries));
    const remainingSlots = Math.max(0, maxEntries - keptProtected.length);
    const keptEvictable = remainingSlots > 0
      ? evictableEntries.slice(Math.max(0, evictableEntries.length - remainingSlots))
      : [];
    return Object.fromEntries([...keptEvictable, ...keptProtected]);
  }
  entries.sort(byLruAsc);
  const kept = entries.slice(entries.length - maxEntries);
  return Object.fromEntries(kept);
}

function mergePersistedChildSpeeds(existingMap, patchMap) {
  const merged = sanitizePersistedChildSpeeds(existingMap);
  const patch = sanitizePersistedChildSpeeds(patchMap);
  for (const [modelKey, entry] of Object.entries(patch)) {
    const prev = merged[modelKey];
    if (!prev || entry.updatedAt >= prev.updatedAt) {
      merged[modelKey] = entry;
    }
  }
  return pruneCacheEntries(merged, MAX_PERSISTED_CHILD_SPEEDS);
}

function writeCompactionCacheMerged(
  cachePath,
  patchDbLimits,
  lastKnownLimit,
  lastKnownMtime = null,
  patchDbPerf = null,
  patchPersistedChildSpeeds = null,
  cacheOpts = {}
) {
  if (!cachePath) return;
  let tmpPath = null;
  try {
    const existing = readCompactionCache(cachePath);
    const mergedLimits = pruneCacheEntries(
      { ...existing._db_limits, ...(patchDbLimits || {}) },
      MAX_DB_CACHE_ENTRIES
    );
    const nextCache = {
      ...existing,
      _db_limits: mergedLimits,
    };
    const maxPerfDbEntries = Number.isInteger(cacheOpts?.maxPerfDbEntries) && cacheOpts.maxPerfDbEntries > 0
      ? cacheOpts.maxPerfDbEntries
      : MAX_PERF_DB_ENTRIES;
    const protectedPerfKeys = cacheOpts?.protectedPerfKeys instanceof Set ? cacheOpts.protectedPerfKeys : null;
    const hasPerfPatch = patchDbPerf && typeof patchDbPerf === 'object' && Object.keys(patchDbPerf).length > 0;
    const hasExistingPerf = existing._db_perf && Object.keys(existing._db_perf).length > 0;
    if (hasPerfPatch || hasExistingPerf) {
      nextCache._db_perf = pruneCacheEntries(
        { ...existing._db_perf, ...(patchDbPerf || {}) },
        maxPerfDbEntries,
        protectedPerfKeys
      );
    } else {
      delete nextCache._db_perf;
    }
    const mergedChildSpeeds = mergePersistedChildSpeeds(
      existing._persisted_child_speeds,
      patchPersistedChildSpeeds
    );
    if (Object.keys(mergedChildSpeeds).length > 0) {
      nextCache._persisted_child_speeds = mergedChildSpeeds;
    } else {
      delete nextCache._persisted_child_speeds;
    }
    if (isValidCompactionLimit(lastKnownLimit)) {
      nextCache._last_known_limit = Math.trunc(lastKnownLimit);
      if (typeof lastKnownMtime === 'number' && Number.isFinite(lastKnownMtime) && lastKnownMtime >= 0) {
        nextCache._last_known_mtime = lastKnownMtime;
      }
    }

    fs.mkdirSync(path.dirname(cachePath), { recursive: true, mode: 0o700 });
    tmpPath = `${cachePath}.${process.pid}.${Date.now()}.tmp`;
    fs.writeFileSync(tmpPath, JSON.stringify(nextCache), { mode: 0o600 });
    fs.renameSync(tmpPath, cachePath);
    tmpPath = null;
  } catch {
    // Cache persistence is best-effort
  } finally {
    if (tmpPath) {
      try {
        fs.unlinkSync(tmpPath);
      } catch {
        // Ignore cleanup failure
      }
    }
  }
}

function collectCandidateRoots(options, transcriptBaseDir) {
  const baseRoots = Array.isArray(options.roots) && options.roots.length > 0
    ? options.roots.filter(Boolean)
    : getAntigravityRoots(options.env, options.homeDir);

  if (transcriptBaseDir && !baseRoots.includes(transcriptBaseDir)) {
    return [transcriptBaseDir, ...baseRoots];
  }
  return baseRoots;
}

function resolveActiveDbPath(conversationId, candidateRoots) {
  if (!conversationId) return null;
  for (const root of candidateRoots) {
    const candidate = path.join(root, 'conversations', `${conversationId}.db`);
    try {
      if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
        return candidate;
      }
    } catch {
      // Ignore stat error
    }
  }
  return null;
}

function listRecentConversationDbs(roots, excludePath = null, maxDbs = MAX_RECENT_DBS) {
  const dbs = [];
  const seen = new Set();

  for (const root of roots) {
    const convDir = path.join(root, 'conversations');
    try {
      if (!fs.existsSync(convDir)) continue;
      for (const name of fs.readdirSync(convDir)) {
        if (!name.endsWith('.db')) continue;
        const fullPath = path.join(convDir, name);
        if (fullPath === excludePath || seen.has(fullPath)) continue;
        seen.add(fullPath);
        try {
          const st = fs.statSync(fullPath);
          if (st.isFile()) {
            dbs.push({ path: fullPath, mtimeMs: getDbMaxMtimeMs(fullPath) });
          }
        } catch {
          // Ignore unreadable DB file
        }
      }
    } catch {
      // Ignore unreadable directory
    }
  }

  dbs.sort((a, b) => b.mtimeMs - a.mtimeMs);
  return dbs.slice(0, maxDbs);
}

/**
 * Resolves the server compaction limit for the active session, using:
 *   1. Active conversation `<convId>.db` (with size+mtimeMs WAL-aware disk cache)
 *   2. `_last_known_limit` from compaction cache when no newer recent DB exists on disk
 *   3. Top 5 most recently modified `conversations/*.db` files
 *   4. `_last_known_limit` fallback when recent DB scans yield no limit
 *   5. `fallbackLimit` (default 512000)
 *
 * @param {Object} [options]
 * @returns {{ serverLimit: number, source: 'db'|'cache'|'recent_db'|'fallback' }}
 */
function resolveServerCompactionLimit(options = {}) {
  const fallbackLimit = isValidCompactionLimit(options.fallbackLimit)
    ? Math.trunc(options.fallbackLimit)
    : DEFAULT_COMPACTION_LIMIT;

  const { baseDir: transcriptBaseDir, conversationId: derivedConvId } = extractTranscriptContext(
    options.transcriptPath
  );
  const conversationId = options.conversationId || derivedConvId || null;
  const candidateRoots = collectCandidateRoots(options, transcriptBaseDir);
  const cachePath = resolveCompactionCachePath(options);
  const cache = readCompactionCache(cachePath);
  const activeDbPath = resolveActiveDbPath(conversationId, candidateRoots);

  if (activeDbPath) {
    const sig = getDbSignature(activeDbPath);
    const cached = sig ? cache._db_limits[activeDbPath] : null;
    if (cached && cached.sig === sig && isValidCompactionLimit(cached.limit)) {
      return { serverLimit: Math.trunc(cached.limit), source: 'cache' };
    }

    const lim = querySqliteCompactionLimit(activeDbPath, options);
    if (isValidCompactionLimit(lim)) {
      const mtimeMs = getDbMaxMtimeMs(activeDbPath);
      const entry = {
        [activeDbPath]: {
          sig: sig || '',
          limit: Math.trunc(lim),
          updatedAt: Date.now(),
        },
      };
      writeCompactionCacheMerged(cachePath, entry, lim, mtimeMs);
      return { serverLimit: Math.trunc(lim), source: 'db' };
    }
  }

  const recentDbs = listRecentConversationDbs(candidateRoots, activeDbPath);
  const newestRecentMtime = recentDbs.length > 0 ? recentDbs[0].mtimeMs : 0;
  const lastKnownMtime = (
    typeof cache._last_known_mtime === 'number' &&
    Number.isFinite(cache._last_known_mtime) &&
    cache._last_known_mtime >= 0
  )
    ? cache._last_known_mtime
    : 0;

  if (
    isValidCompactionLimit(cache._last_known_limit) &&
    (recentDbs.length === 0 || newestRecentMtime <= lastKnownMtime)
  ) {
    return { serverLimit: Math.trunc(cache._last_known_limit), source: 'cache' };
  }

  for (const { path: recentDbPath, mtimeMs: recentDbMtime } of recentDbs) {
    const sig = getDbSignature(recentDbPath);
    const cached = sig ? cache._db_limits[recentDbPath] : null;
    if (cached && cached.sig === sig && isValidCompactionLimit(cached.limit)) {
      writeCompactionCacheMerged(cachePath, {}, cached.limit, recentDbMtime);
      return { serverLimit: Math.trunc(cached.limit), source: 'recent_db' };
    }

    const lim = querySqliteCompactionLimit(recentDbPath, options);
    if (isValidCompactionLimit(lim)) {
      const entry = {
        [recentDbPath]: {
          sig: sig || '',
          limit: Math.trunc(lim),
          updatedAt: Date.now(),
        },
      };
      writeCompactionCacheMerged(cachePath, entry, lim, recentDbMtime);
      return { serverLimit: Math.trunc(lim), source: 'recent_db' };
    }
  }

  if (isValidCompactionLimit(cache._last_known_limit)) {
    return { serverLimit: Math.trunc(cache._last_known_limit), source: 'cache' };
  }

  return { serverLimit: fallbackLimit, source: 'fallback' };
}

function mergeCostBreakdownModels(target, sourceModels) {
  if (!sourceModels || typeof sourceModels !== 'object') return;
  for (const [modelKey, stats] of Object.entries(sourceModels)) {
    if (!modelKey || !stats || typeof stats !== 'object') continue;
    const costUsd = Number.isFinite(stats.costUsd) && stats.costUsd >= 0 ? stats.costUsd : 0;
    const inputTokens = Number.isFinite(stats.inputTokens) && stats.inputTokens >= 0 ? stats.inputTokens : 0;
    const cacheReadTokens = Number.isFinite(stats.cacheReadTokens) && stats.cacheReadTokens >= 0 ? stats.cacheReadTokens : 0;
    const cacheWriteTokens = Number.isFinite(stats.cacheWriteTokens) && stats.cacheWriteTokens >= 0 ? stats.cacheWriteTokens : 0;
    const outputTokens = Number.isFinite(stats.outputTokens) && stats.outputTokens >= 0 ? stats.outputTokens : 0;
    const steps = Number.isFinite(stats.steps) && stats.steps >= 0 ? stats.steps : 0;
    if (steps === 0 && costUsd === 0 && inputTokens === 0 && cacheReadTokens === 0 && cacheWriteTokens === 0 && outputTokens === 0) {
      continue;
    }
    if (!target[modelKey]) {
      target[modelKey] = {
        costUsd: 0,
        inputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        outputTokens: 0,
        steps: 0,
      };
    }
    target[modelKey].costUsd += costUsd;
    target[modelKey].inputTokens += inputTokens;
    target[modelKey].cacheReadTokens += cacheReadTokens;
    target[modelKey].cacheWriteTokens += cacheWriteTokens;
    target[modelKey].outputTokens += outputTokens;
    target[modelKey].steps += steps;
  }
}

function readPersistedChildSpeeds(options = {}) {
  const cachePath = resolveCompactionCachePath(options);
  return readCompactionCache(cachePath)._persisted_child_speeds || {};
}

function writePersistedChildSpeeds(patchMap, options = {}) {
  const sanitized = sanitizePersistedChildSpeeds(patchMap);
  if (Object.keys(sanitized).length === 0) return;
  const cachePath = resolveCompactionCachePath(options);
  if (!cachePath) return;
  const existing = readCompactionCache(cachePath)._persisted_child_speeds || {};
  let changed = false;
  for (const [modelKey, entry] of Object.entries(sanitized)) {
    const prev = existing[modelKey];
    if (!prev || entry.updatedAt > prev.updatedAt || (entry.updatedAt === prev.updatedAt && Math.abs(entry.speed - prev.speed) > 1e-9)) {
      changed = true;
      break;
    }
  }
  if (!changed) return;
  writeCompactionCacheMerged(cachePath, null, null, null, null, sanitized);
}

function hasValidModelPerfSamples(models) {
  if (!models || typeof models !== 'object') return false;
  for (const stats of Object.values(models)) {
    if (!stats || typeof stats !== 'object') continue;
    const streamSamples = Number.isFinite(stats.streamSamples) && stats.streamSamples > 0 ? stats.streamSamples : 0;
    const ttftSamples = Number.isFinite(stats.ttftSamples) && stats.ttftSamples > 0 ? stats.ttftSamples : 0;
    if (streamSamples > 0 || ttftSamples > 0) return true;
  }
  return false;
}

/**
 * Aggregates per-model pure streaming weighted average speed (sumOut / sumStreamSec),
 * arithmetic average TTFT across the most recent 30 non-empty `conversations/*.db` databases,
 * and (when `conversationId` or `childConversationIds` is provided) the per-model
 * cost breakdown across the session tree (main + all swarm/subagent child DBs),
 * using incremental signature caching in `agy-pulse-compaction-cache.json` (`_db_perf`).
 *
 * @param {Object} [options]
 * @returns {{ models: Record<string, { model: string, avgSpeed: number|null, avgTtftSec: number|null, streamSamples: number, ttftSamples: number, lastUpdatedMs: number }>, dbCount: number, sessionCostTree: Object|null }}
 */
function resolveModelPerfStats(options = {}) {
  const { baseDir: transcriptBaseDir, conversationId: derivedConvId } = extractTranscriptContext(
    options.transcriptPath
  );
  const conversationId = options.conversationId || derivedConvId || null;
  const childIdsSet = new Set(
    Array.isArray(options.childConversationIds)
      ? options.childConversationIds.filter(id => typeof id === 'string' && id && id !== conversationId)
      : []
  );
  const hasSessionScope = Boolean(conversationId || childIdsSet.size > 0);
  const maxPerfDbEntries = Number.isInteger(options.maxPerfDbEntries) && options.maxPerfDbEntries > 0
    ? options.maxPerfDbEntries
    : MAX_PERF_DB_ENTRIES;
  const candidateRoots = collectCandidateRoots(options, transcriptBaseDir);
  const cachePath = resolveCompactionCachePath(options);
  const cache = readCompactionCache(cachePath);
  const activeDbPath = resolveActiveDbPath(conversationId, candidateRoots);
  const allDbs = listRecentConversationDbs(candidateRoots, null, Infinity);

  if (!activeDbPath && allDbs.length === 0 && !hasSessionScope) {
    return { models: {}, dbCount: 0, sessionCostTree: null };
  }

  let maxCachedAccessAt = 0;
  for (const entry of Object.values(cache._db_perf || {})) {
    const ts = getCacheEntryLruTime(entry);
    if (ts > maxCachedAccessAt) maxCachedAccessAt = ts;
  }
  let nextAccessAt = Math.max(Date.now(), maxCachedAccessAt + 1);

  const patchDbPerf = {};
  const loadedByPath = new Map();
  const attemptedPaths = new Set();

  const loadDbRecord = (dbPath, touchLru = false) => {
    if (!dbPath) return null;
    if (loadedByPath.has(dbPath)) {
      const existing = loadedByPath.get(dbPath);
      if (touchLru && existing && getCacheEntryLruTime(existing) < maxCachedAccessAt && !patchDbPerf[dbPath]) {
        const touched = { ...existing, lastAccessAt: nextAccessAt++ };
        loadedByPath.set(dbPath, touched);
        patchDbPerf[dbPath] = touched;
        return touched;
      }
      return existing;
    }
    if (attemptedPaths.has(dbPath)) return null;
    attemptedPaths.add(dbPath);

    const sig = getDbSignature(dbPath);
    const cached = sig ? cache._db_perf?.[dbPath] : null;
    if (
      cached &&
      cached.schemaVer === PERF_DB_SCHEMA_VERSION &&
      cached.sig === sig &&
      cached.models &&
      typeof cached.models === 'object' &&
      !Array.isArray(cached.models)
    ) {
      if (touchLru && getCacheEntryLruTime(cached) < maxCachedAccessAt) {
        const touched = { ...cached, lastAccessAt: nextAccessAt++ };
        loadedByPath.set(dbPath, touched);
        patchDbPerf[dbPath] = touched;
        return touched;
      }
      loadedByPath.set(dbPath, cached);
      return cached;
    }

    const telemetry = querySqliteModelTelemetry(dbPath, options);
    if (telemetry !== null) {
      const nowTs = Date.now();
      const record = {
        schemaVer: PERF_DB_SCHEMA_VERSION,
        sig: sig || '',
        conversationId: path.basename(dbPath, '.db'),
        parentCascadeId: telemetry.parentCascadeId || null,
        rootCascadeId: telemetry.rootCascadeId || null,
        userTurns: Number.isFinite(telemetry.userTurns) && telemetry.userTurns >= 0 ? telemetry.userTurns : 0,
        models: telemetry.models,
        totalCostUsd: telemetry.totalCostUsd,
        updatedAt: nowTs,
        lastAccessAt: nextAccessAt++,
      };
      loadedByPath.set(dbPath, record);
      patchDbPerf[dbPath] = record;
      return record;
    }
    return null;
  };

  // Select up to 30 non-empty DBs (always prioritizing activeDbPath if it has valid samples)
  const perfWindowDbs = [];
  const perfSeenPaths = new Set();

  if (activeDbPath) {
    const activeRecord = loadDbRecord(activeDbPath, true);
    if (activeRecord && hasValidModelPerfSamples(activeRecord.models)) {
      perfWindowDbs.push({ path: activeDbPath, mtimeMs: getDbMaxMtimeMs(activeDbPath) });
      perfSeenPaths.add(activeDbPath);
    }
  }

  for (const item of allDbs) {
    if (perfWindowDbs.length >= MAX_PERF_RECENT_DBS) break;
    if (perfSeenPaths.has(item.path)) continue;
    const record = loadDbRecord(item.path, false);
    if (record && hasValidModelPerfSamples(record.models)) {
      perfWindowDbs.push(item);
      perfSeenPaths.add(item.path);
    }
  }

  const lineageConvIds = new Set();
  if (conversationId) lineageConvIds.add(conversationId);
  for (const id of childIdsSet) lineageConvIds.add(id);

  const expandLineageConvIds = (records) => {
    let added = true;
    while (added) {
      added = false;
      for (const entry of records) {
        if (!entry || typeof entry !== 'object') continue;
        const cid = entry.conversationId;
        if (!cid || lineageConvIds.has(cid)) continue;
        if (
          (entry.rootCascadeId && lineageConvIds.has(entry.rootCascadeId)) ||
          (entry.parentCascadeId && lineageConvIds.has(entry.parentCascadeId))
        ) {
          lineageConvIds.add(cid);
          added = true;
        }
      }
    }
  };

  if (hasSessionScope) {
    if (activeDbPath) loadDbRecord(activeDbPath, true);
    for (const childId of childIdsSet) {
      const childDbPath = resolveActiveDbPath(childId, candidateRoots);
      if (childDbPath) loadDbRecord(childDbPath, true);
    }
    for (const item of allDbs.slice(0, maxPerfDbEntries)) {
      loadDbRecord(item.path, false);
    }

    // Transitively discover all lineage descendants (children, grandchildren, etc.) from cache & loaded records
    let prevSize = -1;
    while (lineageConvIds.size !== prevSize) {
      prevSize = lineageConvIds.size;
      expandLineageConvIds([
        ...Object.values(cache._db_perf || {}),
        ...loadedByPath.values(),
      ]);
      for (const [cachedPath, cachedEntry] of Object.entries(cache._db_perf || {})) {
        if (!cachedEntry || typeof cachedEntry !== 'object') continue;
        if (cachedEntry.conversationId && lineageConvIds.has(cachedEntry.conversationId) && !loadedByPath.has(cachedPath)) {
          try {
            if (fs.existsSync(cachedPath) && fs.statSync(cachedPath).isFile()) {
              loadDbRecord(cachedPath, true);
            }
          } catch {
            // Ignore missing/unreadable DB
          }
        }
      }
      for (const cid of lineageConvIds) {
        const resolvedPath = resolveActiveDbPath(cid, candidateRoots);
        if (resolvedPath && !loadedByPath.has(resolvedPath)) {
          loadDbRecord(resolvedPath, true);
        }
      }
    }
  }

  if (loadedByPath.size === 0 && perfWindowDbs.length === 0) {
    return { models: {}, dbCount: 0, sessionCostTree: null };
  }

  const protectedPerfKeys = new Set();
  if (hasSessionScope && lineageConvIds.size > 0) {
    for (const [dbPath, entry] of Object.entries(cache._db_perf || {})) {
      const cid = entry?.conversationId || path.basename(dbPath, '.db');
      if (cid && lineageConvIds.has(cid)) {
        protectedPerfKeys.add(dbPath);
      }
    }
    for (const [dbPath, entry] of loadedByPath.entries()) {
      const cid = entry?.conversationId || path.basename(dbPath, '.db');
      if (cid && lineageConvIds.has(cid)) {
        protectedPerfKeys.add(dbPath);
      }
    }
  }

  if (Object.keys(patchDbPerf).length > 0) {
    writeCompactionCacheMerged(cachePath, null, null, null, patchDbPerf, null, {
      maxPerfDbEntries,
      protectedPerfKeys,
    });
  }

  const totals = {};
  let validDbCount = 0;
  for (const { path: dbPath, mtimeMs } of perfWindowDbs) {
    const record = loadedByPath.get(dbPath);
    if (!record || !record.models) continue;
    validDbCount += 1;
    const dbMtimeMs = Number.isFinite(mtimeMs) && mtimeMs > 0 ? mtimeMs : 0;

    for (const [modelKey, stats] of Object.entries(record.models)) {
      if (!modelKey || !stats || typeof stats !== 'object') continue;
      const streamSamples = Number.isFinite(stats.streamSamples) && stats.streamSamples > 0 ? stats.streamSamples : 0;
      const sumOut = Number.isFinite(stats.sumOut) && stats.sumOut > 0 ? stats.sumOut : 0;
      const sumStreamSec = Number.isFinite(stats.sumStreamSec) && stats.sumStreamSec > 0 ? stats.sumStreamSec : 0;
      const ttftSamples = Number.isFinite(stats.ttftSamples) && stats.ttftSamples > 0 ? stats.ttftSamples : 0;
      const sumTtftSec = Number.isFinite(stats.sumTtftSec) && stats.sumTtftSec >= 0 ? stats.sumTtftSec : 0;
      if (streamSamples === 0 && ttftSamples === 0) continue;

      if (!totals[modelKey]) {
        totals[modelKey] = {
          streamSamples: 0,
          sumOut: 0,
          sumStreamSec: 0,
          ttftSamples: 0,
          sumTtftSec: 0,
          lastUpdatedMs: 0,
        };
      }
      totals[modelKey].streamSamples += streamSamples;
      totals[modelKey].sumOut += sumOut;
      totals[modelKey].sumStreamSec += sumStreamSec;
      totals[modelKey].ttftSamples += ttftSamples;
      totals[modelKey].sumTtftSec += sumTtftSec;
      if (dbMtimeMs > totals[modelKey].lastUpdatedMs) {
        totals[modelKey].lastUpdatedMs = dbMtimeMs;
      }
    }
  }

  const mtimeByPath = new Map(allDbs.map(d => [d.path, d.mtimeMs]));
  const byConvId = new Map();
  for (const [dbPath, record] of loadedByPath.entries()) {
    if (!record || typeof record !== 'object') continue;
    const cid = record.conversationId || path.basename(dbPath, '.db');
    if (!cid) continue;
    const dbMtimeMs = mtimeByPath.has(dbPath) ? mtimeByPath.get(dbPath) : getDbMaxMtimeMs(dbPath);
    const prev = byConvId.get(cid);
    if (!prev || dbMtimeMs > prev.mtimeMs) {
      byConvId.set(cid, { dbPath, record, mtimeMs: dbMtimeMs });
    }
  }

  const resolveRootKey = (item) => {
    const cid = item.record.conversationId || path.basename(item.dbPath, '.db');
    if (conversationId && lineageConvIds.has(cid)) {
      return { rootKey: conversationId, rootId: conversationId, isOrphan: false };
    }
    if (item.record.rootCascadeId) {
      if (byConvId.has(item.record.rootCascadeId)) {
        return { rootKey: item.record.rootCascadeId, rootId: item.record.rootCascadeId, isOrphan: false };
      }
      return { rootKey: `orphan:${cid}`, rootId: cid, isOrphan: true };
    }
    let curId = cid;
    const visited = new Set([curId]);
    while (true) {
      const curRec = byConvId.get(curId)?.record;
      const parentId = curRec?.parentCascadeId || curRec?.rootCascadeId || null;
      if (!parentId) {
        return { rootKey: curId, rootId: curId, isOrphan: false };
      }
      if (visited.has(parentId)) {
        return { rootKey: curId, rootId: curId, isOrphan: false };
      }
      if (!byConvId.has(parentId)) {
        return { rootKey: `orphan:${cid}`, rootId: cid, isOrphan: true };
      }
      visited.add(parentId);
      curId = parentId;
    }
  };

  const rootNodesMap = new Map();
  for (const item of byConvId.values()) {
    const { rootKey, rootId, isOrphan } = resolveRootKey(item);
    let node = rootNodesMap.get(rootKey);
    if (!node) {
      node = {
        rootId,
        isCurrent: Boolean(conversationId && !isOrphan && rootId === conversationId),
        isOrphan,
        mtimeMs: 0,
        userTurns: 0,
        dbModels: new Set(),
      };
      rootNodesMap.set(rootKey, node);
    }
    if (item.mtimeMs > node.mtimeMs) {
      node.mtimeMs = item.mtimeMs;
    }
    if (!isOrphan && (item.record.conversationId || path.basename(item.dbPath, '.db')) === rootId) {
      node.userTurns = Number.isFinite(item.record.userTurns) && item.record.userTurns >= 0
        ? item.record.userTurns
        : 0;
    }
    for (const [mKey, stats] of Object.entries(item.record.models || {})) {
      if (!mKey || !stats || typeof stats !== 'object') continue;
      const ss = Number.isFinite(stats.streamSamples) && stats.streamSamples > 0 ? stats.streamSamples : 0;
      const ts = Number.isFinite(stats.ttftSamples) && stats.ttftSamples > 0 ? stats.ttftSamples : 0;
      if (ss > 0 || ts > 0) {
        node.dbModels.add(mKey);
      }
    }
  }

  const currentRoot = conversationId ? (rootNodesMap.get(conversationId) || null) : null;
  const historicalRoots = Array.from(rootNodesMap.values())
    .filter(node => node !== currentRoot)
    .sort((a, b) => b.mtimeMs - a.mtimeMs);

  const minTurnsAgo = {};
  let cumulativeTurns = 0;
  const hasCurrentTurnContext = (
    Number.isFinite(options.currentSessionTurns) ||
    (options.currentSessionModelTurnsAgo && typeof options.currentSessionModelTurnsAgo === 'object')
  );

  if (hasCurrentTurnContext) {
    if (options.currentSessionModelTurnsAgo && typeof options.currentSessionModelTurnsAgo === 'object') {
      for (const [mKey, ago] of Object.entries(options.currentSessionModelTurnsAgo)) {
        if (!mKey || !Number.isFinite(ago) || ago < 0) continue;
        minTurnsAgo[mKey] = minTurnsAgo[mKey] === undefined ? ago : Math.min(minTurnsAgo[mKey], ago);
      }
    }
    if (currentRoot) {
      for (const mKey of currentRoot.dbModels) {
        if (minTurnsAgo[mKey] === undefined) {
          minTurnsAgo[mKey] = 0;
        }
      }
    }
    if (Number.isFinite(options.currentSessionTurns) && options.currentSessionTurns > 0) {
      cumulativeTurns += Math.trunc(options.currentSessionTurns);
    }
  } else if (currentRoot) {
    for (const mKey of currentRoot.dbModels) {
      if (minTurnsAgo[mKey] === undefined) {
        minTurnsAgo[mKey] = 0;
      }
    }
    cumulativeTurns += currentRoot.isOrphan ? 0 : Math.max(1, currentRoot.userTurns || 1);
  }

  for (const node of historicalRoots) {
    for (const mKey of node.dbModels) {
      if (minTurnsAgo[mKey] === undefined) {
        minTurnsAgo[mKey] = cumulativeTurns;
      }
    }
    cumulativeTurns += node.isOrphan ? 0 : Math.max(1, node.userTurns || 1);
  }

  const sessionLastActiveMs = (
    options.currentSessionModelLastActiveMs &&
    typeof options.currentSessionModelLastActiveMs === 'object'
  )
    ? options.currentSessionModelLastActiveMs
    : null;

  const models = {};
  for (const [modelKey, agg] of Object.entries(totals)) {
    const rawSpeed = agg.sumStreamSec > 0 && agg.sumOut > 0 ? agg.sumOut / agg.sumStreamSec : null;
    const avgSpeed = rawSpeed !== null && rawSpeed <= MAX_VALID_STREAM_SPEED ? rawSpeed : null;
    const avgTtftSec = agg.ttftSamples > 0 ? agg.sumTtftSec / agg.ttftSamples : null;
    const sessionActive = Number(sessionLastActiveMs?.[modelKey]);
    const lastUpdatedMs = Math.max(
      agg.lastUpdatedMs || 0,
      Number.isFinite(sessionActive) && sessionActive > 0 ? sessionActive : 0
    );
    models[modelKey] = {
      model: modelKey,
      avgSpeed,
      avgTtftSec,
      streamSamples: agg.streamSamples,
      ttftSamples: agg.ttftSamples,
      lastUpdatedMs,
      turnsAgo: Number.isFinite(minTurnsAgo[modelKey]) ? minTurnsAgo[modelKey] : 0,
    };
  }

  let sessionCostTree = null;
  if (hasSessionScope) {
    let mainCostUsd = 0;
    let childrenCostUsd = 0;
    let childSessionCount = 0;
    const byModel = {};
    const mainByModel = {};
    const childrenByModel = {};
    const matchedDbIds = [];
    const matchedCostById = {};
    const matchedModelsById = {};
    const seenConvIds = new Set();

    for (const [dbPath, record] of loadedByPath.entries()) {
      const dbConvId = record.conversationId || path.basename(dbPath, '.db');
      if (!dbConvId || seenConvIds.has(dbConvId)) continue;

      const isMain = Boolean(conversationId && dbConvId === conversationId);
      const isChild = !isMain && (
        lineageConvIds.has(dbConvId) ||
        Boolean(conversationId && (record.rootCascadeId === conversationId || record.parentCascadeId === conversationId))
      );
      if (!isMain && !isChild) continue;

      seenConvIds.add(dbConvId);
      matchedDbIds.push(dbConvId);
      const dbCost = Number.isFinite(record.totalCostUsd) && record.totalCostUsd >= 0 ? record.totalCostUsd : 0;
      matchedCostById[dbConvId] = dbCost;
      matchedModelsById[dbConvId] = record.models || {};

      if (isMain) {
        mainCostUsd += dbCost;
        mergeCostBreakdownModels(mainByModel, record.models);
      } else {
        childSessionCount += 1;
        childrenCostUsd += dbCost;
        mergeCostBreakdownModels(childrenByModel, record.models);
      }
      mergeCostBreakdownModels(byModel, record.models);
    }

    if (matchedDbIds.length > 0) {
      sessionCostTree = {
        totalCostUsd: mainCostUsd + childrenCostUsd,
        mainCostUsd,
        childrenCostUsd,
        childSessionCount,
        byModel,
        mainByModel,
        childrenByModel,
        matchedDbIds,
        matchedCostById,
        matchedModelsById,
        source: 'db',
      };
    }
  }

  return { models, dbCount: validDbCount, sessionCostTree };
}

/**
 * Resolves the session cost tree (main + all swarm/subagent child DBs) for a conversation.
 *
 * @param {Object} [options]
 * @returns {Object|null}
 */
function resolveSessionCostTree(options = {}) {
  return resolveModelPerfStats(options).sessionCostTree;
}

module.exports = {
  DEFAULT_COMPACTION_LIMIT,
  MIN_COMPACTION_LIMIT,
  MAX_COMPACTION_LIMIT,
  MAX_DB_CACHE_ENTRIES,
  MAX_PERF_RECENT_DBS,
  MAX_PERF_DB_ENTRIES,
  PERF_DB_SCHEMA_VERSION,
  MIN_VALID_STREAM_SEC,
  MAX_VALID_STREAM_SEC,
  MAX_VALID_STREAM_SPEED,
  MAX_VALID_TTFT_SEC,
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
  sanitizePersistedChildSpeeds,
  readPersistedChildSpeeds,
  writePersistedChildSpeeds,
};
