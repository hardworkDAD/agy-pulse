import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { quotaModule, CACHE_PATH } from './_helpers/quota-test-utils.mjs';

const { getQuota } = quotaModule;

describe('quota / getQuota orchestrator', () => {
  describe('fast path with cache', () => {
    test('returns cached quota without background refresh', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        const cachedQuota = [{
          id: 'gemini-3-flash-agent',
          displayName: 'Gemini 3.5 Flash (High)',
          remainingFraction: 0.42,
          resetTime: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
        }];
        quotaModule.writeCache(cachedQuota, 'cached-token');

        let refreshes = 0;
        const quota = await getQuota({
          fast: true,
          tokenReader: () => ({ accessToken: 'cached-token' }),
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.deepEqual(quota, [{ ...cachedQuota[0], windows: {} }]);
        assert.equal(refreshes, 0);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('preserves fresh cache when the access token rotates', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        const tokenSourcePath = path.join('same-user', 'antigravity-oauth-token');
        const cachedQuota = [{
          id: 'gemini-3-flash-agent',
          displayName: 'Gemini 3.5 Flash (High)',
          remainingFraction: 0.77,
          resetTime: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
        }];
        quotaModule.writeCache(cachedQuota, {
          accessToken: 'old-access-token',
          sourcePath: tokenSourcePath,
        });

        let refreshes = 0;
        const quota = await getQuota({
          fast: true,
          tokenReader: () => ({
            accessToken: 'new-access-token',
            sourcePath: tokenSourcePath,
          }),
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.deepEqual(quota, [{ ...cachedQuota[0], windows: {} }]);
        assert.equal(refreshes, 1);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('starts background refresh when no cache exists', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let refreshes = 0;

        const quota = await getQuota({
          fast: true,
          tokenReader: () => ({ accessToken: 'uncached-token' }),
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.deepEqual(quota, []);
        assert.equal(refreshes, 1);
      } finally {
        if (previousCache !== null) fs.writeFileSync(CACHE_PATH, previousCache);
        else fs.rmSync(CACHE_PATH, { force: true });
      }
    });
  });

  describe('expired tokens', () => {
    test('reports expired tokens without spawning quota refresh', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let refreshes = 0;

        const quota = await getQuota({
          fast: true,
          platform: 'darwin',
          tokenReader: () => ({
            accessToken: 'expired-token',
            expiry: '2000-01-01T00:00:00.000Z',
            sourcePath: path.join('expired', 'oauth_creds.json'),
          }),
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'expired_token');
        assert.equal(refreshes, 0);
      } finally {
        if (previousCache !== null) fs.writeFileSync(CACHE_PATH, previousCache);
        else fs.rmSync(CACHE_PATH, { force: true });
      }
    });

    test('reports expired_token when token is expired, even if fallback cache exists', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        const payload = {
          version: 3,
          expiresAt: Date.now() + 60000,
          lastRefreshed: Date.now() - 50000,
          cacheKeyHash: 'token-A-hash',
          tokenHash: 'token-A-hash',
          data: [{ id: 'gemini-3-flash-agent', remainingFraction: 0.77 }],
        };
        fs.writeFileSync(CACHE_PATH, JSON.stringify(payload), { mode: 0o600 });

        const result = await getQuota({
          fast: true,
          tokenReader: () => ({
            accessToken: 'token-B',
            sourcePath: '/token/B',
            expiry: new Date(Date.now() - 10000).toISOString(), // expired 10s ago
          }),
          backgroundRefresh: () => {},
        });

        assert.equal(result.unavailableReason, 'expired_token');
        assert.equal(result.length, 0);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });
  });

  describe('Windows Credential Manager refresh', () => {
    test('fast path wakes credential refresh when no token is visible', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let refreshes = 0;

        const quota = await getQuota({
          fast: true,
          platform: 'win32',
          tokenReader: () => null,
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'not_logged_in');
        assert.equal(refreshes, 1);
      } finally {
        if (previousCache !== null) fs.writeFileSync(CACHE_PATH, previousCache);
        else fs.rmSync(CACHE_PATH, { force: true });
      }
    });

    test('fast path wakes credential refresh when file token is expired', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let refreshes = 0;

        const quota = await getQuota({
          fast: true,
          platform: 'win32',
          tokenReader: () => ({
            accessToken: 'expired-file-token',
            expiry: '2000-01-01T00:00:00.000Z',
            sourcePath: path.join('expired', 'oauth_creds.json'),
          }),
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'expired_token');
        assert.equal(refreshes, 1);
      } finally {
        if (previousCache !== null) fs.writeFileSync(CACHE_PATH, previousCache);
        else fs.rmSync(CACHE_PATH, { force: true });
      }
    });
  });

  describe('token-null fallback to fresh cache (with file-existence check)', () => {
    test('returns fresh cache when token file exists but is temporarily unreadable', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-transient-'));
      try {
        const tokenDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(tokenDir, { recursive: true });
        // Token file exists but contains garbage (simulates mid-write)
        fs.writeFileSync(path.join(tokenDir, 'antigravity-oauth-token'), '{corrupt');

        const cachedQuota = [{
          id: 'gemini-3-flash-agent',
          displayName: 'Gemini 3.5 Flash (High)',
          remainingFraction: 0.55,
          resetTime: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
        }];
        quotaModule.writeCache(cachedQuota, 'some-token');

        const quota = await getQuota({
          fast: true,
          tokenReader: () => null,
          backgroundRefresh: () => {},
          roots: [tokenDir],
        });

        assert.deepEqual(quota, [{ ...cachedQuota[0], windows: {} }]);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('reports not_logged_in when genuinely logged out despite fresh cache', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-no-token-'));
      try {
        // Empty root — no token file exists at all
        const emptyDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(emptyDir, { recursive: true });

        const freshPayload = {
          version: 3,
          expiresAt: Date.now() + 60_000,
          lastRefreshed: Date.now(),
          cacheKeyHash: 'abc',
          tokenHash: 'def',
          data: [{ id: 'test', remainingFraction: 0.5, resetTime: null }],
        };
        fs.writeFileSync(CACHE_PATH, JSON.stringify(freshPayload), { mode: 0o600 });

        const quota = await getQuota({
          fast: true,
          tokenReader: () => null,
          backgroundRefresh: () => {},
          roots: [emptyDir],
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'not_logged_in');
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('reports not_logged_in when token is null and cache is expired', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-expired-'));
      try {
        const tokenDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(tokenDir, { recursive: true });
        fs.writeFileSync(path.join(tokenDir, 'antigravity-oauth-token'), '{corrupt');

        const expiredPayload = {
          version: 3,
          expiresAt: Date.now() - 1000,
          lastRefreshed: Date.now() - 600_000,
          cacheKeyHash: 'abc',
          tokenHash: 'def',
          data: [{ id: 'test', remainingFraction: 0.5 }],
        };
        fs.writeFileSync(CACHE_PATH, JSON.stringify(expiredPayload), { mode: 0o600 });

        const quota = await getQuota({
          fast: true,
          tokenReader: () => null,
          backgroundRefresh: () => {},
          roots: [tokenDir],
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'not_logged_in');
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
  });

  describe('refresh dedup', () => {
    test('debounces background refresh using cache lastRefreshed even without token match', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        const recentPayload = {
          version: 3,
          expiresAt: Date.now() - 1000,
          lastRefreshed: Date.now() - 5000,
          cacheKeyHash: 'different-user',
          tokenHash: 'different-token',
          data: [{ id: 'test', remainingFraction: 0.5, resetTime: null }],
        };
        fs.writeFileSync(CACHE_PATH, JSON.stringify(recentPayload), { mode: 0o600 });

        let refreshes = 0;
        await getQuota({
          fast: true,
          tokenReader: () => ({ accessToken: 'new-unmatched-token', sourcePath: '/new/path' }),
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.equal(refreshes, 0, 'should not refresh when cache was recently refreshed');
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('falls back to readCacheFallback and returns data when token is unmatched', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        const payload = {
          version: 3,
          expiresAt: Date.now() + 60000,
          lastRefreshed: Date.now() - 50000,
          cacheKeyHash: 'token-A-hash',
          tokenHash: 'token-A-hash',
          data: [{ id: 'gemini-3-flash-agent', remainingFraction: 0.77 }],
        };
        fs.writeFileSync(CACHE_PATH, JSON.stringify(payload), { mode: 0o600 });

        let refreshes = 0;
        const result = await getQuota({
          fast: true,
          tokenReader: () => ({ accessToken: 'token-B', sourcePath: '/token/B' }),
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.equal(refreshes, 1, 'should trigger background refresh due to token rotation');
        assert.deepEqual(result, payload.data, 'should fall back and return the cached data to avoid Quota loading flicker');
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });
  });

  describe('Linux fast path and failed-refresh storm prevention', () => {
    test('fast path passes skipLinuxKeyring: true on linux', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let receivedOpts = null;
        await getQuota({
          fast: true,
          platform: 'linux',
          tokenReader: (opts) => {
            receivedOpts = opts;
            return null;
          },
          backgroundRefresh: () => {},
        });
        assert.equal(receivedOpts?.skipLinuxKeyring, true);
        assert.equal(receivedOpts?.skipWindowsCredential, false);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('fast path wakes credential refresh on linux when no token is visible', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let refreshes = 0;

        const quota = await getQuota({
          fast: true,
          platform: 'linux',
          tokenReader: () => null,
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'not_logged_in');
        assert.equal(refreshes, 1);
      } finally {
        if (previousCache !== null) fs.writeFileSync(CACHE_PATH, previousCache);
        else fs.rmSync(CACHE_PATH, { force: true });
      }
    });

    test('fast path wakes credential refresh on linux when file token is expired', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        let refreshes = 0;

        const quota = await getQuota({
          fast: true,
          platform: 'linux',
          tokenReader: () => ({
            accessToken: 'expired-file-token',
            expiry: '2000-01-01T00:00:00.000Z',
            sourcePath: path.join('expired', 'oauth_creds.json'),
          }),
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });

        assert.equal(quota.length, 0);
        assert.equal(quota.unavailableReason, 'expired_token');
        assert.equal(refreshes, 1);
      } finally {
        if (previousCache !== null) fs.writeFileSync(CACHE_PATH, previousCache);
        else fs.rmSync(CACHE_PATH, { force: true });
      }
    });

    test('applyRefreshResult never wipes valid cache with empty array and prevents 30s spawn storm', async () => {
      const { applyRefreshResult, createUnavailableQuotaResult, writeCache } = quotaModule;
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        const tokA = { accessToken: 'tok-A', sourcePath: '/p/oauth' };
        const tokB = { accessToken: 'tok-B', sourcePath: '/p/oauth' };

        writeCache([{ id: 'gemini-4-carbon-high', remainingFraction: 0.9, resetTime: null }], tokA, 'Pro', 'a@example.com');
        // Simulate stale cache
        const raw = JSON.parse(fs.readFileSync(CACHE_PATH, 'utf8'));
        raw.expiresAt = Date.now() - 10_000;
        raw.lastRefreshed = Date.now() - 60_000;
        fs.writeFileSync(CACHE_PATH, JSON.stringify(raw));

        // Background refresh fails even though accountEmail succeeded
        applyRefreshResult(createUnavailableQuotaResult('quota_fetch_failed'), tokB, null, 'a@example.com');

        let refreshes = 0;
        const result = await getQuota({
          fast: true,
          tokenReader: () => tokB,
          backgroundRefresh: () => { refreshes += 1; },
        });
        assert.equal(result.length, 1);
        assert.equal(result[0].id, 'gemini-4-carbon-high');
        assert.equal(refreshes, 0, 'recent failed refresh attempt must debounce spawn storm within 30s');

        // When no prior cache exists, failure marker surfaces unavailableReason instead of fake loading []
        fs.rmSync(CACHE_PATH, { force: true });
        applyRefreshResult(createUnavailableQuotaResult('quota_fetch_failed'), tokB, null, 'a@example.com');
        const emptyFailure = await getQuota({
          fast: true,
          tokenReader: () => tokB,
          backgroundRefresh: () => { refreshes += 1; },
        });
        assert.equal(emptyFailure.length, 0);
        assert.equal(emptyFailure.unavailableReason, 'quota_fetch_failed');
        assert.equal(refreshes, 0);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('debounces platform credential refresh across processes via disk timestamp when not logged in or token expired', async () => {
      const { runBackgroundRefresh } = quotaModule;
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-cross-proc-'));
      try {
        const isolatedRoots = [path.join(tmp, 'antigravity-cli')];
        fs.mkdirSync(isolatedRoots[0], { recursive: true });

        // Simulate --refresh subprocess seeing no token and recording disk timestamp before exit.
        await runBackgroundRefresh({
          tokenReader: () => null,
          cacheOpts: { roots: isolatedRoots },
        });
        assert.ok(quotaModule.readCacheLastRefreshAttempt({ roots: isolatedRoots }) > 0);

        // Next short-lived process with default 30s debounce must NOT spawn again.
        let refreshes = 0;
        const result = await getQuota({
          fast: true,
          platform: 'linux',
          roots: isolatedRoots,
          tokenReader: () => null,
          backgroundRefresh: () => { refreshes += 1; },
        });
        assert.equal(result.unavailableReason, 'not_logged_in');
        assert.equal(refreshes, 0, 'disk refresh timestamp must debounce cross-process spawns');

        // Also when --refresh sees an expired token, it persists attempt timestamp on disk.
        fs.rmSync(path.join(isolatedRoots[0], 'agy-pulse-quota-cache.json'), { force: true });
        await runBackgroundRefresh({
          tokenReader: () => ({
            accessToken: 'expired',
            expiry: '2000-01-01T00:00:00.000Z',
          }),
          cacheOpts: { roots: isolatedRoots },
        });
        const resultExpired = await getQuota({
          fast: true,
          platform: 'win32',
          roots: isolatedRoots,
          tokenReader: () => ({
            accessToken: 'expired',
            expiry: '2000-01-01T00:00:00.000Z',
          }),
          backgroundRefresh: () => { refreshes += 1; },
        });
        assert.equal(resultExpired.unavailableReason, 'expired_token');
        assert.equal(refreshes, 0);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('returns expired_token when token is expired and matching payload cache is stale, while refreshing platform credentials', async () => {
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        const tok = {
          accessToken: 'expired-tok',
          expiry: '2000-01-01T00:00:00.000Z',
          sourcePath: '/p/oauth_creds.json',
        };
        quotaModule.writeCache([{ id: 'gemini-4-carbon-high', remainingFraction: 0.9, resetTime: null }], tok);

        // Make cache stale
        const raw = JSON.parse(fs.readFileSync(CACHE_PATH, 'utf8'));
        raw.expiresAt = Date.now() - 10_000;
        fs.writeFileSync(CACHE_PATH, JSON.stringify(raw));

        let refreshes = 0;
        const staleExpired = await getQuota({
          fast: true,
          platform: 'linux',
          tokenReader: () => tok,
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });
        assert.equal(staleExpired.length, 0);
        assert.equal(staleExpired.unavailableReason, 'expired_token');
        assert.equal(refreshes, 1, 'should wake platform credential refresh');

        // When cache is still fresh, temporarily serve fresh cache while waking credential refresh
        raw.expiresAt = Date.now() + 60_000;
        fs.writeFileSync(CACHE_PATH, JSON.stringify(raw));
        const freshExpired = await getQuota({
          fast: true,
          platform: 'linux',
          tokenReader: () => tok,
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });
        assert.equal(freshExpired.length, 1);
        assert.equal(refreshes, 2);
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('explicit account email switch across unmatched sources bypasses previous account 30s debounce and hides prior quota', async () => {
      const { applyRefreshResult, createUnavailableQuotaResult } = quotaModule;
      const previousCache = fs.existsSync(CACHE_PATH) ? fs.readFileSync(CACHE_PATH, 'utf8') : null;
      try {
        fs.rmSync(CACHE_PATH, { force: true });
        const userAPayload = {
          version: 3,
          expiresAt: Date.now() + 60_000,
          lastRefreshed: Date.now() - 5_000,
          cacheKeyHash: 'userA-source-hash',
          tokenHash: 'userA-token-hash',
          accountEmail: 'userA@example.com',
          data: [{ id: 'gemini-4-carbon-high', remainingFraction: 0.25, resetTime: null }],
        };
        fs.writeFileSync(CACHE_PATH, JSON.stringify(userAPayload), { mode: 0o600 });

        const tokB = {
          accessToken: 'userB-token',
          sourceFormat: 'oauth-creds',
          sourcePath: '/other/oauth_creds.json',
          email: 'userB@example.com',
        };
        let refreshes = 0;
        const result = await getQuota({
          fast: true,
          tokenReader: () => tokB,
          backgroundRefresh: () => { refreshes += 1; },
        });
        assert.equal(refreshes, 1, 'explicit email switch must immediately trigger refresh even within 30s');
        assert.deepEqual(result, [], 'must not leak userA cached quota to userB');

        // If userB refresh fails, subsequent calls within 30s for that exact tokB debounce normally
        applyRefreshResult(createUnavailableQuotaResult('quota_fetch_failed'), tokB, null, 'userB@example.com');
        await getQuota({
          fast: true,
          tokenReader: () => tokB,
          backgroundRefresh: () => { refreshes += 1; },
        });
        assert.equal(refreshes, 1, 'same failed token for userB must debounce within 30s');
      } finally {
        if (previousCache === null) fs.rmSync(CACHE_PATH, { force: true });
        else fs.writeFileSync(CACHE_PATH, previousCache);
      }
    });

    test('smoothly serves cached quota or loading state when expired mirror yields tok === null in fast mode', async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-expired-mirror-'));
      try {
        const root = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(root, { recursive: true });

        // 1) Non-empty quota cache present on disk -> serve cache and wake background refresh
        quotaModule.writeCache([{ id: 'gemini-4-carbon-high', remainingFraction: 0.72, resetTime: null }], 'old-tok', 'Pro', 'a@example.com', null, {
          roots: [root],
        });
        let refreshes = 0;
        const cachedResult = await getQuota({
          fast: true,
          platform: 'linux',
          roots: [root],
          tokenReader: () => null,
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });
        assert.equal(cachedResult.length, 1);
        assert.equal(cachedResult[0].id, 'gemini-4-carbon-high');
        assert.equal(refreshes, 1);

        // 2) No quota cache, only agy-pulse-token.json mirror exists -> return [] (loading) and refresh
        fs.rmSync(path.join(root, 'agy-pulse-quota-cache.json'), { force: true });
        fs.writeFileSync(path.join(root, 'agy-pulse-token.json'), JSON.stringify({
          tokens: [{ accessToken: 'expired-tok', expiry: '2000-01-01T00:00:00.000Z' }],
          writtenAt: Date.now() - 3600_000,
        }));
        const loadingResult = await getQuota({
          fast: true,
          platform: 'win32',
          roots: [root],
          tokenReader: () => null,
          backgroundRefresh: () => { refreshes += 1; },
          windowsCredentialRefreshDebounceMs: 0,
        });
        assert.deepEqual(loadingResult, []);
        assert.equal(loadingResult.unavailableReason, undefined);
        assert.equal(refreshes, 2);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('rejects matched payload when tok.email differs from payload.accountEmail on shared keyring source', async () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-email-consistency-'));
      try {
        const root = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(root, { recursive: true });

        const tokA = { accessToken: 'kr-tok-A', sourceFormat: 'linux-keyring', email: 'a@example.com' };
        quotaModule.writeCache([{ id: 'gemini-4-carbon-high', remainingFraction: 0.2, resetTime: null }], tokA, 'Pro', 'a@example.com', null, {
          roots: [root],
        });

        const tokB = { accessToken: 'kr-tok-B', sourceFormat: 'linux-keyring', email: 'b@example.com' };
        let refreshes = 0;
        const result = await getQuota({
          fast: true,
          platform: 'linux',
          roots: [root],
          tokenReader: () => tokB,
          backgroundRefresh: () => { refreshes += 1; },
        });

        assert.deepEqual(result, []);
        assert.equal(refreshes, 1);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
  });
});
