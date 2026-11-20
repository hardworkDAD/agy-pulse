'use strict';

const fs = require('fs');
const path = require('path');
const { createHash } = require('crypto');
const { resolveAntigravityPath } = require('./paths.js');
const { sanitizeTerminalText, compactModelName } = require('./renderer/format.js');

// Below 500ms API time a rate is mostly noise
const MIN_API_DELTA_MS = 500;
// Active open-turn plain-text reply window (2 minutes)
const MAX_OPEN_TURN_TRAILING_MS = 120000;
const MAX_FIRST_TURN_TRAILING_MS = MAX_OPEN_TURN_TRAILING_MS;
const MAX_SPEED_CACHE_FILES = 32;
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
    const speed = raw.speed ?? null;
    if (!isCount(raw.apiDurationMs) || typeof raw.usageKey !== 'string' || (speed !== null && !isValidSpeed(speed))) {
      return null;
    }
    return {
      apiDurationMs: raw.apiDurationMs,
      usageKey: raw.usageKey,
      modelKey: typeof raw.modelKey === 'string' && raw.modelKey.trim() ? raw.modelKey.trim() : null,
      speed,
      totalOutput: isCount(raw.totalOutput) ? raw.totalOutput : null,
      plannerStartMs: isCount(raw.plannerStartMs) ? raw.plannerStartMs : null,
      plannerEndMs: isCount(raw.plannerEndMs) ? raw.plannerEndMs : null,
      turnBaselineOutput: isCount(raw.turnBaselineOutput) ? raw.turnBaselineOutput : null,
      pending: Boolean(raw.pending),
    };
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
    fs.writeFileSync(tmp, JSON.stringify(state), { mode: 0o600 });
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

function resolveTurnTokens(hasPerTurnOutput, turnOutput, totalOutput, turnBaselineOutput) {
  if (hasPerTurnOutput) {
    return turnOutput;
  }
  if (totalOutput !== null && isCount(turnBaselineOutput) && totalOutput >= turnBaselineOutput) {
    return totalOutput - turnBaselineOutput;
  }
  return 0;
}

/**
 * Calculates output token speed (tokens / second) for the finished response.
 * Follows claude-hud contract and holds the reading until the next response.
 *
 * @param {Object} agyData
 * @param {string} [rootDir]
 * @param {Object} [state]
 * @param {number} [now=Date.now()]
 * @returns {number|null}
 */
function getOutputSpeed(agyData, rootDir = null, state = null, now = Date.now()) {
  const transcriptPath = (agyData?.transcript_path || state?.transcriptPath || agyData?.session_id || '')?.trim();
  const usage = agyData?.context_window?.current_usage || agyData?.context_window || state?.usage?.current_usage || state?.usage;
  const turnOutput = isCount(usage?.output_tokens) ? usage.output_tokens : null;
  const totalOutput = isCount(agyData?.context_window?.total_output_tokens)
    ? agyData.context_window.total_output_tokens
    : (isCount(state?.usage?.total_output_tokens)
      ? state.usage.total_output_tokens
      : (isCount(state?.totalOutputTokens) ? state.totalOutputTokens : null));
  const hasPerTurnOutput = turnOutput !== null;
  const output = turnOutput ?? totalOutput;

  const nativeApiDurationMs = (
    agyData?.cost?.total_api_duration_ms ??
    agyData?.cost?.total_duration_ms ??
    agyData?.total_api_duration_ms
  );
  const hasNativeDuration = isCount(nativeApiDurationMs);
  const planner = !hasNativeDuration ? state?.lastPlannerResponse : null;
  const apiDurationMs = hasNativeDuration ? nativeApiDurationMs : state?.totalApiDurationMs;

  if (!transcriptPath || !usage || !isCount(output)) {
    return null;
  }

  const file = getSpeedCachePath(rootDir, transcriptPath);
  const usageKey = [
    usage.input_tokens ?? 0,
    output,
    usage.cache_creation_input_tokens ?? 0,
    usage.cache_read_input_tokens ?? 0,
    totalOutput ?? '',
  ].join(':');

  const previous = readState(file);
  const rawModel = (
    agyData?.model?.display_name ||
    agyData?.model?.id ||
    (typeof agyData?.model === 'string' ? agyData.model : '') ||
    state?.lastTurnUsage?.model ||
    ''
  );
  const modelKey = rawModel ? (compactModelName(rawModel) || String(rawModel).trim()) : null;
  const isModelChanged = Boolean(previous?.modelKey && modelKey && previous.modelKey !== modelKey);

  // Path A: Transcript-derived per-response timing from agy PLANNER_RESPONSE steps
  if (planner && isCount(planner.startMs)) {
    const isFreshOrLegacy = !previous || previous.plannerStartMs === null;
    const hasPendingFirstTurn = Boolean(previous && previous.pending && previous.speed === null);
    const hasClosedTurn = isCount(planner.endMs) && planner.endMs >= planner.startMs;
    const plannerEndMs = isCount(planner.endMs) ? planner.endMs : null;
    const trailingMs = !hasClosedTurn && now >= planner.startMs ? now - planner.startMs : 0;
    const isSamePlannerTurn = Boolean(
      previous && previous.plannerStartMs !== null && planner.startMs === previous.plannerStartMs
    );
    const isCounterReset = Boolean(
      !hasPerTurnOutput && totalOutput !== null && isCount(previous?.totalOutput) && totalOutput < previous.totalOutput
    );

    let turnBaselineOutput;
    if (isCounterReset) {
      turnBaselineOutput = 0;
    } else if (isSamePlannerTurn) {
      turnBaselineOutput = previous.turnBaselineOutput === null ? 0 : previous.turnBaselineOutput;
    } else if (isFreshOrLegacy) {
      turnBaselineOutput = 0;
    } else {
      turnBaselineOutput = isCount(previous.totalOutput) ? previous.totalOutput : null;
    }

    const baseState = {
      apiDurationMs: isCount(apiDurationMs) ? apiDurationMs : 0,
      usageKey,
      modelKey,
      totalOutput,
      plannerStartMs: planner.startMs,
      plannerEndMs,
      turnBaselineOutput,
    };

    if (!hasClosedTurn && (planner.interrupted === true || trailingMs > MAX_OPEN_TURN_TRAILING_MS)) {
      const fallbackSpeed = (isFreshOrLegacy || isCounterReset || isModelChanged) ? null : previous.speed;
      if (
        !isFreshOrLegacy &&
        !isCounterReset &&
        !isModelChanged &&
        previous.pending === false &&
        previous.usageKey === usageKey &&
        previous.plannerStartMs === planner.startMs &&
        previous.plannerEndMs === null
      ) {
        return fallbackSpeed;
      }
      writeState(file, { ...baseState, speed: fallbackSpeed, pending: false });
      return fallbackSpeed;
    }

    const hasActiveFirstTurn = (
      (!previous || hasPendingFirstTurn) &&
      !hasClosedTurn &&
      trailingMs >= MIN_API_DELTA_MS &&
      trailingMs <= MAX_OPEN_TURN_TRAILING_MS
    );
    const resetRequired = (
      ((isFreshOrLegacy || hasPendingFirstTurn) && !hasClosedTurn && !hasActiveFirstTurn) ||
      isCounterReset
    );
    if (resetRequired) {
      writeState(file, { ...baseState, speed: null, pending: true });
      return null;
    }

    if (
      !isFreshOrLegacy &&
      !isModelChanged &&
      !previous.pending &&
      previous.speed !== null &&
      (previous.plannerEndMs !== null || plannerEndMs === null) &&
      usageKey === previous.usageKey &&
      planner.startMs === previous.plannerStartMs
    ) {
      return previous.speed;
    }

    const turnTokens = resolveTurnTokens(
      hasPerTurnOutput,
      turnOutput,
      totalOutput,
      turnBaselineOutput
    );
    const durationMs = hasClosedTurn
      ? planner.endMs - planner.startMs
      : (now >= planner.startMs ? now - planner.startMs : 0);
    const tokensAligned = (
      planner.outputTokens === null ||
      Math.abs(turnTokens - planner.outputTokens) <= Math.max(5, Math.round(turnTokens * 0.05))
    );
    const fallbackSpeed = (isFreshOrLegacy || isModelChanged) ? null : previous.speed;
    const rawComputedSpeed = tokensAligned && turnTokens > 0 && durationMs >= MIN_API_DELTA_MS
      ? turnTokens / (durationMs / 1000)
      : null;
    const speed = isValidSpeed(rawComputedSpeed) ? rawComputedSpeed : fallbackSpeed;

    const pending = Boolean(durationMs < MIN_API_DELTA_MS || speed === null);
    writeState(file, { ...baseState, speed, pending });
    return speed;
  }

  // Path B: Cumulative API duration (native agyData.cost or legacy state.totalApiDurationMs)
  if (!isCount(apiDurationMs)) {
    return null;
  }

  const resetRequired = (
    !previous ||
    apiDurationMs < previous.apiDurationMs ||
    (!hasPerTurnOutput && totalOutput !== null && previous.totalOutput !== null && totalOutput < previous.totalOutput)
  );
  if (resetRequired) {
    writeState(file, {
      apiDurationMs,
      usageKey,
      modelKey,
      speed: null,
      totalOutput,
      plannerStartMs: null,
      turnBaselineOutput: null,
    });
    return null;
  }

  if (apiDurationMs === previous.apiDurationMs) {
    if (isModelChanged) {
      writeState(file, {
        apiDurationMs,
        usageKey,
        modelKey,
        speed: null,
        totalOutput,
        plannerStartMs: null,
        turnBaselineOutput: null,
      });
      return null;
    }
    return previous.speed;
  }

  const deltaMs = apiDurationMs - previous.apiDurationMs;
  const pathBBaseline = isCount(previous.totalOutput) ? previous.totalOutput : null;
  const turnTokens = resolveTurnTokens(hasPerTurnOutput, turnOutput, totalOutput, pathBBaseline);
  const isNewResponse = usageKey !== previous.usageKey && turnTokens > 0;
  const rawComputedSpeed = isNewResponse && deltaMs >= MIN_API_DELTA_MS
    ? turnTokens / (deltaMs / 1000)
    : null;
  const fallbackSpeed = isModelChanged ? null : previous.speed;
  const speed = isValidSpeed(rawComputedSpeed) ? rawComputedSpeed : fallbackSpeed;

  writeState(file, {
    apiDurationMs,
    usageKey,
    modelKey,
    speed,
    totalOutput,
    plannerStartMs: null,
    turnBaselineOutput: null,
  });
  return speed;
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
