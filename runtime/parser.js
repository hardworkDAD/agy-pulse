'use strict';

const fs = require('fs');
const path = require('path');
const os = require('os');
const { createHash } = require('crypto');
const { execFileSync } = require('child_process');
const { getAntigravityRoots, resolveSafeExecutable, resolveAntigravityPath } = require('./paths.js');
const {
  resolveServerCompactionLimit,
  resolveModelPerfStats,
  readPersistedChildSpeeds,
  writePersistedChildSpeeds,
} = require('./compaction.js');
const { compactModelName } = require('./renderer/format.js');
const { MAX_CHILD_MODELS_DISPLAY, MAX_VALID_SPEED } = require('./speed.js');
const { calculateTokenCost, modelIncludesCacheInInput } = require('./cost.js');

function getGeminiHome() {
  const home = process.env.HOME || process.env.USERPROFILE || os.homedir();
  return path.join(home, '.gemini');
}

/**
 * Decodes the email claim from an unverified JWT string using base64url.
 * Display-only: the JWT signature is NOT verified.
 * @param {string} idToken
 * @returns {string|null}
 */
function decodeEmailFromJwt(idToken) {
  if (!idToken || typeof idToken !== 'string') return null;
  const parts = idToken.split('.');
  if (parts.length !== 3 || !parts[1]) return null;
  try {
    const payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
    if (payload && typeof payload.email === 'string' && payload.email.length > 0) {
      return payload.email;
    }
  } catch {
    // Ignore malformed JWT payloads
  }
  return null;
}

/**
 * Last-resort email source: the id_token in ~/.gemini/oauth_creds.json. This is
 * the Gemini-CLI credential file, NOT agy's active-account source — agy neither
 * refreshes it nor rewrites it on account switch, so it can name a stale
 * account. It is only used when the authoritative source (the userinfo email
 * cached against the live token) is unavailable (offline / first run).
 * Display-only: the JWT signature is NOT verified.
 * @returns {string|null}
 */
function getOauthCredsEmail() {
  try {
    const credsPath = path.join(getGeminiHome(), 'oauth_creds.json');
    if (!fs.existsSync(credsPath)) return null;

    const raw = JSON.parse(fs.readFileSync(credsPath, 'utf8'));
    return decodeEmailFromJwt(raw.id_token);
  } catch {}
  return null;
}

/**
 * Fallback only: agy's account registry (~/.gemini/google_accounts.json `active`).
 * NOTE: agy does NOT rewrite this on account switch (verified — it lagged behind
 * a live switch), so it can be stale. Kept as a best-effort guess ranked below
 * the authoritative cached userinfo email.
 * @returns {string|null}
 */
function getActiveAccountFromRegistry() {
  try {
    const registryPath = path.join(getGeminiHome(), 'google_accounts.json');
    if (!fs.existsSync(registryPath)) return null;
    const registry = JSON.parse(fs.readFileSync(registryPath, 'utf8'));
    if (registry && typeof registry.active === 'string' && registry.active.includes('@')) {
      return registry.active;
    }
  } catch {}
  return null;
}

/**
 * Resolves the email of agy's active account for display.
 *
 * agy resolves the signed-in account from the live OAuth access token at
 * runtime and never persists it, so no local file reliably names the active
 * account after a switch. The authoritative value is the userinfo email the
 * quota refresh caches against the current token; we read that first and fall
 * back to the account registry / oauth_creds id_token only when it is absent.
 * @returns {string|null}
 */
function getActiveAccountEmail() {
  try {
    const { readToken } = require('./quota/token.js');
    const { getCachedAccountEmail } = require('./quota/cache.js');
    const geminiHome = getGeminiHome();
    const homeDir = process.env.HOME || process.env.USERPROFILE || os.homedir();
    const isCustomHome = geminiHome !== path.join(os.homedir(), '.gemini');
    const configuredRoots = getAntigravityRoots(process.env, homeDir);
    const homeCliRoot = path.join(geminiHome, 'antigravity-cli');
    const roots = configuredRoots.includes(homeCliRoot)
      ? configuredRoots
      : [...configuredRoots, homeCliRoot];
    const tok = readToken({
      roots,
      skipKeychain: isCustomHome,
      skipWindowsCredential: true,
      skipLinuxKeyring: true,
    });
    if (tok) {
      if (tok.email && tok.sourceFormat !== 'oauth-creds') return tok.email;
      const email = getCachedAccountEmail(tok);
      if (email) return email;
      if (tok.email) return tok.email;
    }
  } catch {}
  return getActiveAccountFromRegistry() || getOauthCredsEmail();
}

/**
 * Falls back to the OS username when no OAuth credentials are available.
 * @returns {string}
 */
function getFallbackUsername() {
  try {
    const userInfo = os.userInfo();
    return userInfo.username || '';
  } catch {
    return '';
  }
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isContextWindow(value) {
  return isObject(value) && (
    Object.hasOwn(value, 'total_input_tokens') ||
    Object.hasOwn(value, 'total_output_tokens') ||
    Object.hasOwn(value, 'context_window_size') ||
    Object.hasOwn(value, 'used_percentage') ||
    isObject(value.current_usage)
  );
}

function findContextWindow(value, depth = 0) {
  if (!isObject(value) || depth > 4) return null;
  if (isContextWindow(value.context_window)) return value.context_window;
  if (isContextWindow(value)) return value;

  for (const child of Object.values(value)) {
    const found = findContextWindow(child, depth + 1);
    if (found) return found;
  }
  return null;
}

/**
 * Parses the transcript log to count steps and get branch info.
 * Also scans local config / workspace metadata (memory files, rules count, MCPs, hooks).
 * @param {string} transcriptPath
 * @returns {Promise<SessionState>}
 */
function parseDurationToMs(durationStr) {
  if (typeof durationStr !== 'string') return 0;
  let ms = 0;
  const segments = [...durationStr.matchAll(/(\d+(?:\.\d+)?)\s*(ms|h|m|s)/gi)];
  if (segments.length > 0) {
    for (const [, val, unit] of segments) {
      const n = parseFloat(val);
      if (!Number.isFinite(n)) continue;
      const u = unit.toLowerCase();
      if (u === 'ms') ms += n;
      else if (u === 'h') ms += n * 3600000;
      else if (u === 'm') ms += n * 60000;
      else if (u === 's') ms += n * 1000;
    }
  } else {
    const val = parseFloat(durationStr);
    if (Number.isFinite(val)) {
      ms = val * 1000;
    }
  }
  return Math.round(ms);
}

/**
 * Recursively searches an object tree for a key, up to maxDepth levels deep.
 * Safer than JSON.stringify + regex for extracting nested API error fields.
 * @param {unknown} obj
 * @param {string} key
 * @param {number} [maxDepth=6]
 * @returns {unknown}
 */
function deepFind(obj, key, maxDepth = 6) {
  if (!obj || typeof obj !== 'object' || maxDepth === 0) return undefined;
  if (Object.prototype.hasOwnProperty.call(obj, key)) return obj[key];
  for (const v of Object.values(obj)) {
    const found = deepFind(v, key, maxDepth - 1);
    if (found !== undefined) return found;
  }
  return undefined;
}

function isNonNegFinite(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function toNonNegInt(value) {
  return isNonNegFinite(value) ? Math.trunc(value) : 0;
}

const TRANSCRIPT_CACHE_VERSION = 9;
const MAX_MODEL_IDLE_TURNS = 30;
const MAX_MODEL_IDLE_MS = 5 * 60 * 1000;
const TRANSCRIPT_CACHE_HEAD_BYTES = 256;
const MAX_TRANSCRIPT_CACHE_FILES = 32;
const FULL_CHUNK_BYTES = 64 * 1024;
const MAX_FULL_LINE_BYTES = 4 * 1024 * 1024;
const MAX_FULL_TRANSCRIPT_BYTES = 64 * 1024 * 1024;
const MAX_CHILD_AGENTS = 256;
const MAX_CHILD_TAIL_BYTES = 65536;
const MAX_CHILD_OPEN_TURN_MS = 120000;
const MIN_CHILD_API_DELTA_MS = 500;

function createEmptyTranscriptState() {
  return {
    steps: 0,
    usage: null,
    maxHistoricalCache: 0,
    imageExhausted: null,
    totalApiDurationMs: 0,
    pendingPlannerStartMs: null,
    lastPlannerResponse: null,
    compactionCount: 0,
    totalInputTokens: 0,
    totalOutputTokens: 0,
    totalCacheReadTokens: 0,
    totalCacheCreationTokens: 0,
    transcriptCostUsd: 0,
    transcriptCostByModel: {},
    lastKnownModel: '',
    peakRawTurnTokens: 0,
    lastTurnUsage: null,
    turnHistoryMaxByMode: {
      inclusivePeak: 0,
      exclusivePeak: 0,
    },
    childAgents: {},
    persistedChildSpeeds: {},
    userTurns: 0,
    hasPlannerResponse: false,
    modelLastSeenTurn: {},
    modelLastActiveMs: {},
    lastUserInputStep: 0,
    lastUserInputMs: 0,
    pendingSubagentHints: [],
  };
}

function sanitizeNonNegIntMap(raw) {
  if (!isObject(raw)) return {};
  const result = {};
  for (const [key, val] of Object.entries(raw)) {
    if (!key || !isNonNegFinite(val)) continue;
    result[key] = toNonNegInt(val);
  }
  return result;
}

function sanitizeTranscriptCostByModel(raw) {
  if (!isObject(raw)) return {};
  const result = {};
  for (const [key, val] of Object.entries(raw)) {
    if (!key || !isObject(val)) continue;
    result[key] = {
      costUsd: isNonNegFinite(val.costUsd) ? val.costUsd : 0,
      inputTokens: toNonNegInt(val.inputTokens),
      cacheReadTokens: toNonNegInt(val.cacheReadTokens),
      cacheWriteTokens: toNonNegInt(val.cacheWriteTokens),
      outputTokens: toNonNegInt(val.outputTokens),
      steps: toNonNegInt(val.steps),
    };
  }
  return result;
}

function getTranscriptCachePath(transcriptPath) {
  const resolved = path.resolve(transcriptPath);
  const hash = createHash('sha256').update(resolved).digest('hex');
  return resolveAntigravityPath(path.join('transcript-cache', `${hash}.json`));
}

function computeHeadHash(transcriptPath, byteLength) {
  const len = Math.min(byteLength, TRANSCRIPT_CACHE_HEAD_BYTES);
  if (len <= 0) return '';
  let fd = null;
  try {
    fd = fs.openSync(transcriptPath, 'r');
    const buf = Buffer.allocUnsafe(len);
    let total = 0;
    while (total < len) {
      const n = fs.readSync(fd, buf, total, len - total, total);
      if (n <= 0) break;
      total += n;
    }
    return createHash('sha256').update(buf.subarray(0, total)).digest('hex');
  } catch {
    return null;
  } finally {
    if (fd !== null) {
      try {
        fs.closeSync(fd);
      } catch {}
    }
  }
}

function pruneTranscriptCacheDir(dirPath, maxFiles = MAX_TRANSCRIPT_CACHE_FILES) {
  try {
    const entries = [];
    for (const name of fs.readdirSync(dirPath)) {
      if (!name.endsWith('.json')) continue;
      const fullPath = path.join(dirPath, name);
      try {
        const st = fs.statSync(fullPath);
        if (st.isFile()) {
          entries.push({ path: fullPath, mtimeMs: st.mtimeMs });
        }
      } catch {}
    }
    if (entries.length <= maxFiles) return;
    entries.sort((a, b) => a.mtimeMs - b.mtimeMs);
    const toRemove = entries.length - maxFiles;
    for (let i = 0; i < toRemove; i += 1) {
      try {
        fs.unlinkSync(entries[i].path);
      } catch {}
    }
  } catch {}
}

function readTranscriptCache(cachePath, expectedIno, fileSize, transcriptPath) {
  try {
    if (!cachePath || !fs.existsSync(cachePath)) return null;
    const raw = JSON.parse(fs.readFileSync(cachePath, 'utf8'));
    if (!raw || typeof raw !== 'object') return null;
    if (raw.version !== TRANSCRIPT_CACHE_VERSION) return null;
    if (raw.ino !== expectedIno) return null;
    if (!Number.isInteger(raw.offset) || raw.offset < 0 || raw.offset > fileSize) return null;

    const expectedHeadLen = Number.isInteger(raw.headLen) && raw.headLen >= 0 && raw.headLen <= TRANSCRIPT_CACHE_HEAD_BYTES
      ? raw.headLen
      : Math.min(fileSize, TRANSCRIPT_CACHE_HEAD_BYTES);
    if (fileSize < expectedHeadLen || typeof raw.headHash !== 'string') return null;
    const currentHeadHash = computeHeadHash(transcriptPath, expectedHeadLen);
    if (currentHeadHash === null || currentHeadHash !== raw.headHash) return null;

    const base = createEmptyTranscriptState();
    const state = {
      steps: toNonNegInt(raw.steps),
      usage: isObject(raw.usage) ? raw.usage : null,
      maxHistoricalCache: toNonNegInt(raw.maxHistoricalCache),
      imageExhausted: isObject(raw.imageExhausted) && typeof raw.imageExhausted.resetTime === 'string'
        ? raw.imageExhausted
        : null,
      totalApiDurationMs: toNonNegInt(raw.totalApiDurationMs),
      pendingPlannerStartMs: isNonNegFinite(raw.pendingPlannerStartMs) ? raw.pendingPlannerStartMs : null,
      lastPlannerResponse: isObject(raw.lastPlannerResponse) && isNonNegFinite(raw.lastPlannerResponse.startMs)
        ? {
            startMs: raw.lastPlannerResponse.startMs,
            endMs: isNonNegFinite(raw.lastPlannerResponse.endMs) ? raw.lastPlannerResponse.endMs : null,
            outputTokens: isNonNegFinite(raw.lastPlannerResponse.outputTokens)
              ? Math.trunc(raw.lastPlannerResponse.outputTokens)
              : null,
            ...(raw.lastPlannerResponse.interrupted === true ? { interrupted: true } : {}),
          }
        : null,
      compactionCount: toNonNegInt(raw.compactionCount),
      totalInputTokens: toNonNegInt(raw.totalInputTokens),
      totalOutputTokens: toNonNegInt(raw.totalOutputTokens),
      totalCacheReadTokens: toNonNegInt(raw.totalCacheReadTokens),
      totalCacheCreationTokens: toNonNegInt(raw.totalCacheCreationTokens),
      transcriptCostUsd: isNonNegFinite(raw.transcriptCostUsd) ? raw.transcriptCostUsd : 0,
      transcriptCostByModel: sanitizeTranscriptCostByModel(raw.transcriptCostByModel),
      lastKnownModel: typeof raw.lastKnownModel === 'string' ? raw.lastKnownModel : '',
      peakRawTurnTokens: toNonNegInt(raw.peakRawTurnTokens),
      lastTurnUsage: isObject(raw.lastTurnUsage) ? raw.lastTurnUsage : null,
      turnHistoryMaxByMode: isObject(raw.turnHistoryMaxByMode)
        ? {
            inclusivePeak: toNonNegInt(raw.turnHistoryMaxByMode.inclusivePeak),
            exclusivePeak: toNonNegInt(raw.turnHistoryMaxByMode.exclusivePeak),
          }
        : base.turnHistoryMaxByMode,
      childAgents: sanitizeCachedChildAgents(raw.childAgents),
      persistedChildSpeeds: sanitizePersistedChildSpeedsMap(raw.persistedChildSpeeds),
      userTurns: toNonNegInt(raw.userTurns),
      hasPlannerResponse: Boolean(raw.hasPlannerResponse),
      modelLastSeenTurn: sanitizeNonNegIntMap(raw.modelLastSeenTurn),
      modelLastActiveMs: sanitizeNonNegIntMap(raw.modelLastActiveMs),
      lastUserInputStep: toNonNegInt(raw.lastUserInputStep),
      lastUserInputMs: toNonNegInt(raw.lastUserInputMs),
      pendingSubagentHints: Array.isArray(raw.pendingSubagentHints)
        ? raw.pendingSubagentHints
            .filter(h => isObject(h))
            .slice(-32)
            .map(h => ({
              model: typeof h.model === 'string' ? h.model : '',
              role: typeof h.role === 'string' ? h.role : '',
              typeName: typeof h.typeName === 'string' ? h.typeName : '',
              stepIndex: toNonNegInt(h.stepIndex),
            }))
        : [],
    };

    return { offset: raw.offset, state };
  } catch {
    return null;
  }
}

function writeTranscriptCache(cachePath, ino, offset, accState, transcriptPath, fileSize) {
  if (!cachePath) return;
  let tmpPath = null;
  try {
    const dir = path.dirname(cachePath);
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    const headLen = Math.min(fileSize, TRANSCRIPT_CACHE_HEAD_BYTES);
    const headHash = computeHeadHash(transcriptPath, headLen);
    if (headHash === null) return;
    tmpPath = `${cachePath}.${process.pid}.${Date.now()}.tmp`;
    const payload = {
      version: TRANSCRIPT_CACHE_VERSION,
      ino,
      offset,
      headLen,
      headHash,
      ...accState,
    };
    fs.writeFileSync(tmpPath, JSON.stringify(payload), { mode: 0o600 });
    fs.renameSync(tmpPath, cachePath);
    tmpPath = null;
    pruneTranscriptCacheDir(dir, MAX_TRANSCRIPT_CACHE_FILES);
  } catch {
    // Best-effort cache write
  } finally {
    if (tmpPath) {
      try {
        fs.unlinkSync(tmpPath);
      } catch {}
    }
  }
}

function loadFullEntriesMap(transcriptPath, neededSteps = null) {
  const map = new Map();
  if (neededSteps instanceof Set && neededSteps.size === 0) return map;
  let fd = null;
  try {
    const fullPath = path.join(path.dirname(transcriptPath), 'transcript_full.jsonl');
    if (!fs.existsSync(fullPath)) return map;
    const st = fs.statSync(fullPath);
    if (!st.isFile() || st.size === 0 || st.size > MAX_FULL_TRANSCRIPT_BYTES) return map;

    const stepNeedles = neededSteps instanceof Set
      ? [...neededSteps].map(s => String(s))
      : null;

    const processLineBuf = (lineBuf) => {
      if (lineBuf.length === 0 || lineBuf.length > MAX_FULL_LINE_BYTES) return;
      if (stepNeedles && !stepNeedles.some(s => lineBuf.includes(s))) return;
      const line = lineBuf.toString('utf8').trim();
      if (!line) return;
      try {
        const obj = JSON.parse(line);
        if (typeof obj.step_index === 'number') {
          if (!neededSteps || neededSteps.has(obj.step_index)) {
            map.set(obj.step_index, obj);
          }
        }
      } catch {
        // Skip invalid line in transcript_full.jsonl
      }
    };

    fd = fs.openSync(fullPath, 'r');
    const buf = Buffer.allocUnsafe(FULL_CHUNK_BYTES);
    let carry = Buffer.alloc(0);
    let oversizedLine = false;
    let bytesRead;
    while ((bytesRead = fs.readSync(fd, buf, 0, FULL_CHUNK_BYTES, null)) > 0) {
      const slice = buf.subarray(0, bytesRead);
      let start = 0;
      while (start < slice.length) {
        const nlIdx = slice.indexOf(0x0a, start);
        if (nlIdx === -1) {
          const tail = slice.subarray(start);
          if (!oversizedLine) {
            if (carry.length + tail.length > MAX_FULL_LINE_BYTES) {
              oversizedLine = true;
              carry = Buffer.alloc(0);
            } else {
              carry = carry.length > 0 ? Buffer.concat([carry, tail]) : Buffer.from(tail);
            }
          }
          break;
        }
        const part = slice.subarray(start, nlIdx);
        if (oversizedLine) {
          oversizedLine = false;
          carry = Buffer.alloc(0);
        } else {
          const lineBuf = carry.length > 0 ? Buffer.concat([carry, part]) : part;
          carry = Buffer.alloc(0);
          processLineBuf(lineBuf);
          if (neededSteps && map.size >= neededSteps.size) {
            return map;
          }
        }
        start = nlIdx + 1;
      }
    }
    if (!oversizedLine && carry.length > 0) {
      processLineBuf(carry);
    }
  } catch {
    // Ignore missing or unreadable transcript_full.jsonl
  } finally {
    if (fd !== null) {
      try {
        fs.closeSync(fd);
      } catch {}
    }
  }
  return map;
}

function sanitizePersistedChildSpeedsMap(raw) {
  if (!isObject(raw)) return {};
  const result = {};
  for (const [key, val] of Object.entries(raw)) {
    if (!isObject(val)) continue;
    const model = typeof val.model === 'string' && val.model.trim()
      ? val.model.trim()
      : String(key || '').trim();
    const speed = Number(val.speed);
    if (!model || !Number.isFinite(speed) || speed <= 0 || speed > MAX_VALID_SPEED) continue;
    const eventMs = isNonNegFinite(val.eventMs)
      ? val.eventMs
      : (isNonNegFinite(val.updatedAt) ? val.updatedAt : 0);
    const mtimeMs = isNonNegFinite(val.mtimeMs) ? val.mtimeMs : eventMs;
    const updatedAt = isNonNegFinite(val.updatedAt) ? val.updatedAt : eventMs;
    const firstOrder = toNonNegInt(val.firstOrder);
    const entry = {
      model,
      speed,
      updatedAt,
      eventMs,
      mtimeMs,
      firstOrder,
    };
    if (isNonNegFinite(val.lastActiveTurn)) {
      entry.lastActiveTurn = toNonNegInt(val.lastActiveTurn);
    }
    if (isNonNegFinite(val.lastActiveMs)) {
      entry.lastActiveMs = toNonNegInt(val.lastActiveMs);
    }
    result[model] = entry;
  }
  return result;
}

function sanitizeCachedChildAgents(raw) {
  if (!isObject(raw)) return {};
  const result = {};
  for (const [key, val] of Object.entries(raw)) {
    if (!isObject(val)) continue;
    const id = typeof val.id === 'string' && val.id.trim() ? val.id.trim() : key;
    if (!id) continue;
    const kind = val.kind === 'swarm' ? 'swarm' : 'subagent';
    result[id] = {
      id,
      kind,
      name: typeof val.name === 'string' && val.name.trim() ? val.name.trim() : id.slice(0, 8),
      modelHint: typeof val.modelHint === 'string' ? val.modelHint : '',
      active: Boolean(val.active),
      killed: Boolean(val.killed),
      addedStep: toNonNegInt(val.addedStep),
      lastActiveMs: toNonNegInt(val.lastActiveMs),
      addedTurn: toNonNegInt(val.addedTurn),
      lastActiveTurn: toNonNegInt(val.lastActiveTurn),
    };
  }
  return result;
}

function pruneChildAgents(childAgents, maxEntries = MAX_CHILD_AGENTS) {
  const entries = Object.values(childAgents);
  if (entries.length <= maxEntries) return;
  entries.sort((a, b) => {
    const aAlive = a.active && !a.killed ? 1 : 0;
    const bAlive = b.active && !b.killed ? 1 : 0;
    if (aAlive !== bAlive) return bAlive - aAlive;
    if (a.addedStep !== b.addedStep) return b.addedStep - a.addedStep;
    return b.lastActiveMs - a.lastActiveMs;
  });
  const keepIds = new Set(entries.slice(0, maxEntries).map(e => e.id));
  for (const id of Object.keys(childAgents)) {
    if (!keepIds.has(id)) {
      delete childAgents[id];
    }
  }
}

function resolveBrainContext(transcriptPath) {
  if (!transcriptPath || typeof transcriptPath !== 'string') {
    return { brainDir: null, mainConversationId: null };
  }
  const resolved = path.resolve(transcriptPath);
  const logsDir = path.dirname(resolved);
  const sysDir = path.dirname(logsDir);
  const convDir = path.dirname(sysDir);
  const maybeBrain = path.dirname(convDir);
  if (path.basename(maybeBrain) === 'brain') {
    return { brainDir: maybeBrain, mainConversationId: path.basename(convDir) || null };
  }
  const norm = resolved.replace(/\\/g, '/');
  const marker = '/brain/';
  const idx = norm.lastIndexOf(marker);
  if (idx >= 0) {
    const afterBrain = norm.slice(idx + marker.length);
    const slashIdx = afterBrain.indexOf('/');
    const convId = slashIdx >= 0 ? afterBrain.slice(0, slashIdx) : afterBrain;
    const brainDir = resolved.slice(0, idx + marker.length - 1);
    return { brainDir, mainConversationId: convId || null };
  }
  return { brainDir: null, mainConversationId: null };
}

function extractJsonObjects(text, maxObjects = 16) {
  if (typeof text !== 'string' || !text.includes('{')) return [];
  const objects = [];
  const limit = Math.min(text.length, 262144);
  let i = 0;
  let retries = 0;
  while (i < limit && objects.length < maxObjects) {
    const start = text.indexOf('{', i);
    if (start === -1 || start >= limit) break;
    let depth = 0;
    let inString = false;
    let escape = false;
    let end = -1;
    for (let j = start; j < limit; j += 1) {
      const ch = text[j];
      if (inString) {
        if (escape) {
          escape = false;
        } else if (ch === '\\') {
          escape = true;
        } else if (ch === '"') {
          inString = false;
        }
      } else if (ch === '"') {
        inString = true;
      } else if (ch === '{') {
        depth += 1;
      } else if (ch === '}') {
        depth -= 1;
        if (depth === 0) {
          end = j;
          break;
        }
      }
    }
    if (end === -1) {
      i = start + 1;
      retries += 1;
      if (retries > 32) break;
      continue;
    }
    try {
      const parsed = JSON.parse(text.slice(start, end + 1));
      if (isObject(parsed)) {
        objects.push(parsed);
      }
      i = end + 1;
    } catch {
      i = start + 1;
      retries += 1;
      if (retries > 32) break;
    }
  }
  return objects;
}

function hasMention(text, name) {
  if (!text || !name) return false;
  const needle = `@${name}`;
  let idx = text.indexOf(needle);
  while (idx !== -1) {
    const nextChar = text[idx + needle.length] || '';
    if (!/[a-z0-9_-]/i.test(nextChar)) {
      return true;
    }
    idx = text.indexOf(needle, idx + needle.length);
  }
  return false;
}

function getEffectiveTurn(acc) {
  const userTurns = toNonNegInt(acc?.userTurns);
  if (userTurns > 0) return userTurns;
  return acc?.hasPlannerResponse ? 1 : 0;
}

function registerSwarmMember(acc, id, name, modelHint, stepIdx, validTimeMs, mainConvId) {
  if (!id || id === mainConvId) return;
  const existing = acc.childAgents[id];
  const curTurn = getEffectiveTurn(acc);
  const resolvedName = (typeof name === 'string' && name.trim())
    ? name.trim()
    : (existing?.name || id.slice(0, 8));
  const resolvedHint = (typeof modelHint === 'string' && modelHint.trim())
    ? modelHint.trim()
    : (existing?.modelHint || '');
  acc.childAgents[id] = {
    id,
    kind: 'swarm',
    name: resolvedName,
    modelHint: resolvedHint,
    active: true,
    killed: false,
    addedStep: stepIdx,
    lastActiveMs: Math.max(validTimeMs || 0, existing?.lastActiveMs || 0),
    addedTurn: existing && isNonNegFinite(existing.addedTurn) ? toNonNegInt(existing.addedTurn) : curTurn,
    lastActiveTurn: curTurn,
  };
}

function registerSubagent(acc, id, modelHint, roleHint, stepIdx, validTimeMs, mainConvId) {
  if (!id || id === mainConvId) return;
  const existing = acc.childAgents[id];
  if (existing && existing.kind === 'swarm') return;
  const curTurn = getEffectiveTurn(acc);
  let hint = null;
  if (!existing && Array.isArray(acc.pendingSubagentHints) && acc.pendingSubagentHints.length > 0) {
    hint = acc.pendingSubagentHints.shift();
  }
  const resolvedHint = (typeof modelHint === 'string' && modelHint.trim())
    ? modelHint.trim()
    : (hint?.model || existing?.modelHint || '');
  const resolvedName = (typeof roleHint === 'string' && roleHint.trim())
    ? roleHint.trim()
    : (hint?.role || hint?.typeName || existing?.name || id.slice(0, 8));
  acc.childAgents[id] = {
    id,
    kind: 'subagent',
    name: resolvedName,
    modelHint: resolvedHint,
    active: true,
    killed: false,
    addedStep: stepIdx,
    lastActiveMs: Math.max(validTimeMs || 0, hint?.timeMs || 0, existing?.lastActiveMs || 0),
    addedTurn: existing && isNonNegFinite(existing.addedTurn) ? toNonNegInt(existing.addedTurn) : curTurn,
    lastActiveTurn: curTurn,
  };
}

function processPlannerToolCalls(toolCalls, acc, stepIdx, validTimeMs) {
  if (!Array.isArray(toolCalls)) return;
  if (!Array.isArray(acc.pendingSubagentHints)) {
    acc.pendingSubagentHints = [];
  }
  const curTurn = getEffectiveTurn(acc);
  for (const tc of toolCalls) {
    if (!isObject(tc)) continue;
    const toolName = tc.name || tc.function?.name || '';
    let args = tc.arguments ?? tc.args ?? tc.function?.arguments;
    if (typeof args === 'string') {
      try {
        args = JSON.parse(args);
      } catch {
        args = null;
      }
    }
    if (!isObject(args)) args = {};

    if (toolName === 'invoke_subagent') {
      acc.pendingSubagentHints.push({
        model: String(args.Model || args.model || ''),
        role: String(args.Role || args.role || ''),
        typeName: String(args.TypeName || args.typeName || ''),
        stepIndex: stepIdx,
        timeMs: validTimeMs || 0,
      });
      if (acc.pendingSubagentHints.length > 32) {
        acc.pendingSubagentHints.shift();
      }
    } else if (toolName === 'send_message') {
      const recipient = args.Recipient || args.recipient || args.conversationId || args.conversation_id;
      if (typeof recipient === 'string' && acc.childAgents[recipient]) {
        acc.childAgents[recipient].active = true;
        acc.childAgents[recipient].killed = false;
        acc.childAgents[recipient].addedStep = stepIdx;
        acc.childAgents[recipient].lastActiveTurn = curTurn;
        if (validTimeMs) {
          acc.childAgents[recipient].lastActiveMs = Math.max(acc.childAgents[recipient].lastActiveMs || 0, validTimeMs);
        }
      }
    } else if (toolName === 'manage_subagents') {
      const action = args.Action || args.action;
      if (action === 'kill_all') {
        for (const child of Object.values(acc.childAgents)) {
          if (child.kind === 'subagent') {
            child.active = false;
            child.killed = true;
          }
        }
      } else if (action === 'kill') {
        const rawIds = args.ConversationIds ?? args.conversationIds ?? args.ConversationId ?? args.conversationId;
        const ids = Array.isArray(rawIds) ? rawIds : (typeof rawIds === 'string' ? [rawIds] : []);
        for (const targetId of ids) {
          if (typeof targetId === 'string' && acc.childAgents[targetId]?.kind === 'subagent') {
            acc.childAgents[targetId].active = false;
            acc.childAgents[targetId].killed = true;
          }
        }
      }
    } else if (toolName === 'post_message') {
      const msgText = typeof args.text === 'string' ? args.text : (typeof args.Text === 'string' ? args.Text : '');
      if (msgText) {
        const mentionAll = hasMention(msgText, 'all');
        for (const child of Object.values(acc.childAgents)) {
          if (child.kind === 'swarm' && !child.killed && (mentionAll || hasMention(msgText, child.name))) {
            child.active = true;
            child.lastActiveTurn = curTurn;
            if (validTimeMs) {
              child.lastActiveMs = Math.max(child.lastActiveMs || 0, validTimeMs);
            }
          }
        }
      }
    }
  }
}

function processGenericChildEvents(entry, acc, context, stepIdx, validTimeMs) {
  const rawContent = entry.content;
  let objects = [];
  let contentStr = '';
  if (isObject(rawContent)) {
    objects = [rawContent];
  } else if (typeof rawContent === 'string') {
    contentStr = rawContent;
    const hasChildSignal = (
      contentStr.includes('conversation_id') ||
      contentStr.includes('conversationId') ||
      contentStr.includes('model_switched') ||
      contentStr.includes('killed')
    );
    if (!hasChildSignal) return;
    objects = extractJsonObjects(contentStr);
  } else {
    return;
  }

  const mainConvId = context?.mainConversationId || null;
  const seenSubIds = new Set();

  for (const obj of objects) {
    if (obj.status === 'added') {
      if (typeof obj.conversation_id === 'string' && obj.conversation_id.trim()) {
        registerSwarmMember(
          acc,
          obj.conversation_id.trim(),
          obj.name,
          obj.model_display_name || obj.model,
          stepIdx,
          validTimeMs,
          mainConvId
        );
      }
      if (Array.isArray(obj.members)) {
        for (const member of obj.members) {
          if (isObject(member) && typeof member.conversation_id === 'string' && member.conversation_id.trim()) {
            registerSwarmMember(
              acc,
              member.conversation_id.trim(),
              member.name,
              member.model_display_name || member.model || obj.model_display_name || obj.model,
              stepIdx,
              validTimeMs,
              mainConvId
            );
          }
        }
      }
      continue;
    }

    const isModelSwitch = (
      obj.status === 'model_switched' ||
      (typeof obj.status === 'string' && obj.status.includes('switch') && (obj.model || obj.model_display_name))
    );
    if (isModelSwitch) {
      const nextModel = String(obj.model_display_name || obj.model || '').trim();
      if (nextModel) {
        for (const child of Object.values(acc.childAgents)) {
          if (
            child.kind === 'swarm' &&
            ((typeof obj.name === 'string' && child.name === obj.name) ||
             (typeof obj.conversation_id === 'string' && child.id === obj.conversation_id))
          ) {
            child.modelHint = nextModel;
          }
        }
      }
      continue;
    }

    if (obj.status === 'killed') {
      const targets = [];
      if (typeof obj.name === 'string' && obj.name.trim()) targets.push(obj.name.trim());
      if (Array.isArray(obj.names)) {
        for (const n of obj.names) {
          if (typeof n === 'string' && n.trim()) targets.push(n.trim());
        }
      }
      const targetId = typeof obj.conversation_id === 'string' ? obj.conversation_id.trim() : '';
      for (const child of Object.values(acc.childAgents)) {
        if (child.kind !== 'swarm') continue;
        if (
          (targetId && child.id === targetId) ||
          targets.some(t => child.name === t || child.name.startsWith(`${t}-`))
        ) {
          child.active = false;
          child.killed = true;
        }
      }
      continue;
    }

    if (typeof obj.conversationId === 'string' && obj.conversationId.trim()) {
      const subId = obj.conversationId.trim();
      seenSubIds.add(subId);
      registerSubagent(
        acc,
        subId,
        obj.model_display_name || obj.model,
        obj.name || obj.role,
        stepIdx,
        validTimeMs,
        mainConvId
      );
    }
  }

  if (contentStr && contentStr.includes('conversationId')) {
    const uuidRe = /conversationId["']?\s*[:=]\s*["']?([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/gi;
    for (const match of contentStr.matchAll(uuidRe)) {
      const subId = match[1];
      if (!subId || seenSubIds.has(subId)) continue;
      seenSubIds.add(subId);
      registerSubagent(acc, subId, '', '', stepIdx, validTimeMs, mainConvId);
    }
  }

  pruneChildAgents(acc.childAgents);
}

function processTranscriptLine(line, acc, context) {
  if (!line.trim()) return;
  let entry;
  try {
    entry = JSON.parse(line);
  } catch {
    return;
  }
  if (!isObject(entry)) return;

  if (typeof entry.step_index === 'number' && entry.step_index > acc.steps) {
    acc.steps = entry.step_index;
  }

  if (entry.type === 'CHECKPOINT') {
    acc.compactionCount += 1;
    acc.lastTurnUsage = null;
    acc.maxHistoricalCache = 0;
    if (isObject(acc.usage)) {
      delete acc.usage.current_usage;
      delete acc.usage.used_percentage;
      acc.usage.input_tokens = 0;
      acc.usage.cache_read_input_tokens = 0;
      acc.usage.cache_creation_input_tokens = 0;
      if (Object.hasOwn(acc.usage, 'cache_read_tokens')) acc.usage.cache_read_tokens = 0;
      if (Object.hasOwn(acc.usage, 'cache_creation_tokens')) acc.usage.cache_creation_tokens = 0;
    }
  }

  const entryTimeMs = entry.created_at ? Date.parse(entry.created_at) : null;
  const validTimeMs = entryTimeMs !== null && Number.isFinite(entryTimeMs) ? entryTimeMs : null;
  const stepIdx = typeof entry.step_index === 'number' ? entry.step_index : acc.steps;

  const isInterruptingStep = (
    entry.type === 'USER_INPUT' ||
    entry.type === 'SYSTEM_MESSAGE' ||
    entry.type === 'CHECKPOINT' ||
    entry.source === 'SYSTEM'
  );

  if (isInterruptingStep && acc.lastPlannerResponse && acc.lastPlannerResponse.endMs === null) {
    acc.lastPlannerResponse.interrupted = true;
  }

  if (acc.pendingPlannerStartMs !== null) {
    // `created_at` records when a step started; the next non-user, non-system step
    // marks when the preceding PLANNER_RESPONSE ended. USER_INPUT, SYSTEM_MESSAGE,
    // source==='SYSTEM', or missing timestamps invalidate the pending span.
    if (validTimeMs === null || isInterruptingStep) {
      acc.pendingPlannerStartMs = null;
    } else if (validTimeMs >= acc.pendingPlannerStartMs) {
      acc.totalApiDurationMs += (validTimeMs - acc.pendingPlannerStartMs);
      if (acc.lastPlannerResponse && acc.lastPlannerResponse.endMs === null) {
        acc.lastPlannerResponse.endMs = validTimeMs;
      }
      if (acc.lastKnownModel && validTimeMs > 0) {
        const endModelKey = compactModelName(acc.lastKnownModel);
        if (endModelKey) {
          if (!isObject(acc.modelLastActiveMs)) acc.modelLastActiveMs = {};
          acc.modelLastActiveMs[endModelKey] = Math.max(acc.modelLastActiveMs[endModelKey] || 0, validTimeMs);
        }
      }
      acc.pendingPlannerStartMs = null;
    } else {
      acc.pendingPlannerStartMs = null;
    }
  }

  if (entry.type === 'USER_INPUT') {
    acc.userTurns = toNonNegInt(acc.userTurns) + 1;
    if (typeof entry.step_index === 'number') {
      acc.lastUserInputStep = entry.step_index;
    }
    if (validTimeMs !== null) {
      acc.lastUserInputMs = validTimeMs;
    }
  }

  if (entry.type === 'SYSTEM_MESSAGE') {
    if (
      typeof entry.content === 'string' &&
      entry.content.includes('All your subagents and background tasks have been stopped')
    ) {
      for (const child of Object.values(acc.childAgents)) {
        child.active = false;
      }
    }
    const curTurn = getEffectiveTurn(acc);
    if (typeof entry.sender === 'string' && acc.childAgents[entry.sender]) {
      acc.childAgents[entry.sender].lastActiveTurn = curTurn;
      if (validTimeMs) {
        acc.childAgents[entry.sender].lastActiveMs = Math.max(acc.childAgents[entry.sender].lastActiveMs || 0, validTimeMs);
      }
    }
    if (typeof entry.content === 'string') {
      const senderMatch = entry.content.match(/\bsender=([^\s\]]+)/);
      if (senderMatch && acc.childAgents[senderMatch[1]]) {
        acc.childAgents[senderMatch[1]].lastActiveTurn = curTurn;
        if (validTimeMs) {
          acc.childAgents[senderMatch[1]].lastActiveMs = Math.max(acc.childAgents[senderMatch[1]].lastActiveMs || 0, validTimeMs);
        }
      }
    }
  }

  if (entry.type === 'PLANNER_RESPONSE') {
    acc.hasPlannerResponse = true;
    if (validTimeMs !== null) {
      acc.pendingPlannerStartMs = validTimeMs;
      const rawPlannerOut = entry.output_tokens ?? entry.context_window?.current_usage?.output_tokens;
      acc.lastPlannerResponse = {
        startMs: validTimeMs,
        endMs: null,
        outputTokens: isNonNegFinite(rawPlannerOut) ? Math.trunc(rawPlannerOut) : null,
      };
    }
    processPlannerToolCalls(entry.tool_calls, acc, stepIdx, validTimeMs);
  }

  if (entry.type === 'GENERIC') {
    processGenericChildEvents(entry, acc, context, stepIdx, validTimeMs);
  }

  const contextWindow = findContextWindow(entry);
  if (contextWindow) {
    const prevWindowSize = isNonNegFinite(acc.usage?.context_window_size)
      ? acc.usage.context_window_size
      : undefined;
    acc.usage = (prevWindowSize !== undefined && !isNonNegFinite(contextWindow.context_window_size))
      ? { ...contextWindow, context_window_size: prevWindowSize }
      : contextWindow;
    const cur = isObject(contextWindow.current_usage) ? contextWindow.current_usage : contextWindow;
    const cacheReadVal = toNonNegInt(
      cur.cache_read_input_tokens ??
      cur.cache_read_tokens ??
      contextWindow.cache_read_input_tokens ??
      contextWindow.cache_read_tokens
    );
    if (cacheReadVal > acc.maxHistoricalCache) {
      acc.maxHistoricalCache = cacheReadVal;
    }
  }

  const hasTopLevelTokens = (
    isNonNegFinite(entry.input_tokens) ||
    isNonNegFinite(entry.output_tokens) ||
    isNonNegFinite(entry.cache_read_tokens) ||
    isNonNegFinite(entry.cache_read_input_tokens) ||
    isNonNegFinite(entry.cache_creation_tokens) ||
    isNonNegFinite(entry.cache_creation_input_tokens)
  );

  const rawModel = (entry.type === 'PLANNER_RESPONSE')
    ? (typeof entry.model === 'string' && entry.model.trim()
      ? entry.model.trim()
      : (typeof entry.model_name === 'string' && entry.model_name.trim()
        ? entry.model_name.trim()
        : (typeof entry.model_display_name === 'string' && entry.model_display_name.trim()
          ? entry.model_display_name.trim()
          : undefined)))
    : undefined;
  if (rawModel) {
    acc.lastKnownModel = rawModel;
    const mKey = compactModelName(rawModel);
    if (mKey) {
      if (!isObject(acc.modelLastSeenTurn)) acc.modelLastSeenTurn = {};
      acc.modelLastSeenTurn[mKey] = getEffectiveTurn(acc);
    }
  }
  if (entry.type === 'PLANNER_RESPONSE' && validTimeMs !== null && validTimeMs > 0) {
    const stepModelKey = compactModelName(rawModel || acc.lastKnownModel || '');
    if (stepModelKey) {
      if (!isObject(acc.modelLastActiveMs)) acc.modelLastActiveMs = {};
      acc.modelLastActiveMs[stepModelKey] = Math.max(acc.modelLastActiveMs[stepModelKey] || 0, validTimeMs);
    }
  }

  if (entry.type === 'PLANNER_RESPONSE' && (hasTopLevelTokens || isObject(contextWindow?.current_usage))) {
    const cur = isObject(contextWindow?.current_usage) ? contextWindow.current_usage : null;
    const inp = toNonNegInt(entry.input_tokens ?? cur?.input_tokens);
    const out = toNonNegInt(entry.output_tokens ?? cur?.output_tokens);
    const cRead = toNonNegInt(
      entry.cache_read_tokens ??
      entry.cache_read_input_tokens ??
      cur?.cache_read_input_tokens ??
      cur?.cache_read_tokens
    );
    const cCreate = toNonNegInt(
      entry.cache_creation_tokens ??
      entry.cache_creation_input_tokens ??
      cur?.cache_creation_input_tokens ??
      cur?.cache_creation_tokens
    );

    acc.totalInputTokens += inp;
    acc.totalOutputTokens += out;
    acc.totalCacheReadTokens += cRead;
    acc.totalCacheCreationTokens += cCreate;
    if (cRead > acc.maxHistoricalCache) {
      acc.maxHistoricalCache = cRead;
    }

    const inclusiveTurn = Math.max(inp, cCreate + cRead) + out;
    const exclusiveTurn = inp + cCreate + cRead + out;
    if (inclusiveTurn > acc.turnHistoryMaxByMode.inclusivePeak) {
      acc.turnHistoryMaxByMode.inclusivePeak = inclusiveTurn;
    }
    if (exclusiveTurn > acc.turnHistoryMaxByMode.exclusivePeak) {
      acc.turnHistoryMaxByMode.exclusivePeak = exclusiveTurn;
    }
    if (exclusiveTurn > acc.peakRawTurnTokens) {
      acc.peakRawTurnTokens = exclusiveTurn;
    }

    const stepModel = rawModel || acc.lastKnownModel || '';
    const modelKey = compactModelName(stepModel) || 'Unknown';
    const cacheTotal = cRead + cCreate;
    // Antigravity top-level PLANNER_RESPONSE without contextWindow uses mutually exclusive input/cache counts
    const cacheInclusive = Boolean(
      contextWindow &&
      modelIncludesCacheInInput(stepModel) &&
      cacheTotal > 0 &&
      inp >= cacheTotal
    );
    const effectiveUncachedIn = cacheInclusive ? (inp - cacheTotal) : inp;
    const stepCostUsd = calculateTokenCost(stepModel, {
      input_tokens: inp,
      output_tokens: out,
      cache_read_input_tokens: cRead,
      cache_creation_input_tokens: cCreate,
    }, { cacheInclusive }) || 0;

    acc.transcriptCostUsd += stepCostUsd;
    if (!acc.transcriptCostByModel[modelKey]) {
      acc.transcriptCostByModel[modelKey] = {
        costUsd: 0,
        inputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        outputTokens: 0,
        steps: 0,
      };
    }
    const modelAcc = acc.transcriptCostByModel[modelKey];
    modelAcc.costUsd += stepCostUsd;
    modelAcc.inputTokens += effectiveUncachedIn;
    modelAcc.cacheReadTokens += cRead;
    modelAcc.cacheWriteTokens += cCreate;
    modelAcc.outputTokens += out;
    modelAcc.steps += 1;

    acc.lastTurnUsage = {
      input_tokens: inp,
      output_tokens: out,
      cache_read_input_tokens: cRead,
      cache_creation_input_tokens: cCreate,
      ...(rawModel ? { model: rawModel } : {}),
      ...(!contextWindow ? { cacheExclusive: true } : {}),
    };

    if (!contextWindow) {
      const prevWindowSize = isNonNegFinite(acc.usage?.context_window_size)
        ? acc.usage.context_window_size
        : undefined;
      acc.usage = {
        total_input_tokens: acc.totalInputTokens,
        total_output_tokens: acc.totalOutputTokens,
        ...(prevWindowSize !== undefined ? { context_window_size: prevWindowSize } : {}),
        current_usage: {
          input_tokens: inp,
          output_tokens: out,
          cache_read_input_tokens: cRead,
          cache_creation_input_tokens: cCreate,
        },
      };
    }
  }

  // Parse image model 429 rate limits strictly from structured error entries
  // to avoid false positives from plain-text code/log discussions.
  const isStructuredError = (
    entry.status === 'ERROR' ||
    isObject(entry.error) ||
    entry.type === 'ERROR'
  );
  if (isStructuredError) {
    let targetEntry = entry;
    if (
      Array.isArray(entry.truncated_fields) &&
      entry.truncated_fields.includes('content') &&
      typeof entry.step_index === 'number'
    ) {
      if (!context.fullEntriesMap) {
        context.fullEntriesMap = loadFullEntriesMap(context.transcriptPath, context.neededErrorSteps);
      }
      const fullEntry = context.fullEntriesMap.get(entry.step_index);
      if (isObject(fullEntry)) {
        targetEntry = { ...entry, ...fullEntry };
      }
    }

    const targetContent = typeof targetEntry.content === 'string' ? targetEntry.content : '';
    const isRateLimit = (
      deepFind(targetEntry, 'RESOURCE_EXHAUSTED') !== undefined ||
      deepFind(targetEntry, 'quotaResetDelay') !== undefined ||
      deepFind(targetEntry, 'quotaResetTimeStamp') !== undefined ||
      (isObject(targetEntry.error) && (targetEntry.error.code === 429 || targetEntry.error.status === 'RESOURCE_EXHAUSTED')) ||
      targetContent.includes('HTTP 429') ||
      targetContent.includes('RESOURCE_EXHAUSTED')
    );

    if (isRateLimit) {
      let delayMs = 0;
      let timestampMs = 0;

      let delayVal = deepFind(targetEntry, 'quotaResetDelay') ?? deepFind(targetEntry, 'retryDelay');
      if (typeof delayVal !== 'string' && targetContent) {
        const m = targetContent.match(/quotaResetDelay["']?\s*[:=]\s*["']?([0-9.hms]+)/i);
        if (m) delayVal = m[1];
      }
      if (typeof delayVal === 'string' && delayVal) {
        delayMs = parseDurationToMs(delayVal);
      }

      let tsVal = deepFind(targetEntry, 'quotaResetTimeStamp');
      if (typeof tsVal !== 'string' && targetContent) {
        const m = targetContent.match(/quotaResetTimeStamp["']?\s*[:=]\s*["']?([0-9T:\-.+Z]+)/i);
        if (m) tsVal = m[1];
      }
      if (typeof tsVal === 'string' && tsVal) {
        const parsedTs = Date.parse(tsVal);
        if (Number.isFinite(parsedTs)) {
          timestampMs = parsedTs;
        }
      }

      let resetTimeMs = 0;
      if (timestampMs > 0) {
        resetTimeMs = timestampMs;
      } else if (delayMs > 0) {
        const entryTime = targetEntry.created_at ? new Date(targetEntry.created_at).getTime() : Date.now();
        resetTimeMs = entryTime + delayMs;
      }

      if (resetTimeMs > Date.now()) {
        if (!acc.imageExhausted || resetTimeMs > new Date(acc.imageExhausted.resetTime).getTime()) {
          acc.imageExhausted = {
            exhausted: true,
            resetTime: new Date(resetTimeMs).toISOString(),
          };
        }
      }
    }
  }
}

function scanTranscriptIncremental(transcriptPath) {
  const acc = createEmptyTranscriptState();
  if (!transcriptPath || typeof transcriptPath !== 'string') return acc;

  let stat;
  try {
    stat = fs.statSync(transcriptPath);
    if (!stat.isFile()) return acc;
  } catch {
    return acc;
  }

  const fIno = Number(stat.ino || 0);
  const fSize = Number(stat.size || 0);
  const cachePath = getTranscriptCachePath(transcriptPath);
  const cached = readTranscriptCache(cachePath, fIno, fSize, transcriptPath);

  let offset = 0;
  let state = acc;
  if (cached) {
    offset = cached.offset;
    state = cached.state;
  }

  if (offset < fSize) {
    let fd = null;
    try {
      const toRead = fSize - offset;
      const buf = Buffer.allocUnsafe(toRead);
      fd = fs.openSync(transcriptPath, 'r');
      let totalRead = 0;
      while (totalRead < toRead) {
        const n = fs.readSync(fd, buf, totalRead, toRead - totalRead, offset + totalRead);
        if (n <= 0) break;
        totalRead += n;
      }
      const view = totalRead < toRead ? buf.subarray(0, totalRead) : buf;
      const lastNl = view.lastIndexOf(0x0a);
      if (lastNl >= 0) {
        const chunk = view.subarray(0, lastNl + 1).toString('utf8');
        offset += lastNl + 1;
        const lines = chunk.split('\n');
        const neededErrorSteps = new Set();
        for (const line of lines) {
          if (line.includes('truncated_fields')) {
            try {
              const candidate = JSON.parse(line);
              if (
                isObject(candidate) &&
                typeof candidate.step_index === 'number' &&
                Array.isArray(candidate.truncated_fields) &&
                candidate.truncated_fields.includes('content') &&
                (candidate.status === 'ERROR' || isObject(candidate.error) || candidate.type === 'ERROR')
              ) {
                neededErrorSteps.add(candidate.step_index);
              }
            } catch {}
          }
        }
        const { mainConversationId } = resolveBrainContext(transcriptPath);
        const ctx = { transcriptPath, mainConversationId, fullEntriesMap: null, neededErrorSteps };
        for (const line of lines) {
          processTranscriptLine(line, state, ctx);
        }
        writeTranscriptCache(cachePath, fIno, offset, state, transcriptPath, fSize);
      }
    } catch {
      // Ignore transient read errors
    } finally {
      if (fd !== null) {
        try {
          fs.closeSync(fd);
        } catch {}
      }
    }
  } else if (!cached && fSize === 0 && fs.existsSync(cachePath)) {
    writeTranscriptCache(cachePath, fIno, 0, state, transcriptPath, 0);
  }

  return state;
}

function extractChildOutputTokens(entry, prevTotalOut, isFromStart) {
  const rawPerTurn = (
    entry.output_tokens ??
    entry.context_window?.current_usage?.output_tokens ??
    entry.usage?.current_usage?.output_tokens ??
    entry.usage?.output_tokens
  );
  if (isNonNegFinite(rawPerTurn)) {
    return { tokens: Math.trunc(rawPerTurn), nextTotalOut: prevTotalOut };
  }
  const rawTotal = entry.context_window?.total_output_tokens ?? entry.usage?.total_output_tokens;
  if (isNonNegFinite(rawTotal)) {
    const totalOut = Math.trunc(rawTotal);
    if (prevTotalOut !== null && totalOut >= prevTotalOut) {
      return { tokens: totalOut - prevTotalOut, nextTotalOut: totalOut };
    }
    if (isFromStart && prevTotalOut === null) {
      return { tokens: totalOut, nextTotalOut: totalOut };
    }
    return { tokens: 0, nextTotalOut: totalOut };
  }
  return { tokens: 0, nextTotalOut: prevTotalOut };
}

function readChildTailTelemetry(childTranscriptPath, child, now) {
  let stat;
  try {
    stat = fs.statSync(childTranscriptPath);
    if (!stat.isFile() || stat.size === 0) return null;
  } catch {
    return null;
  }

  let chunk = '';
  let offset = 0;
  let fd = null;
  try {
    const toRead = Math.min(stat.size, MAX_CHILD_TAIL_BYTES);
    offset = stat.size - toRead;
    const buf = Buffer.allocUnsafe(toRead);
    fd = fs.openSync(childTranscriptPath, 'r');
    let totalRead = 0;
    while (totalRead < toRead) {
      const n = fs.readSync(fd, buf, totalRead, toRead - totalRead, offset + totalRead);
      if (n <= 0) break;
      totalRead += n;
    }
    chunk = buf.subarray(0, totalRead).toString('utf8');
  } catch {
    return null;
  } finally {
    if (fd !== null) {
      try {
        fs.closeSync(fd);
      } catch {}
    }
  }

  const lines = chunk.split('\n');
  if (offset > 0 && lines.length > 0) {
    lines.shift();
  }

  let latestModel = '';
  let latestPlannerMs = 0;
  let latestPlannerModel = '';
  let pendingPlanner = null;
  let lastClosedSpeed = null;
  let prevTotalOut = null;

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    let entry;
    try {
      entry = JSON.parse(trimmed);
    } catch {
      continue;
    }
    if (!isObject(entry)) continue;

    const entryTimeMs = entry.created_at ? Date.parse(entry.created_at) : null;
    const validTimeMs = entryTimeMs !== null && Number.isFinite(entryTimeMs) ? entryTimeMs : null;

    if (pendingPlanner !== null) {
      const isInterruptingStep = (
        entry.type === 'USER_INPUT' ||
        entry.type === 'SYSTEM_MESSAGE' ||
        entry.source === 'SYSTEM'
      );
      if (isInterruptingStep || validTimeMs === null) {
        pendingPlanner = null;
      } else if (validTimeMs >= pendingPlanner.startMs) {
        const durationMs = validTimeMs - pendingPlanner.startMs;
        if (durationMs >= MIN_CHILD_API_DELTA_MS && pendingPlanner.outputTokens > 0) {
          const candidateSpeed = pendingPlanner.outputTokens / (durationMs / 1000);
          if (Number.isFinite(candidateSpeed) && candidateSpeed > 0 && candidateSpeed <= MAX_VALID_SPEED) {
            lastClosedSpeed = {
              speed: candidateSpeed,
              eventMs: validTimeMs,
              model: pendingPlanner.model || latestModel,
            };
          }
        }
        pendingPlanner = null;
      } else {
        pendingPlanner = null;
      }
    }

    if (entry.type === 'PLANNER_RESPONSE') {
      const rawModel = (typeof entry.model === 'string' && entry.model.trim())
        ? entry.model.trim()
        : ((typeof entry.model_name === 'string' && entry.model_name.trim())
          ? entry.model_name.trim()
          : ((typeof entry.model_display_name === 'string' && entry.model_display_name.trim())
            ? entry.model_display_name.trim()
            : ''));
      if (rawModel) {
        latestModel = rawModel;
      }
      if (validTimeMs !== null && validTimeMs > 0 && validTimeMs >= latestPlannerMs) {
        latestPlannerMs = validTimeMs;
        latestPlannerModel = rawModel || latestModel || child.modelHint || '';
      }
      const { tokens, nextTotalOut } = extractChildOutputTokens(entry, prevTotalOut, offset === 0);
      prevTotalOut = nextTotalOut;
      const hasToolCalls = Array.isArray(entry.tool_calls) && entry.tool_calls.length > 0;
      if (validTimeMs !== null && !hasToolCalls) {
        pendingPlanner = {
          startMs: validTimeMs,
          outputTokens: tokens,
          model: rawModel || latestModel,
        };
      } else {
        pendingPlanner = null;
      }
    }
  }

  let selectedSpeed = lastClosedSpeed;
  if (pendingPlanner !== null && pendingPlanner.outputTokens > 0 && now >= pendingPlanner.startMs) {
    const trailingMs = now - pendingPlanner.startMs;
    if (trailingMs >= MIN_CHILD_API_DELTA_MS && trailingMs <= MAX_CHILD_OPEN_TURN_MS) {
      const candidateSpeed = pendingPlanner.outputTokens / (trailingMs / 1000);
      if (Number.isFinite(candidateSpeed) && candidateSpeed > 0 && candidateSpeed <= MAX_VALID_SPEED) {
        selectedSpeed = {
          speed: candidateSpeed,
          eventMs: now,
          model: pendingPlanner.model || latestModel,
        };
      }
    }
  }

  let speedResult = null;
  if (selectedSpeed && Number.isFinite(selectedSpeed.speed) && selectedSpeed.speed > 0 && selectedSpeed.speed <= MAX_VALID_SPEED) {
    const effectiveModel = selectedSpeed.model || latestModel || child.modelHint || '';
    const modelLabel = compactModelName(effectiveModel) || 'Sub';
    speedResult = {
      modelLabel,
      speed: selectedSpeed.speed,
      eventMs: selectedSpeed.eventMs,
      mtimeMs: stat.mtimeMs,
    };
  }

  const latestEventMs = Math.max(latestPlannerMs || 0, speedResult?.eventMs || 0);
  const rawLatestModel = (latestPlannerMs >= (speedResult?.eventMs || 0)
    ? latestPlannerModel
    : (selectedSpeed?.model || latestPlannerModel)) || latestModel || child.modelHint || '';
  const latestModelLabel = compactModelName(rawLatestModel) || speedResult?.modelLabel || '';

  return {
    speedResult,
    latestEventMs,
    latestModelLabel,
  };
}

function readChildTailSpeed(childTranscriptPath, child, now) {
  return readChildTailTelemetry(childTranscriptPath, child, now)?.speedResult || null;
}

function resolveChildModelFallbackFromPersisted(child, persistedInTranscript, diskPersistedSpeeds, now, childLastActiveMs = 0) {
  const rawHint = typeof child.modelHint === 'string' ? child.modelHint.trim() : '';
  if (!rawHint) return null;
  const modelLabel = compactModelName(rawHint);
  if (!modelLabel) return null;
  const fromTranscript = persistedInTranscript?.[modelLabel];
  const fromDisk = diskPersistedSpeeds?.[modelLabel];
  let chosen = null;
  if (fromTranscript && fromDisk) {
    const tTs = fromTranscript.lastActiveMs || fromTranscript.updatedAt || fromTranscript.eventMs || 0;
    const dTs = fromDisk.updatedAt || 0;
    chosen = dTs > tTs ? fromDisk : fromTranscript;
  } else {
    chosen = fromTranscript || fromDisk || null;
  }
  if (!chosen || !Number.isFinite(chosen.speed) || chosen.speed <= 0 || chosen.speed > MAX_VALID_SPEED) return null;
  const effectiveLastActiveMs = Math.max(
    chosen.lastActiveMs || 0,
    chosen.eventMs || 0,
    chosen.updatedAt || 0,
    childLastActiveMs || 0,
    child.lastActiveMs || 0
  );
  const ts = effectiveLastActiveMs || now;
  const mtimeMs = chosen.mtimeMs || chosen.updatedAt || childLastActiveMs || child.lastActiveMs || now;
  return {
    modelLabel,
    speed: chosen.speed,
    eventMs: ts,
    mtimeMs,
    lastActiveMs: effectiveLastActiveMs,
  };
}

function resolveChildModelFallbackFromPerf(child, modelPerf, now, childLastActiveMs = 0) {
  if (!modelPerf || typeof modelPerf !== 'object') return null;
  const rawHint = typeof child.modelHint === 'string' ? child.modelHint.trim() : '';
  if (!rawHint) return null;
  const modelLabel = compactModelName(rawHint);
  if (!modelLabel) return null;
  const perf = modelPerf[modelLabel];
  if (!perf || !Number.isFinite(perf.avgSpeed) || perf.avgSpeed <= 0 || perf.avgSpeed > MAX_VALID_SPEED) return null;

  const effectiveLastActiveMs = Math.max(
    childLastActiveMs || 0,
    child.lastActiveMs || 0,
    Number(perf.lastUpdatedMs) || 0
  );
  const ts = effectiveLastActiveMs || now;
  return {
    modelLabel,
    speed: perf.avgSpeed,
    eventMs: ts,
    mtimeMs: ts,
    lastActiveMs: effectiveLastActiveMs,
  };
}

function mergeChildSpeedCandidate(map, modelLabel, speed, eventMs, mtimeMs, addedStep, lastActiveTurn, lastActiveMs = 0) {
  const safeLastActiveMs = isNonNegFinite(lastActiveMs) ? lastActiveMs : 0;
  const existing = map.get(modelLabel);
  if (!existing) {
    map.set(modelLabel, {
      model: modelLabel,
      speed,
      eventMs,
      mtimeMs,
      firstOrder: addedStep || 0,
      lastActiveTurn,
      lastActiveMs: safeLastActiveMs,
    });
    return;
  }
  const isNewer = (
    eventMs > existing.eventMs ||
    (eventMs === existing.eventMs && mtimeMs > existing.mtimeMs)
  );
  if (isNewer) {
    existing.speed = speed;
    existing.eventMs = eventMs;
    existing.mtimeMs = mtimeMs;
  }
  if ((addedStep || 0) < existing.firstOrder) {
    existing.firstOrder = addedStep || 0;
  }
  if (isNonNegFinite(lastActiveTurn) && (!isNonNegFinite(existing.lastActiveTurn) || lastActiveTurn > existing.lastActiveTurn)) {
    existing.lastActiveTurn = lastActiveTurn;
  }
  if (safeLastActiveMs > (existing.lastActiveMs || 0)) {
    existing.lastActiveMs = safeLastActiveMs;
  }
}

function persistTranscriptChildSpeeds(transcriptPath, nextPersistedMap) {
  if (!transcriptPath || typeof transcriptPath !== 'string') return;
  try {
    const stat = fs.statSync(transcriptPath);
    if (!stat.isFile()) return;
    const fIno = Number(stat.ino || 0);
    const fSize = Number(stat.size || 0);
    const cachePath = getTranscriptCachePath(transcriptPath);
    const cached = readTranscriptCache(cachePath, fIno, fSize, transcriptPath);
    if (!cached) return;
    cached.state.persistedChildSpeeds = sanitizePersistedChildSpeedsMap(nextPersistedMap);
    writeTranscriptCache(cachePath, fIno, cached.offset, cached.state, transcriptPath, fSize);
  } catch {
    // Best-effort cache persistence
  }
}

/**
 * Resolves per-model deduplicated rolling speeds for all registered swarm workers
 * and subagents in the session (persisting across worker kill, turn transitions,
 * and cleaned-up child logs via transcript-cache and disk speed cache).
 *
 * @param {string} transcriptPath
 * @param {Object} scanned
 * @param {number} [now=Date.now()]
 * @param {Record<string, { avgSpeed?: number|null, avgTtftSec?: number|null }>|null} [modelPerf=null]
 * @param {Object} [options={}]
 * @returns {Array<{ model: string, speed: number }>}
 */
function resolveChildModelSpeeds(transcriptPath, scanned, now = Date.now(), modelPerf = null, options = {}) {
  if (!scanned || !isObject(scanned.childAgents)) return [];
  const effectiveNow = isNonNegFinite(options?.now)
    ? options.now
    : (isNonNegFinite(now) ? now : Date.now());
  const { brainDir } = resolveBrainContext(transcriptPath);
  const persistedInTranscript = sanitizePersistedChildSpeedsMap(scanned.persistedChildSpeeds);
  const maxIdleTurns = isNonNegFinite(options?.maxIdleTurns)
    ? Math.trunc(options.maxIdleTurns)
    : MAX_MODEL_IDLE_TURNS;
  const maxIdleMs = Number.isFinite(options?.maxIdleMs) && options.maxIdleMs > 0
    ? options.maxIdleMs
    : MAX_MODEL_IDLE_MS;
  const childTailById = options?.childTailById instanceof Map ? options.childTailById : null;
  const sessionModelLastActiveMs = isObject(options?.modelLastActiveMs)
    ? { ...options.modelLastActiveMs }
    : sanitizeNonNegIntMap(scanned.modelLastActiveMs);
  const currentTurns = Math.max(toNonNegInt(scanned.userTurns), scanned.hasPlannerResponse ? 1 : 0);
  const resolveChildTurn = (child) => {
    if (isNonNegFinite(child?.lastActiveTurn)) return Math.trunc(child.lastActiveTurn);
    if (isNonNegFinite(child?.addedTurn)) return Math.trunc(child.addedTurn);
    return currentTurns;
  };

  const registeredChildren = Object.values(scanned.childAgents)
    .filter(child => child && typeof child.id === 'string' && child.id)
    .sort((a, b) => (b.addedStep - a.addedStep) || (b.lastActiveMs - a.lastActiveMs))
    .slice(0, MAX_CHILD_AGENTS);

  if (registeredChildren.length === 0 && Object.keys(persistedInTranscript).length === 0) {
    return [];
  }

  const liveByModel = new Map();
  const unresolvedChildren = [];

  for (const child of registeredChildren) {
    let tail = childTailById && childTailById.has(child.id) ? childTailById.get(child.id) : undefined;
    if (tail === undefined) {
      const childTranscriptPath = brainDir
        ? path.join(brainDir, child.id, '.system_generated', 'logs', 'transcript.jsonl')
        : null;
      tail = childTranscriptPath ? readChildTailTelemetry(childTranscriptPath, child, effectiveNow) : null;
    }
    const childLastActiveMs = Math.max(child.lastActiveMs || 0, tail?.latestEventMs || 0);
    const tailModelKey = tail?.latestModelLabel || tail?.speedResult?.modelLabel || '';
    if (tailModelKey && tail?.latestEventMs > 0) {
      sessionModelLastActiveMs[tailModelKey] = Math.max(
        sessionModelLastActiveMs[tailModelKey] || 0,
        childLastActiveMs
      );
    } else if (child.modelHint && child.lastActiveMs > 0) {
      const hintKey = compactModelName(child.modelHint);
      if (hintKey) {
        sessionModelLastActiveMs[hintKey] = Math.max(sessionModelLastActiveMs[hintKey] || 0, child.lastActiveMs);
      }
    }

    const result = tail?.speedResult || null;
    if (result) {
      const candLastActiveMs = Math.max(
        childLastActiveMs,
        result.eventMs || 0,
        sessionModelLastActiveMs[result.modelLabel] || 0
      );
      mergeChildSpeedCandidate(
        liveByModel,
        result.modelLabel,
        result.speed,
        result.eventMs,
        result.mtimeMs,
        child.addedStep || 0,
        resolveChildTurn(child),
        candLastActiveMs
      );
    } else {
      unresolvedChildren.push({ child, childLastActiveMs });
    }
  }

  const byModel = new Map();
  for (const [k, v] of liveByModel.entries()) {
    byModel.set(k, { ...v });
  }

  let diskPersistedSpeeds = {};
  if (unresolvedChildren.length > 0) {
    try {
      diskPersistedSpeeds = readPersistedChildSpeeds(options);
    } catch {
      diskPersistedSpeeds = {};
    }
    for (const { child, childLastActiveMs } of unresolvedChildren) {
      const fallback = (
        resolveChildModelFallbackFromPerf(child, modelPerf, effectiveNow, childLastActiveMs) ||
        resolveChildModelFallbackFromPersisted(child, persistedInTranscript, diskPersistedSpeeds, effectiveNow, childLastActiveMs)
      );
      if (!fallback || liveByModel.has(fallback.modelLabel)) continue;
      const candLastActiveMs = Math.max(
        fallback.lastActiveMs || 0,
        sessionModelLastActiveMs[fallback.modelLabel] || 0
      );
      mergeChildSpeedCandidate(
        byModel,
        fallback.modelLabel,
        fallback.speed,
        fallback.eventMs,
        fallback.mtimeMs,
        child.addedStep || 0,
        resolveChildTurn(child),
        candLastActiveMs
      );
    }
  }

  // Restore any session-observed child model whose transcript was cleaned up
  // and whose main-transcript registration lacked a modelHint.
  for (const entry of Object.values(persistedInTranscript)) {
    const entryTurn = isNonNegFinite(entry.lastActiveTurn) ? Math.trunc(entry.lastActiveTurn) : currentTurns;
    const entryLastActiveMs = Math.max(
      entry.lastActiveMs || 0,
      entry.eventMs || 0,
      entry.updatedAt || 0,
      sessionModelLastActiveMs[entry.model] || 0
    );
    if (byModel.has(entry.model)) {
      const existing = byModel.get(entry.model);
      if (isNonNegFinite(entry.lastActiveTurn) && (!isNonNegFinite(existing.lastActiveTurn) || entryTurn > existing.lastActiveTurn)) {
        existing.lastActiveTurn = entryTurn;
      }
      if (entryLastActiveMs > (existing.lastActiveMs || 0)) {
        existing.lastActiveMs = entryLastActiveMs;
      }
      continue;
    }
    const perf = modelPerf && typeof modelPerf === 'object' ? modelPerf[entry.model] : null;
    const perfSpeed = perf && Number.isFinite(perf.avgSpeed) && perf.avgSpeed > 0 && perf.avgSpeed <= MAX_VALID_SPEED
      ? perf.avgSpeed
      : null;
    const effectiveSpeed = perfSpeed ?? entry.speed;
    if (!Number.isFinite(effectiveSpeed) || effectiveSpeed <= 0 || effectiveSpeed > MAX_VALID_SPEED) continue;
    byModel.set(entry.model, {
      model: entry.model,
      speed: effectiveSpeed,
      eventMs: entry.eventMs || entry.updatedAt || 0,
      mtimeMs: entry.mtimeMs || entry.updatedAt || 0,
      firstOrder: entry.firstOrder || 0,
      lastActiveTurn: entryTurn,
      lastActiveMs: entryLastActiveMs,
    });
  }

  const nextPersisted = { ...persistedInTranscript };
  const diskPatch = {};
  let transcriptChanged = false;

  if (liveByModel.size > 0) {
    for (const [modelLabel, liveEntry] of liveByModel.entries()) {
      const updatedAt = liveEntry.eventMs || liveEntry.mtimeMs || effectiveNow;
      const effectiveTurn = isNonNegFinite(liveEntry.lastActiveTurn) ? Math.trunc(liveEntry.lastActiveTurn) : currentTurns;
      const effectiveLastActiveMs = Math.max(
        liveEntry.lastActiveMs || 0,
        liveEntry.eventMs || 0,
        sessionModelLastActiveMs[modelLabel] || 0
      );
      const prev = nextPersisted[modelLabel];
      if (
        !prev ||
        Math.abs(prev.speed - liveEntry.speed) > 1e-9 ||
        prev.eventMs !== liveEntry.eventMs ||
        prev.mtimeMs !== liveEntry.mtimeMs ||
        prev.firstOrder !== liveEntry.firstOrder ||
        prev.lastActiveTurn !== effectiveTurn ||
        (prev.lastActiveMs || 0) !== effectiveLastActiveMs
      ) {
        nextPersisted[modelLabel] = {
          model: modelLabel,
          speed: liveEntry.speed,
          updatedAt,
          eventMs: liveEntry.eventMs,
          mtimeMs: liveEntry.mtimeMs,
          firstOrder: liveEntry.firstOrder,
          lastActiveTurn: effectiveTurn,
          lastActiveMs: effectiveLastActiveMs,
        };
        transcriptChanged = true;
      }
      diskPatch[modelLabel] = {
        model: modelLabel,
        speed: liveEntry.speed,
        updatedAt,
      };
    }
  }

  for (const [modelLabel, entry] of Object.entries(nextPersisted)) {
    const activeEntry = byModel.get(modelLabel);
    const lastTurn = activeEntry && isNonNegFinite(activeEntry.lastActiveTurn)
      ? Math.trunc(activeEntry.lastActiveTurn)
      : (isNonNegFinite(entry.lastActiveTurn) ? Math.trunc(entry.lastActiveTurn) : currentTurns);
    const turnsAgo = Math.max(0, currentTurns - lastTurn);
    const entryLastActiveMs = Math.max(
      activeEntry?.lastActiveMs || 0,
      entry.lastActiveMs || 0,
      entry.eventMs || 0,
      entry.updatedAt || 0,
      sessionModelLastActiveMs[modelLabel] || 0
    );
    const isTimeExpired = entryLastActiveMs > 0 && (effectiveNow - entryLastActiveMs) > maxIdleMs;
    if (turnsAgo > maxIdleTurns || isTimeExpired) {
      delete nextPersisted[modelLabel];
      delete diskPatch[modelLabel];
      transcriptChanged = true;
    }
  }

  if (transcriptChanged) {
    scanned.persistedChildSpeeds = nextPersisted;
    persistTranscriptChildSpeeds(transcriptPath, nextPersisted);
  }
  if (Object.keys(diskPatch).length > 0) {
    try {
      writePersistedChildSpeeds(diskPatch, options);
    } catch {
      // Best-effort disk speed cache persistence
    }
  }

  if (byModel.size === 0) return [];

  const activeCandidates = [];
  for (const item of byModel.values()) {
    if (!Number.isFinite(item.speed) || item.speed <= 0 || item.speed > MAX_VALID_SPEED) continue;
    const lastTurn = isNonNegFinite(item.lastActiveTurn) ? Math.trunc(item.lastActiveTurn) : currentTurns;
    const turnsAgo = Math.max(0, currentTurns - lastTurn);
    if (turnsAgo > maxIdleTurns) continue;
    const lastActiveMs = Math.max(item.lastActiveMs || 0, sessionModelLastActiveMs[item.model] || 0);
    if (lastActiveMs > 0 && (effectiveNow - lastActiveMs) > maxIdleMs) continue;
    activeCandidates.push({ ...item, turnsAgo, lastActiveMs });
  }

  if (activeCandidates.length === 0) return [];

  const ranked = activeCandidates
    .sort((a, b) => (b.eventMs - a.eventMs) || (b.mtimeMs - a.mtimeMs))
    .slice(0, MAX_CHILD_MODELS_DISPLAY);

  ranked.sort((a, b) => (a.firstOrder - b.firstOrder) || a.model.localeCompare(b.model));
  return ranked.map(({ model, speed, turnsAgo, lastActiveMs }) => {
    const out = { model, speed };
    Object.defineProperty(out, 'turnsAgo', {
      value: turnsAgo,
      enumerable: false,
      configurable: true,
    });
    Object.defineProperty(out, 'lastActiveMs', {
      value: lastActiveMs || 0,
      enumerable: false,
      configurable: true,
    });
    return out;
  });
}

function mergeCostModelsInto(target, source) {
  if (!isObject(source)) return;
  for (const [modelKey, stats] of Object.entries(source)) {
    if (!modelKey || !isObject(stats)) continue;
    const costUsd = isNonNegFinite(stats.costUsd) ? stats.costUsd : 0;
    const inputTokens = toNonNegInt(stats.inputTokens);
    const cacheReadTokens = toNonNegInt(stats.cacheReadTokens);
    const cacheWriteTokens = toNonNegInt(stats.cacheWriteTokens);
    const outputTokens = toNonNegInt(stats.outputTokens);
    const steps = toNonNegInt(stats.steps);
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

function buildSessionCostBreakdown(transcriptPath, scanned, allChildIds, dbCostTree) {
  const { brainDir, mainConversationId } = resolveBrainContext(transcriptPath);
  const byModel = {};

  if (dbCostTree && typeof dbCostTree === 'object') {
    const coveredIds = new Set(Array.isArray(dbCostTree.matchedDbIds) ? dbCostTree.matchedDbIds : []);
    const useTranscriptMain = scanned.transcriptCostUsd > (dbCostTree.mainCostUsd || 0) + 1e-9;
    const mainCostUsd = useTranscriptMain ? scanned.transcriptCostUsd : (dbCostTree.mainCostUsd || 0);
    mergeCostModelsInto(byModel, useTranscriptMain ? scanned.transcriptCostByModel : dbCostTree.mainByModel);

    let childrenCostUsd = dbCostTree.childrenCostUsd || 0;
    let childSessionCount = dbCostTree.childSessionCount || 0;
    const coveredChildOverrides = new Map();
    const uncoveredChildren = [];

    if (brainDir && allChildIds.length > 0) {
      for (const childId of allChildIds) {
        const childTranscriptPath = path.join(brainDir, childId, '.system_generated', 'logs', 'transcript.jsonl');
        if (!fs.existsSync(childTranscriptPath)) continue;
        const childScanned = scanTranscriptIncremental(childTranscriptPath);
        if (coveredIds.has(childId)) {
          const dbChildCost = Number.isFinite(dbCostTree.matchedCostById?.[childId])
            ? dbCostTree.matchedCostById[childId]
            : 0;
          if (childScanned.transcriptCostUsd > dbChildCost + 1e-9) {
            childrenCostUsd += (childScanned.transcriptCostUsd - dbChildCost);
            coveredChildOverrides.set(childId, childScanned.transcriptCostByModel);
          }
        } else if (
          childScanned.steps > 0 ||
          childScanned.transcriptCostUsd > 0 ||
          Object.keys(childScanned.transcriptCostByModel).length > 0
        ) {
          childSessionCount += 1;
          childrenCostUsd += childScanned.transcriptCostUsd;
          uncoveredChildren.push(childScanned.transcriptCostByModel);
        }
      }
    }

    if (coveredChildOverrides.size > 0 && isObject(dbCostTree.matchedModelsById)) {
      for (const dbId of coveredIds) {
        if (mainConversationId && dbId === mainConversationId) continue;
        if (coveredChildOverrides.has(dbId)) {
          mergeCostModelsInto(byModel, coveredChildOverrides.get(dbId));
        } else {
          mergeCostModelsInto(byModel, dbCostTree.matchedModelsById[dbId]);
        }
      }
    } else {
      mergeCostModelsInto(byModel, dbCostTree.childrenByModel);
      if (coveredChildOverrides.size > 0) {
        for (const overrideModels of coveredChildOverrides.values()) {
          mergeCostModelsInto(byModel, overrideModels);
        }
      }
    }
    for (const extraModels of uncoveredChildren) {
      mergeCostModelsInto(byModel, extraModels);
    }

    return {
      totalCostUsd: mainCostUsd + childrenCostUsd,
      mainCostUsd,
      childrenCostUsd,
      childSessionCount,
      byModel,
      source: 'db',
    };
  }

  const mainCostUsd = scanned.transcriptCostUsd || 0;
  mergeCostModelsInto(byModel, scanned.transcriptCostByModel);
  let childrenCostUsd = 0;
  let childSessionCount = 0;

  if (brainDir && allChildIds.length > 0) {
    for (const childId of allChildIds) {
      const childTranscriptPath = path.join(brainDir, childId, '.system_generated', 'logs', 'transcript.jsonl');
      if (!fs.existsSync(childTranscriptPath)) continue;
      const childScanned = scanTranscriptIncremental(childTranscriptPath);
      if (childScanned.steps > 0 || childScanned.transcriptCostUsd > 0 || Object.keys(childScanned.transcriptCostByModel).length > 0) {
        childSessionCount += 1;
        childrenCostUsd += childScanned.transcriptCostUsd;
        mergeCostModelsInto(byModel, childScanned.transcriptCostByModel);
      }
    }
  }

  return {
    totalCostUsd: mainCostUsd + childrenCostUsd,
    mainCostUsd,
    childrenCostUsd,
    childSessionCount,
    byModel,
    source: 'transcript',
  };
}

async function getSessionState(transcriptPath, customCwd = null, options = {}) {
  let branch = null;
  let gitPath = null;

  const scanned = scanTranscriptIncremental(transcriptPath);
  const {
    steps,
    usage,
    maxHistoricalCache,
    totalApiDurationMs,
    lastPlannerResponse,
    compactionCount,
    totalInputTokens,
    totalOutputTokens,
    totalCacheReadTokens,
    totalCacheCreationTokens,
    peakRawTurnTokens,
    lastTurnUsage,
    turnHistoryMaxByMode,
  } = scanned;

  const activeImageExhausted = (
    scanned.imageExhausted &&
    Date.parse(scanned.imageExhausted.resetTime) > Date.now()
  )
    ? scanned.imageExhausted
    : null;

  const cwd = customCwd || process.cwd();

  try {
    gitPath = resolveSafeExecutable('git');
    if (gitPath) {
      const gitBranch = execFileSync(gitPath, ['rev-parse', '--abbrev-ref', 'HEAD'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        cwd,
        timeout: 1500,
      }).trim();
      if (gitBranch) {
        branch = gitBranch;
      }
    }
  } catch {
    branch = null;
  }

  const normalizedCwd = cwd.replace(/\\/g, '/');
  const projectKey = normalizedCwd.replace(/:/g, '').replace(/\//g, '-');
  const projectMemoryDir = path.join(os.homedir(), '.claude', 'projects', projectKey, 'memory');

  // Detect memory files (AGENTS.md first).
  const memoryCandidates = [
    'AGENTS.md', 'agents.md',
    'GEMINI.md', 'gemini.md',
    'CLAUDE.md', 'claude.md',
    'MEMORY.md', 'memory.md',
  ];
  let memoryFile;
  try {
    const workspaceEntries = new Set(fs.readdirSync(cwd));
    for (const candidate of memoryCandidates) {
      if (workspaceEntries.has(candidate)) {
        memoryFile = candidate;
        break;
      }
    }
  } catch {}
  if (memoryFile === undefined) {
    for (const candidate of memoryCandidates) {
      if (fs.existsSync(path.join(cwd, candidate))) {
        memoryFile = candidate;
        break;
      }
    }
  }
  if (memoryFile === undefined) {
    const projectMemoryPath = path.join(projectMemoryDir, 'MEMORY.md');
    if (fs.existsSync(projectMemoryPath)) {
      memoryFile = 'MEMORY.md';
    }
  }

  // Count rules
  let rulesCount = 0;
  const ruleDirs = [
    path.join(cwd, '.claude', 'rules'),
    path.join(cwd, '.cursor', 'rules'),
    path.join(cwd, '.github', 'rules'),
    path.join(cwd, '.gemini', 'rules')
  ];
  for (const dir of ruleDirs) {
    if (fs.existsSync(dir)) {
      try {
        const files = fs.readdirSync(dir);
        rulesCount += files.filter(f => f.endsWith('.md')).length;
      } catch {}
    }
  }

  // Count git hooks
  let hooksCount = 0;
  let hooksDir = path.join(cwd, '.git', 'hooks');
  if (gitPath) {
    try {
      const gitHooksPath = execFileSync(gitPath, ['rev-parse', '--git-path', 'hooks'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        cwd,
        timeout: 1500,
      }).trim();
      hooksDir = path.isAbsolute(gitHooksPath)
        ? gitHooksPath
        : path.resolve(cwd, gitHooksPath);
    } catch {}
  }
  if (fs.existsSync(hooksDir)) {
    try {
      const files = fs.readdirSync(hooksDir);
      const activeHooks = files.filter(f => {
        return !f.endsWith('.sample') &&
               !f.endsWith('.disabled') &&
               fs.statSync(path.join(hooksDir, f)).isFile();
      });
      hooksCount = activeHooks.length;
    } catch {}
  }

  // Count MCP servers
  let mcpCount = 0;
  try {
    const settingsPath = resolveAntigravityPath('settings.json');
    if (fs.existsSync(settingsPath)) {
      const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
      const mcps = settings.mcpServers || settings.mcp;
      if (mcps && typeof mcps === 'object') {
        mcpCount += Object.keys(mcps).length;
      }
    }
  } catch {}
  try {
    const homeDir = process.env.HOME || process.env.USERPROFILE || os.homedir();
    const claudeConfigPath = process.platform === 'win32'
      ? path.join(homeDir, 'AppData', 'Roaming', 'Claude', 'claude_desktop_config.json')
      : process.platform === 'darwin'
        ? path.join(homeDir, 'Library', 'Application Support', 'Claude', 'claude_desktop_config.json')
        : path.join(process.env.XDG_CONFIG_HOME || path.join(homeDir, '.config'), 'Claude', 'claude_desktop_config.json');
    if (fs.existsSync(claudeConfigPath)) {
      const config = JSON.parse(fs.readFileSync(claudeConfigPath, 'utf8'));
      const mcps = config.mcpServers || config.mcp;
      if (mcps && typeof mcps === 'object') {
        mcpCount += Object.keys(mcps).length;
      }
    }
  } catch {}

  const compactionOpts = {};
  if (Array.isArray(options?.roots)) compactionOpts.roots = options.roots;
  if (options?.env) compactionOpts.env = options.env;
  if (options?.homeDir) compactionOpts.homeDir = options.homeDir;
  if (Object.prototype.hasOwnProperty.call(options || {}, 'sqliteModule')) {
    compactionOpts.sqliteModule = options.sqliteModule;
  }
  if (Object.prototype.hasOwnProperty.call(options || {}, 'sqlite3Path')) {
    compactionOpts.sqlite3Path = options.sqlite3Path;
  }

  let serverCompactionLimit = 512000;
  try {
    const resolvedCompaction = resolveServerCompactionLimit({ transcriptPath, ...compactionOpts });
    if (resolvedCompaction && typeof resolvedCompaction.serverLimit === 'number') {
      serverCompactionLimit = resolvedCompaction.serverLimit;
    }
  } catch {
    // Fallback to default limit
  }

  const nowMs = isNonNegFinite(options?.now) ? options.now : Date.now();
  const allChildIds = Object.keys(scanned.childAgents || {});
  const effectiveTurns = scanned.userTurns > 0
    ? scanned.userTurns
    : ((scanned.hasPlannerResponse || scanned.steps > 0) ? 1 : 0);
  const currentSessionModelTurnsAgo = {};
  const recordSessionModelAgo = (mKey, lastTurn) => {
    if (!mKey || !isNonNegFinite(lastTurn)) return;
    const ago = Math.max(0, effectiveTurns - Math.trunc(lastTurn));
    currentSessionModelTurnsAgo[mKey] = currentSessionModelTurnsAgo[mKey] === undefined
      ? ago
      : Math.min(currentSessionModelTurnsAgo[mKey], ago);
  };
  for (const [mKey, lastTurn] of Object.entries(scanned.modelLastSeenTurn || {})) {
    recordSessionModelAgo(mKey, lastTurn);
  }
  for (const child of Object.values(scanned.childAgents || {})) {
    if (!child || typeof child !== 'object') continue;
    const mKey = compactModelName(child.modelHint || '');
    const childTurn = isNonNegFinite(child.lastActiveTurn)
      ? child.lastActiveTurn
      : (isNonNegFinite(child.addedTurn) ? child.addedTurn : effectiveTurns);
    if (mKey) recordSessionModelAgo(mKey, childTurn);
  }
  for (const [mKey, entry] of Object.entries(scanned.persistedChildSpeeds || {})) {
    if (entry && isNonNegFinite(entry.lastActiveTurn)) {
      recordSessionModelAgo(mKey, entry.lastActiveTurn);
    }
  }

  const currentSessionModelLastActiveMs = {};
  const recordSessionModelActiveMs = (mKey, timeMs) => {
    if (!mKey || !isNonNegFinite(timeMs) || timeMs <= 0) return;
    const ts = Math.trunc(timeMs);
    currentSessionModelLastActiveMs[mKey] = Math.max(currentSessionModelLastActiveMs[mKey] || 0, ts);
  };
  for (const [mKey, timeMs] of Object.entries(scanned.modelLastActiveMs || {})) {
    recordSessionModelActiveMs(mKey, timeMs);
  }
  const { brainDir } = resolveBrainContext(transcriptPath);
  const childTailById = new Map();
  for (const child of Object.values(scanned.childAgents || {})) {
    if (!child || typeof child !== 'object' || !child.id) continue;
    const childTranscriptPath = brainDir
      ? path.join(brainDir, child.id, '.system_generated', 'logs', 'transcript.jsonl')
      : null;
    const tail = childTranscriptPath ? readChildTailTelemetry(childTranscriptPath, child, nowMs) : null;
    childTailById.set(child.id, tail);
    const childLastActiveMs = Math.max(child.lastActiveMs || 0, tail?.latestEventMs || 0);
    const tailModelKey = tail?.latestModelLabel || tail?.speedResult?.modelLabel || '';
    if (tailModelKey && tail?.latestEventMs > 0) {
      recordSessionModelActiveMs(tailModelKey, childLastActiveMs);
    }
    const hintKey = compactModelName(child.modelHint || '');
    if (hintKey && child.lastActiveMs > 0) {
      recordSessionModelActiveMs(hintKey, tailModelKey && tailModelKey !== hintKey ? child.lastActiveMs : childLastActiveMs);
    }
  }
  for (const [mKey, entry] of Object.entries(scanned.persistedChildSpeeds || {})) {
    if (!entry || typeof entry !== 'object') continue;
    const ts = Math.max(entry.lastActiveMs || 0, entry.eventMs || 0, entry.updatedAt || 0);
    if (ts > 0) {
      recordSessionModelActiveMs(mKey, ts);
    }
  }

  let modelPerf = {};
  let dbCostTree = null;
  try {
    const resolvedPerf = resolveModelPerfStats({
      transcriptPath,
      childConversationIds: allChildIds,
      currentSessionTurns: effectiveTurns,
      currentSessionModelTurnsAgo,
      currentSessionModelLastActiveMs,
      ...compactionOpts,
    });
    if (resolvedPerf && resolvedPerf.models && typeof resolvedPerf.models === 'object') {
      modelPerf = resolvedPerf.models;
    }
    if (resolvedPerf && resolvedPerf.sessionCostTree && typeof resolvedPerf.sessionCostTree === 'object') {
      dbCostTree = resolvedPerf.sessionCostTree;
    }
  } catch {
    // Fallback to empty modelPerf
  }

  const costBreakdown = buildSessionCostBreakdown(transcriptPath, scanned, allChildIds, dbCostTree);

  const childOpts = {
    ...compactionOpts,
    childTailById,
    modelLastActiveMs: currentSessionModelLastActiveMs,
  };
  if (isNonNegFinite(options?.maxIdleTurns)) {
    childOpts.maxIdleTurns = options.maxIdleTurns;
  }
  if (Number.isFinite(options?.maxIdleMs) && options.maxIdleMs > 0) {
    childOpts.maxIdleMs = options.maxIdleMs;
  }
  const childModelSpeeds = resolveChildModelSpeeds(transcriptPath, scanned, nowMs, modelPerf, childOpts);

  const username = getActiveAccountEmail() || getFallbackUsername();
  const currentDir = path.basename(cwd);
  const state = {
    steps,
    userTurns: effectiveTurns,
    branch,
    memoryFile,
    rulesCount,
    mcpCount,
    hooksCount,
    currentDir,
    username,
    maxHistoricalCache,
    transcriptPath,
    compactionCount,
    totalInputTokens,
    totalOutputTokens,
    totalCacheReadTokens,
    totalCacheCreationTokens,
    peakRawTurnTokens,
    lastTurnUsage,
    turnHistoryMaxByMode,
    serverCompactionLimit,
    childModelSpeeds,
    modelPerf,
    modelLastActiveMs: currentSessionModelLastActiveMs,
    costBreakdown,
  };
  if (usage) state.usage = usage;
  if (activeImageExhausted) state.imageExhausted = activeImageExhausted;
  if (totalApiDurationMs > 0) state.totalApiDurationMs = totalApiDurationMs;
  if (lastPlannerResponse) state.lastPlannerResponse = lastPlannerResponse;
  return state;
}

/**
 * Parses the stdin JSON data provided by agy.
 * @param {string} jsonStr
 * @returns {Object|null}
 */
function parseAgyInput(jsonStr) {
  try {
    return JSON.parse(jsonStr);
  } catch {
    return null;
  }
}

function parseQuotaWindow(entry) {
  if (!entry || typeof entry !== 'object' || Array.isArray(entry)) return null;
  const rawFraction = entry.remaining_fraction;
  if (typeof rawFraction !== 'number' || !Number.isFinite(rawFraction)) return null;
  const remainingFraction = Math.min(1, Math.max(0, rawFraction));
  let resetTime = null;
  if (typeof entry.reset_time === 'string') {
    const trimmed = entry.reset_time.trim();
    if (trimmed && Number.isFinite(Date.parse(trimmed))) {
      resetTime = trimmed;
    }
  }
  return { remainingFraction, resetTime };
}

/**
 * Parses agy stdin `quota` map into internal providerQuota structure.
 * Supports gemini-5h, gemini-weekly, 3p-5h/claude-5h/anthropic-5h,
 * 3p-weekly/claude-weekly/anthropic-weekly, argon* prefix, and
 * openai-* / codex-* / gpt-* prefixes.
 * @param {Object} quotaObj
 * @returns {Object|null}
 */
function parseAgyQuota(quotaObj) {
  if (!quotaObj || typeof quotaObj !== 'object' || Array.isArray(quotaObj)) return null;
  const result = {};

  const assignWindow = (provider, windowKey, rawEntry) => {
    const win = parseQuotaWindow(rawEntry);
    if (!win) return;
    if (!result[provider]) result[provider] = {};
    result[provider][windowKey] = win;
  };

  const assignWindowByKeyPattern = (provider, lowerKey, rawEntry) => {
    if (/5h|five[_-]?hour/.test(lowerKey)) {
      assignWindow(provider, 'fiveHour', rawEntry);
    } else if (/week/.test(lowerKey)) {
      assignWindow(provider, 'weekly', rawEntry);
    } else {
      const win = parseQuotaWindow(rawEntry);
      if (win) {
        let targetWin = 'weekly';
        if (win.resetTime) {
          const delta = Date.parse(win.resetTime) - Date.now();
          if (Number.isFinite(delta) && delta > 0 && delta <= 6 * 3600 * 1000) {
            targetWin = 'fiveHour';
          }
        }
        if (!result[provider]) result[provider] = {};
        result[provider][targetWin] = win;
      }
    }
  };

  if (quotaObj['gemini-5h']) assignWindow('google', 'fiveHour', quotaObj['gemini-5h']);
  if (quotaObj['gemini-weekly']) assignWindow('google', 'weekly', quotaObj['gemini-weekly']);

  if (quotaObj['anthropic-5h']) assignWindow('claude', 'fiveHour', quotaObj['anthropic-5h']);
  if (quotaObj['claude-5h']) assignWindow('claude', 'fiveHour', quotaObj['claude-5h']);
  if (quotaObj['3p-5h']) assignWindow('claude', 'fiveHour', quotaObj['3p-5h']);

  if (quotaObj['anthropic-weekly']) assignWindow('claude', 'weekly', quotaObj['anthropic-weekly']);
  if (quotaObj['claude-weekly']) assignWindow('claude', 'weekly', quotaObj['claude-weekly']);
  if (quotaObj['3p-weekly']) assignWindow('claude', 'weekly', quotaObj['3p-weekly']);

  for (const [key, entry] of Object.entries(quotaObj)) {
    const lower = key.toLowerCase();
    if (lower.startsWith('argon')) {
      assignWindowByKeyPattern('argon', lower, entry);
    } else if (lower.startsWith('openai') || lower.startsWith('codex') || lower.startsWith('gpt')) {
      assignWindowByKeyPattern('openai', lower, entry);
    }
  }

  return Object.keys(result).length > 0 ? result : null;
}

module.exports = {
  getSessionState,
  parseAgyInput,
  parseAgyQuota,
  resolveChildModelSpeeds,
};
