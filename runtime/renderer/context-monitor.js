'use strict';

const {
  ANSI_COLORS,
  modelIncludesCacheInInput,
  sanitizeTerminalText,
} = require('./format.js');
const {
  LANGUAGE_TEXT,
  resolveLanguage,
} = require('./lang.js');

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

function resolveModelLabel(agyData) {
  const modelObj = agyData?.model;
  if (modelObj && typeof modelObj === 'object') {
    let name = modelObj.display_name || modelObj.id || 'Unknown Model';
    const effort = modelObj.effort || '';
    if (effort && !name.toLowerCase().includes(effort.toLowerCase())) {
      name = `${name} (${effort})`;
    }
    return sanitizeTerminalText(name, 80);
  }
  if (typeof modelObj === 'string' && modelObj.trim()) {
    return sanitizeTerminalText(modelObj.trim(), 80);
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
  const cols = Number.isFinite(rawCols) && rawCols > 0 ? rawCols : 120;

  const modelName = resolveModelLabel(agyData);
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
  const promptCtx = (inp >= cacheTotal && isInclusive) ? inp : inp + cacheTotal;
  let activeCtx = promptCtx + turnOut;

  if (activeCtx === 0 && rawUsedPct > 0 && rawWinSize > 0) {
    activeCtx = Math.round((rawWinSize * rawUsedPct) / 100);
  }

  const ckpts = Math.max(0, Number(state?.compactionCount || 0));
  const hasModePeaks = Boolean(state?.turnHistoryMaxByMode && typeof state.turnHistoryMaxByMode === 'object');
  const historyPeak = hasModePeaks
    ? (isInclusive
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

  let barColor = cyan;
  if (realPct >= 85) barColor = red;
  else if (realPct >= 65) barColor = yellow;

  const barWidth = cols >= 80 ? 12 : 8;
  const bar = buildSmoothBar(realPct, barWidth, unicode);

  const ctxSeg = (activeCtx > 0 || hasTurn)
    ? `${dim}${text.ctxLabel}${reset} ${barColor}${bar}${reset} ${bold}${realPct.toFixed(1)}%${reset} ${dim}(${formatMonitorTokens(activeCtx)}/${formatMonitorTokens(effectiveLimit)})${reset}`
    : `${dim}${text.ctxLabel}${reset} ${dim}${bar}${reset} ${bold}0.0%${reset} ${dim}(${text.limitLabel} ${formatMonitorTokens(effectiveLimit)})${reset}`;

  const sep = unicode ? `${dim} · ${reset}` : `${dim} | ${reset}`;
  const modelGlyph = unicode ? '◈' : '*';
  const timesGlyph = unicode ? '×' : 'x';

  const parts = [];
  if (cols >= 80 && modelName) {
    parts.push(`${magenta}${modelGlyph} ${modelName}${reset}`);
  }

  parts.push(ctxSeg);

  if (ckpts > 0) {
    if (cols >= 95 && peakCtx > 0) {
      const peakPrefix = resolvedLang === 'zh' ? text.peakLabel : `${text.peakLabel} `;
      parts.push(`${yellow}${text.compactedLabel}${timesGlyph}${ckpts}${reset}${dim}(${peakPrefix}${formatMonitorTokens(peakCtx)})${reset}`);
    } else {
      parts.push(`${yellow}${text.compactedLabel}${timesGlyph}${ckpts}${reset}`);
    }
  }

  if (promptCtx > 0) {
    const cachePct = Math.min(100, Math.max(0, (cRead * 100) / promptCtx));
    const cacheColor = cachePct >= 50 ? green : dim;
    if (cols >= 105 && cRead > 0) {
      parts.push(`${dim}${text.cacheLabelMonitor}${reset} ${cacheColor}${cachePct.toFixed(0)}%${reset} ${dim}(${formatMonitorTokens(cRead)})${reset}`);
    } else {
      parts.push(`${dim}${text.cacheLabelMonitor}${reset} ${cacheColor}${cachePct.toFixed(0)}%${reset}`);
    }
  }

  const totOut = Math.max(Number(cw.total_output_tokens || 0), Number(state?.totalOutputTokens || 0));
  if (cols >= 85 && totOut > 0) {
    parts.push(`${dim}${text.outLabelMonitor}${reset} ${bold}${formatMonitorTokens(totOut)}${reset}`);
  }

  const rawUsd = agyData?.cost?.total_usd ?? agyData?.cost?.total_cost_usd ?? sessionCostUsd;
  if (cols >= 95 && typeof rawUsd === 'number' && Number.isFinite(rawUsd) && rawUsd > 0) {
    const usdFmt = rawUsd < 0.01 ? `$${rawUsd.toFixed(3)}` : `$${rawUsd.toFixed(2)}`;
    parts.push(`${dim}${text.costLabelMonitor}${reset} ${bold}${usdFmt}${reset}`);
  }

  if (realPct >= 85) {
    parts.push(`${red}${bold}${text.compactionImminent}${reset}`);
  }

  return parts.join(sep);
}

module.exports = {
  formatMonitorTokens,
  buildSmoothBar,
  renderContextMonitorLine,
};
