'use strict';

const { supportsUnicode } = require('./encoding.js');
const {
  ANSI_COLORS,
  normalizeThemeObject,
  DEFAULT_THRESHOLDS,
  DEFAULT_COLUMN_WIDTH,
  QUOTA_CHROME_WIDTH,
  abbreviateDisplayName,
  simplifyModelName,
  compactModelName,
  modelNamesMatch,
  modelIncludesCacheInInput,
  calculateTurnCacheMetrics,
  formatTurnCacheBadge,
  sanitizeTerminalText,
  getAnsiDisplayWidth,
  truncateAnsiToDisplayWidth,
  truncateToDisplayWidth,
  formatTokens,
  formatDuration,
  formatQuotaPercent,
} = require('./renderer/format.js');
const {
  LANGUAGE_TEXT,
  resolveLanguage,
} = require('./renderer/lang.js');
const { createQuotaRenderers } = require('./renderer/quota-render.js');
const { renderContextMonitorLine, renderModelCacheLine } = require('./renderer/context-monitor.js');
const { pickCriticalWindow, isObservationExpired, resolveModelDisplayName } = require('./quota/models.js');
const { isNewerVersion } = require('./update-checker.js');
let PACKAGE_VERSION = '0.0.0';
try {
  PACKAGE_VERSION = require('../package.json').version || '0.0.0';
} catch {}
const { getSessionCostUsd, formatUsd } = require('./cost.js');
const { getCostTotals } = require('./daily-cost.js');
const {
  getOutputSpeed,
  formatTtft,
  formatChildModelSpeeds,
  MAX_CHILD_MODELS_DISPLAY,
} = require('./speed.js');

/**
 * Merges session-level child model speeds with cross-session persisted models
 * from `state.modelPerf` (excluding `mainModelKey`), capped at MAX_CHILD_MODELS_DISPLAY.
 *
 * @param {Object} state
 * @param {string} mainModelKey
 * @param {number} [maxIdleTurns=30]
 * @param {number} [now=Date.now()]
 * @param {number} [maxIdleMs=300000]
 * @returns {Array<{ model: string, speed: number }>}
 */
function resolveDisplayChildModelSpeeds(
  state,
  mainModelKey,
  maxIdleTurns = 30,
  now = Date.now(),
  maxIdleMs = 5 * 60 * 1000
) {
  const merged = [];
  const seen = new Set();
  const modelPerf = (state?.modelPerf && typeof state.modelPerf === 'object') ? state.modelPerf : null;
  const modelLastActive = (state?.modelLastActiveMs && typeof state.modelLastActiveMs === 'object')
    ? state.modelLastActiveMs
    : null;
  const pickPositiveTs = (...vals) => {
    for (const v of vals) {
      if (Number.isFinite(v) && v > 0) return v;
    }
    return 0;
  };

  if (Array.isArray(state?.childModelSpeeds)) {
    for (const item of state.childModelSpeeds) {
      if (!item || typeof item !== 'object') continue;
      const modelKey = String(item.model || '').trim();
      if (!modelKey || seen.has(modelKey)) continue;
      const normKey = compactModelName(modelKey) || modelKey;
      if (mainModelKey && (modelKey === mainModelKey || normKey === mainModelKey)) continue;
      const perf = modelPerf ? (modelPerf[modelKey] || modelPerf[normKey]) : null;
      if (Number.isFinite(item.turnsAgo) && item.turnsAgo > maxIdleTurns) continue;
      if (item.turnsAgo === undefined && perf && Number.isFinite(perf.turnsAgo) && perf.turnsAgo > maxIdleTurns) continue;
      const lastActiveMs = pickPositiveTs(
        item.lastActiveMs,
        modelLastActive?.[modelKey],
        modelLastActive?.[normKey],
        perf?.lastUpdatedMs
      );
      if (lastActiveMs > 0 && (now - lastActiveMs) > maxIdleMs) continue;
      const effectiveSpeed = (perf && Number.isFinite(perf.avgSpeed) && perf.avgSpeed > 0)
        ? perf.avgSpeed
        : item.speed;
      if (!Number.isFinite(effectiveSpeed) || effectiveSpeed <= 0) continue;
      seen.add(modelKey);
      merged.push({ model: modelKey, speed: item.speed });
      if (merged.length >= MAX_CHILD_MODELS_DISPLAY) {
        return merged;
      }
    }
  }

  if (modelPerf && merged.length < MAX_CHILD_MODELS_DISPLAY) {
    const candidates = [];
    for (const [rawKey, perf] of Object.entries(modelPerf)) {
      const modelKey = String(rawKey || perf?.model || '').trim();
      if (!modelKey || seen.has(modelKey)) continue;
      const normKey = compactModelName(modelKey) || modelKey;
      if (mainModelKey && (modelKey === mainModelKey || normKey === mainModelKey)) continue;
      if (!perf || typeof perf !== 'object' || !Number.isFinite(perf.avgSpeed) || perf.avgSpeed <= 0) continue;
      if (Number.isFinite(perf.turnsAgo) && perf.turnsAgo > maxIdleTurns) continue;
      const lastActiveMs = Math.max(
        pickPositiveTs(modelLastActive?.[modelKey], modelLastActive?.[normKey]),
        pickPositiveTs(perf.lastUpdatedMs)
      );
      if (lastActiveMs > 0 && (now - lastActiveMs) > maxIdleMs) continue;
      candidates.push({
        modelKey,
        avgSpeed: perf.avgSpeed,
        lastUpdatedMs: Number.isFinite(perf.lastUpdatedMs) ? perf.lastUpdatedMs : 0,
        streamSamples: Number.isFinite(perf.streamSamples) ? perf.streamSamples : 0,
      });
    }
    candidates.sort((a, b) => (
      (b.lastUpdatedMs - a.lastUpdatedMs) ||
      (b.streamSamples - a.streamSamples) ||
      a.modelKey.localeCompare(b.modelKey)
    ));
    for (const candidate of candidates) {
      if (merged.length >= MAX_CHILD_MODELS_DISPLAY) break;
      seen.add(candidate.modelKey);
      merged.push({ model: candidate.modelKey, speed: candidate.avgSpeed });
    }
  }

  return merged;
}

/**
 * Renders the status line using ANSI escape codes.
 * Falls back to ASCII when the terminal can't render box-drawing / Nerd Font
 * glyphs (detected via encoding.js, override via config.display.unicode).
 *
 * @param {Object} state
 * @param {Object} agyData
 * @param {Object} config
 * @param {Array}  quotaData  — from quota.js getQuota()
 * @param {Object} updateInfo — local cache info about updates
 * @returns {string}
 */
function isValidProviderQuotaWindow(win, now) {
  return Boolean(win && typeof win.remainingFraction === 'number' && !isObservationExpired(win, now));
}

function hasValidProviderQuota(pq, now) {
  if (!pq || typeof pq !== 'object') return false;
  return Object.entries(pq).some(([key, entry]) => {
    if (/^openai$/i.test(String(key).trim())) return false;
    if (!entry || typeof entry !== 'object') return false;
    return isValidProviderQuotaWindow(entry.fiveHour, now) || isValidProviderQuotaWindow(entry.weekly, now);
  });
}

const CYCLE_MODE_STYLES = {
  auto: { label: 'auto', color: ANSI_COLORS.yellow },
  'accept-edits': { label: 'accept-edits', color: ANSI_COLORS.green },
  plan: { label: 'plan', color: '\x1b[94m' },
  bestn: { label: 'best-of-n', color: ANSI_COLORS.magenta },
};

/**
 * Format the interactive cycle mode badge (toggled via Shift+Tab in agy CLI)
 * using fixed semantic ANSI colors independent of theme presets.
 *
 * @param {unknown} cycleMode
 * @param {{ bold?: string, reset?: string }} [options]
 * @returns {string}
 */
function formatCycleModeSegment(cycleMode, { bold = '\x1b[1m', reset = '\x1b[0m' } = {}) {
  if (typeof cycleMode !== 'string') return '';
  const trimmed = cycleMode.trim();
  if (!trimmed) return '';
  const known = CYCLE_MODE_STYLES[trimmed.toLowerCase()];
  if (known) {
    return `${bold}${known.color}${known.label}${reset}`;
  }
  const sanitized = sanitizeTerminalText(trimmed, 24).trim();
  if (!sanitized) return '';
  return `${bold}${ANSI_COLORS.cyan}${sanitized}${reset}`;
}

function renderStatusLine(state, agyData, config, quotaData, tierName, updateInfo) {
  if (config?.enabled === false) return '';
  const display = config?.display || {};
  const useNerd = display.useNerdFonts === true;
  const unicode = typeof display.unicode === 'boolean'
    ? display.unicode
    : supportsUnicode();
  const showGitBranch = display.showGitBranch !== false;
  const showTokenBar = display.showTokenBar !== false;
  const showBreadcrumbs = display.showBreadcrumbs !== false;
  const showCurrentDir = display.showCurrentDir !== false;
  const showUsername = display.showUsername === true;
  const showTurnCacheHitRate = display.showTurnCacheHitRate === true;
  const showImageQuota = display.showImageQuota === true;
  const showCost = display.showCost === true;
  const showDailyCost = display.showDailyCost === true;
  const showWeeklyCost = display.showWeeklyCost === true;
  const showSpeed = display.showSpeed === true;
  // Composable visibility flags (default true, so upstream behaviour is kept).
  // Useful when another status line provides model/context info.
  const showModel = display.showModel !== false;
  const showContextBar = display.showContextBar !== false;
  const showCycleMode = display.showCycleMode !== false;
  const quotaStyle = display.quotaStyle || 'table';
  const isCompact = quotaStyle === 'compact';
  const resolvedLang = resolveLanguage(config);
  const text = LANGUAGE_TEXT[resolvedLang];

  // ANSI escape sequences
  const reset = '\x1b[0m';
  const bold = '\x1b[1m';
  const dim = '\x1b[2m';

  const resolvedTheme = normalizeThemeObject(config?.theme);
  const getThemeColor = (key, defaultColor) => {
    const name = resolvedTheme[key] || defaultColor;
    return ANSI_COLORS[name] || ANSI_COLORS[defaultColor];
  };

  const primaryColor = getThemeColor('primary', 'green');
  const secondaryColor = getThemeColor('secondary', 'gray');
  const warningColor = getThemeColor('warning', 'yellow');
  const criticalColor = getThemeColor('critical', 'red');

  const gray = secondaryColor;
  const blue = ANSI_COLORS.blue;
  const magenta = ANSI_COLORS.magenta;
  const yellow = warningColor;
  const cyan = ANSI_COLORS.cyan;
  const green = primaryColor;
  const red = criticalColor;

  const warnThresh = typeof config?.thresholds?.warning === 'number'
    ? config.thresholds.warning
    : DEFAULT_THRESHOLDS.warning;
  const critThresh = typeof config?.thresholds?.critical === 'number'
    ? config.thresholds.critical
    : DEFAULT_THRESHOLDS.critical;

  const rawCols = Number(agyData?.terminal_width || display.terminalWidth || process.env.COLUMNS || 0);
  const explicitCols = Number.isFinite(rawCols) && rawCols > 0 ? Math.floor(rawCols) : 0;

  const columnWidth = typeof display.columnWidth === 'number'
    ? display.columnWidth
    : DEFAULT_COLUMN_WIDTH;
  const effectiveColWidth = explicitCols > 0
    ? (explicitCols < 56
      ? Math.min(columnWidth, Math.max(16, explicitCols - 2))
      : Math.min(columnWidth, Math.max(16, Math.floor((explicitCols - 5) / 2))))
    : columnWidth;
  const nameWidth = Math.max(10, effectiveColWidth - QUOTA_CHROME_WIDTH);

  // Box-drawing glyphs with ASCII fallback
  const glyph = unicode
    ? { bar: '█', empty: '░', vbar: '│', hbar: '─', ellipsis: '…' }
    : { bar: '#', empty: '-', vbar: '|', hbar: '-', ellipsis: '...' };

  // Icons: Nerd Font > emoji > plain ASCII
  let branchIcon, planIcon, stepIcon, taskIcon, tokenIcon, ctxIcon, modelIcon;
  if (useNerd) {
    branchIcon = ' ';
    planIcon = '󰌢 ';
    stepIcon = ' ';
    taskIcon = ' ';
    tokenIcon = '󰚩 ';
    ctxIcon = '󱔐 ';
    modelIcon = '󰚗 ';
  } else if (unicode) {
    branchIcon = '⎇ ';
    planIcon = '❖ ';
    stepIcon = '⚡ ';
    taskIcon = '✓ ';
    tokenIcon = '⚿ ';
    ctxIcon = '⛁ ';
    modelIcon = '🤖 ';
  } else {
    branchIcon = '[B] ';
    planIcon = '[P] ';
    stepIcon = '[S] ';
    taskIcon = '[T] ';
    tokenIcon = '[Tk] ';
    ctxIcon = '[C] ';
    modelIcon = '[M] ';
  }

  // Override via config.icons if present — strip control/escape sequences first
  if (config && config.icons) {
    const sanitizeIcon = (v) => sanitizeTerminalText(v, 8);
    if (config.icons.branch !== undefined) branchIcon = sanitizeIcon(config.icons.branch);
    if (config.icons.plan !== undefined) planIcon = sanitizeIcon(config.icons.plan);
    if (config.icons.step !== undefined) stepIcon = sanitizeIcon(config.icons.step);
    if (config.icons.task !== undefined) taskIcon = sanitizeIcon(config.icons.task);
    if (config.icons.token !== undefined) tokenIcon = sanitizeIcon(config.icons.token);
    if (config.icons.ctx !== undefined) ctxIcon = sanitizeIcon(config.icons.ctx);
    if (config.icons.model !== undefined) modelIcon = sanitizeIcon(config.icons.model);
  }

  const rawBranch = sanitizeTerminalText(state?.branch || '', 80);
  const branchName = rawBranch ? `${blue}${branchIcon}${rawBranch}${reset}` : '';

  // Compact arrow glyphs for token breakdown
  const inArrow = unicode ? '↑' : '^';
  const outArrow = unicode ? '↓' : 'v';
  const cacheArrow = unicode ? '⟳' : 'c:';

  // Data extraction
  const usage = agyData?.context_window || {};
  const totalInput = usage.total_input_tokens ?? state?.totalInputTokens ?? 0;
  const totalOutput = usage.total_output_tokens ?? state?.totalOutputTokens ?? 0;
  const ctxPercent = usage.used_percentage || 0;
  const plan = sanitizeTerminalText(tierName || agyData?.plan_tier || 'Free', 80);
  const tasks = agyData?.task_count || 0;

  // Extract model information
  const ltFallbackModel = typeof state?.lastTurnUsage?.model === 'string' ? state.lastTurnUsage.model.trim() : '';
  const rawModelName = (
    agyData?.model?.display_name ||
    agyData?.model?.id ||
    (typeof agyData?.model === 'string' ? agyData.model : '') ||
    (ltFallbackModel ? (resolveModelDisplayName(ltFallbackModel, undefined) || ltFallbackModel) : '') ||
    'Unknown Model'
  );
  const modelName = sanitizeTerminalText(simplifyModelName(rawModelName), 80);

  const createProgressBar = (percent, color, width = 10, isUsage = true) => {
    const safeWidth = Math.max(0, Math.floor(Number.isFinite(Number(width)) ? Number(width) : 10));
    const numPct = Number(percent);
    const safePct = Math.max(0, Math.min(100, Number.isFinite(numPct) ? numPct : 0));
    const completed = Math.max(0, Math.min(safeWidth, Math.round((safePct / 100) * safeWidth)));
    const remaining = Math.max(0, safeWidth - completed);

    // Auto-color based on usage vs remaining
    let finalColor = color;
    if (isUsage) {
      if (safePct > critThresh * 100) finalColor = red;
      else if (safePct > warnThresh * 100) finalColor = yellow;
    } else {
      if (safePct <= (1 - critThresh) * 100) finalColor = red;
      else if (safePct <= (1 - warnThresh) * 100) finalColor = yellow;
      else finalColor = green;
    }

    return `${finalColor}[${glyph.bar.repeat(completed)}${glyph.empty.repeat(remaining)}]${reset}`;
  };

  const truncateAndPad = (str, width) => truncateToDisplayWidth(str, width, glyph.ellipsis);

  const divider = ` ${gray}${glyph.vbar}${reset} `;

  // Layer 1: identity + status
  const currentDir = sanitizeTerminalText(state?.currentDir || '', 80);
  const username = sanitizeTerminalText(display.username || state?.username || '', 80);
  const usernameSeg = (showUsername && username) ? `${cyan}${username}${reset}` : '';
  const currentDirSeg = (showCurrentDir && currentDir) ? `${blue}${currentDir}${reset}` : '';
  const branchSeg = (showGitBranch && branchName) ? branchName : '';
  const modelSeg = showModel ? `${green}${modelName}${reset}` : '';
  const planSeg = `${magenta}${plan}${reset}`;
  const modeSeg = showCycleMode ? formatCycleModeSegment(agyData?.cycle_mode, { bold, reset }) : '';

  // Cost tracking & formatting
  let sevenDayResetAt = null;
  if (quotaData?.providerQuota?.google?.weekly?.resetTime) {
    sevenDayResetAt = quotaData.providerQuota.google.weekly.resetTime;
  } else if (Array.isArray(quotaData)) {
    const weeklyQ = quotaData.find(q => q.windows?.weekly?.resetTime);
    if (weeklyQ) sevenDayResetAt = weeklyQ.windows.weekly.resetTime;
  }

  const computedSessionCost = (showCost || display.showContextMonitor === true)
    ? getSessionCostUsd(agyData, state, { allowRoutedCost: display.showRoutedCost })
    : null;
  const sessionCost = showCost ? computedSessionCost : null;
  const costTotals = state?.costTotals ?? ((showDailyCost || showWeeklyCost)
    ? getCostTotals(agyData, { allowRoutedCost: display.showRoutedCost, sevenDayResetAt }, undefined, state)
    : null);

  const costSegments = [
    sessionCost !== null ? `${text.costLabel} ${formatUsd(sessionCost)}` : null,
    showDailyCost && costTotals ? `${text.todayLabel} ${formatUsd(costTotals.todayUsd)}` : null,
    showWeeklyCost && costTotals?.weekUsd != null ? `${text.weekLabel} ${formatUsd(costTotals.weekUsd)}` : null,
  ].filter(Boolean);
  const costSeg = costSegments.length > 0 ? `${gray}${costSegments.join(' | ')}${reset}` : '';

  // Token speed & TTFT calculation & formatting
  const mainModelIdent = (
    agyData?.model?.display_name ||
    agyData?.model?.id ||
    (typeof agyData?.model === 'string' ? agyData.model : '') ||
    state?.lastTurnUsage?.model ||
    ''
  );
  const maxIdleTurns = Number.isFinite(display.modelSpeedExpireTurns) && display.modelSpeedExpireTurns > 0
    ? display.modelSpeedExpireTurns
    : 30;
  const maxIdleMs = (
    Number.isFinite(display.modelSpeedExpireMinutes) && display.modelSpeedExpireMinutes > 0
      ? display.modelSpeedExpireMinutes
      : 5
  ) * 60 * 1000;
  const now = Number.isFinite(config?._now) ? config._now : Date.now();
  const mainModelKey = compactModelName(mainModelIdent);
  const rawMainPerf = (mainModelKey && state?.modelPerf && typeof state.modelPerf === 'object')
    ? state.modelPerf[mainModelKey]
    : null;
  const mainPerfExpired = Boolean(
    !state?.steps &&
    rawMainPerf &&
    Number.isFinite(rawMainPerf.turnsAgo) &&
    rawMainPerf.turnsAgo > maxIdleTurns
  );
  const mainPerf = mainPerfExpired ? null : rawMainPerf;
  const hasPerfSpeed = Boolean(mainPerf && Number.isFinite(mainPerf.avgSpeed) && mainPerf.avgSpeed > 0);
  let outputSpeed = hasPerfSpeed
    ? mainPerf.avgSpeed
    : (state?.outputSpeed !== undefined
      ? state.outputSpeed
      : (showSpeed ? getOutputSpeed(agyData, null, state) : null));
  let mainTtftStr = (mainPerf && Number.isFinite(mainPerf.avgTtftSec))
    ? formatTtft(mainPerf.avgTtftSec, resolvedLang)
    : null;
  const mainLastActiveMs = Math.max(
    Number(state?.modelLastActiveMs?.[mainModelKey]) || 0,
    Number(state?.lastPlannerResponse?.endMs) || 0,
    Number(state?.lastPlannerResponse?.startMs) || 0,
    Number(mainPerf?.lastUpdatedMs) || 0
  );
  if (mainLastActiveMs > 0 && (now - mainLastActiveMs) > maxIdleMs) {
    outputSpeed = null;
    mainTtftStr = null;
  }
  const displayChildSpeeds = showSpeed
    ? resolveDisplayChildModelSpeeds(state, mainModelKey, maxIdleTurns, now, maxIdleMs)
    : [];

  const updateSeg = (
    updateInfo &&
    updateInfo.updateAvailable &&
    updateInfo.latestVersion &&
    isNewerVersion(updateInfo.latestVersion, PACKAGE_VERSION)
  )
    ? `${yellow}${unicode ? '⟳' : '[UP]'} v${updateInfo.latestVersion}${reset}`
    : '';

  const assembleLine1 = ({
    childLimit,
    includeChildTtft,
    includeMainTtft,
    includeChildSpeed,
    includeUpdate,
    includeBranch,
    includeDir,
    includeUsername,
  }) => {
    const parts = [];
    if (includeUsername && usernameSeg) parts.push(usernameSeg);
    if (includeDir && currentDirSeg) parts.push(currentDirSeg);
    if (includeBranch && branchSeg) parts.push(branchSeg);
    if (modelSeg) parts.push(modelSeg);
    parts.push(planSeg);
    if (modeSeg) parts.push(modeSeg);
    if (costSeg) parts.push(costSeg);

    if (showSpeed) {
      if (outputSpeed !== null && Number.isFinite(outputSpeed)) {
        const ttftSuffix = (includeMainTtft && mainTtftStr) ? ` (${mainTtftStr})` : '';
        parts.push(`${gray}${text.outLabel}: ${outputSpeed.toFixed(1)} ${text.tokPerSec}${ttftSuffix}${reset}`);
      } else if (includeMainTtft && mainTtftStr) {
        parts.push(`${gray}${mainTtftStr}${reset}`);
      }
      if (includeChildSpeed && displayChildSpeeds.length > 0) {
        const sliced = displayChildSpeeds.slice(0, childLimit);
        let childSpeedStr = formatChildModelSpeeds(sliced, {
          unicode,
          useNerd,
          modelPerf: state?.modelPerf,
          lang: resolvedLang,
          includeTtft: includeChildTtft,
        });
        if (childSpeedStr && !includeChildTtft) {
          childSpeedStr = childSpeedStr.replace(/ \((?:首字|ttft) [^)]+\)/gi, '');
        }
        if (childSpeedStr) {
          parts.push(`${magenta}${childSpeedStr}${reset}`);
        }
      }
    }

    if (includeUpdate && updateSeg) parts.push(updateSeg);
    return parts.join(divider);
  };

  const line1Opts = {
    childLimit: displayChildSpeeds.length,
    includeChildTtft: true,
    includeMainTtft: true,
    includeChildSpeed: true,
    includeUpdate: true,
    includeBranch: true,
    includeDir: true,
    includeUsername: true,
  };

  let line1 = assembleLine1(line1Opts);
  if (explicitCols > 0 && getAnsiDisplayWidth(line1) > explicitCols) {
    const line1Steps = [];
    if (displayChildSpeeds.length > 2) line1Steps.push(() => { line1Opts.childLimit = 2; });
    if (displayChildSpeeds.length > 1) line1Steps.push(() => { line1Opts.childLimit = 1; });
    line1Steps.push(
      () => { line1Opts.includeChildTtft = false; },
      () => { line1Opts.includeMainTtft = false; },
      () => { line1Opts.includeChildSpeed = false; },
      () => { line1Opts.includeUpdate = false; },
      () => { line1Opts.includeBranch = false; },
      () => { line1Opts.includeDir = false; },
      () => { line1Opts.includeUsername = false; },
    );
    for (const applyStep of line1Steps) {
      applyStep();
      line1 = assembleLine1(line1Opts);
      if (getAnsiDisplayWidth(line1) <= explicitCols) break;
    }
    if (getAnsiDisplayWidth(line1) > explicitCols) {
      line1 = truncateAnsiToDisplayWidth(line1, explicitCols, glyph.ellipsis);
    }
  }

  const firstNumber = (...values) => values.find(value => Number.isFinite(value));
  const agyCurrentUsage = usage.current_usage || {};
  const transcriptUsage = state?.usage || {};
  const transcriptCurrentUsage = transcriptUsage.current_usage || {};
  const cacheRead = firstNumber(
    agyCurrentUsage.cache_read_input_tokens,
    usage.cache_read_input_tokens,
    transcriptCurrentUsage.cache_read_input_tokens,
    transcriptUsage.cache_read_input_tokens
  ) ?? 0;
  const cacheWrite = firstNumber(
    agyCurrentUsage.cache_creation_input_tokens,
    usage.cache_creation_input_tokens,
    transcriptCurrentUsage.cache_creation_input_tokens,
    transcriptUsage.cache_creation_input_tokens
  ) ?? 0;
  const cacheTotal = cacheRead + cacheWrite;

  let inTokens = firstNumber(
    agyCurrentUsage.input_tokens,
    transcriptCurrentUsage.input_tokens
  );
  if (inTokens === undefined) {
    inTokens = Math.max(0, totalInput - cacheTotal);
  } else if (inTokens >= cacheTotal && modelIncludesCacheInInput(rawModelName)) {
    inTokens = Math.max(0, inTokens - cacheTotal);
  }
  const outTokens = totalOutput;

  const hasCacheTelemetry = candidate => candidate && typeof candidate === 'object' && (
    Object.hasOwn(candidate, 'cache_read_input_tokens') ||
    Object.hasOwn(candidate, 'cache_creation_input_tokens')
  );
  const hasCompleteTurnCacheUsage = candidate => candidate && typeof candidate === 'object' &&
    Number.isFinite(candidate.input_tokens) && candidate.input_tokens >= 0 &&
    Number.isFinite(candidate.cache_read_input_tokens) && candidate.cache_read_input_tokens >= 0 &&
    (candidate.cache_creation_input_tokens === undefined || (Number.isFinite(candidate.cache_creation_input_tokens) && candidate.cache_creation_input_tokens >= 0));
  const turnUsageCandidates = [
    agyCurrentUsage,
    usage,
    transcriptCurrentUsage,
    transcriptUsage,
  ];
  const turnUsage = turnUsageCandidates.find(hasCompleteTurnCacheUsage);
  const turnCacheMetrics = turnUsage
    ? calculateTurnCacheMetrics(turnUsage, rawModelName)
    : turnUsageCandidates.some(hasCacheTelemetry) ? { available: false } : null;

  // Apply cache smoothing adaption to absorb CLI truncation/fluctuation bugs
  let displayCache = cacheTotal;
  let displayIn = inTokens;
  let isCacheSmoothApplied = false;

  const maxHistCache = state.maxHistoricalCache || 0;
  if (cacheTotal === 0 && maxHistCache > 10000 && inTokens >= maxHistCache * 0.9) {
    displayCache = maxHistCache;
    displayIn = Math.max(0, inTokens - maxHistCache);
    isCacheSmoothApplied = true;
  }

  const tokenTotal = displayIn + outTokens + displayCache;

  // Compact token format: Tokens 150k (in: 127k, out: 23k, cache: Xk)
  const detailParts = [`in: ${formatTokens(displayIn)}`, `out: ${formatTokens(outTokens)}`];
  if (displayCache > 0) {
    const cacheLabel = isCacheSmoothApplied ? `${formatTokens(displayCache)}*` : formatTokens(displayCache);
    detailParts.push(`cache: ${cacheLabel}`);
  }
  const tokenParts = isCompact
    ? formatTokens(tokenTotal)
    : `${formatTokens(tokenTotal)} ${gray}(${reset}${detailParts.join(', ')}${gray})${reset}`;
  const tokenPrefix = tokenIcon === '[Tk] ' ? 'Tokens' : `${tokenIcon}Tokens`;
  const tokensStr = `${cyan}${tokenPrefix} ${tokenParts}${reset}`;
  const effectiveCtxSize = usage.context_window_size || state?.serverCompactionLimit || state?.usage?.context_window_size || 0;
  const hasPositiveUsedPct = Number.isFinite(Number(usage.used_percentage)) && Number(usage.used_percentage) > 0;
  const hasAgyOrTranscriptTurn = agyCurrentUsage.input_tokens !== undefined || transcriptCurrentUsage.input_tokens !== undefined;
  const hasLastTurnUsage = Boolean(state?.lastTurnUsage && typeof state.lastTurnUsage === 'object');
  const hasTurnSnapshot = hasAgyOrTranscriptTurn || hasLastTurnUsage;
  const useActiveTurnCtx = !hasPositiveUsedPct && (
    hasTurnSnapshot ||
    (effectiveCtxSize > 0 && totalInput > effectiveCtxSize) ||
    Number(state?.compactionCount || 0) > 0
  );

  let activeTurnCtx = 0;
  if (hasAgyOrTranscriptTurn) {
    const turnOut = agyCurrentUsage.output_tokens ?? transcriptCurrentUsage.output_tokens ?? state?.lastTurnUsage?.output_tokens ?? 0;
    activeTurnCtx = displayIn + displayCache + Math.max(0, Number(turnOut) || 0);
  } else if (hasLastTurnUsage) {
    const lt = state.lastTurnUsage;
    const ltIn = Math.max(0, Number(lt.input_tokens || 0));
    const ltCache = Math.max(0, Number(lt.cache_read_input_tokens || 0)) + Math.max(0, Number(lt.cache_creation_input_tokens || 0));
    const ltOut = Math.max(0, Number(lt.output_tokens || 0));
    const ltModel = lt.model || rawModelName;
    const ltInclusive = (!ltModel || modelIncludesCacheInInput(ltModel)) && ltIn >= ltCache;
    activeTurnCtx = (ltInclusive ? ltIn : ltIn + ltCache) + ltOut;
  }

  const displayCtxTokens = useActiveTurnCtx ? activeTurnCtx : totalInput;
  const effectiveCtxPercent = useActiveTurnCtx
    ? (effectiveCtxSize > 0 ? Math.min(100, (activeTurnCtx * 100) / effectiveCtxSize) : 0)
    : ctxPercent;

  const ctxBar = createProgressBar(effectiveCtxPercent, cyan, 10, true);
  const ctxPctStr = `${Math.round(effectiveCtxPercent)}%`;
  const ctxStr = `${cyan}${ctxIcon}${formatTokens(displayCtxTokens)}/${formatTokens(effectiveCtxSize)}${reset} ${ctxBar} ${cyan}${ctxPctStr}${reset}`;

  // In compact mode, find current model's quota and append to line 2
  let currentModelQuota = null;
  if (isCompact && quotaData && quotaData.length > 0) {
    const rawName = agyData?.model?.display_name || (typeof agyData?.model === 'string' ? agyData.model : '') || '';
    const modelId = agyData?.model?.id || (typeof agyData?.model === 'string' ? agyData.model : '') || '';

    // 1) Exact / normalized match.
    currentModelQuota = quotaData.find(q =>
      (modelId && q.id === modelId) ||
      q.displayName === rawName ||
      simplifyModelName(q.displayName) === modelName ||
      modelNamesMatch(q.displayName, rawName)
    );

    // 2) Family + tier fallback for cross-generation matching.
    if (!currentModelQuota && (rawName || modelName)) {
      const extractFamilyAndTier = (name) => {
        const s = String(name).toLowerCase();
        let family = '';
        if (s.includes('orchestrator')) family = 'orchestrator';
        else if (s.includes('argon')) family = 'argon';
        else if (s.includes('carbon')) family = 'carbon';
        else if (s.includes('hydrogen')) family = 'hydrogen';
        else if (s.includes('neon')) family = 'neon';
        else if (s.includes('flash')) family = 'flash';
        else if (s.includes('pro')) family = 'pro';
        else if (s.includes('sonnet')) family = 'sonnet';
        else if (s.includes('opus')) family = 'opus';
        else if (s.includes('haiku')) family = 'haiku';
        else if (s.includes('gpt')) family = 'gpt';

        let tier = '';
        if (s.includes('xhigh') || s.includes('(xh)')) tier = 'xhigh';
        else if (s.includes('high') || s.includes('(h)') || s.includes('thinking') || s.includes('(th)')) tier = 'high';
        else if (s.includes('medium') || s.includes('(m)') || s.includes('tiered')) tier = 'medium';
        else if (s.includes('low') || s.includes('(l)') || s.includes('extra-low') || s.includes('lite')) tier = 'low';

        return { family, tier };
      };

      const current = extractFamilyAndTier(rawName || modelName);
      if (current.family) {
        currentModelQuota = quotaData.find(q => {
          const target = extractFamilyAndTier(q.displayName || q.id);
          return target.family === current.family && current.tier && target.tier === current.tier;
        }) || quotaData.find(q => {
          const target = extractFamilyAndTier(q.displayName || q.id);
          return target.family === current.family;
        });
      }
    }
  }

  // Layer 2: resource consumption
  const line2Parts = [];
  if (showTokenBar) line2Parts.push(tokensStr);
  if (showTurnCacheHitRate && (turnCacheMetrics || isCacheSmoothApplied)) {
    const cacheBadge = formatTurnCacheBadge(
      isCacheSmoothApplied ? null : turnCacheMetrics,
      text.cacheLabel,
      { green, cyan, yellow, gray, reset, bold, dim },
      isCompact,
      display.cacheHitThresholds
    );
    line2Parts.push(cacheBadge);
  }
  if (showContextBar) line2Parts.push(ctxStr);
  if (isCompact && currentModelQuota) {
    const critical = pickCriticalWindow(currentModelQuota.windows, Date.now()) || currentModelQuota;
    const pct = formatQuotaPercent(critical.remainingFraction);
    const pctColor = pct <= (1 - critThresh) * 100 ? red : pct <= (1 - warnThresh) * 100 ? yellow : green;
    let timeStr = '';
    const resetTime = critical.resetTime || currentModelQuota.resetTime;
    if (resetTime) {
      const resetMs = new Date(resetTime).getTime();
      if (Number.isFinite(resetMs) && resetMs > Date.now()) {
        const secsLeft = Math.max(0, Math.round((resetMs - Date.now()) / 1000));
        timeStr = ` ${gray}~${formatDuration(secsLeft)}${reset}`;
      }
    }
    line2Parts.push(`${pctColor}Quota: ${pct}%${reset}${timeStr}`);
  }

  // Render Image Quota or rate limit exhaustion status
  const imageExhausted = state.imageExhausted;
  let isImageExhaustedDisplayed = false;
  if (imageExhausted) {
    const secsLeft = Math.max(0, Math.round((new Date(imageExhausted.resetTime).getTime() - Date.now()) / 1000));
    if (secsLeft > 0) {
      const pad = (n) => String(n).padStart(2, '0');
      const h = Math.floor(secsLeft / 3600);
      const m = Math.floor((secsLeft % 3600) / 60);
      const countdownStr = `${pad(h)}h${pad(m)}m`;
      const icon = unicode ? '⚠️ ' : '[!] ';
      line2Parts.push(`${red}${icon}Image Quota Exhausted (Resets in: ${countdownStr})${reset}`);
      isImageExhaustedDisplayed = true;
    }
  }

  if (!isImageExhaustedDisplayed && showImageQuota) {
    const imgQ = quotaData && quotaData.find(q => (q.id && q.id.includes('image')) || (q.displayName && q.displayName.toLowerCase().includes('image')));
    if (imgQ) {
      const pct = formatQuotaPercent(imgQ.remainingFraction);
      const pctColor = pct <= (1 - critThresh) * 100 ? red : pct <= (1 - warnThresh) * 100 ? yellow : green;
      const bar = createProgressBar(pct, pctColor, 10, false);
      const imgIcon = unicode ? '🖼️ ' : '[IMG] ';
      let timeStr = '';
      if (imgQ.resetTime) {
        const secsLeft = Math.max(0, Math.round((new Date(imgQ.resetTime).getTime() - Date.now()) / 1000));
        if (secsLeft > 0) timeStr = ` ${gray}~${formatDuration(secsLeft)}${reset}`;
      }
      line2Parts.push(`${cyan}${imgIcon}Image Quota: ${bar} ${pctColor}${pct}%${reset}${timeStr}`);
    }
  }

  let line2 = line2Parts.join(divider);
  if (explicitCols > 0 && line2 && getAnsiDisplayWidth(line2) > explicitCols) {
    line2 = truncateAnsiToDisplayWidth(line2, explicitCols, glyph.ellipsis);
  }

  // Layer 3: project metadata (non-zero items only)
  const metaParts = [];
  const breadcrumbCount = typeof display.breadcrumbCount === 'number'
    ? Math.max(0, Math.floor(display.breadcrumbCount))
    : 3;
  if (showBreadcrumbs && breadcrumbCount > 0) {
    if (Array.isArray(state.breadcrumbs) && state.breadcrumbs.length > 0) {
      for (const item of state.breadcrumbs.filter(Boolean).slice(-breadcrumbCount)) {
        metaParts.push(`${gray}${sanitizeTerminalText(item)}${reset}`);
      }
    } else if (state.memoryFile) {
      metaParts.push(`${gray}1 ${sanitizeTerminalText(state.memoryFile)}${reset}`);
    }
  }
  const rulesCount = state.rulesCount || 0;
  const mcpCount = state.mcpCount || 0;
  const hooksCount = state.hooksCount || 0;
  if (rulesCount > 0) metaParts.push(`${gray}${rulesCount} rules${reset}`);
  if (mcpCount > 0) metaParts.push(`${gray}${mcpCount} MCPs${reset}`);
  if (hooksCount > 0) metaParts.push(`${gray}${hooksCount} hooks${reset}`);
  let line3 = metaParts.length > 0 ? metaParts.join(divider) : '';
  if (explicitCols > 0 && line3 && getAnsiDisplayWidth(line3) > explicitCols) {
    line3 = truncateAnsiToDisplayWidth(line3, explicitCols, glyph.ellipsis);
  }

  const { renderQuotaColumn, renderProviderQuotaTable, renderCompactQuotaLine } = createQuotaRenderers({
    colors: { cyan, reset, gray, red, yellow, green },
    glyph,
    thresholds: { warnThresh, critThresh },
    nameWidth,
    columnWidth: effectiveColWidth,
    divider,
    createProgressBar,
    truncateAndPad,
    explicitCols,
  });

  // Build quota lines
  let quotaLines = '';
  const quotaNow = Date.now();
  const hasValidPQ = hasValidProviderQuota(quotaData?.providerQuota, quotaNow);
  const barLen = explicitCols > 0
    ? Math.min(columnWidth * 2 + 1, Math.max(10, explicitCols - 2))
    : columnWidth * 2 + 1;

  if (quotaStyle !== 'none') {
    if (quotaData && (quotaData.length > 0 || hasValidPQ)) {
      if (isCompact) {
        let compactLine = renderCompactQuotaLine(quotaData, quotaNow);
        if (compactLine) {
          if (explicitCols > 0 && getAnsiDisplayWidth(compactLine) > explicitCols) {
            compactLine = truncateAnsiToDisplayWidth(compactLine, explicitCols, glyph.ellipsis);
          }
          quotaLines = `\n${compactLine}`;
        }
      } else if (quotaStyle === 'models' && quotaData.length > 0) {
        const isImageModel = (q) => (q.id && q.id.includes('image')) || (q.displayName && q.displayName.toLowerCase().includes('image'));
        const tableQuota = quotaData.filter(q => !isImageModel(q));
        if (tableQuota.length === 0 && hasValidPQ) {
          quotaLines = `\n${renderProviderQuotaTable(quotaData, quotaNow)}`;
        } else {
          const cols = tableQuota.map(q => renderQuotaColumn(q, quotaNow));
          const singleColumn = explicitCols > 0 && explicitCols < 56;
          const rows = [];
          if (singleColumn) {
            for (const col of cols) {
              rows.push(`  ${col}`);
            }
          } else {
            for (let i = 0; i < cols.length; i += 2) {
              const col1 = cols[i];
              const col2 = cols[i + 1];
              if (col2) {
                rows.push(`  ${col1} ${gray}${glyph.vbar}${reset} ${col2}`);
              } else {
                rows.push(`  ${col1}`);
              }
            }
          }
          const finalRows = explicitCols > 0
            ? rows.map(r => (getAnsiDisplayWidth(r) > explicitCols ? truncateAnsiToDisplayWidth(r, explicitCols, glyph.ellipsis) : r))
            : rows;
          const dividerLine = `  ${gray}${glyph.hbar.repeat(barLen)}${reset}`;
          quotaLines = `\n${dividerLine}\n` + finalRows.join('\n') + `\n${dividerLine}`;
        }
      } else {
        quotaLines = `\n${renderProviderQuotaTable(quotaData, quotaNow)}`;
      }
    } else if (quotaData && quotaData.unavailableReason) {
      const dividerLine = `  ${gray}${glyph.hbar.repeat(barLen)}${reset}`;
      const reason = sanitizeTerminalText(text.quotaReasons[quotaData.unavailableReason] || quotaData.unavailableReason);
      let msgLine = `  ${yellow}${text.quotaUnavailable}:${reset} ${gray}${reason}${reset}`;
      if (explicitCols > 0 && getAnsiDisplayWidth(msgLine) > explicitCols) {
        msgLine = truncateAnsiToDisplayWidth(msgLine, explicitCols, glyph.ellipsis);
      }
      quotaLines = `\n${dividerLine}\n${msgLine}\n${dividerLine}`;
    } else if (quotaData && quotaData.length === 0) {
      const dividerLine = `  ${gray}${glyph.hbar.repeat(barLen)}${reset}`;
      let msgLine = `  ${gray}${text.quotaLoading}${glyph.ellipsis}${reset}`;
      if (explicitCols > 0 && getAnsiDisplayWidth(msgLine) > explicitCols) {
        msgLine = truncateAnsiToDisplayWidth(msgLine, explicitCols, glyph.ellipsis);
      }
      quotaLines = `\n${dividerLine}\n${msgLine}\n${dividerLine}`;
    }
  }

  const lines = [line1, line2].filter(Boolean);
  if (line3) lines.push(line3);
  const baseStatusLine = lines.join('\n') + quotaLines;

  const showModelCache = display.showModelCache === true || (display.showModelCache !== false && display.showContextMonitor === true);
  const monitorColors = { reset, bold, dim, red, green, yellow, cyan, magenta };
  const bottomLines = [];

  if (showModelCache) {
    const modelCacheLine = renderModelCacheLine({
      state,
      agyData,
      config,
      colors: monitorColors,
      unicode,
      lang: resolvedLang,
    });
    if (modelCacheLine) {
      bottomLines.push(modelCacheLine);
    }
  }

  if (display.showContextMonitor === true) {
    const monitorLine = renderContextMonitorLine({
      state,
      agyData,
      config,
      colors: monitorColors,
      unicode,
      lang: resolvedLang,
      sessionCostUsd: computedSessionCost,
    });
    if (monitorLine) {
      bottomLines.push(monitorLine);
    }
  }

  if (bottomLines.length > 0) {
    const bottomBlock = bottomLines.join('\n');
    return baseStatusLine ? `${baseStatusLine}\n${bottomBlock}` : bottomBlock;
  }

  return baseStatusLine;
}

module.exports = {
  renderStatusLine,
  abbreviateDisplayName,
  compactModelName,
  formatQuotaPercent,
  formatCycleModeSegment,
  CYCLE_MODE_STYLES,
};
