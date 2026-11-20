'use strict';

const ANSI_COLORS = {
  gray: '\x1b[90m',
  blue: '\x1b[34m',
  magenta: '\x1b[35m',
  yellow: '\x1b[33m',
  cyan: '\x1b[36m',
  green: '\x1b[32m',
  red: '\x1b[31m',
};

const DEFAULT_THEME = {
  primary: 'green',
  secondary: 'gray',
  warning: 'yellow',
  critical: 'red',
};

const THEME_PRESETS = {
  'Emerald Green': { primary: 'green', secondary: 'gray', warning: 'yellow', critical: 'red' },
  'Ocean Blue': { primary: 'blue', secondary: 'gray', warning: 'yellow', critical: 'red' },
  'Cyberpunk Magenta': { primary: 'magenta', secondary: 'gray', warning: 'yellow', critical: 'red' },
  'Amber Gold': { primary: 'yellow', secondary: 'gray', warning: 'cyan', critical: 'red' },
};

/**
 * Normalize a string preset/color name or partial theme object into a 4-color theme.
 *
 * @param {string|Object|undefined|null} theme
 * @returns {{ primary: string, secondary: string, warning: string, critical: string }}
 */
function normalizeThemeObject(theme) {
  if (typeof theme === 'string') {
    const trimmed = theme.trim();
    if (THEME_PRESETS[trimmed]) {
      return { ...THEME_PRESETS[trimmed] };
    }
    const matchedPreset = Object.entries(THEME_PRESETS).find(
      ([name]) => name.toLowerCase() === trimmed.toLowerCase()
    );
    if (matchedPreset) {
      return { ...matchedPreset[1] };
    }
    if (trimmed) {
      const lower = trimmed.toLowerCase();
      return {
        ...DEFAULT_THEME,
        primary: ANSI_COLORS[lower] ? lower : trimmed,
      };
    }
    return { ...DEFAULT_THEME };
  }
  if (theme && typeof theme === 'object' && !Array.isArray(theme)) {
    return {
      primary: theme.primary || DEFAULT_THEME.primary,
      secondary: theme.secondary || DEFAULT_THEME.secondary,
      warning: theme.warning || DEFAULT_THEME.warning,
      critical: theme.critical || DEFAULT_THEME.critical,
    };
  }
  return { ...DEFAULT_THEME };
}

const DEFAULT_THRESHOLDS = {
  warning: 0.7,
  critical: 0.9,
};

const DEFAULT_COLUMN_WIDTH = 40;
const QUOTA_CHROME_WIDTH = 21;

const TIER_ABBREVS = { XHigh: 'XH', Thinking: 'Th', High: 'H', Medium: 'M', Low: 'L', 'Extra-Low': 'XL' };

const ABBREVIATION_RULES = [
  [/^Gemini ([\d.]+) (Flash|Pro|Argon|Carbon|Hydrogen|Neon) \(([\w-]+)\)/, (_, ver, fam, tier) =>
    `Gemini ${ver} ${fam}(${TIER_ABBREVS[tier] || tier[0]})`],
  [/^Orchestrator \(([^)]+)\)/, (_, fam) => `Orch (${fam})`],
  [/^Claude (\w+) ([\d.]+) \(([\w-]+)\)/, (_, fam, ver, tier) =>
    `${fam} ${ver}(${TIER_ABBREVS[tier] || tier[0]})`],
  [/^Claude ([\d.]+) (\w+)(?: \(([\w-]+)\))?/, (_, ver, fam, tier) =>
    (tier ? `${fam} ${ver}(${TIER_ABBREVS[tier] || tier[0]})` : `${fam} ${ver}`)],
  [/^GPT-OSS (.+?) \(([\w-]+)\)/, (_, spec) => `GPT-OSS ${spec}`],
];

function abbreviateDisplayName(name) {
  for (const [re, replacer] of ABBREVIATION_RULES) {
    const m = re.exec(name);
    if (m) return name.replace(re, replacer);
  }
  return name;
}

function simplifyModelName(name) {
  if (!name) return '';
  return abbreviateDisplayName(name);
}

const COMPACT_NAME_RULES = [
  [/^Gemini [\d.]+ (Flash|Pro|Argon|Carbon|Hydrogen|Neon).*/i, (_, fam) => {
    const lower = fam.toLowerCase();
    if (lower === 'hydrogen') return 'Hydro';
    if (lower === 'neon') return 'Neon';
    return fam;
  }],
  [/^Orchestrator \((Carbon|Argon|Neon)\)/i, (_, fam) => {
    const lower = fam.toLowerCase();
    if (lower === 'carbon') return 'Orch-C';
    if (lower === 'neon') return 'Orch-N';
    return 'Orch-A';
  }],
  [/^Claude (\w+) [\d.]+(?: \(([\w-]+)\))?/i, (_, fam) => fam],
  [/^Claude [\d.]+ (\w+)(?: \(([\w-]+)\))?/i, (_, fam) => fam],
  [/^GPT-OSS .*/i, () => 'GPT'],
  [/gemini-.*orchestrator.*argon/i, () => 'Orch-A'],
  [/gemini-.*orchestrator.*neon/i, () => 'Orch-N'],
  [/gemini-.*orchestrator/i, () => 'Orch-C'],
  [/gemini-.*carbon/i, () => 'Carbon'],
  [/gemini-.*hydrogen/i, () => 'Hydro'],
  [/gemini-.*argon/i, () => 'Argon'],
  [/gemini-.*neon/i, () => 'Neon'],
  [/gemini-.*flash/i, () => 'Flash'],
  [/gemini-.*pro/i, () => 'Pro'],
  [/claude-.*sonnet/i, () => 'Sonnet'],
  [/claude-.*opus/i, () => 'Opus'],
  [/claude-.*haiku/i, () => 'Haiku'],
  [/gpt/i, () => 'GPT'],
  [/^hydrogen$/i, () => 'Hydro'],
  [/^carbon$/i, () => 'Carbon'],
  [/^argon$/i, () => 'Argon'],
  [/^neon$/i, () => 'Neon'],
  [/^flash$/i, () => 'Flash'],
  [/^pro$/i, () => 'Pro'],
  [/^sonnet$/i, () => 'Sonnet'],
  [/^opus$/i, () => 'Opus'],
  [/^haiku$/i, () => 'Haiku'],
  [/orchestrator.*argon|argon.*orchestrator/i, () => 'Orch-A'],
  [/orchestrator.*neon|neon.*orchestrator/i, () => 'Orch-N'],
  [/orchestrator/i, () => 'Orch-C'],
  [/hydrogen/i, () => 'Hydro'],
  [/carbon/i, () => 'Carbon'],
  [/argon/i, () => 'Argon'],
  [/\bneon\b|[_-]neon/i, () => 'Neon'],
  [/flash/i, () => 'Flash'],
  [/sonnet/i, () => 'Sonnet'],
  [/opus/i, () => 'Opus'],
  [/haiku/i, () => 'Haiku'],
  [/deepseek/i, () => 'DeepSeek'],
  [/(?:^|[-_\s])pro(?:[-_\s]|$)/i, () => 'Pro'],
];

function compactModelName(displayName) {
  if (!displayName || typeof displayName !== 'string') return '';
  for (const [re, replacer] of COMPACT_NAME_RULES) {
    const m = re.exec(displayName);
    if (m) return replacer(...m);
  }
  return Array.from(displayName).slice(0, 6).join('');
}

function normalizeModelMatchValue(value) {
  if (!value) return '';
  return simplifyModelName(value)
    .replace(/\s+(preview|experimental|beta|latest)$/i, '')
    .trim()
    .toLowerCase();
}

function modelNamesMatch(left, right) {
  const a = normalizeModelMatchValue(left);
  const b = normalizeModelMatchValue(right);
  if (!a || !b) return false;
  return a === b || a.startsWith(`${b} `) || b.startsWith(`${a} `);
}

function modelIncludesCacheInInput(nameOrId) {
  if (!nameOrId) return false;
  return /claude|anthropic|sonnet|opus|haiku|gemini|carbon|hydrogen|\bhydro\b|argon|neon|orchestrator|\borch-[can]\b|gpt|deepseek|o1-|o3-|o4-/i.test(String(nameOrId));
}

function normalizeTokenCount(value) {
  if (value === undefined || value === null || value === '') return null;
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? number : null;
}

/** Calculate a per-turn cache hit rate respecting model cache-inclusive semantics. */
function calculateTurnCacheMetrics(usage, rawModelName) {
  if (!usage || typeof usage !== 'object') return null;

  const cacheRead = normalizeTokenCount(usage.cache_read_input_tokens);
  const cacheWrite = normalizeTokenCount(usage.cache_creation_input_tokens) ?? 0;
  const input = normalizeTokenCount(usage.input_tokens);

  if (cacheRead === null || input === null) return { available: false };

  const cacheTotal = cacheRead + cacheWrite;
  const isInclusive = !rawModelName || modelIncludesCacheInInput(rawModelName);
  const totalPrompt = (input >= cacheTotal && isInclusive) ? input : input + cacheTotal;
  if (totalPrompt === 0) return { available: false };

  return {
    cacheRead,
    totalPrompt,
    hitRate: Math.min(100, Math.max(0, (cacheRead / totalPrompt) * 100)),
    available: true,
  };
}

/**
 * Format a token count into "1.2M", "150k", or "42" (compact).
 * Boundary thresholds use 999950 / 999.5 so the rounding never spits out
 * spike values like "1000k" (regression covered by a unit test).
 */
function formatTokens(n) {
  if (n >= 999950) {
    const val = n / 1000000;
    let str = val.toFixed(1);
    if (str.endsWith('.0')) str = str.slice(0, -2);
    return str + 'M';
  }
  if (n >= 999.5) {
    const val = n / 1000;
    let str = val.toFixed(1);
    if (str.endsWith('.0')) str = str.slice(0, -2);
    if (str === '1000') return '1M';
    return str + 'k';
  }
  return Math.round(n).toString();
}

function formatTurnCacheBadge(metrics, label, colors, isCompact, thresholds = {}) {
  const palette = colors || {};
  const reset = palette.reset || '';
  const dim = palette.dim || '';
  const safeLabel = label || 'cache';

  if (!metrics || metrics.available === false) {
    return `${palette.gray || ''}${dim}${safeLabel} --${reset}`;
  }

  const levels = thresholds && typeof thresholds === 'object' ? thresholds : {};
  const excellent = Number.isFinite(levels.excellent) ? levels.excellent : 80;
  const partial = Number.isFinite(levels.partial) ? levels.partial : 50;
  const hitRate = Number.isFinite(metrics.hitRate)
    ? Math.min(100, Math.max(0, metrics.hitRate))
    : 0;

  let style;
  if (hitRate >= excellent) {
    style = `${palette.green || palette.cyan || ''}${palette.bold || ''}`;
  } else if (hitRate >= partial) {
    style = palette.yellow || '';
  } else if (hitRate > 0) {
    style = `${palette.yellow || ''}${dim}`;
  } else {
    style = `${palette.gray || ''}${dim}`;
  }

  const absolute = isCompact
    ? ''
    : ` (${formatTokens(normalizeTokenCount(metrics.cacheRead) ?? 0)}/${formatTokens(normalizeTokenCount(metrics.totalPrompt) ?? 0)})`;
  return `${style}${safeLabel} ${hitRate.toFixed(1)}%${absolute}${reset}`;
}

/** Format seconds into "XhYm" / "Ym" / "now" (compact). */
function formatDuration(secs) {
  if (secs <= 0) return 'now';
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d >= 10) return `${d}d`;
  if (d > 0) return `${d}d${h}h`;
  if (h >= 10) return `${h}h`;
  if (h > 0) return `${h}h${m}m`;
  return `${m}m`;
}

function sanitizeTerminalText(value, maxLength = 120) {
  if (value === undefined || value === null) return '';
  const cleaned = String(value)
    .replace(/\x1b\][^\x07]*?(?:\x07|\x1b\\|$)/g, '')
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
    .replace(/[\x00-\x1f\x7f]/g, '');
  const codePoints = Array.from(cleaned);
  return codePoints.length > maxLength ? codePoints.slice(0, maxLength).join('') : cleaned;
}

function charDisplayWidth(codePoint) {
  if (!codePoint) return 0;
  if (
    (codePoint >= 0x0300 && codePoint <= 0x036f) ||
    (codePoint >= 0x1ab0 && codePoint <= 0x1aff) ||
    (codePoint >= 0x1dc0 && codePoint <= 0x1dff) ||
    (codePoint >= 0x20d0 && codePoint <= 0x20ff) ||
    (codePoint >= 0xfe20 && codePoint <= 0xfe2f) ||
    (codePoint >= 0xfe00 && codePoint <= 0xfe0f) ||
    (codePoint >= 0xe0100 && codePoint <= 0xe01ef) ||
    (codePoint >= 0x200b && codePoint <= 0x200f) ||
    codePoint === 0x2060 ||
    codePoint === 0xfeff ||
    (codePoint >= 0x1f3fb && codePoint <= 0x1f3ff)
  ) {
    return 0;
  }
  if (
    codePoint >= 0x1100 && (
      codePoint <= 0x115f ||
      codePoint === 0x2329 || codePoint === 0x232a ||
      (codePoint >= 0x2e80 && codePoint <= 0xa4cf && codePoint !== 0x303f) ||
      (codePoint >= 0xac00 && codePoint <= 0xd7a3) ||
      (codePoint >= 0xf900 && codePoint <= 0xfaff) ||
      (codePoint >= 0xfe10 && codePoint <= 0xfe19) ||
      (codePoint >= 0xfe30 && codePoint <= 0xfe6f) ||
      (codePoint >= 0xff00 && codePoint <= 0xff60) ||
      (codePoint >= 0xffe0 && codePoint <= 0xffe6) ||
      (codePoint >= 0x1f300 && codePoint <= 0x1faf8) ||
      (codePoint >= 0x20000 && codePoint <= 0x2fffd) ||
      (codePoint >= 0x30000 && codePoint <= 0x3fffd)
    )
  ) {
    return 2;
  }
  return 1;
}

function getUnitDisplayWidth(chars, index) {
  const cp = chars[index].codePointAt(0);
  const nextCp = index + 1 < chars.length ? chars[index + 1].codePointAt(0) : 0;
  const baseWidth = charDisplayWidth(cp);
  return nextCp === 0xfe0f && baseWidth === 1 ? 2 : baseWidth;
}

function getDisplayWidth(str) {
  const chars = Array.from(String(str || ''));
  let width = 0;
  for (let i = 0; i < chars.length; i++) {
    width += getUnitDisplayWidth(chars, i);
  }
  return width;
}

const ANSI_SEQUENCE_RE = /(\x1b\][^\x07]*?(?:\x07|\x1b\\|$)|\x1b\[[0-?]*[ -/]*[@-~])/g;

function stripAnsi(str) {
  if (str === undefined || str === null) return '';
  return String(str)
    .replace(/\x1b\][^\x07]*?(?:\x07|\x1b\\|$)/g, '')
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
}

function getAnsiDisplayWidth(str) {
  return getDisplayWidth(stripAnsi(str));
}

function truncateAnsiToDisplayWidth(str, maxWidth, ellipsis = '…') {
  if (str === undefined || str === null) return '';
  const input = String(str);
  const limit = Number(maxWidth);
  if (!Number.isFinite(limit) || limit <= 0) return '';
  if (getAnsiDisplayWidth(input) <= limit) return input;

  const ellipsisStr = String(ellipsis ?? '…');
  const ellipsisWidth = getDisplayWidth(ellipsisStr);
  let safeEllipsis = ellipsisStr;
  let budget = limit - ellipsisWidth;

  if (budget < 0) {
    budget = 0;
    const eChars = Array.from(ellipsisStr);
    let eOut = '';
    let eUsed = 0;
    for (let i = 0; i < eChars.length; i++) {
      const w = getUnitDisplayWidth(eChars, i);
      if (eUsed + w > limit) break;
      eOut += eChars[i];
      eUsed += w;
    }
    safeEllipsis = eOut;
  }

  let out = '';
  let used = 0;
  let truncated = false;
  const tokens = input.split(ANSI_SEQUENCE_RE);

  for (let t = 0; t < tokens.length; t++) {
    const token = tokens[t];
    if (!token) continue;
    if (t % 2 === 1) {
      out += token;
      continue;
    }
    const chars = Array.from(token);
    for (let i = 0; i < chars.length; i++) {
      const w = getUnitDisplayWidth(chars, i);
      if (used + w > budget) {
        truncated = true;
        break;
      }
      out += chars[i];
      used += w;
    }
    if (truncated) break;
  }

  return `${out}${safeEllipsis}\x1b[0m`;
}

function truncateToDisplayWidth(str, width, ellipsis = '...') {
  const input = String(str || '');
  const currentWidth = getDisplayWidth(input);
  if (currentWidth <= width) {
    return input + ' '.repeat(Math.max(0, width - currentWidth));
  }
  const ellipsisWidth = getDisplayWidth(ellipsis);
  const budget = Math.max(0, width - ellipsisWidth);
  const chars = Array.from(input);
  let out = '';
  let used = 0;
  for (let i = 0; i < chars.length; i++) {
    const w = getUnitDisplayWidth(chars, i);
    if (used + w > budget) break;
    out += chars[i];
    used += w;
  }
  const result = out + ellipsis;
  return result + ' '.repeat(Math.max(0, width - getDisplayWidth(result)));
}

/**
 * Format remainingFraction into an integer percentage.
 * Clamps non-100% values (e.g. 0.9953883) to at most 99% so that
 * fractional usage does not get rounded up to 100% and look stuck.
 */
function formatQuotaPercent(fraction) {
  if (fraction === undefined || fraction === null || Number.isNaN(fraction)) return 0;
  if (fraction >= 1) return 100;
  if (fraction <= 0) return 0;
  return Math.min(99, Math.max(0, Math.floor(fraction * 100)));
}

module.exports = {
  ANSI_COLORS,
  DEFAULT_THEME,
  THEME_PRESETS,
  normalizeThemeObject,
  DEFAULT_THRESHOLDS,
  DEFAULT_COLUMN_WIDTH,
  QUOTA_CHROME_WIDTH,
  TIER_ABBREVS,
  abbreviateDisplayName,
  simplifyModelName,
  compactModelName,
  normalizeModelMatchValue,
  modelNamesMatch,
  modelIncludesCacheInInput,
  calculateTurnCacheMetrics,
  formatTurnCacheBadge,
  sanitizeTerminalText,
  charDisplayWidth,
  getDisplayWidth,
  stripAnsi,
  getAnsiDisplayWidth,
  truncateAnsiToDisplayWidth,
  truncateToDisplayWidth,
  formatTokens,
  formatDuration,
  formatQuotaPercent,
};
