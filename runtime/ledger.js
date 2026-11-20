'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');
const {
  getAntigravityRoots,
  getAntigravityIdeRoots,
  resolveAntigravityPath,
  resolveSafeExecutable,
} = require('./paths.js');
const { compactModelName } = require('./renderer/format.js');
const { calculateTokenCost } = require('./cost.js');
const {
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
} = require('./compaction.js');

const DEFAULT_LEDGER_FILENAME = 'agy-pulse-ledger.sqlite';
const FALLBACK_LEDGER_FILENAME = 'agy-pulse-ledger.json';
const SYNC_THROTTLE_MS = 15000;
const LEDGER_SCHEMA_VERSION = '2';
const DAY_MS = 24 * 60 * 60 * 1000;
const HOUR_MS = 60 * 60 * 1000;

// In-process throttle state: `${ledgerPath}::${conversationId}` -> { lastSyncedMs, sig }
const memoryThrottleMap = new Map();

function toBuffer(buf) {
  if (!buf) return null;
  if (Buffer.isBuffer(buf)) return buf;
  if (buf instanceof Uint8Array) {
    return Buffer.from(buf.buffer, buf.byteOffset, buf.byteLength);
  }
  return null;
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

function hasSqliteSupport(options = {}) {
  if (options.forceJson || options.backend === 'json') return false;
  const mod = loadNodeSqlite(options);
  return Boolean(mod && typeof mod.DatabaseSync === 'function');
}

function normalizeSource(value) {
  return String(value || '').trim().toLowerCase() === 'ide' ? 'ide' : 'cli';
}

function normalizeSourceFilter(value) {
  const norm = String(value || '').trim().toLowerCase();
  if (norm === 'cli' || norm === 'ide' || norm === 'all') {
    return norm;
  }
  return 'all';
}

function resolveCandidateRoots(options = {}) {
  if (Array.isArray(options.roots) && options.roots.length > 0) {
    return options.roots.filter(Boolean);
  }
  if (options.dataDir && typeof options.dataDir === 'string') {
    return [options.dataDir];
  }
  return getAntigravityRoots(options.env, options.homeDir);
}

function resolveLedgerSourceRoots(options = {}) {
  const rawEntries = [];
  if (Array.isArray(options.sourceRoots)) {
    for (const item of options.sourceRoots) {
      if (typeof item === 'string') {
        rawEntries.push({ root: item, source: 'cli' });
      } else if (item && typeof item === 'object' && typeof item.root === 'string') {
        rawEntries.push({ root: item.root, source: normalizeSource(item.source) });
      }
    }
  } else {
    const hasExplicitCli = Array.isArray(options.roots) || (typeof options.dataDir === 'string' && options.dataDir.trim() !== '');
    const cliRoots = hasExplicitCli
      ? (Array.isArray(options.roots) ? options.roots.filter(Boolean) : [options.dataDir.trim()])
      : getAntigravityRoots(options.env, options.homeDir);

    const hasExplicitIde = Array.isArray(options.ideRoots) || (typeof options.ideDataDir === 'string' && options.ideDataDir.trim() !== '');
    const ideRoots = hasExplicitIde
      ? (Array.isArray(options.ideRoots) ? options.ideRoots.filter(Boolean) : [options.ideDataDir.trim()])
      : (hasExplicitCli ? [] : getAntigravityIdeRoots(options.env, options.homeDir));

    for (const root of cliRoots) {
      rawEntries.push({ root, source: 'cli' });
    }
    for (const root of ideRoots) {
      rawEntries.push({ root, source: 'ide' });
    }
  }

  const deduped = [];
  const seen = new Set();
  for (const entry of rawEntries) {
    if (!entry || typeof entry.root !== 'string') continue;
    const trimmed = entry.root.trim();
    if (!trimmed) continue;
    const key = path.resolve(trimmed);
    if (seen.has(key)) continue;
    seen.add(key);
    deduped.push({
      root: trimmed,
      source: normalizeSource(entry.source),
    });
  }
  return deduped;
}

/**
 * Resolves the permanent telemetry ledger path, respecting `AGY_PULSE_DATA_DIR`,
 * `options.ledgerPath`, `options.dataDir`, `options.roots`, and backend availability.
 *
 * @param {Object} [options]
 * @returns {string}
 */
function resolveLedgerPath(options = {}) {
  const sqliteAvailable = hasSqliteSupport(options);
  if (typeof options.ledgerPath === 'string' && options.ledgerPath.trim()) {
    const explicit = options.ledgerPath.trim();
    if (!sqliteAvailable && /\.sqlite$/i.test(explicit)) {
      return explicit.replace(/\.sqlite$/i, '.json');
    }
    return explicit;
  }

  const filename = sqliteAvailable ? DEFAULT_LEDGER_FILENAME : FALLBACK_LEDGER_FILENAME;
  if (typeof options.dataDir === 'string' && options.dataDir.trim()) {
    return path.join(options.dataDir.trim(), filename);
  }
  if (Array.isArray(options.roots) && options.roots.length > 0) {
    for (const root of options.roots) {
      if (!root) continue;
      const candidate = path.join(root, filename);
      try {
        if (fs.existsSync(candidate)) return candidate;
      } catch {
        // Ignore stat error
      }
    }
    const firstRoot = options.roots.find(Boolean);
    if (firstRoot) return path.join(firstRoot, filename);
  }
  if (Array.isArray(options.sourceRoots) && options.sourceRoots.length > 0) {
    const cliRoots = options.sourceRoots
      .filter(e => e && typeof e.root === 'string' && e.root.trim() && normalizeSource(e.source) === 'cli')
      .map(e => e.root.trim());
    for (const root of cliRoots) {
      const candidate = path.join(root, filename);
      try {
        if (fs.existsSync(candidate)) return candidate;
      } catch {
        // Ignore stat error
      }
    }
    if (cliRoots.length > 0) return path.join(cliRoots[0], filename);
  }
  return resolveAntigravityPath(filename, options.env, options.homeDir);
}

function resolveBackendKind(ledgerPath, options = {}) {
  if (options.forceJson || options.backend === 'json' || /\.json$/i.test(ledgerPath)) {
    return 'json';
  }
  return hasSqliteSupport(options) ? 'sqlite' : 'json';
}

function pad2(n) {
  return String(n).padStart(2, '0');
}

function formatLocalDateStr(ms) {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

function parseDateInput(val, isEnd) {
  if (val === undefined || val === null || val === '') return null;
  if (typeof val === 'number') {
    return Number.isFinite(val) ? val : null;
  }
  const str = String(val).trim();
  if (!str) return null;
  if (/^\d{10,13}$/.test(str)) {
    const num = Number(str);
    return Number.isFinite(num) ? num : null;
  }
  const ymd = /^(\d{4})-(\d{2})-(\d{2})$/.exec(str);
  if (ymd) {
    const y = Number(ymd[1]);
    const m = Number(ymd[2]) - 1;
    const d = Number(ymd[3]);
    const dt = isEnd
      ? new Date(y, m, d, 23, 59, 59, 999)
      : new Date(y, m, d, 0, 0, 0, 0);
    const ms = dt.getTime();
    return Number.isFinite(ms) ? ms : null;
  }
  const parsed = Date.parse(str);
  return Number.isFinite(parsed) ? parsed : null;
}

/**
 * Resolves a human-friendly time range specification into concrete `[fromMs, toMs]` bounds.
 *
 * @param {Object} [params]
 * @returns {{ range: string, fromMs: number, toMs: number, label: string }}
 */
function resolveTimeRange({ range = '7d', from = null, to = null, now = Date.now() } = {}) {
  const nowMs = Number.isFinite(now) ? now : Date.now();
  const parsedFrom = parseDateInput(from, false);
  const parsedTo = parseDateInput(to, true);

  if (parsedFrom !== null || parsedTo !== null) {
    const fromMs = parsedFrom !== null ? parsedFrom : 0;
    const toMs = parsedTo !== null ? parsedTo : nowMs;
    return {
      range: 'custom',
      fromMs,
      toMs,
      label: `${parsedFrom !== null ? formatLocalDateStr(fromMs) : '...'} ~ ${formatLocalDateStr(toMs)}`,
    };
  }

  const normRange = typeof range === 'string' && range.trim() ? range.trim().toLowerCase() : '7d';
  const nowDate = new Date(nowMs);

  switch (normRange) {
    case 'today': {
      const start = new Date(nowDate.getFullYear(), nowDate.getMonth(), nowDate.getDate(), 0, 0, 0, 0).getTime();
      const end = new Date(nowDate.getFullYear(), nowDate.getMonth(), nowDate.getDate(), 23, 59, 59, 999).getTime();
      return { range: 'today', fromMs: start, toMs: end, label: 'Today' };
    }
    case '24h':
      return { range: '24h', fromMs: nowMs - DAY_MS, toMs: nowMs, label: 'Last 24h' };
    case '30d':
      return { range: '30d', fromMs: nowMs - 30 * DAY_MS, toMs: nowMs, label: 'Last 30d' };
    case 'month': {
      const start = new Date(nowDate.getFullYear(), nowDate.getMonth(), 1, 0, 0, 0, 0).getTime();
      return { range: 'month', fromMs: start, toMs: nowMs, label: 'This Month' };
    }
    case 'all':
      return { range: 'all', fromMs: 0, toMs: nowMs, label: 'All Time' };
    case 'custom':
      return { range: 'custom', fromMs: nowMs - 7 * DAY_MS, toMs: nowMs, label: 'Custom' };
    case '7d':
    default:
      return { range: '7d', fromMs: nowMs - 7 * DAY_MS, toMs: nowMs, label: 'Last 7d' };
  }
}

/**
 * Decodes a `google.protobuf.Timestamp` submessage (`{ tag 1: seconds, tag 2: nanos }`)
 * into epoch milliseconds.
 *
 * @param {Buffer|Uint8Array|null} tsSub
 * @returns {number|null}
 */
function decodeProtobufTimestampMs(tsSub) {
  if (!tsSub) return null;
  const sec = pbGetVarint(tsSub, 1);
  if (typeof sec !== 'number' || !Number.isFinite(sec) || sec <= 0 || sec > 1e12) {
    return null;
  }
  const nanos = pbGetVarint(tsSub, 2) ?? 0;
  if (typeof nanos !== 'number' || !Number.isFinite(nanos) || nanos < 0 || nanos >= 1e9) {
    return null;
  }
  return sec * 1000 + Math.floor(nanos / 1e6);
}

/**
 * Extracts step timestamp (epoch ms) from `steps.metadata` protobuf (`tag 1` Timestamp).
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {number|null}
 */
function extractStepTimestampMs(buf) {
  const b = toBuffer(buf);
  if (!b) return null;
  return decodeProtobufTimestampMs(pbGetSub(b, 1));
}

function normalizeWorkspaceUri(raw) {
  if (!raw || typeof raw !== 'string') return null;
  let s = raw.trim();
  if (!s) return null;
  if (/^file:\/\//i.test(s)) {
    s = s.replace(/^file:\/\/\/([A-Za-z]:)/, '$1').replace(/^file:\/\//i, '');
  }
  try {
    s = decodeURIComponent(s);
  } catch {
    // Keep raw string when percent-encoding is malformed
  }
  if (s.length > 1 && !/^[A-Za-z]:\/?$/.test(s)) {
    s = s.replace(/\/+$/, '');
  }
  return s || null;
}

/**
 * Extracts workspace path, git branch, creation timestamp, and cascade ID
 * from `trajectory_metadata_blob` (`WHERE id = 'main'`).
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {{ workspace: string|null, branch: string|null, createdAtMs: number|null, cascadeId: string|null }}
 */
function extractTrajectoryMeta(buf) {
  const empty = { workspace: null, branch: null, createdAtMs: null, cascadeId: null };
  const b = toBuffer(buf);
  if (!b) return empty;

  let workspace = null;
  let branch = null;
  for (const w of pbGetAllSub(b, 1)) {
    if (!workspace) {
      const wsUri = pbGetString(w, 1) || pbGetString(w, 2);
      workspace = normalizeWorkspaceUri(wsUri);
    }
    if (!branch) {
      const br = pbGetString(w, 4);
      if (br && br.trim()) branch = br.trim();
    }
  }
  if (!workspace) {
    workspace = normalizeWorkspaceUri(pbGetString(b, 7));
  }

  const createdAtMs = decodeProtobufTimestampMs(pbGetSub(b, 2));
  const rawCascade = pbGetString(b, 6);
  const cascadeId = rawCascade && rawCascade.trim() ? rawCascade.trim() : null;

  return { workspace, branch, createdAtMs, cascadeId };
}

/**
 * Extracts `step_indices` from top-level `tag 2` (packed or unpacked varints)
 * in a `gen_metadata` protobuf blob.
 *
 * @param {Buffer|Uint8Array} buf
 * @returns {number[]}
 */
function extractStepIndicesFromGenMetadata(buf) {
  const b = toBuffer(buf);
  if (!b) return [];

  const indices = [];
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
      if (fn === 2 && Number.isSafeInteger(vDec.value) && vDec.value >= 0) {
        indices.push(vDec.value);
      }
    } else if (wire === 2) {
      const lenDec = decodeVarint(b, o);
      if (lenDec.value === null || lenDec.value < 0 || lenDec.nextOffset + lenDec.value > n) break;
      o = lenDec.nextOffset;
      if (fn === 2) {
        const sub = b.subarray(o, o + lenDec.value);
        let so = 0;
        while (so < sub.length) {
          const item = decodeVarint(sub, so);
          if (item.value === null) break;
          so = item.nextOffset;
          if (Number.isSafeInteger(item.value) && item.value >= 0) {
            indices.push(item.value);
          }
        }
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
  return indices;
}

function extractGenMetadataRowDetails(buf) {
  const b = toBuffer(buf);
  if (!b) {
    return { entries: [], stepIndices: [], parentCascadeId: null, rootCascadeId: null, cascadeId: null };
  }

  const stepIndices = extractStepIndicesFromGenMetadata(b);
  const entries = [];
  let parentCascadeId = null;
  let rootCascadeId = null;
  let cascadeId = null;

  for (const f1 of pbGetAllSub(b, 1)) {
    const modelDisplayName = pbGetString(f1, 21);
    const rawModel = pbGetString(f1, 19);
    const trimmedDisplay = modelDisplayName ? modelDisplayName.trim() : '';
    const trimmedRaw = rawModel ? rawModel.trim() : '';
    let trimmedEnum = '';
    let lastStepIndex = null;

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
      } else if (k === 'cascade_id' && !cascadeId) {
        cascadeId = trimmedVal;
      } else if (k === 'last_step_index' && lastStepIndex === null) {
        const parsed = Number.parseInt(trimmedVal, 10);
        if (Number.isInteger(parsed) && parsed >= 0) {
          lastStepIndex = parsed;
        }
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
    const rawBotUuid = usage ? pbGetString(usage, 7) : null;
    const botUuid = rawBotUuid && rawBotUuid.trim() ? rawBotUuid.trim() : null;

    const ttftSec = pbGetDurationSec(f1, 11);
    const streamSec = pbGetDurationSec(f1, 12);

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
    if (!hasAnyToken) continue;

    entries.push({
      modelKey,
      stepModel,
      inputTokens,
      cacheReadTokens,
      cacheWriteTokens,
      outputTokens,
      streamSec,
      ttftSec,
      validStream,
      validTtft,
      lastStepIndex,
      botUuid,
    });
  }

  return { entries, stepIndices, parentCascadeId, rootCascadeId, cascadeId };
}

function getDbMaxMtimeMs(dbPath) {
  let maxMtime = 0;
  try {
    const st = fs.statSync(dbPath);
    if (st.isFile()) maxMtime = Math.max(maxMtime, st.mtimeMs);
  } catch {
    // Ignore
  }
  try {
    const walSt = fs.statSync(`${dbPath}-wal`);
    if (walSt.isFile() && walSt.size > 0) maxMtime = Math.max(maxMtime, walSt.mtimeMs);
  } catch {
    // Ignore
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

function isSqliteOpenOrAccessError(err) {
  const msg = String(err?.message || '').toLowerCase();
  const code = String(err?.errcode || err?.code || '').toLowerCase();
  return (
    code.includes('cantopen') ||
    code.includes('busy') ||
    code.includes('locked') ||
    code.includes('perm') ||
    msg.includes('unable to open') ||
    msg.includes('readonly') ||
    msg.includes('database is locked') ||
    msg.includes('busy')
  );
}

function isNoSuchTableError(err) {
  return Boolean(err && /no such table/i.test(String(err.message || '')));
}

function createTempDbCopy(dbPath) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-ledger-db-'));
  const baseName = path.basename(dbPath);
  const tmpDbPath = path.join(tmpDir, baseName);
  fs.copyFileSync(dbPath, tmpDbPath);

  for (const suffix of ['-wal', '-shm']) {
    const srcSidecar = `${dbPath}${suffix}`;
    if (fs.existsSync(srcSidecar)) {
      fs.copyFileSync(srcSidecar, `${tmpDbPath}${suffix}`);
    }
  }
  return { tmpDir, tmpDbPath };
}

function querySourceDbWithDatabaseSync(DatabaseSync, targetPath, readOnly) {
  let db = null;
  try {
    db = new DatabaseSync(targetPath, { open: true, readOnly });

    let trajectoryMeta = { workspace: null, branch: null, createdAtMs: null, cascadeId: null };
    try {
      let tRow = db.prepare("SELECT * FROM trajectory_metadata_blob WHERE id = 'main' LIMIT 1").get();
      if (!tRow) {
        tRow = db.prepare('SELECT * FROM trajectory_metadata_blob LIMIT 1').get();
      }
      if (tRow) {
        const blob = tRow.data ?? tRow.blob ?? tRow.metadata ?? tRow.value ?? null;
        if (blob) trajectoryMeta = extractTrajectoryMeta(blob);
      }
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
      if (!isNoSuchTableError(err)) {
        // Ignore unexpected schema variation on optional metadata
      }
    }

    const stepTsMap = new Map();
    const botTsMap = new Map();
    try {
      const stepRows = db.prepare('SELECT idx, metadata FROM steps').all();
      for (const sRow of stepRows) {
        if (!sRow || !sRow.metadata) continue;
        const tsMs = extractStepTimestampMs(sRow.metadata);
        if (tsMs !== null) {
          if (Number.isInteger(sRow.idx)) {
            stepTsMap.set(sRow.idx, tsMs);
          }
          const sub9 = pbGetSub(sRow.metadata, 9);
          const botUuid = sub9 ? pbGetString(sub9, 7) : null;
          if (botUuid && botUuid.trim() && !botTsMap.has(botUuid.trim())) {
            botTsMap.set(botUuid.trim(), tsMs);
          }
        }
      }
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
      if (!isNoSuchTableError(err)) {
        // Ignore missing optional columns
      }
    }

    const genRows = [];
    try {
      const rows = db.prepare('SELECT idx, data FROM gen_metadata ORDER BY idx ASC').all();
      for (const row of rows) {
        if (row && row.data) {
          genRows.push({ idx: row.idx, data: row.data });
        }
      }
    } catch (err) {
      if (isSqliteOpenOrAccessError(err)) throw err;
      if (!isNoSuchTableError(err)) {
        return { ok: false, payload: null };
      }
    }

    return {
      ok: true,
      payload: { trajectoryMeta, stepTsMap, botTsMap, genRows },
    };
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

function querySourceDbWithSqlite3Cli(sqlite3Path, dbPath) {
  const runHex = (target, argsPrefix, sql) => {
    try {
      const out = execFileSync(sqlite3Path, [...argsPrefix, target, sql], {
        encoding: 'utf8',
        timeout: 2500,
        maxBuffer: 64 * 1024 * 1024,
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      return { ok: true, out };
    } catch (err) {
      const stderr = String(err?.stderr || '');
      if (/no such table/i.test(stderr)) return { ok: true, out: '' };
      return { ok: false, out: '' };
    }
  };

  const runOnTarget = (target, readOnly) => {
    const prefix = readOnly
      ? (hasNonEmptyWal(target) ? ['-readonly'] : ['-uri'])
      : [];
    const argTarget = prefix[0] === '-uri' ? toImmutableSqliteUri(target) : target;

    const gmRes = runHex(argTarget, prefix, "SELECT idx || '|' || hex(data) FROM gen_metadata ORDER BY idx ASC;");
    if (!gmRes.ok) return null;

    const trajRes = runHex(argTarget, prefix, "SELECT hex(data) FROM trajectory_metadata_blob ORDER BY (id = 'main') DESC LIMIT 1;");
    const stepsRes = runHex(argTarget, prefix, "SELECT idx || '|' || hex(metadata) FROM steps;");

    let trajectoryMeta = { workspace: null, branch: null, createdAtMs: null, cascadeId: null };
    if (trajRes.ok && trajRes.out.trim()) {
      const hexLine = trajRes.out.trim().split(/\r?\n/)[0]?.trim();
      if (hexLine && /^[0-9a-fA-F]+$/.test(hexLine)) {
        trajectoryMeta = extractTrajectoryMeta(Buffer.from(hexLine, 'hex'));
      }
    }

    const stepTsMap = new Map();
    const botTsMap = new Map();
    if (stepsRes.ok && stepsRes.out) {
      for (const line of stepsRes.out.split(/\r?\n/)) {
        const sep = line.indexOf('|');
        if (sep <= 0) continue;
        const idx = Number.parseInt(line.slice(0, sep), 10);
        const hex = line.slice(sep + 1).trim();
        if (!hex || !/^[0-9a-fA-F]+$/.test(hex)) continue;
        const buf = Buffer.from(hex, 'hex');
        const tsMs = extractStepTimestampMs(buf);
        if (tsMs !== null) {
          if (Number.isInteger(idx)) stepTsMap.set(idx, tsMs);
          const sub9 = pbGetSub(buf, 9);
          const botUuid = sub9 ? pbGetString(sub9, 7) : null;
          if (botUuid && botUuid.trim() && !botTsMap.has(botUuid.trim())) {
            botTsMap.set(botUuid.trim(), tsMs);
          }
        }
      }
    }

    const genRows = [];
    for (const line of gmRes.out.split(/\r?\n/)) {
      const sep = line.indexOf('|');
      if (sep <= 0) continue;
      const idx = Number.parseInt(line.slice(0, sep), 10);
      const hex = line.slice(sep + 1).trim();
      if (!hex || !/^[0-9a-fA-F]+$/.test(hex)) continue;
      genRows.push({ idx, data: Buffer.from(hex, 'hex') });
    }

    return { trajectoryMeta, stepTsMap, botTsMap, genRows };
  };

  if (!hasNonEmptyWal(dbPath)) {
    const direct = runOnTarget(dbPath, true);
    if (direct) return direct;
  }

  let tmpDir = null;
  try {
    const copy = createTempDbCopy(dbPath);
    tmpDir = copy.tmpDir;
    return runOnTarget(copy.tmpDbPath, false);
  } catch {
    return null;
  } finally {
    if (tmpDir) {
      try {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      } catch {
        // Ignore cleanup errors
      }
    }
  }
}

function readSourceConversationDb(dbPath, options = {}) {
  const sqliteMod = options.sourceSqliteModule !== undefined
    ? options.sourceSqliteModule
    : (Object.prototype.hasOwnProperty.call(options, 'sqliteModule') ? options.sqliteModule : loadNodeSqlite());

  if (sqliteMod && typeof sqliteMod.DatabaseSync === 'function') {
    if (!hasNonEmptyWal(dbPath)) {
      try {
        const res = querySourceDbWithDatabaseSync(sqliteMod.DatabaseSync, toImmutableSqliteUri(dbPath), true);
        if (res.ok) return res.payload;
      } catch (err) {
        if (!isSqliteOpenOrAccessError(err)) return null;
      }
    }

    let tmpDir = null;
    try {
      const copy = createTempDbCopy(dbPath);
      tmpDir = copy.tmpDir;
      try {
        const res = querySourceDbWithDatabaseSync(sqliteMod.DatabaseSync, copy.tmpDbPath, true);
        if (res.ok) return res.payload;
      } catch (err) {
        if (!isSqliteOpenOrAccessError(err)) return null;
      }
      const resRw = querySourceDbWithDatabaseSync(sqliteMod.DatabaseSync, copy.tmpDbPath, false);
      return resRw.ok ? resRw.payload : null;
    } catch {
      return null;
    } finally {
      if (tmpDir) {
        try {
          fs.rmSync(tmpDir, { recursive: true, force: true });
        } catch {
          // Ignore cleanup errors
        }
      }
    }
  }

  const sqlite3Path = Object.prototype.hasOwnProperty.call(options, 'sqlite3Path')
    ? options.sqlite3Path
    : resolveSafeExecutable('sqlite3', options.env);
  if (sqlite3Path) {
    return querySourceDbWithSqlite3Cli(sqlite3Path, dbPath);
  }
  return null;
}

function resolveStepTimestamp(entry, stepIndices, stepTsMap, botTsMap, fallbackTsMs) {
  let resolvedStepIndex = null;
  if (Array.isArray(stepIndices) && stepIndices.length > 0) {
    resolvedStepIndex = stepIndices[stepIndices.length - 1];
    for (let i = stepIndices.length - 1; i >= 0; i -= 1) {
      const idx = stepIndices[i];
      if (stepTsMap.has(idx)) {
        return { tsMs: stepTsMap.get(idx), stepIndex: idx };
      }
    }
  }
  if (entry.lastStepIndex !== null) {
    if (resolvedStepIndex === null) resolvedStepIndex = entry.lastStepIndex;
    if (stepTsMap.has(entry.lastStepIndex)) {
      return { tsMs: stepTsMap.get(entry.lastStepIndex), stepIndex: entry.lastStepIndex };
    }
  }
  if (entry.botUuid && botTsMap.has(entry.botUuid)) {
    return { tsMs: botTsMap.get(entry.botUuid), stepIndex: resolvedStepIndex };
  }
  return { tsMs: fallbackTsMs, stepIndex: resolvedStepIndex };
}

function buildConversationIngestionRecords(conversationId, dbPath, sig, nowMs, options = {}) {
  const payload = readSourceConversationDb(dbPath, options);
  if (!payload) return null;

  const source = normalizeSource(options.source);
  const dbMtimeMs = Math.trunc(getDbMaxMtimeMs(dbPath)) || nowMs;
  const { trajectoryMeta, stepTsMap, botTsMap, genRows } = payload;
  const fallbackTsMs = trajectoryMeta.createdAtMs || dbMtimeMs || nowMs;

  let parentConversationId = options.fallbackParentId || null;
  let rootConversationId = options.fallbackRootId || null;
  const steps = [];

  for (const row of genRows) {
    const genIdx = Number.isInteger(row.idx) ? row.idx : 0;
    const details = extractGenMetadataRowDetails(row.data);
    if (details.parentCascadeId && !parentConversationId) {
      parentConversationId = details.parentCascadeId;
    }
    if (details.rootCascadeId && !rootConversationId) {
      rootConversationId = details.rootCascadeId;
    }

    for (let entryIdx = 0; entryIdx < details.entries.length; entryIdx += 1) {
      const entry = details.entries[entryIdx];
      const { tsMs, stepIndex } = resolveStepTimestamp(
        entry,
        details.stepIndices,
        stepTsMap,
        botTsMap,
        fallbackTsMs
      );
      const costUsd = calculateTokenCost(entry.stepModel, {
        input_tokens: entry.inputTokens,
        output_tokens: entry.outputTokens,
        cache_read_input_tokens: entry.cacheReadTokens,
        cache_creation_input_tokens: entry.cacheWriteTokens,
      }, { cacheInclusive: false }) || 0;

      steps.push({
        stepUid: `${conversationId}:${genIdx}:${entryIdx}`,
        conversationId,
        genIdx,
        entryIdx,
        stepIndex,
        tsMs: Math.trunc(tsMs),
        model: entry.modelKey,
        rawModel: entry.stepModel,
        inputTokens: entry.inputTokens,
        cacheReadTokens: entry.cacheReadTokens,
        cacheWriteTokens: entry.cacheWriteTokens,
        outputTokens: entry.outputTokens,
        costUsd,
        streamSec: entry.streamSec,
        ttftSec: entry.ttftSec,
        validStream: entry.validStream ? 1 : 0,
        validTtft: entry.validTtft ? 1 : 0,
        parentConversationId: details.parentCascadeId || parentConversationId,
        rootConversationId: details.rootCascadeId || rootConversationId,
        workspace: trajectoryMeta.workspace,
        branch: trajectoryMeta.branch,
        source,
      });
    }
  }

  const dbRecord = {
    conversationId,
    dbPath,
    sig: sig || '',
    lastSyncedMs: Math.trunc(nowMs),
    workspace: trajectoryMeta.workspace,
    branch: trajectoryMeta.branch,
    createdAtMs: trajectoryMeta.createdAtMs,
    cascadeId: trajectoryMeta.cascadeId || conversationId,
    parentConversationId,
    rootConversationId,
    dbMtimeMs,
    source,
  };

  return { dbRecord, steps };
}

const LEDGER_SCHEMA_SQL = `
CREATE TABLE IF NOT EXISTS ledger_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ingested_dbs (
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
  db_mtime_ms INTEGER NOT NULL,
  source TEXT NOT NULL DEFAULT 'cli'
);
CREATE TABLE IF NOT EXISTS telemetry_steps (
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
  branch TEXT,
  source TEXT NOT NULL DEFAULT 'cli'
);
CREATE INDEX IF NOT EXISTS idx_steps_ts ON telemetry_steps(ts_ms);
CREATE INDEX IF NOT EXISTS idx_steps_conv ON telemetry_steps(conversation_id);
CREATE INDEX IF NOT EXISTS idx_steps_root ON telemetry_steps(root_conversation_id);
CREATE INDEX IF NOT EXISTS idx_steps_model ON telemetry_steps(model);
`;

function hasTableColumn(db, tableName, columnName) {
  const rows = db.prepare(`PRAGMA table_info(${tableName})`).all();
  return rows.some(col => col && col.name === columnName);
}

function migrateLedgerSqliteSchema(db) {
  if (!hasTableColumn(db, 'ingested_dbs', 'source')) {
    db.exec("ALTER TABLE ingested_dbs ADD COLUMN source TEXT NOT NULL DEFAULT 'cli';");
  }
  if (!hasTableColumn(db, 'telemetry_steps', 'source')) {
    db.exec("ALTER TABLE telemetry_steps ADD COLUMN source TEXT NOT NULL DEFAULT 'cli';");
  }
  db.exec('CREATE INDEX IF NOT EXISTS idx_steps_source ON telemetry_steps(source);');
  db.prepare('INSERT OR REPLACE INTO ledger_meta (key, value) VALUES (?, ?)').run(
    'schema_version',
    LEDGER_SCHEMA_VERSION
  );
}

function openLedgerSqlite(ledgerPath, options = {}, createIfMissing = true) {
  if (!createIfMissing && !fs.existsSync(ledgerPath)) {
    return null;
  }
  const sqliteMod = loadNodeSqlite(options);
  if (!sqliteMod || typeof sqliteMod.DatabaseSync !== 'function') {
    return null;
  }
  fs.mkdirSync(path.dirname(ledgerPath), { recursive: true, mode: 0o700 });
  const isNew = !fs.existsSync(ledgerPath);
  const db = new sqliteMod.DatabaseSync(ledgerPath, { open: true });
  db.exec('PRAGMA journal_mode = WAL;');
  db.exec('PRAGMA busy_timeout = 3000;');
  db.exec(LEDGER_SCHEMA_SQL);
  migrateLedgerSqliteSchema(db);
  if (isNew) {
    try {
      fs.chmodSync(ledgerPath, 0o600);
    } catch {
      // Best-effort permissions on platforms supporting POSIX chmod
    }
  }
  return db;
}

function readJsonLedger(ledgerPath) {
  const empty = { schemaVer: Number(LEDGER_SCHEMA_VERSION), ingestedDbs: {}, steps: {} };
  try {
    if (!fs.existsSync(ledgerPath)) {
      return empty;
    }
    const parsed = JSON.parse(fs.readFileSync(ledgerPath, 'utf8'));
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return empty;
    }
    const rawDbs = parsed.ingestedDbs && typeof parsed.ingestedDbs === 'object' ? parsed.ingestedDbs : {};
    const rawSteps = parsed.steps && typeof parsed.steps === 'object' ? parsed.steps : {};

    const ingestedDbs = {};
    for (const [cid, row] of Object.entries(rawDbs)) {
      if (!row || typeof row !== 'object') continue;
      ingestedDbs[cid] = {
        ...row,
        source: normalizeSource(row.source),
      };
    }

    const steps = {};
    for (const [uid, row] of Object.entries(rawSteps)) {
      if (!row || typeof row !== 'object') continue;
      const fallbackSource = row.conversationId && ingestedDbs[row.conversationId]
        ? ingestedDbs[row.conversationId].source
        : 'cli';
      steps[uid] = {
        ...row,
        source: normalizeSource(row.source || fallbackSource),
      };
    }

    return {
      schemaVer: Number(parsed.schemaVer) || Number(LEDGER_SCHEMA_VERSION),
      ingestedDbs,
      steps,
    };
  } catch {
    return empty;
  }
}

function writeJsonLedger(ledgerPath, data) {
  fs.mkdirSync(path.dirname(ledgerPath), { recursive: true, mode: 0o700 });
  const tmpPath = `${ledgerPath}.${process.pid}.${Date.now()}.tmp`;
  try {
    fs.writeFileSync(tmpPath, JSON.stringify(data), { mode: 0o600 });
    fs.renameSync(tmpPath, ledgerPath);
  } catch (err) {
    try {
      if (fs.existsSync(tmpPath)) fs.unlinkSync(tmpPath);
    } catch {
      // Ignore cleanup error
    }
    throw err;
  }
}

function upsertBatchToSqlite(db, records) {
  const upsertDbStmt = db.prepare(`
    INSERT OR REPLACE INTO ingested_dbs (
      conversation_id, db_path, sig, last_synced_ms,
      workspace, branch, created_at_ms, cascade_id,
      parent_conversation_id, root_conversation_id, db_mtime_ms, source
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `);
  const upsertStepStmt = db.prepare(`
    INSERT OR REPLACE INTO telemetry_steps (
      step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
      model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
      cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
      parent_conversation_id, root_conversation_id, workspace, branch, source
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  `);

  let stepsUpserted = 0;
  db.exec('BEGIN IMMEDIATE;');
  try {
    for (const { dbRecord, steps } of records) {
      const dbSource = normalizeSource(dbRecord.source);
      upsertDbStmt.run(
        dbRecord.conversationId,
        dbRecord.dbPath,
        dbRecord.sig,
        dbRecord.lastSyncedMs,
        dbRecord.workspace,
        dbRecord.branch,
        dbRecord.createdAtMs,
        dbRecord.cascadeId,
        dbRecord.parentConversationId,
        dbRecord.rootConversationId,
        dbRecord.dbMtimeMs,
        dbSource
      );
      for (const s of steps) {
        upsertStepStmt.run(
          s.stepUid,
          s.conversationId,
          s.genIdx,
          s.entryIdx,
          s.stepIndex,
          s.tsMs,
          s.model,
          s.rawModel,
          s.inputTokens,
          s.cacheReadTokens,
          s.cacheWriteTokens,
          s.outputTokens,
          s.costUsd,
          s.streamSec,
          s.ttftSec,
          s.validStream,
          s.validTtft,
          s.parentConversationId,
          s.rootConversationId,
          s.workspace,
          s.branch,
          normalizeSource(s.source || dbSource)
        );
        stepsUpserted += 1;
      }
    }
    db.exec('COMMIT;');
  } catch (err) {
    try {
      db.exec('ROLLBACK;');
    } catch {
      // Ignore rollback errors
    }
    throw err;
  }
  return stepsUpserted;
}

function upsertBatchToJson(ledgerPath, records) {
  const data = readJsonLedger(ledgerPath);
  data.schemaVer = Number(LEDGER_SCHEMA_VERSION);
  let stepsUpserted = 0;
  for (const { dbRecord, steps } of records) {
    const dbSource = normalizeSource(dbRecord.source);
    data.ingestedDbs[dbRecord.conversationId] = {
      ...dbRecord,
      source: dbSource,
    };
    for (const s of steps) {
      data.steps[s.stepUid] = {
        ...s,
        source: normalizeSource(s.source || dbSource),
      };
      stepsUpserted += 1;
    }
  }
  writeJsonLedger(ledgerPath, data);
  return stepsUpserted;
}

function findConversationDbPath(conversationId, candidateRoots) {
  if (!conversationId || typeof conversationId !== 'string') return null;
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

function listAllConversationDbs(candidateRootsOrEntries) {
  const dbs = [];
  const seenIds = new Set();
  const entries = Array.isArray(candidateRootsOrEntries) ? candidateRootsOrEntries : [];
  for (const entry of entries) {
    const root = typeof entry === 'string' ? entry : entry?.root;
    if (!root || typeof root !== 'string') continue;
    const source = typeof entry === 'string' ? 'cli' : normalizeSource(entry?.source);
    const convDir = path.join(root, 'conversations');
    try {
      if (!fs.existsSync(convDir)) continue;
      for (const name of fs.readdirSync(convDir)) {
        if (!name.endsWith('.db')) continue;
        const conversationId = name.slice(0, -3);
        if (!conversationId || seenIds.has(conversationId)) continue;
        const dbPath = path.join(convDir, name);
        try {
          const st = fs.statSync(dbPath);
          if (st.isFile()) {
            seenIds.add(conversationId);
            dbs.push({ conversationId, dbPath, source });
          }
        } catch {
          // Ignore unreadable file
        }
      }
    } catch {
      // Ignore unreadable dir
    }
  }
  return dbs;
}

/**
 * Incrementally archives a single active conversation (and optional child conversations)
 * into the permanent ledger with per-session 15s throttling.
 *
 * @param {string} conversationId
 * @param {Object} [options]
 * @returns {{ ok: boolean, skipped: boolean, syncedDbs: number, stepsUpserted: number, ledgerPath: string }}
 */
function syncSingleConversationToLedger(conversationId, options = {}) {
  const ledgerPath = resolveLedgerPath(options);
  try {
    if (!conversationId || typeof conversationId !== 'string') {
      return { ok: true, skipped: true, syncedDbs: 0, stepsUpserted: 0, ledgerPath };
    }
    const nowMs = Number.isFinite(options.now) ? options.now : Date.now();
    const throttleMs = Number.isFinite(options.throttleMs) && options.throttleMs >= 0
      ? options.throttleMs
      : SYNC_THROTTLE_MS;
    const candidateRoots = resolveCandidateRoots(options);

    const targets = [{ conversationId, isChild: false }];
    if (Array.isArray(options.childConversationIds)) {
      for (const cid of options.childConversationIds) {
        if (typeof cid === 'string' && cid && cid !== conversationId) {
          targets.push({ conversationId: cid, isChild: true });
        }
      }
    }

    // Fast in-memory check before touching ledger DB
    const dbTargets = [];
    for (const t of targets) {
      const dbPath = findConversationDbPath(t.conversationId, candidateRoots);
      if (!dbPath) continue;
      const sig = getDbSignature(dbPath);
      if (sig === null) continue;
      const memKey = `${ledgerPath}::${t.conversationId}`;
      const mem = memoryThrottleMap.get(memKey);
      if (
        !options.force &&
        mem &&
        mem.sig === sig &&
        normalizeSource(mem.source) === 'cli' &&
        nowMs >= mem.lastSyncedMs &&
        (nowMs - mem.lastSyncedMs) < throttleMs
      ) {
        continue;
      }
      dbTargets.push({ ...t, dbPath, sig, memKey });
    }

    if (dbTargets.length === 0) {
      return { ok: true, skipped: true, syncedDbs: 0, stepsUpserted: 0, ledgerPath };
    }

    const backend = resolveBackendKind(ledgerPath, options);
    let sqliteDb = null;
    let jsonData = null;

    try {
      if (backend === 'sqlite') {
        sqliteDb = openLedgerSqlite(ledgerPath, options, true);
      }
      if (!sqliteDb) {
        jsonData = readJsonLedger(ledgerPath);
      }

      const selectDbStmt = sqliteDb
        ? sqliteDb.prepare('SELECT sig, last_synced_ms, source FROM ingested_dbs WHERE conversation_id = ?')
        : null;

      const recordsToUpsert = [];
      for (const target of dbTargets) {
        let prev = null;
        if (selectDbStmt) {
          const row = selectDbStmt.get(target.conversationId);
          if (row) {
            prev = {
              sig: row.sig,
              lastSyncedMs: row.last_synced_ms,
              source: normalizeSource(row.source),
            };
          }
        } else if (jsonData) {
          const row = jsonData.ingestedDbs[target.conversationId];
          if (row) {
            prev = {
              sig: row.sig,
              lastSyncedMs: row.lastSyncedMs,
              source: normalizeSource(row.source),
            };
          }
        }

        if (!options.force && prev && prev.sig === target.sig && prev.source === 'cli') {
          memoryThrottleMap.set(target.memKey, { lastSyncedMs: prev.lastSyncedMs, sig: prev.sig, source: 'cli' });
          continue;
        }

        const rec = buildConversationIngestionRecords(
          target.conversationId,
          target.dbPath,
          target.sig,
          nowMs,
          {
            ...options,
            source: 'cli',
            fallbackParentId: target.isChild ? conversationId : null,
            fallbackRootId: target.isChild ? conversationId : null,
          }
        );
        if (rec) {
          recordsToUpsert.push({ rec, memKey: target.memKey, sig: target.sig });
        }
      }

      if (recordsToUpsert.length === 0) {
        return { ok: true, skipped: true, syncedDbs: 0, stepsUpserted: 0, ledgerPath };
      }

      const batch = recordsToUpsert.map(r => r.rec);
      const stepsUpserted = sqliteDb
        ? upsertBatchToSqlite(sqliteDb, batch)
        : upsertBatchToJson(ledgerPath, batch);

      for (const r of recordsToUpsert) {
        memoryThrottleMap.set(r.memKey, { lastSyncedMs: nowMs, sig: r.sig, source: 'cli' });
      }

      return {
        ok: true,
        skipped: false,
        syncedDbs: batch.length,
        stepsUpserted,
        ledgerPath,
      };
    } finally {
      if (sqliteDb) {
        try {
          sqliteDb.close();
        } catch {
          // Ignore close errors
        }
      }
    }
  } catch {
    return { ok: false, skipped: false, syncedDbs: 0, stepsUpserted: 0, ledgerPath };
  }
}

/**
 * Incrementally scans all `conversations/*.db` across candidate CLI & IDE roots
 * and archives any new or modified DBs into the permanent ledger.
 *
 * @param {Object} [options]
 * @returns {{ ok: boolean, totalDbs: number, cliDbs: number, ideDbs: number, newlySyncedDbs: number, stepsUpserted: number, durationMs: number, ledgerPath: string }}
 */
function syncAllConversationsToLedger(options = {}) {
  const startedAt = Date.now();
  const ledgerPath = resolveLedgerPath(options);
  try {
    const nowMs = Number.isFinite(options.now) ? options.now : startedAt;
    const sourceRoots = resolveLedgerSourceRoots(options);
    const allDbs = listAllConversationDbs(sourceRoots);
    let cliDbs = 0;
    let ideDbs = 0;
    for (const item of allDbs) {
      if (item.source === 'ide') ideDbs += 1;
      else cliDbs += 1;
    }

    const backend = resolveBackendKind(ledgerPath, options);

    let sqliteDb = null;
    let jsonData = null;

    try {
      if (backend === 'sqlite') {
        sqliteDb = openLedgerSqlite(ledgerPath, options, true);
      }
      if (!sqliteDb) {
        jsonData = readJsonLedger(ledgerPath);
      }

      const existingDbMap = new Map();
      if (sqliteDb) {
        for (const row of sqliteDb.prepare('SELECT conversation_id, sig, source FROM ingested_dbs').all()) {
          existingDbMap.set(row.conversation_id, {
            sig: row.sig,
            source: normalizeSource(row.source),
          });
        }
      } else if (jsonData) {
        for (const [cid, row] of Object.entries(jsonData.ingestedDbs)) {
          if (row) {
            existingDbMap.set(cid, {
              sig: row.sig,
              source: normalizeSource(row.source),
            });
          }
        }
      }

      const recordsToUpsert = [];
      for (const item of allDbs) {
        const sig = getDbSignature(item.dbPath);
        if (sig === null) continue;
        const existing = existingDbMap.get(item.conversationId);
        if (!options.force && existing && existing.sig === sig && existing.source === item.source) {
          continue;
        }
        const rec = buildConversationIngestionRecords(
          item.conversationId,
          item.dbPath,
          sig,
          nowMs,
          { ...options, source: item.source }
        );
        if (rec) {
          recordsToUpsert.push({ rec, conversationId: item.conversationId, sig, source: item.source });
        }
      }

      let stepsUpserted = 0;
      if (recordsToUpsert.length > 0) {
        const batch = recordsToUpsert.map(r => r.rec);
        stepsUpserted = sqliteDb
          ? upsertBatchToSqlite(sqliteDb, batch)
          : upsertBatchToJson(ledgerPath, batch);
        for (const r of recordsToUpsert) {
          memoryThrottleMap.set(`${ledgerPath}::${r.conversationId}`, {
            lastSyncedMs: nowMs,
            sig: r.sig,
            source: r.source,
          });
        }
      }

      return {
        ok: true,
        totalDbs: allDbs.length,
        cliDbs,
        ideDbs,
        newlySyncedDbs: recordsToUpsert.length,
        stepsUpserted,
        durationMs: Math.max(0, Date.now() - startedAt),
        ledgerPath,
      };
    } finally {
      if (sqliteDb) {
        try {
          sqliteDb.close();
        } catch {
          // Ignore close errors
        }
      }
    }
  } catch {
    return {
      ok: false,
      totalDbs: 0,
      cliDbs: 0,
      ideDbs: 0,
      newlySyncedDbs: 0,
      stepsUpserted: 0,
      durationMs: Math.max(0, Date.now() - startedAt),
      ledgerPath,
    };
  }
}

function getWorkspaceLabel(workspace) {
  if (!workspace || typeof workspace !== 'string') return '';
  const trimmed = workspace.replace(/[\\/]+$/, '');
  if (!trimmed) return workspace.startsWith('/') ? '/' : '';
  const lastSlash = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return lastSlash >= 0 ? trimmed.slice(lastSlash + 1) : trimmed;
}

function resolveGranularity(requested, spanMs) {
  if (requested === 'hour' || requested === 'day' || requested === 'week') {
    return requested;
  }
  if (spanMs <= 48 * HOUR_MS) return 'hour';
  if (spanMs <= 90 * DAY_MS) return 'day';
  return 'week';
}

function getBucketInfo(tsMs, granularity) {
  const d = new Date(tsMs);
  const y = d.getFullYear();
  const m = d.getMonth();
  const day = d.getDate();

  if (granularity === 'hour') {
    const hh = d.getHours();
    const startMs = new Date(y, m, day, hh, 0, 0, 0).getTime();
    return {
      bucketKey: `${y}-${pad2(m + 1)}-${pad2(day)} ${pad2(hh)}:00`,
      bucketStartMs: startMs,
      label: `${pad2(m + 1)}-${pad2(day)} ${pad2(hh)}:00`,
    };
  }

  if (granularity === 'week') {
    const offsetFromMon = (d.getDay() + 6) % 7;
    const mon = new Date(y, m, day - offsetFromMon, 0, 0, 0, 0);
    const key = `${mon.getFullYear()}-${pad2(mon.getMonth() + 1)}-${pad2(mon.getDate())}`;
    return {
      bucketKey: key,
      bucketStartMs: mon.getTime(),
      label: `W ${pad2(mon.getMonth() + 1)}-${pad2(mon.getDate())}`,
    };
  }

  const startMs = new Date(y, m, day, 0, 0, 0, 0).getTime();
  return {
    bucketKey: `${y}-${pad2(m + 1)}-${pad2(day)}`,
    bucketStartMs: startMs,
    label: `${pad2(m + 1)}-${pad2(day)}`,
  };
}

function createPerfMetricsAccumulator() {
  return {
    costUsd: 0,
    mainCostUsd: 0,
    subagentCostUsd: 0,
    steps: 0,
    inputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    outputTokens: 0,
    streamSamples: 0,
    sumOut: 0,
    sumStreamSec: 0,
    ttftSamples: 0,
    sumTtftSec: 0,
  };
}

function accumulateStepInto(acc, step, isSubagent) {
  acc.costUsd += step.costUsd;
  if (isSubagent) {
    acc.subagentCostUsd += step.costUsd;
  } else {
    acc.mainCostUsd += step.costUsd;
  }
  acc.steps += 1;
  acc.inputTokens += step.inputTokens;
  acc.cacheReadTokens += step.cacheReadTokens;
  acc.cacheWriteTokens += step.cacheWriteTokens;
  acc.outputTokens += step.outputTokens;

  if (
    step.validStream &&
    step.outputTokens > 0 &&
    step.streamSec !== null &&
    step.streamSec >= MIN_VALID_STREAM_SEC &&
    step.streamSec <= MAX_VALID_STREAM_SEC &&
    (step.outputTokens / step.streamSec) <= MAX_VALID_STREAM_SPEED
  ) {
    acc.streamSamples += 1;
    acc.sumOut += step.outputTokens;
    acc.sumStreamSec += step.streamSec;
  }
  if (
    step.validTtft &&
    step.ttftSec !== null &&
    step.ttftSec >= 0 &&
    step.ttftSec <= MAX_VALID_TTFT_SEC
  ) {
    acc.ttftSamples += 1;
    acc.sumTtftSec += step.ttftSec;
  }
}

function finalizePerfMetrics(acc) {
  const totalPromptTokens = acc.inputTokens + acc.cacheReadTokens + acc.cacheWriteTokens;
  const totalTokens = totalPromptTokens + acc.outputTokens;
  const cacheHitRate = totalPromptTokens > 0
    ? Math.min(100, Math.max(0, (acc.cacheReadTokens / totalPromptTokens) * 100))
    : 0;
  const rawSpeed = acc.sumStreamSec > 0 && acc.sumOut > 0 ? acc.sumOut / acc.sumStreamSec : null;
  const avgSpeed = rawSpeed !== null && rawSpeed <= MAX_VALID_STREAM_SPEED ? rawSpeed : null;
  const avgTtftSec = acc.ttftSamples > 0 ? acc.sumTtftSec / acc.ttftSamples : null;

  return {
    totalPromptTokens,
    totalTokens,
    cacheHitRate,
    avgSpeed,
    avgTtftSec,
  };
}

function loadAllLedgerRows(ledgerPath, options = {}) {
  const backend = resolveBackendKind(ledgerPath, options);
  if (backend === 'sqlite') {
    if (!fs.existsSync(ledgerPath)) {
      return { backend: 'sqlite', ingestedDbs: [], steps: [] };
    }
    const db = openLedgerSqlite(ledgerPath, options, false);
    if (db) {
      try {
        const dbRows = db.prepare(`
          SELECT conversation_id, workspace, branch, created_at_ms, cascade_id,
                 parent_conversation_id, root_conversation_id, db_mtime_ms, source
          FROM ingested_dbs
        `).all();
        const stepRows = db.prepare(`
          SELECT step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
                 model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                 cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
                 parent_conversation_id, root_conversation_id, workspace, branch, source
          FROM telemetry_steps
        `).all();
        return {
          backend: 'sqlite',
          ingestedDbs: dbRows.map(r => ({
            conversationId: r.conversation_id,
            workspace: r.workspace || null,
            branch: r.branch || null,
            createdAtMs: r.created_at_ms ?? null,
            cascadeId: r.cascade_id || r.conversation_id,
            parentConversationId: r.parent_conversation_id || null,
            rootConversationId: r.root_conversation_id || null,
            dbMtimeMs: r.db_mtime_ms || 0,
            source: normalizeSource(r.source),
          })),
          steps: stepRows.map(r => ({
            stepUid: r.step_uid,
            conversationId: r.conversation_id,
            genIdx: r.gen_idx,
            entryIdx: r.entry_idx,
            stepIndex: r.step_index ?? null,
            tsMs: r.ts_ms,
            model: r.model,
            rawModel: r.raw_model || r.model,
            inputTokens: r.input_tokens || 0,
            cacheReadTokens: r.cache_read_tokens || 0,
            cacheWriteTokens: r.cache_write_tokens || 0,
            outputTokens: r.output_tokens || 0,
            costUsd: Number.isFinite(r.cost_usd) ? r.cost_usd : 0,
            streamSec: r.stream_sec ?? null,
            ttftSec: r.ttft_sec ?? null,
            validStream: Boolean(r.valid_stream),
            validTtft: Boolean(r.valid_ttft),
            parentConversationId: r.parent_conversation_id || null,
            rootConversationId: r.root_conversation_id || null,
            workspace: r.workspace || null,
            branch: r.branch || null,
            source: normalizeSource(r.source),
          })),
        };
      } finally {
        try {
          db.close();
        } catch {
          // Ignore close errors
        }
      }
    }
  }

  const jsonData = readJsonLedger(ledgerPath);
  const ingestedDbs = Object.values(jsonData.ingestedDbs).filter(Boolean).map(r => ({
    ...r,
    source: normalizeSource(r.source),
  }));
  const jsonDbSourceMap = new Map(ingestedDbs.map(r => [r.conversationId, r.source]));
  return {
    backend: 'json',
    ingestedDbs,
    steps: Object.values(jsonData.steps).filter(Boolean).map(s => ({
      ...s,
      validStream: Boolean(s.validStream),
      validTtft: Boolean(s.validTtft),
      source: normalizeSource(s.source || jsonDbSourceMap.get(s.conversationId)),
    })),
  };
}

function isNonEmptyTelemetryStep(step) {
  if (!step || typeof step !== 'object') return false;
  return (
    (step.inputTokens || 0) > 0 ||
    (step.cacheReadTokens || 0) > 0 ||
    (step.cacheWriteTokens || 0) > 0 ||
    (step.outputTokens || 0) > 0 ||
    (step.costUsd || 0) > 0
  );
}

/**
 * Queries the permanent telemetry ledger with time-range, granularity, model,
 * workspace, and subagent filters, returning summary KPIs, timeseries buckets,
 * per-model breakdown, and hierarchical root->subagent session trees.
 *
 * @param {Object} [filter]
 * @param {Object} [options]
 * @returns {Object} LedgerQueryResult
 */
function queryLedger(filter = {}, options = {}) {
  const ledgerPath = resolveLedgerPath(options);
  const { backend, ingestedDbs, steps: rawSteps } = loadAllLedgerRows(ledgerPath, options);
  const steps = rawSteps.filter(isNonEmptyTelemetryStep);

  const timeRange = resolveTimeRange({
    range: filter.range,
    from: filter.from,
    to: filter.to,
    now: filter.now,
  });

  // Build conversation metadata map for transitive lineage, source, & workspace/branch resolution
  const convMetaMap = new Map();
  const dbSourceMap = new Map();
  for (const dbRec of ingestedDbs) {
    if (!dbRec || !dbRec.conversationId) continue;
    const dbSource = normalizeSource(dbRec.source);
    dbSourceMap.set(dbRec.conversationId, dbSource);
    convMetaMap.set(dbRec.conversationId, {
      workspace: dbRec.workspace || null,
      branch: dbRec.branch || null,
      parentConversationId: dbRec.parentConversationId || null,
      rootConversationId: dbRec.rootConversationId || null,
      source: dbSource,
    });
  }
  for (const step of steps) {
    if (!step || !step.conversationId) continue;
    const stepSource = normalizeSource(step.source);
    const existing = convMetaMap.get(step.conversationId);
    if (!existing) {
      convMetaMap.set(step.conversationId, {
        workspace: step.workspace || null,
        branch: step.branch || null,
        parentConversationId: step.parentConversationId || null,
        rootConversationId: step.rootConversationId || null,
        source: stepSource,
      });
    } else {
      if (!existing.workspace && step.workspace) existing.workspace = step.workspace;
      if (!existing.branch && step.branch) existing.branch = step.branch;
      if (!existing.parentConversationId && step.parentConversationId) {
        existing.parentConversationId = step.parentConversationId;
      }
      if (!existing.rootConversationId && step.rootConversationId) {
        existing.rootConversationId = step.rootConversationId;
      }
    }
  }

  const rootCache = new Map();
  const resolveRootConversationId = (cid) => {
    if (rootCache.has(cid)) return rootCache.get(cid);
    let cur = cid;
    const visited = new Set([cur]);
    let stepsWalked = 0;
    while (stepsWalked < 32) {
      stepsWalked += 1;
      const meta = convMetaMap.get(cur);
      if (!meta) break;
      const candidate = (meta.rootConversationId && meta.rootConversationId !== cur)
        ? meta.rootConversationId
        : ((meta.parentConversationId && meta.parentConversationId !== cur) ? meta.parentConversationId : null);
      if (!candidate || visited.has(candidate)) break;
      visited.add(candidate);
      cur = candidate;
    }
    rootCache.set(cid, cur);
    return cur;
  };

  // Collect global metadata (available models, workspaces, sources, total recorded counts)
  const availableModelsSet = new Set();
  const availableWorkspacesSet = new Set();
  const availableSourcesSet = new Set();
  const allSessionsSet = new Set();
  let minRecordedTs = Infinity;
  let maxRecordedTs = 0;

  for (const step of steps) {
    if (step.model) availableModelsSet.add(step.model);
    if (step.conversationId) allSessionsSet.add(step.conversationId);
    availableSourcesSet.add(normalizeSource(step.source));
    const rootId = resolveRootConversationId(step.conversationId);
    const ws = step.workspace || convMetaMap.get(step.conversationId)?.workspace || convMetaMap.get(rootId)?.workspace;
    if (ws) availableWorkspacesSet.add(ws);
    if (Number.isFinite(step.tsMs)) {
      if (step.tsMs < minRecordedTs) minRecordedTs = step.tsMs;
      if (step.tsMs > maxRecordedTs) maxRecordedTs = step.tsMs;
    }
  }
  const availableSources = ['cli', 'ide'].filter(src => availableSourcesSet.has(src));

  const isUnboundedAll = timeRange.range === 'all' && filter.from === undefined && filter.to === undefined;
  const spanMs = isUnboundedAll
    ? (maxRecordedTs > minRecordedTs ? (maxRecordedTs - minRecordedTs) : 7 * DAY_MS)
    : Math.max(0, timeRange.toMs - timeRange.fromMs);
  const granularity = resolveGranularity(filter.granularity || 'auto', spanMs);

  const sourceFilter = normalizeSourceFilter(filter.source);

  const modelFilterRaw = typeof filter.model === 'string' ? filter.model.trim() : '';
  const hasModelFilter = Boolean(modelFilterRaw && modelFilterRaw.toLowerCase() !== 'all');
  const modelFilterNorm = hasModelFilter
    ? (compactModelName(modelFilterRaw) || modelFilterRaw).toLowerCase()
    : '';

  const wsFilterRaw = typeof filter.workspace === 'string' ? filter.workspace.trim() : '';
  const hasWorkspaceFilter = Boolean(wsFilterRaw && wsFilterRaw.toLowerCase() !== 'all');
  const includeSubagents = filter.includeSubagents !== false;

  const summaryAcc = createPerfMetricsAccumulator();
  const bucketMap = new Map();
  const modelMap = new Map();
  const sourceMap = new Map();
  const sessionMap = new Map();
  const matchedSubagentIds = new Set();

  for (const step of steps) {
    if (!Number.isFinite(step.tsMs)) continue;
    if (!isUnboundedAll && (step.tsMs < timeRange.fromMs || step.tsMs > timeRange.toMs)) {
      continue;
    }

    const stepSource = normalizeSource(step.source);
    if (sourceFilter !== 'all' && stepSource !== sourceFilter) {
      continue;
    }

    const rootId = resolveRootConversationId(step.conversationId);
    const isSubagent = rootId !== step.conversationId;
    if (!includeSubagents && isSubagent) continue;

    if (hasModelFilter) {
      const stepModelLower = String(step.model || '').toLowerCase();
      const rawModelLower = String(step.rawModel || '').toLowerCase();
      if (stepModelLower !== modelFilterNorm && stepModelLower !== modelFilterRaw.toLowerCase() && rawModelLower !== modelFilterRaw.toLowerCase()) {
        continue;
      }
    }

    const convMeta = convMetaMap.get(step.conversationId);
    const rootMeta = convMetaMap.get(rootId);
    const effectiveWorkspace = step.workspace || convMeta?.workspace || rootMeta?.workspace || null;
    const effectiveBranch = step.branch || convMeta?.branch || rootMeta?.branch || null;

    if (hasWorkspaceFilter) {
      const wsLabel = getWorkspaceLabel(effectiveWorkspace);
      if (effectiveWorkspace !== wsFilterRaw && wsLabel !== wsFilterRaw) {
        continue;
      }
    }

    // 1. Summary
    accumulateStepInto(summaryAcc, step, isSubagent);
    if (isSubagent) matchedSubagentIds.add(step.conversationId);

    // 2. Timeseries bucket
    const bInfo = getBucketInfo(step.tsMs, granularity);
    let bucket = bucketMap.get(bInfo.bucketKey);
    if (!bucket) {
      bucket = {
        bucketKey: bInfo.bucketKey,
        bucketStartMs: bInfo.bucketStartMs,
        label: bInfo.label,
        acc: createPerfMetricsAccumulator(),
        byModelCost: {},
      };
      bucketMap.set(bInfo.bucketKey, bucket);
    }
    accumulateStepInto(bucket.acc, step, isSubagent);
    bucket.byModelCost[step.model] = (bucket.byModelCost[step.model] || 0) + step.costUsd;

    // 3. By Model
    let modelEntry = modelMap.get(step.model);
    if (!modelEntry) {
      modelEntry = {
        model: step.model,
        acc: createPerfMetricsAccumulator(),
        lastUsedMs: 0,
      };
      modelMap.set(step.model, modelEntry);
    }
    accumulateStepInto(modelEntry.acc, step, isSubagent);
    if (step.tsMs > modelEntry.lastUsedMs) modelEntry.lastUsedMs = step.tsMs;

    // 4. By Source
    let sourceEntry = sourceMap.get(stepSource);
    if (!sourceEntry) {
      sourceEntry = {
        source: stepSource,
        acc: createPerfMetricsAccumulator(),
        rootIds: new Set(),
      };
      sourceMap.set(stepSource, sourceEntry);
    }
    accumulateStepInto(sourceEntry.acc, step, isSubagent);
    sourceEntry.rootIds.add(rootId);

    // 5. By Root Session & Subagents
    let rootSess = sessionMap.get(rootId);
    if (!rootSess) {
      const rootWs = rootMeta?.workspace || effectiveWorkspace;
      const rootBr = rootMeta?.branch || effectiveBranch;
      const rootSource = dbSourceMap.get(rootId) || stepSource || 'cli';
      rootSess = {
        rootConversationId: rootId,
        source: rootSource,
        workspace: rootWs || '',
        workspaceLabel: getWorkspaceLabel(rootWs),
        branch: rootBr || '',
        startedAtMs: step.tsMs,
        lastActivityMs: step.tsMs,
        acc: createPerfMetricsAccumulator(),
        modelsCost: new Map(),
        childrenMap: new Map(),
      };
      sessionMap.set(rootId, rootSess);
    }
    if (step.tsMs < rootSess.startedAtMs) rootSess.startedAtMs = step.tsMs;
    if (step.tsMs > rootSess.lastActivityMs) rootSess.lastActivityMs = step.tsMs;
    accumulateStepInto(rootSess.acc, step, isSubagent);
    rootSess.modelsCost.set(step.model, (rootSess.modelsCost.get(step.model) || 0) + step.costUsd);

    if (isSubagent) {
      let childSess = rootSess.childrenMap.get(step.conversationId);
      if (!childSess) {
        const childSource = dbSourceMap.get(step.conversationId) || stepSource || 'cli';
        childSess = {
          conversationId: step.conversationId,
          source: childSource,
          startedAtMs: step.tsMs,
          lastActivityMs: step.tsMs,
          acc: createPerfMetricsAccumulator(),
          modelsCost: new Map(),
        };
        rootSess.childrenMap.set(step.conversationId, childSess);
      }
      if (step.tsMs < childSess.startedAtMs) childSess.startedAtMs = step.tsMs;
      if (step.tsMs > childSess.lastActivityMs) childSess.lastActivityMs = step.tsMs;
      accumulateStepInto(childSess.acc, step, true);
      childSess.modelsCost.set(step.model, (childSess.modelsCost.get(step.model) || 0) + step.costUsd);
    }
  }

  const summaryFin = finalizePerfMetrics(summaryAcc);

  const timeseries = Array.from(bucketMap.values())
    .sort((a, b) => a.bucketStartMs - b.bucketStartMs)
    .map(b => {
      const fin = finalizePerfMetrics(b.acc);
      return {
        bucketKey: b.bucketKey,
        bucketStartMs: b.bucketStartMs,
        label: b.label,
        costUsd: b.acc.costUsd,
        mainCostUsd: b.acc.mainCostUsd,
        subagentCostUsd: b.acc.subagentCostUsd,
        inputTokens: b.acc.inputTokens,
        cacheReadTokens: b.acc.cacheReadTokens,
        cacheWriteTokens: b.acc.cacheWriteTokens,
        outputTokens: b.acc.outputTokens,
        totalPromptTokens: fin.totalPromptTokens,
        cacheHitRate: fin.cacheHitRate,
        steps: b.acc.steps,
        avgSpeed: fin.avgSpeed,
        avgTtftSec: fin.avgTtftSec,
        byModelCost: b.byModelCost,
      };
    });

  const byModel = Array.from(modelMap.values())
    .sort((a, b) => (b.acc.costUsd - a.acc.costUsd) || (b.acc.steps - a.acc.steps) || a.model.localeCompare(b.model))
    .map(m => {
      const fin = finalizePerfMetrics(m.acc);
      return {
        model: m.model,
        costUsd: m.acc.costUsd,
        costSharePct: summaryAcc.costUsd > 0 ? (m.acc.costUsd / summaryAcc.costUsd) * 100 : 0,
        steps: m.acc.steps,
        inputTokens: m.acc.inputTokens,
        cacheReadTokens: m.acc.cacheReadTokens,
        cacheWriteTokens: m.acc.cacheWriteTokens,
        outputTokens: m.acc.outputTokens,
        totalPromptTokens: fin.totalPromptTokens,
        cacheHitRate: fin.cacheHitRate,
        avgSpeed: fin.avgSpeed,
        avgTtftSec: fin.avgTtftSec,
        streamSamples: m.acc.streamSamples,
        ttftSamples: m.acc.ttftSamples,
        lastUsedMs: m.lastUsedMs,
      };
    });

  const bySource = Array.from(sourceMap.values())
    .sort((a, b) => (b.acc.costUsd - a.acc.costUsd) || (b.acc.steps - a.acc.steps) || a.source.localeCompare(b.source))
    .map(src => {
      const fin = finalizePerfMetrics(src.acc);
      return {
        source: src.source,
        costUsd: src.acc.costUsd,
        costSharePct: summaryAcc.costUsd > 0 ? (src.acc.costUsd / summaryAcc.costUsd) * 100 : 0,
        steps: src.acc.steps,
        sessionCount: src.rootIds.size,
        inputTokens: src.acc.inputTokens,
        cacheReadTokens: src.acc.cacheReadTokens,
        cacheWriteTokens: src.acc.cacheWriteTokens,
        outputTokens: src.acc.outputTokens,
        totalTokens: fin.totalTokens,
        totalPromptTokens: fin.totalPromptTokens,
        cacheHitRate: fin.cacheHitRate,
        avgSpeed: fin.avgSpeed,
        avgTtftSec: fin.avgTtftSec,
      };
    });

  const sortModelList = (modelsCostMap) => Array.from(modelsCostMap.entries())
    .sort((a, b) => (b[1] - a[1]) || a[0].localeCompare(b[0]))
    .map(([name]) => name);

  const sessions = Array.from(sessionMap.values())
    .sort((a, b) => (b.lastActivityMs - a.lastActivityMs) || (b.acc.costUsd - a.acc.costUsd))
    .map(s => {
      const fin = finalizePerfMetrics(s.acc);
      const children = Array.from(s.childrenMap.values())
        .sort((a, b) => (a.startedAtMs - b.startedAtMs) || (b.acc.costUsd - a.acc.costUsd))
        .map(c => {
          const cFin = finalizePerfMetrics(c.acc);
          return {
            conversationId: c.conversationId,
            source: c.source,
            models: sortModelList(c.modelsCost),
            steps: c.acc.steps,
            costUsd: c.acc.costUsd,
            inputTokens: c.acc.inputTokens,
            cacheReadTokens: c.acc.cacheReadTokens,
            cacheWriteTokens: c.acc.cacheWriteTokens,
            outputTokens: c.acc.outputTokens,
            cacheHitRate: cFin.cacheHitRate,
            avgSpeed: cFin.avgSpeed,
            avgTtftSec: cFin.avgTtftSec,
            startedAtMs: c.startedAtMs,
            lastActivityMs: c.lastActivityMs,
          };
        });

      return {
        rootConversationId: s.rootConversationId,
        source: s.source,
        workspace: s.workspace,
        workspaceLabel: s.workspaceLabel,
        branch: s.branch,
        startedAtMs: s.startedAtMs,
        lastActivityMs: s.lastActivityMs,
        totalCostUsd: s.acc.costUsd,
        mainCostUsd: s.acc.mainCostUsd,
        subagentCostUsd: s.acc.subagentCostUsd,
        subagentCount: children.length,
        steps: s.acc.steps,
        inputTokens: s.acc.inputTokens,
        cacheReadTokens: s.acc.cacheReadTokens,
        cacheWriteTokens: s.acc.cacheWriteTokens,
        outputTokens: s.acc.outputTokens,
        totalPromptTokens: fin.totalPromptTokens,
        cacheHitRate: fin.cacheHitRate,
        avgSpeed: fin.avgSpeed,
        avgTtftSec: fin.avgTtftSec,
        models: sortModelList(s.modelsCost),
        children,
      };
    });

  return {
    meta: {
      range: timeRange.range,
      source: sourceFilter,
      fromMs: isUnboundedAll && Number.isFinite(minRecordedTs) ? minRecordedTs : timeRange.fromMs,
      toMs: isUnboundedAll && maxRecordedTs > 0 ? Math.max(timeRange.toMs, maxRecordedTs) : timeRange.toMs,
      granularity,
      ledgerPath,
      backend,
      totalRecordedSteps: steps.length,
      totalRecordedSessions: allSessionsSet.size,
      availableModels: Array.from(availableModelsSet).sort(),
      availableWorkspaces: Array.from(availableWorkspacesSet).sort(),
      availableSources,
    },
    summary: {
      totalCostUsd: summaryAcc.costUsd,
      mainCostUsd: summaryAcc.mainCostUsd,
      subagentCostUsd: summaryAcc.subagentCostUsd,
      totalSteps: summaryAcc.steps,
      rootSessionCount: sessions.length,
      subagentSessionCount: matchedSubagentIds.size,
      inputTokens: summaryAcc.inputTokens,
      cacheReadTokens: summaryAcc.cacheReadTokens,
      cacheWriteTokens: summaryAcc.cacheWriteTokens,
      outputTokens: summaryAcc.outputTokens,
      totalPromptTokens: summaryFin.totalPromptTokens,
      totalTokens: summaryFin.totalTokens,
      cacheHitRate: summaryFin.cacheHitRate,
      avgSpeed: summaryFin.avgSpeed,
      avgTtftSec: summaryFin.avgTtftSec,
      streamSamples: summaryAcc.streamSamples,
      ttftSamples: summaryAcc.ttftSamples,
    },
    timeseries,
    byModel,
    bySource,
    sessions,
  };
}

function csvCell(value) {
  if (value === null || value === undefined) return '';
  const str = String(value);
  if (/[",\n\r]/.test(str)) {
    return `"${str.replace(/"/g, '""')}"`;
  }
  return str;
}

function csvRow(cells) {
  return cells.map(csvCell).join(',');
}

function formatNumCsv(val, digits = 6) {
  if (val === null || val === undefined || !Number.isFinite(val)) return '';
  return Number(val.toFixed(digits));
}

/**
 * Formats a `LedgerQueryResult` as UTF-8 CSV for `timeseries`, `models`, or `sessions`.
 *
 * @param {Object} queryResult
 * @param {string|Object} [kindOrOptions='timeseries']
 * @returns {string}
 */
function formatLedgerCsv(queryResult, kindOrOptions = 'timeseries') {
  const rawKind = (kindOrOptions && typeof kindOrOptions === 'object')
    ? kindOrOptions.kind
    : kindOrOptions;
  const kind = (rawKind === 'models' || rawKind === 'sessions') ? rawKind : 'timeseries';
  const rows = [];

  if (kind === 'models') {
    rows.push(csvRow([
      'model', 'costUsd', 'costSharePct', 'steps',
      'inputTokens', 'cacheReadTokens', 'cacheWriteTokens', 'outputTokens',
      'totalPromptTokens', 'cacheHitRate', 'avgSpeed', 'avgTtftSec', 'lastUsedMs',
    ]));
    for (const m of (queryResult?.byModel || [])) {
      rows.push(csvRow([
        m.model,
        formatNumCsv(m.costUsd, 6),
        formatNumCsv(m.costSharePct, 2),
        m.steps ?? 0,
        m.inputTokens ?? 0,
        m.cacheReadTokens ?? 0,
        m.cacheWriteTokens ?? 0,
        m.outputTokens ?? 0,
        m.totalPromptTokens ?? 0,
        formatNumCsv(m.cacheHitRate, 2),
        formatNumCsv(m.avgSpeed, 2),
        formatNumCsv(m.avgTtftSec, 3),
        m.lastUsedMs ?? '',
      ]));
    }
  } else if (kind === 'sessions') {
    rows.push(csvRow([
      'rootConversationId', 'source', 'workspace', 'workspaceLabel', 'branch',
      'startedAtMs', 'lastActivityMs', 'totalCostUsd', 'mainCostUsd', 'subagentCostUsd',
      'subagentCount', 'steps', 'inputTokens', 'cacheReadTokens', 'cacheWriteTokens',
      'outputTokens', 'totalPromptTokens', 'cacheHitRate', 'avgSpeed', 'avgTtftSec', 'models',
    ]));
    for (const s of (queryResult?.sessions || [])) {
      rows.push(csvRow([
        s.rootConversationId,
        normalizeSource(s.source),
        s.workspace || '',
        s.workspaceLabel || '',
        s.branch || '',
        s.startedAtMs ?? '',
        s.lastActivityMs ?? '',
        formatNumCsv(s.totalCostUsd, 6),
        formatNumCsv(s.mainCostUsd, 6),
        formatNumCsv(s.subagentCostUsd, 6),
        s.subagentCount ?? 0,
        s.steps ?? 0,
        s.inputTokens ?? 0,
        s.cacheReadTokens ?? 0,
        s.cacheWriteTokens ?? 0,
        s.outputTokens ?? 0,
        s.totalPromptTokens ?? 0,
        formatNumCsv(s.cacheHitRate, 2),
        formatNumCsv(s.avgSpeed, 2),
        formatNumCsv(s.avgTtftSec, 3),
        Array.isArray(s.models) ? s.models.join(';') : '',
      ]));
    }
  } else {
    rows.push(csvRow([
      'bucketKey', 'bucketStartMs', 'label', 'costUsd', 'mainCostUsd', 'subagentCostUsd',
      'steps', 'inputTokens', 'cacheReadTokens', 'cacheWriteTokens', 'outputTokens',
      'totalPromptTokens', 'cacheHitRate', 'avgSpeed', 'avgTtftSec',
    ]));
    for (const b of (queryResult?.timeseries || [])) {
      rows.push(csvRow([
        b.bucketKey,
        b.bucketStartMs ?? '',
        b.label,
        formatNumCsv(b.costUsd, 6),
        formatNumCsv(b.mainCostUsd, 6),
        formatNumCsv(b.subagentCostUsd, 6),
        b.steps ?? 0,
        b.inputTokens ?? 0,
        b.cacheReadTokens ?? 0,
        b.cacheWriteTokens ?? 0,
        b.outputTokens ?? 0,
        b.totalPromptTokens ?? 0,
        formatNumCsv(b.cacheHitRate, 2),
        formatNumCsv(b.avgSpeed, 2),
        formatNumCsv(b.avgTtftSec, 3),
      ]));
    }
  }

  return `${rows.join('\n')}\n`;
}

module.exports = {
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
};
