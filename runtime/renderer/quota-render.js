'use strict';

const {
  PROVIDER_LABELS,
} = require('./lang.js');
const {
  simplifyModelName,
  compactModelName,
  sanitizeTerminalText,
  formatDuration,
  formatQuotaPercent,
  getAnsiDisplayWidth,
  truncateAnsiToDisplayWidth,
} = require('./format.js');

/**
 * Build the quota-rendering closures bound to a single renderStatusLine invocation's
 * configuration. Renders each model as a single line "name [bar] pct ~time"
 * and pairs lines into 2 columns at the call site.
 *
 * Note: per-window data (q.windows.fiveHour / q.windows.weekly) is preserved
 * in the cache layer but intentionally not surfaced by this renderer — the
 * binding-window value already lives in q.remainingFraction / q.resetTime.
 * See runtime/quota/models.js for the windows merging logic and PR #59 for
 * a 3-column variant that displays both windows.
 *
 * @param {Object} ctx
 * @param {Object} ctx.colors  { cyan, reset, gray, red, yellow, green }
 * @param {Object} ctx.glyph   { bar, empty, vbar, hbar, ellipsis }
 * @param {Object} ctx.thresholds  { warnThresh, critThresh }
 * @param {number} ctx.nameWidth
 * @param {string} ctx.divider
 * @param {Function} ctx.createProgressBar  (percent, color, width, isUsage) => string
 * @param {Function} ctx.truncateAndPad     (str, width) => string
 */
const {
  pickCriticalWindow,
  classifyQuotaWindow,
  isObservationExpired,
  resolveModelDisplayName,
} = require('../quota/models.js');

function resolveQuotaDisplayName(q) {
  const explicit = (typeof q?.displayName === 'string' && q.displayName.trim() && q.displayName.trim() !== q.id)
    ? q.displayName
    : undefined;
  return resolveModelDisplayName(q?.id, explicit) || q?.displayName || q?.id || '';
}

const PROVIDER_KEY_LABELS = {
  google: 'Google',
  claude: 'Claude',
  argon: 'Argon',
};

function formatProviderDisplayLabel(rawKey) {
  const lower = String(rawKey || '').trim().toLowerCase();
  if (PROVIDER_KEY_LABELS[lower]) return PROVIDER_KEY_LABELS[lower];
  const cleaned = sanitizeTerminalText(String(rawKey || ''), 16).trim();
  if (!cleaned) return 'Other';
  return cleaned.charAt(0).toUpperCase() + cleaned.slice(1);
}

function createQuotaRenderers(ctx) {
  const { colors, glyph, thresholds, nameWidth, divider, createProgressBar, truncateAndPad, explicitCols = 0 } = ctx;
  const { cyan, reset, gray, red, yellow, green } = colors;
  const { warnThresh, critThresh } = thresholds;

  const renderQuotaColumn = (q, now) => {
    const critical = pickCriticalWindow(q.windows, now) || q;
    const pct = formatQuotaPercent(critical.remainingFraction);
    const pctColor = pct <= (1 - critThresh) * 100 ? red
                   : pct <= (1 - warnThresh) * 100 ? yellow
                   : green;

    const rawName = sanitizeTerminalText(simplifyModelName(resolveQuotaDisplayName(q)), 120);
    const namePart = truncateAndPad(rawName, nameWidth);
    const coloredName = `${cyan}${namePart}${reset}`;

    const barPart = createProgressBar(pct, pctColor, 6, false);

    const pctStr = `${pct}%`.padStart(4, ' ');
    const coloredPct = `${pctColor}${pctStr}${reset}`;

    let rawTime = '';
    const resetTime = critical.resetTime || q.resetTime;
    if (resetTime) {
      const resetMs = new Date(resetTime).getTime();
      if (Number.isFinite(resetMs) && resetMs > now) {
        const secsLeft = Math.max(0, Math.round((resetMs - now) / 1000));
        rawTime = `~${formatDuration(secsLeft)}`;
      }
    }
    const timePart = rawTime.padEnd(6, ' ');
    const coloredTime = `${gray}${timePart}${reset}`;

    return `${coloredName} ${barPart} ${coloredPct} ${coloredTime}`;
  };

  const isImageModel = (q) => Boolean((q?.id && q.id.includes('image')) || (q?.displayName && q.displayName.toLowerCase().includes('image')));

  const renderProviderQuotaTable = (data, now = Date.now()) => {
    const providerWindows = {
      Google: { fiveHour: null, weekly: null },
      Claude: { fiveHour: null, weekly: null },
      Argon: { fiveHour: null, weekly: null },
    };

    for (const q of data || []) {
      if (!q || isImageModel(q)) continue;
      const ident = `${q.id || ''} ${q.displayName || ''}`;
      const isArgon = /argon/i.test(ident);
      const isGoogle = !isArgon && (q.modelProvider === 'MODEL_PROVIDER_GOOGLE' || /gemini|carbon|hydrogen|neon|orchestrator/i.test(ident));
      const isClaude = !isArgon && !isGoogle && (q.modelProvider === 'MODEL_PROVIDER_ANTHROPIC' || /claude|sonnet|opus|haiku|gpt/i.test(ident));
      const key = isArgon ? 'Argon' : isGoogle ? 'Google' : isClaude ? 'Claude' : null;
      if (!key) continue;
      if (!providerWindows[key]) {
        providerWindows[key] = { fiveHour: null, weekly: null };
      }

      if (q.windows?.fiveHour && !isObservationExpired(q.windows.fiveHour, now)) {
        const cur = providerWindows[key].fiveHour;
        const win = q.windows.fiveHour;
        if (!cur || (win.observedAt || 0) > (cur.observedAt || 0) ||
            ((win.observedAt || 0) === (cur.observedAt || 0) && win.remainingFraction < cur.remainingFraction)) {
          providerWindows[key].fiveHour = win;
        }
      }
      if (q.windows?.weekly && !isObservationExpired(q.windows.weekly, now)) {
        const cur = providerWindows[key].weekly;
        const win = q.windows.weekly;
        if (!cur || (win.observedAt || 0) > (cur.observedAt || 0) ||
            ((win.observedAt || 0) === (cur.observedAt || 0) && win.remainingFraction < cur.remainingFraction)) {
          providerWindows[key].weekly = win;
        }
      }

      if (q.resetTime && !q.windows?.fiveHour && !q.windows?.weekly) {
        const winType = classifyQuotaWindow(q.resetTime, now);
        if (winType === 'fiveHour') {
          const cur = providerWindows[key].fiveHour;
          if (!cur || q.remainingFraction < cur.remainingFraction) {
            providerWindows[key].fiveHour = { remainingFraction: q.remainingFraction, resetTime: q.resetTime };
          }
        } else if (winType === 'weekly') {
          const cur = providerWindows[key].weekly;
          if (!cur || q.remainingFraction < cur.remainingFraction) {
            providerWindows[key].weekly = { remainingFraction: q.remainingFraction, resetTime: q.resetTime };
          }
        }
      } else if (!q.resetTime && !q.windows?.fiveHour && !q.windows?.weekly) {
        if (!providerWindows[key].fiveHour && key !== 'Argon') {
          providerWindows[key].fiveHour = { remainingFraction: q.remainingFraction, resetTime: null };
        }
        if (!providerWindows[key].weekly) {
          providerWindows[key].weekly = { remainingFraction: q.remainingFraction, resetTime: null };
        }
      }
    }

    // Provider summaries are authoritative and must win over model-derived data.
    const pq = data?.providerQuota;
    if (pq && typeof pq === 'object') {
      for (const [rawKey, entry] of Object.entries(pq)) {
        if (!entry || typeof entry !== 'object') continue;
        if (/^openai$/i.test(String(rawKey).trim())) continue;
        const label = formatProviderDisplayLabel(rawKey);
        if (/^openai$/i.test(label)) continue;
        if (!providerWindows[label]) {
          providerWindows[label] = { fiveHour: null, weekly: null };
        }
        if (entry.fiveHour && !isObservationExpired(entry.fiveHour, now)) {
          providerWindows[label].fiveHour = entry.fiveHour;
        }
        if (entry.weekly && !isObservationExpired(entry.weekly, now)) {
          providerWindows[label].weekly = entry.weekly;
        }
      }
    }

    const renderWindowItem = (label, win) => {
      const namePart = truncateAndPad(label, 11);
      const coloredName = `${cyan}${namePart}${reset}`;

      if (!win) {
        const emptyBar = `${gray}[${glyph.empty.repeat(6)}]${reset}`;
        const missingPct = `${gray}  --${reset}`;
        const missingTime = `${gray}${' '.repeat(6)}${reset}`;
        return `${coloredName} ${emptyBar} ${missingPct} ${missingTime}`;
      }

      const pct = formatQuotaPercent(win.remainingFraction);
      const pctColor = pct <= (1 - critThresh) * 100 ? red
                     : pct <= (1 - warnThresh) * 100 ? yellow
                     : green;

      const barPart = createProgressBar(pct, pctColor, 6, false);

      const pctStr = `${pct}%`.padStart(4, ' ');
      const coloredPct = `${pctColor}${pctStr}${reset}`;

      let rawTime = '';
      if (win.resetTime) {
        const resetMs = new Date(win.resetTime).getTime();
        if (Number.isFinite(resetMs) && resetMs > now) {
          const secsLeft = Math.max(0, Math.round((resetMs - now) / 1000));
          rawTime = `~${formatDuration(secsLeft)}`;
        }
      }
      const timePart = rawTime.padEnd(6, ' ');
      const coloredTime = `${gray}${timePart}${reset}`;

      return `${coloredName} ${barPart} ${coloredPct} ${coloredTime}`;
    };

    const g5 = renderWindowItem('Google 5h', providerWindows.Google.fiveHour);
    const gw = renderWindowItem('Google week', providerWindows.Google.weekly);
    const c5 = renderWindowItem('Claude 5h', providerWindows.Claude.fiveHour);
    const cw = renderWindowItem('Claude week', providerWindows.Claude.weekly);

    const rows = [
      `  ${g5} ${gray}${glyph.vbar}${reset} ${c5}`,
      `  ${gw} ${gray}${glyph.vbar}${reset} ${cw}`,
    ];

    const formatWeeklyWindowLabel = (providerLabel) => {
      const full = `${providerLabel} week`;
      return getAnsiDisplayWidth(full) > 11 ? `${providerLabel} 7d` : full;
    };

    const extraKeys = Object.keys(providerWindows).filter(k => k !== 'Google' && k !== 'Claude' && !/^openai$/i.test(k));
    for (const label of extraKeys) {
      const win = providerWindows[label];
      if (!win || (!win.fiveHour && !win.weekly)) continue;
      if (win.fiveHour && win.weekly) {
        const w5 = renderWindowItem(`${label} 5h`, win.fiveHour);
        const ww = renderWindowItem(formatWeeklyWindowLabel(label), win.weekly);
        rows.push(`  ${w5} ${gray}${glyph.vbar}${reset} ${ww}`);
      } else if (win.weekly) {
        rows.push(`  ${renderWindowItem(formatWeeklyWindowLabel(label), win.weekly)}`);
      } else {
        rows.push(`  ${renderWindowItem(`${label} 5h`, win.fiveHour)}`);
      }
    }

    const plainRowWidth = getAnsiDisplayWidth(rows[0]) - 2;
    const baseDividerWidth = Math.max(67, plainRowWidth);
    const shouldClip = explicitCols > 0 && explicitCols < 69;
    const finalRows = shouldClip
      ? rows.map(r => (getAnsiDisplayWidth(r) > explicitCols ? truncateAnsiToDisplayWidth(r, explicitCols, glyph.ellipsis) : r))
      : rows;
    const dividerWidth = shouldClip
      ? Math.min(baseDividerWidth, Math.max(10, explicitCols - 2))
      : baseDividerWidth;
    const dividerLine = `  ${gray}${glyph.hbar.repeat(dividerWidth)}${reset}`;
    return `${dividerLine}\n` + finalRows.join('\n');
  };

  const inferCompactProviderLabel = (q) => {
    if (q.modelProvider === 'MODEL_PROVIDER_OPENAI') {
      return PROVIDER_LABELS.MODEL_PROVIDER_ANTHROPIC || 'Anthropic';
    }
    if (q.modelProvider && PROVIDER_LABELS[q.modelProvider]) {
      return PROVIDER_LABELS[q.modelProvider];
    }
    const ident = `${q.id || ''} ${q.displayName || ''}`;
    if (/gemini|carbon|hydrogen|argon|neon|orchestrator/i.test(ident)) return 'Google';
    if (/claude|sonnet|opus|haiku|gpt/i.test(ident)) return 'Claude';
    return 'Other';
  };

  // Compact: provider-grouped mini bars based on the most constrained quota window.
  const renderCompactQuotaLine = (data, now = Date.now()) => {
    const groups = new Map();
    for (const q of data || []) {
      if (!q || isImageModel(q)) continue;
      const label = inferCompactProviderLabel(q);
      if (!groups.has(label)) groups.set(label, []);
      groups.get(label).push(q);
    }
    const segments = [];
    for (const [provider, models] of groups) {
      // Family-level dedup: retain the most constrained model (lowest effective remaining fraction).
      const familyMap = new Map();
      for (const q of models) {
        const key = compactModelName(resolveQuotaDisplayName(q));
        const critical = pickCriticalWindow(q?.windows, now) || q;
        const effFraction = typeof critical?.remainingFraction === 'number' ? critical.remainingFraction : 1;
        if (!familyMap.has(key)) {
          familyMap.set(key, { model: q, effFraction, critical });
        } else {
          const current = familyMap.get(key);
          if (effFraction < current.effFraction) {
            familyMap.set(key, { model: q, effFraction, critical });
          }
        }
      }
      const items = Array.from(familyMap.values()).map(({ model: q, critical }) => {
        const name = sanitizeTerminalText(compactModelName(resolveQuotaDisplayName(q)), 20);
        const pct = formatQuotaPercent(critical.remainingFraction);
        const filled = Math.max(0, Math.min(3, Math.round((pct / 100) * 3)));
        const empty = Math.max(0, 3 - filled);
        const barColor = pct <= (1 - critThresh) * 100 ? red : pct <= (1 - warnThresh) * 100 ? yellow : green;
        return `${cyan}${name}${reset}${barColor}${glyph.bar.repeat(filled)}${gray}${glyph.empty.repeat(empty)}${reset}`;
      });
      segments.push(`${gray}${provider}:${reset} ${items.join(' ')}`);
    }

    const pq = data?.providerQuota;
    if (segments.length === 0 && pq && typeof pq === 'object') {
      for (const [rawKey, entry] of Object.entries(pq)) {
        if (!entry || typeof entry !== 'object') continue;
        if (/^openai$/i.test(String(rawKey).trim())) continue;
        const label = formatProviderDisplayLabel(rawKey);
        if (/^openai$/i.test(label)) continue;
        const critical = pickCriticalWindow(entry, now);
        if (!critical || typeof critical.remainingFraction !== 'number') continue;
        const pct = formatQuotaPercent(critical.remainingFraction);
        const pctColor = pct <= (1 - critThresh) * 100 ? red
                       : pct <= (1 - warnThresh) * 100 ? yellow
                       : green;
        segments.push(`${gray}${label}:${reset} ${pctColor}${pct}%${reset}`);
      }
    }

    return segments.join(divider);
  };

  return {
    renderQuotaColumn,
    renderProviderQuotaTable,
    renderCompactQuotaLine,
  };
}

module.exports = {
  createQuotaRenderers,
};
