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

const DEFAULT_COMPACTION_LIMIT = 512000;
const MIN_COMPACTION_LIMIT = 16000;
const MAX_COMPACTION_LIMIT = 10000000;
const MAX_DB_CACHE_ENTRIES = 24;
const MAX_RECENT_DBS = 5;
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
    code.includes('SQLITE_PERM') ||
    code.includes('SQLITE_READONLY') ||
    code === 'EACCES' ||
    code === 'EPERM' ||
    /unable to open|SQLITE_CANTOPEN|readonly|permission|attempt to write/i.test(msg)
  );
}

function queryDatabaseSync(DatabaseSync, targetPath, readOnly) {
  let db = null;
  try {
    db = new DatabaseSync(targetPath, { open: true, readOnly });
    try {
      const row = db.prepare('SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 1').get();
      if (row && row.data) {
        const lim = extractCompactionLimitFromGenMetadata(row.data);
        if (lim !== null) return { ok: true, limit: lim };
      }
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
    }

    try {
      const erow = db.prepare('SELECT data FROM executor_metadata ORDER BY idx DESC LIMIT 1').get();
      if (erow && erow.data) {
        const lim = extractCompactionLimitFromExecutorMetadata(erow.data);
        if (lim !== null) return { ok: true, limit: lim };
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

function runSqliteCliHexQuery(sqlite3Path, dbPath, sql) {
  try {
    const out = execFileSync(sqlite3Path, ['-readonly', dbPath, sql], {
      encoding: 'utf8',
      timeout: 500,
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const hex = (out || '').trim();
    if (!hex || !/^[0-9a-fA-F]+$/.test(hex)) return null;
    return Buffer.from(hex, 'hex');
  } catch {
    return null;
  }
}

function queryWithSqlite3Cli(sqlite3Path, dbPath) {
  const tryTarget = (target) => {
    const genBuf = runSqliteCliHexQuery(
      sqlite3Path,
      target,
      'SELECT hex(data) FROM gen_metadata ORDER BY idx DESC LIMIT 1;'
    );
    if (genBuf) {
      const lim = extractCompactionLimitFromGenMetadata(genBuf);
      if (lim !== null) return lim;
    }

    const execBuf = runSqliteCliHexQuery(
      sqlite3Path,
      target,
      'SELECT hex(data) FROM executor_metadata ORDER BY idx DESC LIMIT 1;'
    );
    if (execBuf) {
      const lim = extractCompactionLimitFromExecutorMetadata(execBuf);
      if (lim !== null) return lim;
    }
    return null;
  };

  const direct = tryTarget(dbPath);
  if (direct !== null) return direct;

  let tmpDir = null;
  try {
    const copy = createTempDbCopy(dbPath);
    tmpDir = copy.tmpDir;
    return tryTarget(copy.tmpDbPath);
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

function extractTranscriptContext(transcriptPath) {
  if (!transcriptPath || typeof transcriptPath !== 'string') {
    return { baseDir: null, conversationId: null };
  }
  const normalized = transcriptPath.replace(/\\/g, '/');
  const marker = '/brain/';
  const pos = normalized.indexOf(marker);
  if (pos === -1) {
    return { baseDir: null, conversationId: null };
  }
  const baseDir = pos > 0 ? path.normalize(normalized.slice(0, pos)) : null;
  const rest = normalized.slice(pos + marker.length);
  const firstSegment = rest.split('/')[0] || null;
  return { baseDir, conversationId: firstSegment };
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

function readCompactionCache(cachePath) {
  try {
    if (!cachePath || !fs.existsSync(cachePath)) {
      return { _db_limits: {} };
    }
    const parsed = JSON.parse(fs.readFileSync(cachePath, 'utf8'));
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return { _db_limits: {} };
    }
    const dbLimits = parsed._db_limits && typeof parsed._db_limits === 'object' && !Array.isArray(parsed._db_limits)
      ? parsed._db_limits
      : {};
    return {
      ...parsed,
      _db_limits: dbLimits,
    };
  } catch {
    return { _db_limits: {} };
  }
}

function pruneDbLimits(dbLimits, maxEntries = MAX_DB_CACHE_ENTRIES) {
  const entries = Object.entries(dbLimits || {});
  if (entries.length <= maxEntries) {
    return { ...dbLimits };
  }
  entries.sort((a, b) => {
    const tA = typeof a[1]?.updatedAt === 'number' ? a[1].updatedAt : 0;
    const tB = typeof b[1]?.updatedAt === 'number' ? b[1].updatedAt : 0;
    return tA - tB;
  });
  const kept = entries.slice(entries.length - maxEntries);
  return Object.fromEntries(kept);
}

function writeCompactionCacheMerged(cachePath, patchDbLimits, lastKnownLimit) {
  if (!cachePath) return;
  let tmpPath = null;
  try {
    const existing = readCompactionCache(cachePath);
    const mergedLimits = pruneDbLimits(
      { ...existing._db_limits, ...patchDbLimits },
      MAX_DB_CACHE_ENTRIES
    );
    const nextCache = {
      ...existing,
      _db_limits: mergedLimits,
    };
    if (isValidCompactionLimit(lastKnownLimit)) {
      nextCache._last_known_limit = Math.trunc(lastKnownLimit);
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

function listRecentConversationDbs(roots, excludePath = null) {
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
            dbs.push({ path: fullPath, mtimeMs: st.mtimeMs });
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
  return dbs.slice(0, MAX_RECENT_DBS).map(item => item.path);
}

/**
 * Resolves the server compaction limit for the active session, using:
 *   1. Active conversation `<convId>.db` (with size+mtimeMs WAL-aware disk cache)
 *   2. `_last_known_limit` from compaction cache on cold start
 *   3. Top 5 most recently modified `conversations/*.db` files
 *   4. `fallbackLimit` (default 512000)
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

  let activeDbPath = null;
  if (conversationId) {
    for (const root of candidateRoots) {
      const candidate = path.join(root, 'conversations', `${conversationId}.db`);
      try {
        if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
          activeDbPath = candidate;
          break;
        }
      } catch {
        // Ignore stat error
      }
    }
  }

  if (activeDbPath) {
    const sig = getDbSignature(activeDbPath);
    const cached = sig ? cache._db_limits[activeDbPath] : null;
    if (cached && cached.sig === sig && isValidCompactionLimit(cached.limit)) {
      return { serverLimit: Math.trunc(cached.limit), source: 'cache' };
    }

    const lim = querySqliteCompactionLimit(activeDbPath, options);
    if (isValidCompactionLimit(lim)) {
      const entry = {
        [activeDbPath]: {
          sig: sig || '',
          limit: Math.trunc(lim),
          updatedAt: Date.now(),
        },
      };
      writeCompactionCacheMerged(cachePath, entry, lim);
      return { serverLimit: Math.trunc(lim), source: 'db' };
    }
  }

  if (isValidCompactionLimit(cache._last_known_limit)) {
    return { serverLimit: Math.trunc(cache._last_known_limit), source: 'cache' };
  }

  const recentDbs = listRecentConversationDbs(candidateRoots, activeDbPath);
  for (const recentDbPath of recentDbs) {
    const sig = getDbSignature(recentDbPath);
    const cached = sig ? cache._db_limits[recentDbPath] : null;
    if (cached && cached.sig === sig && isValidCompactionLimit(cached.limit)) {
      writeCompactionCacheMerged(cachePath, {}, cached.limit);
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
      writeCompactionCacheMerged(cachePath, entry, lim);
      return { serverLimit: Math.trunc(lim), source: 'recent_db' };
    }
  }

  return { serverLimit: fallbackLimit, source: 'fallback' };
}

module.exports = {
  DEFAULT_COMPACTION_LIMIT,
  MIN_COMPACTION_LIMIT,
  MAX_COMPACTION_LIMIT,
  MAX_DB_CACHE_ENTRIES,
  decodeVarint,
  pbGetSub,
  pbGetAllSub,
  pbGetVarint,
  extractCompactionLimitFromGenMetadata,
  extractCompactionLimitFromExecutorMetadata,
  querySqliteCompactionLimit,
  resolveServerCompactionLimit,
};
