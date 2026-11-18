'use strict';

const fs = require('fs');
const path = require('path');
const { createHash } = require('crypto');
const { resolveAntigravityPath } = require('./paths.js');

// Below 500ms API time a rate is mostly noise
const MIN_API_DELTA_MS = 500;

function isCount(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
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

function readState(file) {
  try {
    const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
    const speed = raw.speed ?? null;
    if (!isCount(raw.apiDurationMs) || typeof raw.usageKey !== 'string' || (speed !== null && !isCount(speed))) {
      return null;
    }
    return {
      apiDurationMs: raw.apiDurationMs,
      usageKey: raw.usageKey,
      speed,
      totalOutput: isCount(raw.totalOutput) ? raw.totalOutput : null,
      plannerStartMs: isCount(raw.plannerStartMs) ? raw.plannerStartMs : null,
    };
  } catch {
    return null;
  }
}

function writeState(file, state) {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const tmp = `${file}.${process.pid}.${Date.now()}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(state), { mode: 0o600 });
    fs.renameSync(tmp, file);
  } catch {
    // Speed is cosmetic; safe fallback on permission/filesystem issues
  }
}

function resolveTurnTokens(hasPerTurnOutput, turnOutput, totalOutput, previous) {
  if (hasPerTurnOutput) {
    return turnOutput;
  }
  if (totalOutput !== null && previous && isCount(previous.totalOutput) && totalOutput >= previous.totalOutput) {
    return totalOutput - previous.totalOutput;
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
    : (isCount(state?.usage?.total_output_tokens) ? state.usage.total_output_tokens : null);
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

  // Path A: Transcript-derived per-response timing from agy PLANNER_RESPONSE steps
  if (planner && isCount(planner.startMs)) {
    const isFreshOrLegacy = !previous || previous.plannerStartMs === null;
    const hasClosedTurn = isCount(planner.endMs) && planner.endMs >= planner.startMs && hasPerTurnOutput;
    const resetRequired = (
      (isFreshOrLegacy && !hasClosedTurn) ||
      (!hasPerTurnOutput && totalOutput !== null && previous?.totalOutput !== null && totalOutput < previous.totalOutput)
    );
    const baseState = {
      apiDurationMs: isCount(apiDurationMs) ? apiDurationMs : 0,
      usageKey,
      totalOutput,
      plannerStartMs: planner.startMs,
    };
    if (resetRequired) {
      writeState(file, { ...baseState, speed: null });
      return null;
    }

    if (!isFreshOrLegacy && usageKey === previous.usageKey && planner.startMs === previous.plannerStartMs) {
      return previous.speed;
    }

    const turnTokens = resolveTurnTokens(hasPerTurnOutput, turnOutput, totalOutput, previous);
    const durationMs = isCount(planner.endMs) && planner.endMs >= planner.startMs
      ? planner.endMs - planner.startMs
      : (now >= planner.startMs ? now - planner.startMs : 0);
    const tokensAligned = !hasPerTurnOutput || planner.outputTokens === null || turnTokens === planner.outputTokens;
    const fallbackSpeed = isFreshOrLegacy ? null : previous.speed;
    const speed = tokensAligned && turnTokens > 0 && durationMs >= MIN_API_DELTA_MS
      ? turnTokens / (durationMs / 1000)
      : fallbackSpeed;

    writeState(file, { ...baseState, speed });
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
    writeState(file, { apiDurationMs, usageKey, speed: null, totalOutput, plannerStartMs: null });
    return null;
  }

  if (apiDurationMs === previous.apiDurationMs) {
    return previous.speed;
  }

  const deltaMs = apiDurationMs - previous.apiDurationMs;
  const turnTokens = resolveTurnTokens(hasPerTurnOutput, turnOutput, totalOutput, previous);
  const isNewResponse = usageKey !== previous.usageKey && turnTokens > 0;
  const speed = isNewResponse && deltaMs >= MIN_API_DELTA_MS
    ? turnTokens / (deltaMs / 1000)
    : previous.speed;

  writeState(file, { apiDurationMs, usageKey, speed, totalOutput, plannerStartMs: null });
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

module.exports = {
  getOutputSpeed,
  formatSpeed,
  getSpeedCachePath,
};
