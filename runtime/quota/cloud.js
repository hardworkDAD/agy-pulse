'use strict';

const fs = require('fs');
const path = require('path');
const { execFile } = require('child_process');
const { resolveSafeExecutable } = require('../paths.js');
const {
  FALLBACK_AGENT_MODEL_IDS,
  discoverAgentModelIds,
  resolveDeprecatedIds,
  normalizeQuotaModels,
  classifyQuotaWindow,
  createUnavailableQuotaResult,
} = require('./models.js');

const NATIVE_FETCH = globalThis.fetch;

// Authoritative quota endpoint (daily-cloudcode-pa confirmed source)
const DEFAULT_ENDPOINTS = [
  'https://daily-cloudcode-pa.googleapis.com',
];

// ─── Runtime User-Agent ──────────────────────────────────────────────────────
let _pkg = null;
function getPackageVersion() {
  if (_pkg) return _pkg;
  try {
    _pkg = JSON.parse(fs.readFileSync(path.join(__dirname, '..', '..', 'package.json'), 'utf8')).version;
  } catch {
    _pkg = '0.0.0';
  }
  return _pkg;
}

function getPlatformArch() {
  const plat = { darwin: 'darwin', linux: 'linux', win32: 'windows' }[process.platform] || process.platform;
  const arch = { x64: 'amd64', arm64: 'arm64', ia32: '386' }[process.arch] || process.arch;
  return `${plat}/${arch}`;
}

/**
 * Build headers matching what agy sends.
 * @param {string} accessToken
 */
function buildHeaders(accessToken) {
  return {
    'Authorization': `Bearer ${accessToken}`,
    'Content-Type': 'application/json',
    'User-Agent': `antigravity/${getPackageVersion()} ${getPlatformArch()}`,
    'X-Goog-Api-Client': `gl-node/${process.versions.node}`,
    'Client-Metadata': JSON.stringify({
      ideType: 'IDE_UNSPECIFIED',
      platform: 'PLATFORM_UNSPECIFIED',
      pluginType: 'GEMINI',
    }),
  };
}

function resolveEndpoints(config = {}) {
  const envEndpoints = process.env.AGY_PULSE_ENDPOINTS || process.env.AGY_HUD_ENDPOINTS;
  if (envEndpoints) {
    return envEndpoints.split(',').map(s => s.trim()).filter(Boolean);
  }
  return (config && config.endpoints) || DEFAULT_ENDPOINTS;
}

function escapeCurlConfigValue(value) {
  return String(value ?? '')
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\r/g, '\\r')
    .replace(/\n/g, '\\n')
    .replace(/\t/g, '\\t');
}

function buildCurlConfig(url, { method = 'GET', headers = {}, body, timeoutMs = 5000 } = {}) {
  const maxTimeSec = Math.max(1, Math.ceil((Number(timeoutMs) || 5000) / 1000));
  const lines = [
    `url = "${escapeCurlConfigValue(url)}"`,
    `request = "${escapeCurlConfigValue(String(method || 'GET').toUpperCase())}"`,
    `max-time = "${maxTimeSec}"`,
    'write-out = "\\n%{http_code}"',
  ];
  for (const [k, v] of Object.entries(headers || {})) {
    if (v === undefined || v === null) continue;
    lines.push(`header = "${escapeCurlConfigValue(`${k}: ${v}`)}"`);
  }
  if (body !== undefined && body !== null) {
    lines.push(`data = "${escapeCurlConfigValue(body)}"`);
  }
  return `${lines.join('\n')}\n`;
}

function curlFetchJson(url, options = {}) {
  const curlPath = options._curlPath !== undefined ? options._curlPath : resolveSafeExecutable('curl');
  if (!curlPath) {
    return Promise.reject(new Error('curl executable not found'));
  }
  const execImpl = options._execFile || execFile;
  const timeoutMs = Number(options.timeoutMs) > 0 ? Number(options.timeoutMs) : 5000;
  const configText = buildCurlConfig(url, options);

  return new Promise((resolve, reject) => {
    const child = execImpl(
      curlPath,
      ['-sS', '-K', '-'],
      {
        encoding: 'utf8',
        timeout: timeoutMs + 1000,
        maxBuffer: 4 * 1024 * 1024,
      },
      (err, stdout) => {
        if (err) {
          reject(err);
          return;
        }
        const raw = String(stdout || '').replace(/\r?\n$/, '');
        const lastNl = raw.lastIndexOf('\n');
        const statusStr = lastNl >= 0 ? raw.slice(lastNl + 1).trim() : raw.trim();
        const bodyStr = lastNl >= 0 ? raw.slice(0, lastNl) : '';
        const status = Number.parseInt(statusStr, 10);
        if (!Number.isFinite(status) || status <= 0) {
          reject(new Error(`curl returned invalid status: ${statusStr}`));
          return;
        }
        const ok = status >= 200 && status < 300;
        let data = null;
        if (bodyStr) {
          try {
            data = JSON.parse(bodyStr);
          } catch (parseErr) {
            if (ok) {
              reject(parseErr);
              return;
            }
          }
        } else if (ok) {
          data = {};
        }
        resolve({ ok, status, data });
      }
    );
    if (child && child.stdin) {
      child.stdin.end(configText);
    }
  });
}

async function cloudFetchJson(url, options = {}) {
  const {
    method = 'GET',
    headers = {},
    body,
    timeoutMs = 5000,
    _forceNativeFallback = false,
  } = options;

  const isNativeFetch = _forceNativeFallback || globalThis.fetch === NATIVE_FETCH;
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const fetchOpts = {
      method,
      headers,
      signal: controller.signal,
    };
    if (body !== undefined) {
      fetchOpts.body = body;
    }
    const r = await globalThis.fetch(url, fetchOpts);
    if (!r.ok) {
      return { ok: false, status: r.status || 0, data: null };
    }
    const data = await r.json();
    return { ok: true, status: r.status || 200, data };
  } catch (err) {
    clearTimeout(timeoutId);
    if (!isNativeFetch) {
      throw err;
    }
    return curlFetchJson(url, { ...options, method, headers, body, timeoutMs });
  } finally {
    clearTimeout(timeoutId);
  }
}

/**
 * Extract a human-readable tier name from a loadCodeAssist response.
 * Priority: paidTier.name > first non-free allowedTier name > null
 * @param {Object} data
 * @returns {string|null}
 */
function extractTierName(data) {
  if (data.paidTier && data.paidTier.name) return data.paidTier.name;
  const nonFree = (data.allowedTiers || []).find(t => t.id !== 'free-tier');
  if (nonFree && nonFree.name) return nonFree.name;
  if (data.allowedTiers && data.allowedTiers.length > 0) return data.allowedTiers[0].name;
  return null;
}

// Google OIDC userinfo endpoints. agy resolves the signed-in account from the
// live access token at runtime (it is not persisted to any local file), so this
// is the only authoritative source for the active account email. The OIDC-spec
// host (openidconnect.googleapis.com) is tried first — it is the more reliable
// route in proxied environments; the legacy www host is a fallback.
const USERINFO_ENDPOINTS = [
  'https://openidconnect.googleapis.com/v1/userinfo',
  'https://www.googleapis.com/oauth2/v3/userinfo',
];

/**
 * Resolve the email of the account the live access token belongs to.
 * @param {string} accessToken
 * @returns {Promise<string|null>}
 */
async function fetchAccountEmail(accessToken) {
  for (const endpoint of USERINFO_ENDPOINTS) {
    try {
      const r = await cloudFetchJson(endpoint, {
        method: 'GET',
        headers: { 'Authorization': `Bearer ${accessToken}` },
        timeoutMs: 3000,
      });
      if (!r.ok) continue;
      if (r.data && typeof r.data.email === 'string') return r.data.email;
    } catch {
      /* try next endpoint */
    }
  }
  return null;
}

/**
 * Fetch the user's subscription tier from loadCodeAssist.
 * Returns a display string like "Google AI Pro" or null if unavailable.
 * @param {string} accessToken
 * @returns {Promise<string|null>}
 */
async function fetchTierFromCloud(accessToken, injectedConfig) {
  let config = injectedConfig;
  if (!config) {
    const { loadConfig } = require('../config.js');
    config = await loadConfig().catch(() => ({}));
  }
  const endpoints = resolveEndpoints(config);

  for (const endpoint of endpoints) {
    try {
      const r = await cloudFetchJson(`${endpoint}/v1internal:loadCodeAssist`, {
        method: 'POST',
        headers: buildHeaders(accessToken),
        body: JSON.stringify({
          cloudaicompanionProject: '',
          metadata: {
            ideType: 'IDE_UNSPECIFIED',
            platform: 'PLATFORM_UNSPECIFIED',
            pluginType: 'GEMINI',
          },
        }),
        timeoutMs: 3000,
      });
      if (!r.ok) continue;
      return extractTierName(r.data);
    } catch {
      /* try next endpoint */
    }
  }
  return null;
}

// ─── retrieveUserQuotaSummary helpers ──────────────────────────────────────

function deriveCustomProviderKey(displayName, buckets) {
  const cleanDisplay = String(displayName || '')
    .trim()
    .toLowerCase()
    .replace(/\bmodels?\b/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  if (cleanDisplay) return cleanDisplay;

  for (const b of buckets || []) {
    const bid = String(b?.bucketId || '')
      .trim()
      .toLowerCase()
      .replace(/[-_](weekly|week|5h|fivehour|five_hour|7d)$/i, '')
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '');
    if (bid) return bid;
  }
  return 'custom';
}

function classifySummaryBucketWindow(bucket) {
  const bucketId = String(bucket?.bucketId || '').toLowerCase();
  const windowRaw = String(bucket?.window || '').toLowerCase();
  if (
    windowRaw === 'weekly' ||
    windowRaw === 'week' ||
    windowRaw === '7d' ||
    bucketId.includes('weekly') ||
    bucketId.includes('week')
  ) {
    return 'weekly';
  }
  if (
    windowRaw === '5h' ||
    windowRaw === 'fivehour' ||
    windowRaw === 'five_hour' ||
    bucketId.includes('5h') ||
    bucketId.includes('fivehour') ||
    bucketId.includes('five_hour')
  ) {
    return 'fiveHour';
  }
  return classifyQuotaWindow(bucket?.resetTime);
}

/**
 * Parse the retrieveUserQuotaSummary response into provider-level windows.
 * Expected shape: { groups: [{ displayName, buckets: [{ bucketId, window, remainingFraction, resetTime }] }] }
 * Handles both bucketId (gemini-weekly, 3p-5h) and window (weekly, 5h) signals.
 * @param {Object} data
 * @returns {Object|null}
 */
function parseQuotaSummary(data) {
  if (!data || !Array.isArray(data.groups)) return null;
  const result = { google: {}, claude: {}, argon: {} };
  let hasAny = false;
  for (const group of data.groups) {
    if (!group || !Array.isArray(group.buckets)) continue;
    const buckets = group.buckets;
    const displayName = (group.displayName || '').toLowerCase();
    const bucketIds = buckets
      .map(b => (b && typeof b.bucketId === 'string' ? b.bucketId.toLowerCase() : ''))
      .filter(Boolean);

    const hasArgonBucket = bucketIds.some(id => id.includes('argon'));
    const hasGeminiBucket = bucketIds.some(id =>
      (id.startsWith('gemini-') || id.includes('carbon') || id.includes('hydrogen') || id.includes('orchestrator')) &&
      !id.includes('argon')
    );
    const has3pBucket = bucketIds.some(id =>
      id.startsWith('3p-') || id.includes('claude') || id.includes('gpt-oss') || id.includes('gpt')
    );

    let target = null;
    if (displayName.includes('argon') || hasArgonBucket) {
      target = 'argon';
    } else if (
      displayName.includes('gemini') ||
      displayName.includes('carbon') ||
      displayName.includes('hydrogen') ||
      displayName.includes('orchestrator') ||
      hasGeminiBucket
    ) {
      target = 'google';
    } else if (
      displayName.includes('claude') ||
      displayName.includes('3p') ||
      displayName.includes('gpt-oss') ||
      displayName.includes('gpt') ||
      has3pBucket
    ) {
      target = 'claude';
    } else {
      target = deriveCustomProviderKey(displayName, buckets);
      if (!result[target]) result[target] = {};
    }

    for (const b of buckets) {
      if (!b || typeof b.resetTime !== 'string') continue;
      const ts = Date.parse(b.resetTime);
      if (!Number.isFinite(ts)) continue;
      const winType = classifySummaryBucketWindow(b);
      if (!winType) continue;
      // Proto3 may omit a default 0 remainingFraction.
      let rf = typeof b.remainingFraction === 'number' ? b.remainingFraction : 0;
      if (!Number.isFinite(rf) || rf < 0) rf = 0;
      if (rf > 1) rf = 1;

      const existing = result[target][winType];
      if (!existing || rf < existing.remainingFraction) {
        result[target][winType] = {
          remainingFraction: rf,
          resetTime: b.resetTime,
        };
        hasAny = true;
      }
    }
  }
  if (!hasAny) return null;
  for (const key of Object.keys(result)) {
    if (!result[key].weekly && !result[key].fiveHour) {
      delete result[key];
    }
  }
  if (Object.keys(result).length === 0) return null;
  return result;
}

/**
 * Enrich normalized models with provider-level weekly/5h windows from summary.
 * Provider mapping: GOOGLE/gemini/carbon/hydrogen/orchestrator -> google summary,
 * ANTHROPIC/claude/gpt -> claude summary, ARGON -> argon summary, or matching custom keys.
 * @param {Array} models
 * @param {Object|null} summary
 * @param {number} now
 * @returns {Array}
 */
function enrichModelsWithQuotaSummary(models, summary, now = Date.now()) {
  if (!Array.isArray(models) || !summary || typeof summary !== 'object') return models;
  const validSummaryKeys = Object.keys(summary).filter(
    k => summary[k] && (summary[k].weekly || summary[k].fiveHour)
  );
  if (validSummaryKeys.length === 0) return models;

  return models.map(m => {
    if (!m || !m.id) return m;
    const idAndName = `${m.id} ${m.displayName || ''}`.toLowerCase();
    const isArgon = /argon/i.test(idAndName);
    const isGoogle = !isArgon && (
      m.modelProvider === 'MODEL_PROVIDER_GOOGLE' ||
      /gemini|carbon|hydrogen|orchestrator/i.test(idAndName)
    );
    const isClaude = m.modelProvider === 'MODEL_PROVIDER_ANTHROPIC' || /claude|gpt/i.test(idAndName);
    let key = isArgon ? 'argon' : isGoogle ? 'google' : isClaude ? 'claude' : null;
    if (!key) {
      key = validSummaryKeys.find(k =>
        k !== 'google' && k !== 'claude' && k !== 'argon' && idAndName.includes(k.toLowerCase())
      ) || null;
    }
    if (!key) return m;
    const providerSummary = summary[key];
    if (!providerSummary) return m;
    const windows = { ...(m.windows || {}) };
    if (providerSummary.weekly) {
      windows.weekly = {
        remainingFraction: providerSummary.weekly.remainingFraction,
        resetTime: providerSummary.weekly.resetTime,
        observedAt: now,
      };
    }
    if (providerSummary.fiveHour) {
      windows.fiveHour = {
        remainingFraction: providerSummary.fiveHour.remainingFraction,
        resetTime: providerSummary.fiveHour.resetTime,
        observedAt: now,
      };
    }
    return { ...m, windows };
  });
}

/**
 * Fetch provider-level quota summary (weekly + 5h) from cloud.
 * Non-critical: returns null on any failure to allow graceful downgrade.
 * @param {string} accessToken
 * @param {Object} [injectedConfig] optional pre-loaded config to avoid double load
 * @returns {Promise<{google:Object, claude:Object}|null>}
 */
async function fetchUserQuotaSummaryFromCloud(accessToken, injectedConfig) {
  let config = injectedConfig;
  if (!config) {
    const { loadConfig } = require('../config.js');
    config = await loadConfig().catch(() => ({}));
  }
  const endpoints = resolveEndpoints(config);
  for (const endpoint of endpoints) {
    try {
      const r = await cloudFetchJson(`${endpoint}/v1internal:retrieveUserQuotaSummary`, {
        method: 'POST',
        headers: buildHeaders(accessToken),
        body: JSON.stringify({}),
        timeoutMs: 5000,
      });
      if (!r.ok) continue;
      const parsed = parseQuotaSummary(r.data);
      if (parsed) return parsed;
      // Response ok but unparsable -> try next endpoint
      continue;
    } catch {
      /* try next endpoint */
    }
  }
  return null;
}

/**
 * Fetch quota data from the cloud API.
 * loadCodeAssist no longer returns cloudaicompanionProject; fetchAvailableModels
 * accepts an empty body and returns quota directly.
 * Now also enriches with retrieveUserQuotaSummary for accurate weekly windows.
 * @param {string} accessToken
 * @returns {Promise<ModelQuota[]>}
 */
async function fetchQuotaFromCloud(accessToken, injectedConfig) {
  let sawAuthFailure = false;
  let config = injectedConfig;
  if (!config) {
    const { loadConfig } = require('../config.js');
    config = await loadConfig().catch(() => ({}));
  }

  const endpoints = resolveEndpoints(config);

  const envModelsRaw = process.env.AGY_PULSE_INTERESTING_MODELS || process.env.AGY_HUD_INTERESTING_MODELS;
  const envModelIds = envModelsRaw
    ? envModelsRaw.split(',').map(s => s.trim()).filter(Boolean)
    : null;

  // Kick off summary fetch in parallel — non-blocking, graceful fallback on failure.
  const summaryPromise = fetchUserQuotaSummaryFromCloud(accessToken, config).catch(() => null);

  let normalized = null;
  for (const endpoint of endpoints) {
    try {
      const r = await cloudFetchJson(`${endpoint}/v1internal:fetchAvailableModels`, {
        method: 'POST',
        headers: buildHeaders(accessToken),
        body: JSON.stringify({}),
        timeoutMs: 5000,
      });
      if (!r.ok) {
        if (r.status === 401 || r.status === 403) sawAuthFailure = true;
        continue;
      }
      const data = r.data || {};
      const models = data.models || {};
      let interestingModelIds = envModelIds
        || config.interestingModels
        || resolveDeprecatedIds(
            discoverAgentModelIds(data) || FALLBACK_AGENT_MODEL_IDS,
            data
          );
      if (data && data.models) {
        const imageModelIds = Object.keys(data.models).filter(id => id.includes('-image') || id.toLowerCase().includes('image'));
        interestingModelIds = [...new Set([...interestingModelIds, ...imageModelIds])];
      }
      normalized = normalizeQuotaModels(models, interestingModelIds);
      break;
    } catch {
      /* try next endpoint */
    }
  }
  if (!normalized) {
    return createUnavailableQuotaResult(sawAuthFailure ? 'auth_failed' : 'quota_fetch_failed');
  }
  // Enrich with summary if available; never fail the whole call if summary fetch errors.
  try {
    const summary = await summaryPromise;
    if (summary) {
      normalized = enrichModelsWithQuotaSummary(normalized, summary, Date.now());
      normalized.providerQuota = summary;
    }
  } catch {
    /* graceful downgrade: return models-only */
  }
  return normalized;
}

module.exports = {
  DEFAULT_ENDPOINTS,
  buildHeaders,
  resolveEndpoints,
  extractTierName,
  fetchAccountEmail,
  fetchTierFromCloud,
  fetchQuotaFromCloud,
  // new helpers exported for testing / reuse
  parseQuotaSummary,
  enrichModelsWithQuotaSummary,
  fetchUserQuotaSummaryFromCloud,
  cloudFetchJson,
  buildCurlConfig,
};
