'use strict';

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { resolveAntigravityPath } = require('../paths.js');
const { mergeQuotaWindows } = require('./models.js');

const CACHE_FILENAME = 'agy-hud-quota-cache.json';
const CACHE_PATH = resolveAntigravityPath(CACHE_FILENAME);
const CACHE_VERSION = 3;

function getReadCachePath(options = {}) {
  const roots = options?.roots;
  if (Array.isArray(roots) && roots.length > 0) {
    for (const root of roots) {
      if (!root) continue;
      const candidate = path.join(root, CACHE_FILENAME);
      if (fs.existsSync(candidate)) return candidate;
    }
  }
  return resolveAntigravityPath(CACHE_FILENAME);
}

function getWriteCachePath(options = {}) {
  const roots = options?.roots;
  if (Array.isArray(roots) && roots.length > 0) {
    for (const root of roots) {
      if (!root) continue;
      const candidate = path.join(root, CACHE_FILENAME);
      if (fs.existsSync(candidate)) return candidate;
    }
    const firstValid = roots.find(Boolean);
    if (firstValid) return path.join(firstValid, CACHE_FILENAME);
  }
  return resolveAntigravityPath(CACHE_FILENAME);
}

function writeCacheFileAtomic(cachePath, payload) {
  const tmpPath = `${cachePath}.tmp.${process.pid}.${Date.now()}`;
  try {
    fs.mkdirSync(path.dirname(cachePath), { recursive: true });
    fs.writeFileSync(tmpPath, JSON.stringify(payload), { mode: 0o600 });
    fs.renameSync(tmpPath, cachePath);
  } catch {
    try { fs.unlinkSync(tmpPath); } catch {}
  }
}

function isCachePayloadFresh(raw) {
  return raw &&
    raw.version === CACHE_VERSION &&
    raw.expiresAt &&
    Date.now() < raw.expiresAt &&
    Array.isArray(raw.data);
}

function hashCacheKey(value) {
  return crypto.createHash('sha256').update(value).digest('hex');
}

function normalizeTokenCacheInput(tokenOrAccessToken) {
  if (typeof tokenOrAccessToken === 'string') {
    return { accessToken: tokenOrAccessToken };
  }
  if (!tokenOrAccessToken || typeof tokenOrAccessToken !== 'object') {
    return null;
  }
  return tokenOrAccessToken;
}

function getTokenCacheIdentity(tokenOrAccessToken) {
  const token = normalizeTokenCacheInput(tokenOrAccessToken);
  if (!token) return null;

  if (token.sourcePath) {
    return `sourcePath:${path.resolve(token.sourcePath)}`;
  }

  if (token.sourceFormat) {
    return `sourceFormat:${token.sourceFormat}`;
  }

  if (token.accessToken) {
    return `accessToken:${token.accessToken}`;
  }

  return null;
}

function getTokenHash(tokenOrAccessToken) {
  const token = normalizeTokenCacheInput(tokenOrAccessToken);
  if (!token || !token.accessToken) return null;
  return hashCacheKey(token.accessToken);
}

function getTokenCacheKeyHash(tokenOrAccessToken) {
  const identity = getTokenCacheIdentity(tokenOrAccessToken);
  return identity ? hashCacheKey(identity) : null;
}

function doesCachePayloadMatchToken(raw, tokenOrAccessToken) {
  if (!raw || !Array.isArray(raw.data)) return false;

  const cacheKeyHash = getTokenCacheKeyHash(tokenOrAccessToken);
  if (raw.cacheKeyHash && cacheKeyHash && raw.cacheKeyHash === cacheKeyHash) {
    return true;
  }

  const tokenHash = getTokenHash(tokenOrAccessToken);
  return Boolean(raw.tokenHash && tokenHash && raw.tokenHash === tokenHash);
}

function didAccessTokenRotate(raw, tokenOrAccessToken) {
  const tokenHash = getTokenHash(tokenOrAccessToken);
  return Boolean(raw && raw.tokenHash && tokenHash && raw.tokenHash !== tokenHash);
}

function attachCacheMetadata(raw) {
  if (!raw || !Array.isArray(raw.data)) return raw;
  if (raw.providerQuota) {
    raw.data.providerQuota = raw.providerQuota;
  }
  if (raw.data.length === 0 && raw.unavailableReason) {
    Object.defineProperty(raw.data, 'unavailableReason', {
      value: raw.unavailableReason,
      enumerable: false,
      configurable: true,
    });
  }
  return raw;
}

/**
 * Read cached quota if still valid.
 * @param {string|Object} tokenOrAccessToken
 * @param {Object} [options]
 * @returns {ModelQuota[] | null}
 */
function readCache(tokenOrAccessToken, options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    if (!isCachePayloadFresh(raw)) return null;
    if (!doesCachePayloadMatchToken(raw, tokenOrAccessToken)) return null;
    attachCacheMetadata(raw);
    return raw.data;
  } catch {
    return null;
  }
}

/**
 * Read the previous cache payload (any token) for merging the other window's
 * last observation. Tier and per-window state are account-level, so we
 * intentionally skip the token-match check used by readCache.
 */
function readCacheRaw(options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    if (!raw || !Array.isArray(raw.data)) return null;
    return raw;
  } catch {
    return null;
  }
}

/**
 * Write quota cache. Expires at the earliest resetTime among all buckets.
 * Uses atomic write (tmp + rename) to prevent concurrent readers from seeing
 * truncated/empty content — the main fix for statusline quota flicker.
 * Merges with the previous payload so the window not present in this response
 * (e.g. 5-hour when the API returns weekly) keeps its last observation, but
 * only when the cache belongs to the same credential identity (token rotation
 * is fine, account switch is not).
 */
function writeCache(data, tokenOrAccessToken, tier = null, accountEmail = null, providerQuota = null, options = {}) {
  const now = Date.now();
  const cachePath = getWriteCachePath(options);
  const previousRaw = readCacheRaw(options);
  let sameIdentity = previousRaw && doesCachePayloadMatchToken(previousRaw, tokenOrAccessToken);
  // The OAuth token file path is shared across accounts on one machine, so an
  // access-token rotation (same account) and an account switch look identical
  // at the token level (same cacheKeyHash, different tokenHash). The account
  // email disambiguates them: when the previous and the fresh email are both
  // known and differ, this is a switch — treat it as a new identity so the
  // previous account's windows / tier / email cannot leak into this one.
  if (sameIdentity && accountEmail && previousRaw.accountEmail &&
      accountEmail !== previousRaw.accountEmail) {
    sameIdentity = false;
  }
  const previousData = sameIdentity ? previousRaw.data : [];
  const incomingProviderQuota = providerQuota || data?.providerQuota || null;
  let merged = mergeQuotaWindows(data, previousData, now);
  // Guard against empty fresh arrays accidentally wiping a valid cached array.
  if (Array.isArray(data) && data.length === 0 && !incomingProviderQuota && sameIdentity && previousData.length > 0 && merged.length === 0) {
    merged = previousData;
  }

  // Find earliest resetTime across top-level AND each merged window — a
  // preserved 5-hour observation can reset well before the weekly top-level,
  // and we must refresh by then or risk serving a stale fiveHour.
  let earliest = Infinity;
  const considerResetTime = (value) => {
    if (!value) return;
    const t = new Date(value).getTime();
    if (Number.isFinite(t) && t < earliest) earliest = t;
  };
  for (const m of merged) {
    considerResetTime(m.resetTime);
    considerResetTime(m.windows?.fiveHour?.resetTime);
    considerResetTime(m.windows?.weekly?.resetTime);
  }
  const maxFreshDuration = 2 * 60 * 1000;
  let expiresAt = now + maxFreshDuration;
  if (isFinite(earliest) && earliest < expiresAt) {
    expiresAt = earliest;
  }
  const cacheKeyHash = getTokenCacheKeyHash(tokenOrAccessToken);
  const tokenHash = getTokenHash(tokenOrAccessToken);
  // Preserve the previously cached tier when the caller passes null and the
  // cache belongs to the same identity — fetchTierFromCloud transient failures
  // must not wipe 'Google AI Pro' down to 'Free'.
  const resolvedTier = tier || (sameIdentity ? (previousRaw.tier || null) : null);
  const resolvedEmail = accountEmail || (sameIdentity ? (previousRaw.accountEmail || null) : null);
  const previousProviderQuota = sameIdentity && previousRaw?.providerQuota ? previousRaw.providerQuota : null;
  const resolvedProviderQuota = incomingProviderQuota
    ? (previousProviderQuota ? { ...previousProviderQuota, ...incomingProviderQuota } : incomingProviderQuota)
    : (previousProviderQuota || null);

  // Re-read disk right before final write to reduce concurrent overwrite loss.
  const latestRaw = readCacheRaw(options);
  if (latestRaw && latestRaw !== previousRaw && latestRaw.lastRefreshed > (previousRaw?.lastRefreshed || 0)) {
    const latestSame = doesCachePayloadMatchToken(latestRaw, tokenOrAccessToken) &&
      !(accountEmail && latestRaw.accountEmail && accountEmail !== latestRaw.accountEmail);
    if (latestSame) {
      merged = mergeQuotaWindows(merged, latestRaw.data, now);
    }
  }

  const payload = {
    version: CACHE_VERSION,
    expiresAt,
    lastRefreshed: now,
    cacheKeyHash,
    tokenHash,
    tier: resolvedTier,
    accountEmail: resolvedEmail,
    providerQuota: resolvedProviderQuota,
    data: merged,
  };
  writeCacheFileAtomic(cachePath, payload);
}

/**
 * Records a background refresh attempt (especially on failure) so callers
 * do not respawn background processes on every render within the 30s debounce
 * window, and preserves any existing non-empty cached quota data.
 */
function recordRefreshAttempt(tokenOrAccessToken, reason = 'quota_fetch_failed', options = {}) {
  const now = Date.now();
  const cachePath = getWriteCachePath(options);
  const previousRaw = readCacheRaw(options);
  const tokenHash = getTokenHash(tokenOrAccessToken);
  const cacheKeyHash = getTokenCacheKeyHash(tokenOrAccessToken);

  let sameIdentity = previousRaw && doesCachePayloadMatchToken(previousRaw, tokenOrAccessToken);
  if (sameIdentity && options.accountEmail && previousRaw.accountEmail &&
      options.accountEmail !== previousRaw.accountEmail) {
    sameIdentity = false;
  }

  if (previousRaw && previousRaw.version === CACHE_VERSION && previousRaw.data.length > 0) {
    const payload = {
      ...previousRaw,
      lastRefreshAttempt: now,
      lastAttemptTokenHash: tokenHash,
      lastRefreshError: reason || 'quota_fetch_failed',
      ...(sameIdentity && options.tier && !previousRaw.tier ? { tier: options.tier } : {}),
      ...(sameIdentity && options.accountEmail && !previousRaw.accountEmail ? { accountEmail: options.accountEmail } : {}),
    };
    writeCacheFileAtomic(cachePath, payload);
    return payload;
  }

  const payload = {
    version: CACHE_VERSION,
    expiresAt: 0,
    lastRefreshed: 0,
    lastRefreshAttempt: now,
    lastAttemptTokenHash: tokenHash,
    unavailableReason: reason || 'quota_fetch_failed',
    cacheKeyHash,
    tokenHash,
    tier: options.tier || (sameIdentity ? (previousRaw?.tier || null) : null),
    accountEmail: options.accountEmail || (sameIdentity ? (previousRaw?.accountEmail || null) : null),
    providerQuota: null,
    data: [],
  };
  writeCacheFileAtomic(cachePath, payload);
  return payload;
}

/**
 * Read the cached tier name without requiring a token match.
 * Tier is account-level, not token-level, so we skip token matching.
 */
function getCachedTier(options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    return raw.tier || null;
  } catch {
    return null;
  }
}

/**
 * Read the cached active-account email, but ONLY when the cache belongs to the
 * current token. Unlike tier, a wrong email is worse than none: after an account
 * switch the token no longer matches, so we return null (caller falls back) and
 * let the background refresh repopulate it for the new account.
 * @param {string|Object} tokenOrAccessToken
 * @param {Object} [options]
 * @returns {string|null}
 */
function getCachedAccountEmail(tokenOrAccessToken, options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    if (!raw || !raw.accountEmail) return null;
    if (!doesCachePayloadMatchToken(raw, tokenOrAccessToken)) return null;
    return raw.accountEmail;
  } catch {
    return null;
  }
}

function readCachePayload(tokenOrAccessToken, options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    if (!doesCachePayloadMatchToken(raw, tokenOrAccessToken)) return null;
    return attachCacheMetadata(raw);
  } catch {
    return null;
  }
}

// Read lastRefreshed without requiring token match — used to debounce
// background refresh storms when the caller's token doesn't match the cache.
function readCacheLastRefreshed(options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    return raw.lastRefreshed || 0;
  } catch {
    return 0;
  }
}

function readCacheLastRefreshAttempt(options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    return Math.max(raw.lastRefreshed || 0, raw.lastRefreshAttempt || 0);
  } catch {
    return 0;
  }
}

// Return any readable cache payload regardless of token — fallback for
// transient token-read failures to avoid flashing "not logged in".
function readCacheFallback(options = {}) {
  try {
    const raw = JSON.parse(fs.readFileSync(getReadCachePath(options), 'utf8'));
    if (!raw || !Array.isArray(raw.data) || raw.version !== CACHE_VERSION) return null;
    return attachCacheMetadata(raw);
  } catch {
    return null;
  }
}

module.exports = {
  CACHE_FILENAME,
  CACHE_PATH,
  CACHE_VERSION,
  getReadCachePath,
  getWriteCachePath,
  isCachePayloadFresh,
  hashCacheKey,
  getTokenCacheIdentity,
  getTokenHash,
  getTokenCacheKeyHash,
  doesCachePayloadMatchToken,
  didAccessTokenRotate,
  readCache,
  readCacheRaw,
  writeCache,
  recordRefreshAttempt,
  getCachedTier,
  getCachedAccountEmail,
  readCachePayload,
  readCacheLastRefreshed,
  readCacheLastRefreshAttempt,
  readCacheFallback,
};
