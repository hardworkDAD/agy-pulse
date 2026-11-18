'use strict';

const fs = require('fs');
const path = require('path');
const os = require('os');
const { createHash } = require('crypto');
const { execFileSync } = require('child_process');
const { getAntigravityRoots, resolveSafeExecutable, resolveAntigravityPath } = require('./paths.js');
const { resolveServerCompactionLimit } = require('./compaction.js');

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
  let ms = 0;
  const segments = [...durationStr.matchAll(/(\d+)([hms])/g)];
  if (segments.length > 0) {
    for (const [, val, unit] of segments) {
      const n = parseInt(val, 10);
      if (unit === 'h') ms += n * 60 * 60 * 1000;
      else if (unit === 'm') ms += n * 60 * 1000;
      else if (unit === 's') ms += n * 1000;
    }
  } else {
    const val = parseInt(durationStr, 10);
    if (Number.isFinite(val)) {
      ms = val * 1000;
    }
  }
  return ms;
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

const TRANSCRIPT_CACHE_VERSION = 1;

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
    totalOutputTokens: 0,
    peakRawTurnTokens: 0,
    lastTurnUsage: null,
    turnHistoryMaxByMode: {
      inclusivePeak: 0,
      exclusivePeak: 0,
    },
  };
}

function getTranscriptCachePath(transcriptPath) {
  const resolved = path.resolve(transcriptPath);
  const hash = createHash('sha256').update(resolved).digest('hex');
  return resolveAntigravityPath(path.join('transcript-cache', `${hash}.json`));
}

function readTranscriptCache(cachePath, expectedIno, fileSize) {
  try {
    if (!cachePath || !fs.existsSync(cachePath)) return null;
    const raw = JSON.parse(fs.readFileSync(cachePath, 'utf8'));
    if (!raw || typeof raw !== 'object') return null;
    if (raw.version !== TRANSCRIPT_CACHE_VERSION) return null;
    if (raw.ino !== expectedIno) return null;
    if (!Number.isInteger(raw.offset) || raw.offset < 0 || raw.offset > fileSize) return null;

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
          }
        : null,
      compactionCount: toNonNegInt(raw.compactionCount),
      totalOutputTokens: toNonNegInt(raw.totalOutputTokens),
      peakRawTurnTokens: toNonNegInt(raw.peakRawTurnTokens),
      lastTurnUsage: isObject(raw.lastTurnUsage) ? raw.lastTurnUsage : null,
      turnHistoryMaxByMode: isObject(raw.turnHistoryMaxByMode)
        ? {
            inclusivePeak: toNonNegInt(raw.turnHistoryMaxByMode.inclusivePeak),
            exclusivePeak: toNonNegInt(raw.turnHistoryMaxByMode.exclusivePeak),
          }
        : base.turnHistoryMaxByMode,
    };

    return { offset: raw.offset, state };
  } catch {
    return null;
  }
}

function writeTranscriptCache(cachePath, ino, offset, accState) {
  if (!cachePath) return;
  let tmpPath = null;
  try {
    fs.mkdirSync(path.dirname(cachePath), { recursive: true, mode: 0o700 });
    tmpPath = `${cachePath}.${process.pid}.${Date.now()}.tmp`;
    const payload = {
      version: TRANSCRIPT_CACHE_VERSION,
      ino,
      offset,
      ...accState,
    };
    fs.writeFileSync(tmpPath, JSON.stringify(payload), { mode: 0o600 });
    fs.renameSync(tmpPath, cachePath);
    tmpPath = null;
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

function loadFullEntriesMap(transcriptPath) {
  const map = new Map();
  try {
    const fullPath = path.join(path.dirname(transcriptPath), 'transcript_full.jsonl');
    if (!fs.existsSync(fullPath)) return map;
    const raw = fs.readFileSync(fullPath, 'utf8');
    for (const line of raw.split('\n')) {
      if (!line.trim()) continue;
      try {
        const obj = JSON.parse(line);
        if (typeof obj.step_index === 'number') {
          map.set(obj.step_index, obj);
        }
      } catch {
        // Skip invalid line in transcript_full.jsonl
      }
    }
  } catch {
    // Ignore missing or unreadable transcript_full.jsonl
  }
  return map;
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
  }

  const entryTimeMs = entry.created_at ? Date.parse(entry.created_at) : null;
  const validTimeMs = entryTimeMs !== null && Number.isFinite(entryTimeMs) ? entryTimeMs : null;

  if (acc.pendingPlannerStartMs !== null) {
    // `created_at` records when a step started; the next non-user, non-system step
    // marks when the preceding PLANNER_RESPONSE ended. USER_INPUT, SYSTEM_MESSAGE,
    // source==='SYSTEM', or missing timestamps invalidate the pending span.
    const isInterruptingStep = (
      entry.type === 'USER_INPUT' ||
      entry.type === 'SYSTEM_MESSAGE' ||
      entry.source === 'SYSTEM'
    );
    if (validTimeMs === null || isInterruptingStep) {
      acc.pendingPlannerStartMs = null;
    } else if (validTimeMs >= acc.pendingPlannerStartMs) {
      acc.totalApiDurationMs += (validTimeMs - acc.pendingPlannerStartMs);
      if (acc.lastPlannerResponse && acc.lastPlannerResponse.endMs === null) {
        acc.lastPlannerResponse.endMs = validTimeMs;
      }
      acc.pendingPlannerStartMs = null;
    } else {
      acc.pendingPlannerStartMs = null;
    }
  }

  if (entry.type === 'PLANNER_RESPONSE' && validTimeMs !== null) {
    acc.pendingPlannerStartMs = validTimeMs;
    const rawPlannerOut = entry.output_tokens ?? entry.context_window?.current_usage?.output_tokens;
    acc.lastPlannerResponse = {
      startMs: validTimeMs,
      endMs: null,
      outputTokens: isNonNegFinite(rawPlannerOut) ? Math.trunc(rawPlannerOut) : null,
    };
  }

  const contextWindow = findContextWindow(entry);
  if (contextWindow) {
    acc.usage = contextWindow;
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

    acc.totalOutputTokens += out;
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

    const rawModel = typeof entry.model === 'string' && entry.model.trim()
      ? entry.model.trim()
      : (typeof entry.model_name === 'string' && entry.model_name.trim() ? entry.model_name.trim() : undefined);
    acc.lastTurnUsage = {
      input_tokens: inp,
      output_tokens: out,
      cache_read_input_tokens: cRead,
      cache_creation_input_tokens: cCreate,
      ...(rawModel ? { model: rawModel } : {}),
    };

    if (!contextWindow) {
      acc.usage = {
        total_input_tokens: inp,
        total_output_tokens: acc.totalOutputTokens,
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
        context.fullEntriesMap = loadFullEntriesMap(context.transcriptPath);
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
        const m = targetContent.match(/quotaResetDelay["']?\s*[:=]\s*["']?([0-9hms]+)/i);
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
  const cached = readTranscriptCache(cachePath, fIno, fSize);

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
        const ctx = { transcriptPath, fullEntriesMap: null };
        for (const line of chunk.split('\n')) {
          processTranscriptLine(line, state, ctx);
        }
        writeTranscriptCache(cachePath, fIno, offset, state);
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
    writeTranscriptCache(cachePath, fIno, 0, state);
  }

  return state;
}

async function getSessionState(transcriptPath, customCwd = null) {
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
    totalOutputTokens,
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
    const claudeConfigPath = process.platform === 'win32'
      ? path.join(os.homedir(), 'AppData', 'Roaming', 'Claude', 'claude_desktop_config.json')
      : path.join(os.homedir(), 'Library', 'Application Support', 'Claude', 'claude_desktop_config.json');
    if (fs.existsSync(claudeConfigPath)) {
      const config = JSON.parse(fs.readFileSync(claudeConfigPath, 'utf8'));
      const mcps = config.mcpServers || config.mcp;
      if (mcps && typeof mcps === 'object') {
        mcpCount += Object.keys(mcps).length;
      }
    }
  } catch {}

  let serverCompactionLimit = 512000;
  try {
    const resolvedCompaction = resolveServerCompactionLimit({ transcriptPath });
    if (resolvedCompaction && typeof resolvedCompaction.serverLimit === 'number') {
      serverCompactionLimit = resolvedCompaction.serverLimit;
    }
  } catch {
    // Fallback to default limit
  }

  const username = getActiveAccountEmail() || getFallbackUsername();
  const currentDir = path.basename(cwd);
  const state = {
    steps,
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
    totalOutputTokens,
    peakRawTurnTokens,
    lastTurnUsage,
    turnHistoryMaxByMode,
    serverCompactionLimit,
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

/**
 * Parses agy stdin `quota` map into internal providerQuota structure.
 * Supports gemini-5h, gemini-weekly, 3p-5h, 3p-weekly.
 * @param {Object} quotaObj
 * @returns {Object|null}
 */
function parseAgyQuota(quotaObj) {
  if (!quotaObj || typeof quotaObj !== 'object') return null;
  const result = {};

  if (quotaObj['gemini-5h'] || quotaObj['gemini-weekly']) {
    result.google = {};
    if (quotaObj['gemini-5h'] && typeof quotaObj['gemini-5h'] === 'object') {
      result.google.fiveHour = {
        remainingFraction: quotaObj['gemini-5h'].remaining_fraction,
        resetTime: quotaObj['gemini-5h'].reset_time,
      };
    }
    if (quotaObj['gemini-weekly'] && typeof quotaObj['gemini-weekly'] === 'object') {
      result.google.weekly = {
        remainingFraction: quotaObj['gemini-weekly'].remaining_fraction,
        resetTime: quotaObj['gemini-weekly'].reset_time,
      };
    }
  }

  if (quotaObj['3p-5h'] || quotaObj['3p-weekly']) {
    result.claude = {};
    if (quotaObj['3p-5h'] && typeof quotaObj['3p-5h'] === 'object') {
      result.claude.fiveHour = {
        remainingFraction: quotaObj['3p-5h'].remaining_fraction,
        resetTime: quotaObj['3p-5h'].reset_time,
      };
    }
    if (quotaObj['3p-weekly'] && typeof quotaObj['3p-weekly'] === 'object') {
      result.claude.weekly = {
        remainingFraction: quotaObj['3p-weekly'].remaining_fraction,
        resetTime: quotaObj['3p-weekly'].reset_time,
      };
    }
  }

  return Object.keys(result).length > 0 ? result : null;
}

module.exports = {
  getSessionState,
  parseAgyInput,
  parseAgyQuota,
};
