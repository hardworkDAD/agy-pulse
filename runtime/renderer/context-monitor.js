'use strict';

const {
  ANSI_COLORS,
  DEFAULT_THRESHOLDS,
  compactModelName,
  modelIncludesCacheInInput,
  sanitizeTerminalText,
  getAnsiDisplayWidth,
  truncateAnsiToDisplayWidth,
} = require('./format.js');
const {
  LANGUAGE_TEXT,
  resolveLanguage,
} = require('./lang.js');
const { getNativeCostUsd } = require('../cost.js');
const { resolveModelDisplayName } = require('../quota/models.js');

/**
 * Format token count using uppercase K/M units matching the agy-statusline
 * monitor display (consistent base-1000 scaling, strips trailing `.0`).
 *
 * @param {number} n
 * @returns {string}
 */
function formatMonitorTokens(n) {
  const val = Math.max(0, Math.floor(Number(n) || 0));
  if (val >= 1_000_000) {
    const s = (val / 1_000_000).toFixed(1);
    return (s.endsWith('.0') ? s.slice(0, -2) : s) + 'M';
  }
  if (val >= 1_000) {
    const s = (val / 1_000).toFixed(1);
    if (s === '1000.0' || s === '1000') return '1M';
    return (s.endsWith('.0') ? s.slice(0, -2) : s) + 'K';
  }
  return String(val);
}

/**
 * Build a 5-level smooth progress bar (`█▓▒░·` in UTF-8, `#=-.` in ASCII).
 *
 * @param {number} pct
 * @param {number} [width]
 * @param {boolean} [unicode]
 * @returns {string}
 */
function buildSmoothBar(pct, width = 12, unicode = true) {
  const clamped = Math.max(0, Math.min(100, Number(pct) || 0));
  const exact = (clamped * width) / 100;
  const filled = Math.floor(exact);
  const rem = Math.round((exact - filled) * 100);

  const fullChar = unicode ? '█' : '#';
  const emptyChar = unicode ? '·' : '.';
  const partialChars = unicode
    ? { high: '▓', mid: '▒', low: '░' }
    : { high: '=', mid: '=', low: '-' };

  let out = '';
  for (let i = 0; i < width; i++) {
    if (i < filled) {
      out += fullChar;
    } else if (i === filled) {
      if (rem >= 75) out += partialChars.high;
      else if (rem >= 50) out += partialChars.mid;
      else if (rem >= 25) out += partialChars.low;
      else out += emptyChar;
    } else {
      out += emptyChar;
    }
  }
  return out;
}

function hasPositiveUsage(usage) {
  if (!usage || typeof usage !== 'object') return false;
  return (
    Number(usage.input_tokens) > 0 ||
    Number(usage.cache_creation_input_tokens) > 0 ||
    Number(usage.cache_read_input_tokens) > 0 ||
    Number(usage.output_tokens) > 0
  );
}

function resolveModelLabel(agyData, state) {
  const modelObj = agyData?.model;
  if (modelObj && typeof modelObj === 'object') {
    let name = modelObj.display_name || modelObj.id || '';
    if (name) {
      const effort = modelObj.effort || '';
      if (effort && !name.toLowerCase().includes(effort.toLowerCase())) {
        name = `${name} (${effort})`;
      }
      return sanitizeTerminalText(name, 80);
    }
  }
  if (typeof modelObj === 'string' && modelObj.trim()) {
    return sanitizeTerminalText(modelObj.trim(), 80);
  }
  const ltModel = typeof state?.lastTurnUsage?.model === 'string' ? state.lastTurnUsage.model.trim() : '';
  if (ltModel) {
    return sanitizeTerminalText(resolveModelDisplayName(ltModel, undefined) || ltModel, 80);
  }
  return 'Unknown Model';
}

/**
 * Render the bottom real-time context & compaction monitor status line.
 *
 * @param {object} params
 * @returns {string}
 */
function renderContextMonitorLine({
  state,
  agyData,
  config,
  colors,
  unicode = true,
  lang,
  sessionCostUsd,
} = {}) {
  const resolvedLang = (lang === 'zh' || lang === 'en') ? lang : resolveLanguage(config);
  const text = LANGUAGE_TEXT[resolvedLang] || LANGUAGE_TEXT.en;

  const palette = {
    reset: '\x1b[0m',
    bold: '\x1b[1m',
    dim: '\x1b[2m',
    red: ANSI_COLORS.red,
    green: ANSI_COLORS.green,
    yellow: ANSI_COLORS.yellow,
    cyan: ANSI_COLORS.cyan,
    magenta: ANSI_COLORS.magenta,
    ...(colors || {}),
  };
  const { reset, bold, dim, red, green, yellow, cyan, magenta } = palette;

  const rawCols = Number(agyData?.terminal_width || config?.display?.terminalWidth || process.env.COLUMNS || 0);
  const hasExplicitCols = Number.isFinite(rawCols) && rawCols > 0;
  const cols = hasExplicitCols ? Math.floor(rawCols) : 120;

  const modelName = resolveModelLabel(agyData, state);
  const cw = agyData?.context_window || {};
  const rawWinSize = Number(cw.context_window_size || 0);
  const rawUsedPct = Number(cw.used_percentage || 0);

  const cur = hasPositiveUsage(cw.current_usage)
    ? cw.current_usage
    : (state?.lastTurnUsage && typeof state.lastTurnUsage === 'object' ? state.lastTurnUsage : null);

  const hasTurn = Boolean(cur);
  const inp = hasTurn ? Math.max(0, Number(cur.input_tokens || 0)) : 0;
  const cCreate = hasTurn ? Math.max(0, Number(cur.cache_creation_input_tokens || 0)) : 0;
  const cRead = hasTurn ? Math.max(0, Number(cur.cache_read_input_tokens || 0)) : 0;
  const turnOut = hasTurn ? Math.max(0, Number(cur.output_tokens || 0)) : 0;

  const rawModelIdent =
    agyData?.model?.display_name ||
    agyData?.model?.id ||
    (typeof agyData?.model === 'string' ? agyData.model : '') ||
    cur?.model ||
    '';

  const cacheTotal = cCreate + cRead;
  const isInclusive = !rawModelIdent || modelIncludesCacheInInput(rawModelIdent);
  const isCacheExclusiveTurn = hasTurn && (cur.cacheExclusive === true || inp < cacheTotal);
  const useInclusiveMode = isInclusive && !isCacheExclusiveTurn;
  const promptCtx = useInclusiveMode ? inp : inp + cacheTotal;
  let activeCtx = promptCtx + turnOut;

  if (activeCtx === 0 && rawUsedPct > 0 && rawWinSize > 0) {
    activeCtx = Math.round((rawWinSize * rawUsedPct) / 100);
  }

  const ckpts = Math.max(0, Number(state?.compactionCount || 0));
  const hasModePeaks = Boolean(state?.turnHistoryMaxByMode && typeof state.turnHistoryMaxByMode === 'object');
  const historyPeak = hasModePeaks
    ? (useInclusiveMode
      ? Number(state.turnHistoryMaxByMode.inclusivePeak || 0)
      : Number(state.turnHistoryMaxByMode.exclusivePeak || 0))
    : Number(state?.peakRawTurnTokens || 0);
  const peakCtx = Math.max(historyPeak, activeCtx);

  const serverLimit = Number(state?.serverCompactionLimit || 0);
  let effectiveLimit;
  if (rawWinSize > 0 && serverLimit > 0) {
    effectiveLimit = Math.min(rawWinSize, serverLimit);
  } else {
    effectiveLimit = serverLimit || rawWinSize || 512000;
  }

  const realPct = activeCtx > 0 && effectiveLimit > 0 ? (activeCtx * 100) / effectiveLimit : 0;

  const customWarn = config?.thresholds?.warning;
  const customCrit = config?.thresholds?.critical;
  const warnPct = (typeof customWarn === 'number' && Number.isFinite(customWarn) && customWarn !== DEFAULT_THRESHOLDS.warning)
    ? customWarn * 100
    : 65;
  const critPct = (typeof customCrit === 'number' && Number.isFinite(customCrit) && customCrit !== DEFAULT_THRESHOLDS.critical)
    ? customCrit * 100
    : 85;

  let barColor = cyan;
  if (realPct >= critPct) barColor = red;
  else if (realPct >= warnPct) barColor = yellow;

  const sep = unicode ? `${dim} · ${reset}` : `${dim} | ${reset}`;
  const compactUnicodeSep = `${dim}·${reset}`;
  const modelGlyph = unicode ? '◈' : '*';
  const timesGlyph = unicode ? '×' : 'x';
  const totOut = Math.max(Number(cw.total_output_tokens || 0), Number(state?.totalOutputTokens || 0));
  const rawUsd = (typeof sessionCostUsd === 'number' && Number.isFinite(sessionCostUsd))
    ? sessionCostUsd
    : getNativeCostUsd(agyData, { allowRoutedCost: config?.display?.showRoutedCost });

  const buildLine = ({
    barWidth,
    compactSep,
    showCacheAbs,
    showCompactionPeak,
    showOut,
    modelDisplayMode,
    showCache,
    showCompaction,
  }) => {
    const activeSep = (unicode && compactSep) ? compactUnicodeSep : sep;
    const bar = buildSmoothBar(realPct, barWidth, unicode);
    const ctxSeg = (activeCtx > 0 || hasTurn)
      ? `${dim}${text.ctxLabel}${reset} ${barColor}${bar}${reset} ${bold}${realPct.toFixed(1)}%${reset} ${dim}(${formatMonitorTokens(activeCtx)}/${formatMonitorTokens(effectiveLimit)})${reset}`
      : `${dim}${text.ctxLabel}${reset} ${dim}${bar}${reset} ${bold}0.0%${reset} ${dim}(${text.limitLabel} ${formatMonitorTokens(effectiveLimit)})${reset}`;

    const parts = [];
    if (cols >= 80 && modelName && modelDisplayMode !== 'none') {
      const displayModel = modelDisplayMode === 'compact'
        ? (compactModelName(modelName) || modelName)
        : modelName;
      parts.push(`${magenta}${modelGlyph} ${displayModel}${reset}`);
    }

    parts.push(ctxSeg);

    if (ckpts > 0 && showCompaction) {
      if (showCompactionPeak && cols >= 95 && peakCtx > 0) {
        const peakPrefix = resolvedLang === 'zh' ? text.peakLabel : `${text.peakLabel} `;
        parts.push(`${yellow}${text.compactedLabel}${timesGlyph}${ckpts}${reset}${dim}(${peakPrefix}${formatMonitorTokens(peakCtx)})${reset}`);
      } else {
        parts.push(`${yellow}${text.compactedLabel}${timesGlyph}${ckpts}${reset}`);
      }
    }

    if (promptCtx > 0 && showCache) {
      const cachePct = Math.min(100, Math.max(0, (cRead * 100) / promptCtx));
      const cacheColor = cachePct >= 50 ? green : dim;
      if (showCacheAbs && cols >= 105 && cRead > 0) {
        parts.push(`${dim}${text.cacheLabelMonitor}${reset} ${cacheColor}${cachePct.toFixed(0)}%${reset} ${dim}(${formatMonitorTokens(cRead)})${reset}`);
      } else {
        parts.push(`${dim}${text.cacheLabelMonitor}${reset} ${cacheColor}${cachePct.toFixed(0)}%${reset}`);
      }
    }

    if (showOut && cols >= 85 && totOut > 0) {
      parts.push(`${dim}${text.outLabelMonitor}${reset} ${bold}${formatMonitorTokens(totOut)}${reset}`);
    }

    if (cols >= 95 && typeof rawUsd === 'number' && Number.isFinite(rawUsd) && rawUsd > 0) {
      const usdFmt = rawUsd < 0.01 ? `$${rawUsd.toFixed(3)}` : `$${rawUsd.toFixed(2)}`;
      parts.push(`${dim}${text.costLabelMonitor}${reset} ${bold}${usdFmt}${reset}`);
    }

    if (realPct >= critPct) {
      parts.push(`${red}${bold}${text.compactionImminent}${reset}`);
    }

    return parts.join(activeSep);
  };

  const opts = {
    barWidth: cols >= 80 ? 12 : 8,
    compactSep: false,
    showCacheAbs: true,
    showCompactionPeak: true,
    showOut: true,
    modelDisplayMode: 'full',
    showCache: true,
    showCompaction: true,
  };

  let joined = buildLine(opts);
  if (!hasExplicitCols || getAnsiDisplayWidth(joined) <= cols) {
    return joined;
  }

  const degradationSteps = [
    () => { if (opts.barWidth > 8) opts.barWidth = 8; },
    () => { if (opts.barWidth > 6) opts.barWidth = 6; },
    () => { if (opts.barWidth > 4) opts.barWidth = 4; },
    () => { if (unicode) opts.compactSep = true; },
    () => { opts.showCacheAbs = false; },
    () => { opts.showCompactionPeak = false; },
    () => { opts.showOut = false; },
    () => { opts.modelDisplayMode = 'compact'; },
    () => { opts.modelDisplayMode = 'none'; },
    () => { opts.showCache = false; },
    () => { opts.showCompaction = false; },
  ];

  for (const applyStep of degradationSteps) {
    applyStep();
    joined = buildLine(opts);
    if (getAnsiDisplayWidth(joined) <= cols) {
      return joined;
    }
  }

  return truncateAnsiToDisplayWidth(joined, cols, unicode ? '…' : '...');
}

/**
 * Render the per-model cache hit breakdown line above the context monitor line.
 *
 * @param {object} params
 * @returns {string}
 */
function renderModelCacheLine({
  state,
  agyData,
  config,
  colors,
  unicode = true,
  lang,
} = {}) {
  const resolvedLang = (lang === 'zh' || lang === 'en') ? lang : resolveLanguage(config);
  const text = LANGUAGE_TEXT[resolvedLang] || LANGUAGE_TEXT.en;

  const palette = {
    reset: '\x1b[0m',
    bold: '\x1b[1m',
    dim: '\x1b[2m',
    green: ANSI_COLORS.green,
    yellow: ANSI_COLORS.yellow,
    cyan: ANSI_COLORS.cyan,
    ...(colors || {}),
  };
  const { reset, bold, dim, green, yellow, cyan } = palette;

  const rawCols = Number(agyData?.terminal_width || config?.display?.terminalWidth || process.env.COLUMNS || 0);
  const hasExplicitCols = Number.isFinite(rawCols) && rawCols > 0;
  const cols = hasExplicitCols ? Math.floor(rawCols) : 120;

  const mainModelIdent =
    agyData?.model?.display_name ||
    agyData?.model?.id ||
    (typeof agyData?.model === 'string' ? agyData.model : '') ||
    state?.lastTurnUsage?.model ||
    '';
  const mainModelKey = compactModelName(mainModelIdent) || '';

  const modelMap = new Map();
  const addEntry = (rawKey, inputTokens, cacheReadTokens, cacheWriteTokens) => {
    const inp = Math.max(0, Number(inputTokens) || 0);
    const cRead = Math.max(0, Number(cacheReadTokens) || 0);
    const cWrite = Math.max(0, Number(cacheWriteTokens) || 0);
    const totalPrompt = inp + cRead + cWrite;
    if (totalPrompt <= 0) return;
    const modelKey = compactModelName(rawKey) || String(rawKey || '').trim() || 'Model';
    const existing = modelMap.get(modelKey);
    if (existing) {
      existing.inputTokens += inp;
      existing.cacheReadTokens += cRead;
      existing.cacheWriteTokens += cWrite;
      existing.totalPrompt += totalPrompt;
    } else {
      modelMap.set(modelKey, {
        modelKey,
        inputTokens: inp,
        cacheReadTokens: cRead,
        cacheWriteTokens: cWrite,
        totalPrompt,
      });
    }
  };

  const byModel = state?.costBreakdown?.byModel;
  if (byModel && typeof byModel === 'object' && !Array.isArray(byModel)) {
    for (const [modelKey, stats] of Object.entries(byModel)) {
      if (!modelKey || !stats || typeof stats !== 'object') continue;
      addEntry(modelKey, stats.inputTokens, stats.cacheReadTokens, stats.cacheWriteTokens);
    }
  }

  if (modelMap.size === 0) {
    const cw = agyData?.context_window || {};
    const cur = hasPositiveUsage(cw.current_usage)
      ? cw.current_usage
      : (state?.lastTurnUsage && typeof state.lastTurnUsage === 'object' ? state.lastTurnUsage : null);
    if (cur) {
      const inp = Math.max(0, Number(cur.input_tokens || 0));
      const cCreate = Math.max(0, Number(cur.cache_creation_input_tokens || 0));
      const cRead = Math.max(0, Number(cur.cache_read_input_tokens || 0));
      const cacheTotal = cCreate + cRead;
      const ident = mainModelIdent || cur.model || '';
      const isInclusive = !ident || modelIncludesCacheInInput(ident);
      const isCacheExclusiveTurn = Boolean(cur) && (cur.cacheExclusive === true || inp < cacheTotal);
      const useInclusiveMode = isInclusive && !isCacheExclusiveTurn;
      const uncachedIn = useInclusiveMode ? Math.max(0, inp - cacheTotal) : inp;
      const fallbackKey = mainModelKey || compactModelName(cur.model || '') || 'Model';
      addEntry(fallbackKey, uncachedIn, cRead, cCreate);
    }
  }

  if (modelMap.size === 0) {
    return '';
  }

  const entries = Array.from(modelMap.values()).sort((a, b) => {
    const aIsMain = mainModelKey && a.modelKey === mainModelKey ? 1 : 0;
    const bIsMain = mainModelKey && b.modelKey === mainModelKey ? 1 : 0;
    if (aIsMain !== bIsMain) return bIsMain - aIsMain;
    if (b.totalPrompt !== a.totalPrompt) return b.totalPrompt - a.totalPrompt;
    return a.modelKey.localeCompare(b.modelKey);
  }).slice(0, 4);

  const icon = unicode ? '⟳' : '~';
  const prefix = `${cyan}${icon}${reset} ${dim}${text.modelCacheLabel}${reset}`;
  const normalSep = unicode ? `${dim} · ${reset}` : `${dim} | ${reset}`;
  const compactSep = unicode ? `${dim}·${reset}` : `${dim}|${reset}`;
  const innerDot = unicode ? ' · ' : ', ';

  const formatEntry = (entry, detailMode) => {
    const hitPct = Math.min(100, Math.max(0, (entry.cacheReadTokens * 100) / entry.totalPrompt));
    let hitColor = dim;
    if (hitPct >= 80) hitColor = `${green}${bold}`;
    else if (hitPct >= 50) hitColor = yellow;

    const head = `${entry.modelKey} ${hitColor}${hitPct.toFixed(0)}%${reset}`;
    if (detailMode === 'pctOnly') {
      return head;
    }

    const hitFmt = formatMonitorTokens(entry.cacheReadTokens);
    const totalFmt = formatMonitorTokens(entry.totalPrompt);
    const writeFmt = entry.cacheWriteTokens > 0 ? formatMonitorTokens(entry.cacheWriteTokens) : '';

    if (detailMode === 'hitOnly') {
      return `${head} ${dim}(${hitFmt})${reset}`;
    }
    if (detailMode === 'slash') {
      const detail = writeFmt ? `${hitFmt}+${writeFmt}/${totalFmt}` : `${hitFmt}/${totalFmt}`;
      return `${head} ${dim}(${detail})${reset}`;
    }
    const detail = writeFmt
      ? `${text.cacheHitLabel} ${hitFmt}${innerDot}${text.cacheWriteLabel} ${writeFmt} / ${text.cacheInLabel} ${totalFmt}`
      : `${text.cacheHitLabel} ${hitFmt} / ${text.cacheInLabel} ${totalFmt}`;
    return `${head} ${dim}(${detail})${reset}`;
  };

  const buildLine = ({ detailMode, useCompactSep, limit }) => {
    const activeSep = useCompactSep ? compactSep : normalSep;
    const parts = [prefix];
    for (const entry of entries.slice(0, limit)) {
      parts.push(formatEntry(entry, detailMode));
    }
    return parts.join(activeSep);
  };

  const opts = {
    detailMode: 'full',
    useCompactSep: false,
    limit: entries.length,
  };

  let line = buildLine(opts);
  if (!hasExplicitCols || getAnsiDisplayWidth(line) <= cols) {
    return line;
  }

  const steps = [
    () => { opts.detailMode = 'slash'; },
    () => { opts.detailMode = 'hitOnly'; },
    () => { opts.useCompactSep = true; },
    () => { opts.detailMode = 'pctOnly'; },
  ];
  for (let nextLimit = entries.length - 1; nextLimit >= 1; nextLimit -= 1) {
    steps.push(() => { opts.limit = nextLimit; });
  }

  for (const applyStep of steps) {
    applyStep();
    line = buildLine(opts);
    if (getAnsiDisplayWidth(line) <= cols) {
      return line;
    }
  }

  return truncateAnsiToDisplayWidth(line, cols, unicode ? '…' : '...');
}

module.exports = {
  formatMonitorTokens,
  buildSmoothBar,
  renderContextMonitorLine,
  renderModelCacheLine,
};
