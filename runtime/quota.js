/**
 * quota.js — Real account-level quota fetcher (orchestrator).
 *
 * Calls the same `fetchAvailableModels` endpoint that agy uses for /usage.
 * Token is auto-discovered from known agy app-data locations across platforms.
 * Results are cached via resolveAntigravityPath('agy-pulse-quota-cache.json'). The cache key is
 * the stable credential source when available, so access-token rotation does
 * not hide a still-fresh quota cache from the statusline.
 *
 * Submodules:
 *   ./quota/token.js  — OAuth file + Windows Credential Manager + temp mirror
 *   ./quota/cache.js  — atomic cache read/write, token-keyed match
 *   ./quota/cloud.js  — fetchAvailableModels + loadCodeAssist HTTP calls
 *   ./quota/models.js — model-id discovery, deprecation map, quota normalize
 */

'use strict';

const path = require('path');
const { spawn } = require('child_process');
const { getAntigravityRoots } = require('./paths.js');

const tokenMod = require('./quota/token.js');
const cacheMod = require('./quota/cache.js');
const cloudMod = require('./quota/cloud.js');
const modelsMod = require('./quota/models.js');

const {
  readToken,
  readWindowsCredentialTokens,
  isTokenExpired,
  anyTokenFileExists,
  anyTokenMirrorExists,
} = tokenMod;
const {
  CACHE_PATH,
  isCachePayloadFresh,
  readCachePayload,
  readCacheRaw,
  readCacheLastRefreshed,
  readCacheLastRefreshAttempt,
  readCacheFallback,
  readAnyCachePayload,
  didAccessTokenRotate,
  getTokenHash,
  writeCache,
  recordRefreshAttempt,
  markCacheRefreshAttempt,
} = cacheMod;
const { fetchQuotaFromCloud, fetchTierFromCloud, fetchAccountEmail } = cloudMod;
const { createUnavailableQuotaResult } = modelsMod;

const WINDOWS_CREDENTIAL_REFRESH_DEBOUNCE_MS = 30 * 1000;
let lastWindowsCredentialRefreshAt = 0;

function triggerBackgroundRefresh() {
  try {
    const subprocess = spawn(process.execPath, [
      path.join(__dirname, 'quota.js'),
      '--refresh',
    ], {
      detached: true,
      stdio: 'ignore',
      env: {
        ...process.env,
        NODE_USE_ENV_PROXY: process.env.NODE_USE_ENV_PROXY || '1',
      },
    });
    subprocess.unref();
  } catch { /* ignore spawning issues */ }
}

function triggerWindowsCredentialRefresh(backgroundRefresh, now, debounceMs, cacheOpts = {}) {
  if (debounceMs > 0) {
    if (now - lastWindowsCredentialRefreshAt < debounceMs) return;
    const diskLastAttempt = readCacheLastRefreshAttempt(cacheOpts);
    if (now - diskLastAttempt < debounceMs) return;
  }
  lastWindowsCredentialRefreshAt = now;
  markCacheRefreshAttempt(null, cacheOpts);
  backgroundRefresh();
}

/**
 * Get quota data using a non-blocking Stale-While-Revalidate pattern.
 * @returns {Promise<ModelQuota[]>}
 */
async function getQuota(options = {}) {
  const {
    fast = false,
    platform = process.platform,
    tokenReader = readToken,
    backgroundRefresh = triggerBackgroundRefresh,
    credentialReader = readWindowsCredentialTokens,
    roots = getAntigravityRoots(),
    windowsCredentialRefreshDebounceMs = WINDOWS_CREDENTIAL_REFRESH_DEBOUNCE_MS,
  } = options;
  const cacheOpts = options.roots ? { roots: options.roots } : {};
  const shouldRefreshPlatformCredential = fast && (platform === 'win32' || platform === 'linux');
  const refreshPlatformCredential = () => {
    if (!shouldRefreshPlatformCredential) return;
    triggerWindowsCredentialRefresh(
      backgroundRefresh,
      Date.now(),
      windowsCredentialRefreshDebounceMs,
      cacheOpts
    );
  };
  const tok = tokenReader({
    fast,
    platform,
    roots,
    credentialReader,
    skipWindowsCredential: fast && platform === 'win32',
    skipLinuxKeyring: fast && platform === 'linux',
  });
  if (!tok) {
    // Token file exists but failed to parse → transient (OAuth mid-refresh).
    // Return fresh cache rather than flashing "not logged in".
    if (anyTokenFileExists(roots)) {
      const fallback = readCacheFallback(cacheOpts);
      if (fallback && isCachePayloadFresh(fallback)) {
        return fallback.data;
      }
      refreshPlatformCredential();
      return createUnavailableQuotaResult('not_logged_in');
    }

    // On keyring/credential platforms in fast mode, an expired mirror returns
    // tok === null even when the user is still logged in. Serve non-empty quota
    // cache (or loading [] if an expired mirror exists) while refreshing in background.
    if (fast) {
      const anyCache = readAnyCachePayload(
        options.roots ? { roots: options.roots, strictRoots: true } : {}
      );
      if (anyCache) {
        triggerWindowsCredentialRefresh(
          backgroundRefresh,
          Date.now(),
          windowsCredentialRefreshDebounceMs,
          cacheOpts
        );
        return anyCache.data;
      }
      if (anyTokenMirrorExists(roots)) {
        triggerWindowsCredentialRefresh(
          backgroundRefresh,
          Date.now(),
          windowsCredentialRefreshDebounceMs,
          cacheOpts
        );
        return [];
      }
    }

    refreshPlatformCredential();
    return createUnavailableQuotaResult('not_logged_in');
  }

  // For multi-account (Windows Credential Manager), use the primary token for
  // cache keying but fall back to alternates if the primary has no cache.
  let payload = readCachePayload(tok, cacheOpts) ||
    (tok.all && tok.all.slice(1).reduce((acc, t) => acc || readCachePayload(t, cacheOpts), null));
  if (
    payload &&
    tok.email &&
    payload.accountEmail &&
    tok.email.toLowerCase() !== payload.accountEmail.toLowerCase()
  ) {
    payload = null;
  }
  const isFresh = payload && isCachePayloadFresh(payload);
  const tokenExpired = isTokenExpired(tok);
  const needsRefresh = !tokenExpired && (!isFresh || didAccessTokenRotate(payload, tok));

  const fallback = payload ? null : readCacheFallback(cacheOpts);
  const fallbackEmailMismatch = Boolean(
    tok.email &&
    fallback?.accountEmail &&
    tok.email.toLowerCase() !== fallback.accountEmail.toLowerCase()
  );

  if (needsRefresh) {
    // Read lastAttempt from cache even when payload doesn't match our token,
    // to prevent a storm of concurrent background refresh processes.
    const lastAttempt = payload
      ? Math.max(payload.lastRefreshed || 0, payload.lastRefreshAttempt || 0)
      : readCacheLastRefreshAttempt(cacheOpts);
    const currentTokenHash = getTokenHash(tok);
    const attemptSource = payload || fallback;
    const sameAttemptToken = Boolean(
      attemptSource?.lastAttemptTokenHash &&
      currentTokenHash &&
      attemptSource.lastAttemptTokenHash === currentTokenHash
    );
    const now = Date.now();
    // Debounce stale/no-cache refreshes, but refresh immediately when:
    //  1) a fresh cache belongs to the same source and only the access token rotated, or
    //  2) an explicit account email switch is detected across unmatched sources
    // (unless we already attempted and failed on that exact token within 30s).
    const shouldBypassDebounce = payload
      ? (didAccessTokenRotate(payload, tok) && !sameAttemptToken)
      : (fallbackEmailMismatch && !(sameAttemptToken && now - lastAttempt <= 30 * 1000));
    if (shouldBypassDebounce || now - lastAttempt > 30 * 1000) {
      backgroundRefresh();
    }
  }

  if (payload) {
    if (payload.data.length === 0 && payload.unavailableReason) {
      if (tokenExpired) {
        refreshPlatformCredential();
        return createUnavailableQuotaResult('expired_token');
      }
      return createUnavailableQuotaResult(payload.unavailableReason);
    }
    if (tokenExpired) {
      refreshPlatformCredential();
      if (!isFresh) {
        return createUnavailableQuotaResult('expired_token');
      }
    }
    return payload.data;
  }

  if (tokenExpired) {
    refreshPlatformCredential();
    return createUnavailableQuotaResult('expired_token');
  }

  // Fallback to any readable cache payload on disk to prevent "Quota loading" flicker
  // while we perform a background refresh for the current token — unless the cache
  // explicitly belongs to a different account email.
  if (fallback && !fallbackEmailMismatch) {
    if (fallback.data.length === 0 && fallback.unavailableReason) {
      return createUnavailableQuotaResult(fallback.unavailableReason);
    }
    return fallback.data;
  }

  // If no cache exists at all, return empty. Non-fast callers trigger the
  // refresh above; statusline fast path stays bounded and never waits on it.
  return [];
}

/**
 * Persists a background quota refresh result without overwriting existing valid
 * cached data with an empty array when the cloud fetch fails.
 */
function applyRefreshResult(fresh, tok, tier = null, accountEmail = null, options = {}) {
  if (!tok) return;
  if (fresh && fresh.unavailableReason === 'auth_failed') {
    tokenMod.clearTokenTemp(options.roots);
    recordRefreshAttempt(tok, 'auth_failed', options);
    return;
  }
  if (Array.isArray(fresh) && fresh.length > 0) {
    writeCache(fresh, tok, tier, accountEmail, fresh.providerQuota, options);
    return;
  }
  const reason = (fresh && fresh.unavailableReason) || 'quota_fetch_failed';
  recordRefreshAttempt(tok, reason, { ...options, tier, accountEmail });
}

async function runBackgroundRefresh(options = {}) {
  const {
    tokenReader = readToken,
    loadConfigFn,
    quotaFetcher = fetchQuotaFromCloud,
    tierFetcher = fetchTierFromCloud,
    emailFetcher = fetchAccountEmail,
    cacheOpts = {},
  } = options;
  try {
    const tok = tokenReader();
    if (!tok || isTokenExpired(tok)) {
      markCacheRefreshAttempt(tok || null, cacheOpts);
      return;
    }
    const loadCfg = loadConfigFn || require('./config.js').loadConfig;
    const config = await loadCfg().catch(() => ({}));
    const [fresh, tier, accountEmail] = await Promise.all([
      quotaFetcher(tok.accessToken, config),
      tierFetcher(tok.accessToken, config),
      emailFetcher(tok.accessToken),
    ]);
    applyRefreshResult(fresh, tok, tier, accountEmail, cacheOpts);
  } catch {
    try { markCacheRefreshAttempt(null, cacheOpts); } catch {}
  }
}

// ─── CLI Execution for background refreshes ──────────────────────────────────
if (process.argv.includes('--refresh')) {
  runBackgroundRefresh().finally(() => {
    process.exit(0);
  });
}

module.exports = {
  // orchestrator
  getQuota,
  applyRefreshResult,
  runBackgroundRefresh,
  // cache module
  CACHE_PATH,
  isCachePayloadFresh,
  readCache: cacheMod.readCache,
  writeCache,
  recordRefreshAttempt,
  markCacheRefreshAttempt,
  readCachePayload,
  readCacheRaw,
  readCacheLastRefreshed,
  readCacheLastRefreshAttempt,
  readCacheFallback,
  readAnyCachePayload,
  getCachedTier: cacheMod.getCachedTier,
  getCachedAccountEmail: cacheMod.getCachedAccountEmail,
  // token module
  readToken,
  clearTokenTemp: tokenMod.clearTokenTemp,
  readWindowsCredentialTokens,
  readLinuxKeyringTokens: tokenMod.readLinuxKeyringTokens,
  probeLinuxKeyringAvailability: tokenMod.probeLinuxKeyringAvailability,
  readMacosKeychainTokens: tokenMod.readMacosKeychainTokens,
  probeMacosKeychainAvailability: tokenMod.probeMacosKeychainAvailability,
  isTokenExpired,
  getTokenCandidates: tokenMod.getTokenCandidates,
  getGoogleAccountsCandidates: tokenMod.getGoogleAccountsCandidates,
  parseTokenPayload: tokenMod.parseTokenPayload,
  selectUsableTokens: tokenMod.selectUsableTokens,
  anyTokenFileExists,
  anyTokenMirrorExists,
  // cloud module
  fetchQuotaFromCloud,
  fetchTierFromCloud,
  fetchAccountEmail,
  extractTierName: cloudMod.extractTierName,
  parseQuotaSummary: cloudMod.parseQuotaSummary,
  enrichModelsWithQuotaSummary: cloudMod.enrichModelsWithQuotaSummary,
  fetchUserQuotaSummaryFromCloud: cloudMod.fetchUserQuotaSummaryFromCloud,
  cloudFetchJson: cloudMod.cloudFetchJson,
  buildCurlConfig: cloudMod.buildCurlConfig,
  // models module
  MODEL_DISPLAY_NAMES: modelsMod.MODEL_DISPLAY_NAMES,
  FALLBACK_AGENT_MODEL_IDS: modelsMod.FALLBACK_AGENT_MODEL_IDS,
  resolveModelDisplayName: modelsMod.resolveModelDisplayName,
  normalizeQuotaModels: modelsMod.normalizeQuotaModels,
  discoverAgentModelIds: modelsMod.discoverAgentModelIds,
  resolveDeprecatedIds: modelsMod.resolveDeprecatedIds,
  classifyQuotaWindow: modelsMod.classifyQuotaWindow,
  isObservationExpired: modelsMod.isObservationExpired,
  pruneExpiredWindows: modelsMod.pruneExpiredWindows,
  mergeQuotaWindows: modelsMod.mergeQuotaWindows,
  pickCriticalWindow: modelsMod.pickCriticalWindow,
  FIVE_HOUR_WINDOW_THRESHOLD_MS: modelsMod.FIVE_HOUR_WINDOW_THRESHOLD_MS,
  createUnavailableQuotaResult,
};
