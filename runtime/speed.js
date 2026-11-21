'use strict';

const fs = require('fs');
const path = require('path');
const { createHash } = require('crypto');
const { resolveAntigravityPath } = require('./paths.js');
const { sanitizeTerminalText } = require('./renderer/format.js');

// Below 500ms API time a rate is mostly noise
const MIN_API_DELTA_MS = 500;
// Active open-turn plain-text reply window (2 minutes)
const MAX_OPEN_TURN_TRAILING_MS = 120000;
const MAX_FIRST_TURN_TRAILING_MS = MAX_OPEN_TURN_TRAILING_MS;
const MAX_SPEED_CACHE_FILES = 32;
const SPEED_CACHE_VERSION = 1;
const MAX_CHILD_MODELS_DISPLAY = 3;
const MAX_VALID_SPEED = 5000;

function isCount(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function isValidSpeed(value) {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 && value <= MAX_VALID_SPEED;
}

function getSpeedCachePath(rootDir, transcriptPath) {
  const baseDir = rootDir || resolveAntigravityPath('speed-cache');
  const cacheDir = path.basename(baseDir) === 'speed-cache' ? baseDir : path.join(baseDir, 'speed-cache');
  const str = String(transcriptPath || '');
  const target = (path.isAbsolute(str) || str.includes('/') || str.includes('\\'))
    ? path.resolve(str)
    : str;
  const key = createHash('sha256').update(target).digest('hex');
  return path.join(cacheDir, `${key}.json`);
}

function pruneSpeedCacheDir(dirPath, maxFiles = MAX_SPEED_CACHE_FILES) {
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

function readState(file) {
  try {
    const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
    if (raw?.version !== SPEED_CACHE_VERSION || (raw.speed !== null && !isValidSpeed(raw.speed))) {
      return null;
    }
    return raw;
  } catch {
    return null;
  }
}

function writeState(file, state) {
  let tmp = null;
  try {
    const dir = path.dirname(file);
    fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
    tmp = `${file}.${process.pid}.${Date.now()}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify({ version: SPEED_CACHE_VERSION, ...state }), { mode: 0o600 });
    fs.renameSync(tmp, file);
    tmp = null;
    pruneSpeedCacheDir(dir, MAX_SPEED_CACHE_FILES);
  } catch {
    if (tmp) {
      try {
        fs.unlinkSync(tmp);
      } catch {}
    }
    // Speed is cosmetic; safe fallback on permission/filesystem issues
  }
}

function sampleApiSpeed(file, previous, modelKey, apiDurationMs, totalOutput) {
  let speed = null;
  if (
    previous?.source === 'api' && previous.modelKey === modelKey &&
    isCount(previous.apiDurationMs) && isCount(previous.totalOutput)
  ) {
    const deltaMs = apiDurationMs - previous.apiDurationMs;
    const deltaTokens = totalOutput - previous.totalOutput;
    if (deltaMs === 0 && deltaTokens === 0) return previous.speed;
    if (deltaMs >= MIN_API_DELTA_MS && deltaTokens > 0) {
      const rate = deltaTokens / (deltaMs / 1000);
      if (isValidSpeed(rate)) speed = rate;
    }
  }

  // After a one-sided update or reset, start from the new paired snapshot.
  writeState(file, { source: 'api', modelKey, speed, apiDurationMs, totalOutput });
  return speed;
}

function samplePlannerSpeed(file, previous, modelKey, planner, now) {
  const plannerStartMs = planner.startMs;
  const plannerEndMs = isCount(planner.endMs) ? planner.endMs : null;
  const plannerOutputTokens = isCount(planner.outputTokens) ? planner.outputTokens : null;
  const sameModel = previous?.source === 'transcript' && previous.modelKey === modelKey;
  const sameResponse = previous?.source === 'transcript' && previous.plannerStartMs === plannerStartMs;
  // A model change can arrive before the next response appears in the transcript.
  if (sameResponse && !sameModel) return null;
  const sameSample = sameResponse &&
    previous.plannerEndMs === plannerEndMs && previous.plannerOutputTokens === plannerOutputTokens;
  if (sameSample && previous.pending === false) return previous.speed;

  const isOpen = plannerEndMs === null;
  const durationMs = (plannerEndMs ?? now) - plannerStartMs;
  const expired = isOpen && (planner.interrupted === true || durationMs > MAX_OPEN_TURN_TRAILING_MS);
  let speed = null;
  let pending = false;
  if (plannerOutputTokens > 0) {
    const tooShort = durationMs < MIN_API_DELTA_MS;
    if (expired || (isOpen && tooShort)) {
      speed = sameModel ? previous.speed : null;
    } else if (!tooShort) {
      const rate = plannerOutputTokens / (durationMs / 1000);
      if (isValidSpeed(rate)) speed = rate;
    }
    pending = isOpen && !expired && (tooShort || speed === null);
  }

  if (sameSample && previous.speed === speed && previous.pending === pending) return speed;
  writeState(file, {
    source: 'transcript', modelKey, speed, plannerStartMs, plannerEndMs, plannerOutputTokens, pending,
  });
  return speed;
}

/**
 * Calculates output token speed from paired cumulative API counters or a
 * single timed planner response.
 *
 * @param {Object} agyData
 * @param {string} [rootDir]
 * @param {Object} [state]
 * @param {number} [now=Date.now()]
 * @returns {number|null}
 */
function getOutputSpeed(agyData, rootDir = null, state = null, now = Date.now()) {
  const transcriptPath = (agyData?.transcript_path || state?.transcriptPath || agyData?.session_id || '')?.trim();
  if (!transcriptPath) return null;

  const modelKey = String(
    agyData?.model?.id ||
    agyData?.model?.display_name ||
    (typeof agyData?.model === 'string' ? agyData.model : '') ||
    state?.lastTurnUsage?.model ||
    ''
  ).trim().toLowerCase();
  const file = getSpeedCachePath(rootDir, transcriptPath);
  const previous = readState(file);
  const apiDurationMs = [agyData?.cost?.total_api_duration_ms, agyData?.total_api_duration_ms].find(isCount);
  const totalOutput = agyData?.context_window?.total_output_tokens;
  if (isCount(apiDurationMs) && isCount(totalOutput)) {
    return sampleApiSpeed(file, previous, modelKey, apiDurationMs, totalOutput);
  }

  const planner = state?.lastPlannerResponse;
  if (isCount(planner?.startMs)) {
    return samplePlannerSpeed(file, previous, modelKey, planner, now);
  }

  if (previous?.source) writeState(file, { modelKey, speed: null });
  return null;
}

/**
 * Formats token output speed string according to locale.
 *
 * @param {number|null} speed
 * @param {string} [lang='en']
 * @returns {string|null}
 */
function formatSpeed(speed, lang = 'en') {
  if (speed === null || speed === undefined || !Number.isFinite(speed)) {
    return null;
  }
  const prefix = lang === 'zh' ? '输出' : 'out';
  return `${prefix}: ${speed.toFixed(1)} tok/s`;
}

/**
 * Formats Time-To-First-Token (TTFT) latency in seconds according to locale.
 *
 * @param {number|null|undefined} ttftSec
 * @param {string} [lang='en']
 * @returns {string|null}
 */
function formatTtft(ttftSec, lang = 'en') {
  if (ttftSec === null || ttftSec === undefined || !Number.isFinite(ttftSec) || ttftSec < 0) {
    return null;
  }
  const valStr = ttftSec < 1 ? `${Math.round(ttftSec * 1000)}ms` : `${ttftSec.toFixed(1)}s`;
  return lang === 'zh' ? `首字 ${valStr}` : `ttft ${valStr}`;
}

/**
 * Formats active swarm/subagent per-model rolling speeds, optionally enriched
 * with historical SQLite average streaming speed and TTFT from `modelPerf`.
 *
 * @param {Array<{ model: string, speed: number }>} childModelSpeeds
 * @param {Object} [options]
 * @param {boolean} [options.unicode=true]
 * @param {boolean} [options.useNerd=false]
 * @param {Record<string, { avgSpeed?: number|null, avgTtftSec?: number|null }>|null} [options.modelPerf=null]
 * @param {string} [options.lang='en']
 * @param {boolean} [options.includeTtft=true]
 * @returns {string|null}
 */
function formatChildModelSpeeds(childModelSpeeds, options = {}) {
  if (!Array.isArray(childModelSpeeds) || childModelSpeeds.length === 0) {
    return null;
  }
  const {
    unicode = true,
    useNerd = false,
    modelPerf = null,
    lang = 'en',
    includeTtft = true,
  } = options || {};
  const formattedParts = [];
  for (const item of childModelSpeeds) {
    if (!item || typeof item !== 'object') continue;
    const rawModel = String(item.model || '').trim();
    const label = sanitizeTerminalText(rawModel, 24) || 'Sub';
    const perf = modelPerf && typeof modelPerf === 'object' && rawModel ? modelPerf[rawModel] : null;
    const perfSpeedValid = perf && isValidSpeed(perf.avgSpeed);
    const effectiveSpeed = perfSpeedValid ? perf.avgSpeed : item.speed;
    if (!isValidSpeed(effectiveSpeed)) continue;
    const ttftStr = (includeTtft !== false && perf && Number.isFinite(perf.avgTtftSec))
      ? formatTtft(perf.avgTtftSec, lang)
      : null;
    formattedParts.push(
      ttftStr
        ? `${label}: ${effectiveSpeed.toFixed(1)} tok/s (${ttftStr})`
        : `${label}: ${effectiveSpeed.toFixed(1)} tok/s`
    );
  }
  if (formattedParts.length === 0) {
    return null;
  }
  const sep = unicode ? ' · ' : ' / ';
  const prefix = useNerd ? '\uf0e7 ' : (unicode ? '⚡ ' : '[sub] ');
  return `${prefix}${formattedParts.join(sep)}`;
}

module.exports = {
  getOutputSpeed,
  formatSpeed,
  formatTtft,
  formatChildModelSpeeds,
  getSpeedCachePath,
  MAX_OPEN_TURN_TRAILING_MS,
  MAX_FIRST_TURN_TRAILING_MS,
  MAX_CHILD_MODELS_DISPLAY,
  MAX_VALID_SPEED,
};
