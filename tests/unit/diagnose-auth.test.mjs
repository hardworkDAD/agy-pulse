import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { buildAuthDiagnostic, sanitizeReadTokenResult } = require('../../scripts/diagnose-auth.js');

function withEnv(overrides, fn) {
  const snapshot = {};
  for (const key of Object.keys(overrides)) {
    snapshot[key] = process.env[key];
    if (overrides[key] === undefined) delete process.env[key];
    else process.env[key] = overrides[key];
  }
  try {
    return fn();
  } finally {
    for (const key of Object.keys(snapshot)) {
      if (snapshot[key] === undefined) delete process.env[key];
      else process.env[key] = snapshot[key];
    }
  }
}

test('auth diagnostic reuses token parsing without leaking token values', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-diagnose-auth-'));
  try {
    const home = path.join(tmp, 'home');
    fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
    fs.writeFileSync(
      path.join(home, '.gemini', 'oauth_creds.json'),
      JSON.stringify({
        access_token: 'secret-access-token',
        refresh_token: 'secret-refresh-token',
        expiry_date: Date.parse('2026-05-20T12:10:00Z')
      })
    );

    withEnv({
      HOME: home,
      USERPROFILE: home,
      XDG_DATA_HOME: undefined,
      APPDATA: undefined,
      LOCALAPPDATA: undefined,
    }, () => {
      const diagnostic = buildAuthDiagnostic({
        resolveAgyInfo: () => ({ found: false, candidates: [] }),
        platform: 'linux',
        keyringReader: () => null,
        keyringProbe: () => null,
      });
      const serialized = JSON.stringify(diagnostic);

      assert.equal(diagnostic.readToken.found, true);
      assert.equal(diagnostic.readToken.sourceFormat, 'oauth-creds');
      assert.equal(
        diagnostic.readToken.sourcePath.endsWith(path.join('.gemini', 'oauth_creds.json')),
        true
      );
      assert.equal(
        diagnostic.tokenCandidates.some(candidate =>
          candidate.sourceFormat === 'oauth-creds' && candidate.parseable === true
        ),
        true
      );
      assert.doesNotMatch(serialized, /secret-access-token/);
      assert.doesNotMatch(serialized, /secret-refresh-token/);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('auth diagnostic reports Linux keyring source without leaking token values', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-diagnose-keyring-'));
  try {
    const home = path.join(tmp, 'home');
    fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });

    withEnv({
      HOME: home,
      USERPROFILE: home,
      XDG_DATA_HOME: undefined,
      APPDATA: undefined,
      LOCALAPPDATA: undefined,
    }, () => {
      const diagnostic = buildAuthDiagnostic({
        resolveAgyInfo: () => ({ found: false, candidates: [] }),
        platform: 'linux',
        keyringReader: () => ({
          accessToken: 'secret-keyring-token',
          sourceFormat: 'linux-keyring',
          all: [{ accessToken: 'secret-keyring-token' }],
        }),
        keyringProbe: () => null,
      });
      const serialized = JSON.stringify(diagnostic);

      assert.equal(diagnostic.readToken.found, true);
      assert.equal(diagnostic.readToken.sourceFormat, 'linux-keyring');
      assert.equal(diagnostic.readToken.tokenCount, 1);
      assert.doesNotMatch(serialized, /secret-keyring-token/);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('auth diagnostic surfaces Linux keyring probe result', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-diagnose-probe-'));
  try {
    const home = path.join(tmp, 'home');
    fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });

    withEnv({
      HOME: home,
      USERPROFILE: home,
      XDG_DATA_HOME: undefined,
      APPDATA: undefined,
      LOCALAPPDATA: undefined,
    }, () => {
      const diagnostic = buildAuthDiagnostic({
        resolveAgyInfo: () => ({ found: false, candidates: [] }),
        platform: 'linux',
        keyringReader: () => null,
        keyringProbe: () => ({ available: false, reason: 'python-not-found' }),
      });

      assert.equal(diagnostic.keyringProbe.available, false);
      assert.equal(diagnostic.keyringProbe.reason, 'python-not-found');
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('auth diagnostic reports macOS Keychain source and probe without leaking token values', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-diagnose-macos-'));
  try {
    const home = path.join(tmp, 'home');
    fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });

    withEnv({
      HOME: home,
      USERPROFILE: home,
      XDG_DATA_HOME: undefined,
      APPDATA: undefined,
      LOCALAPPDATA: undefined,
    }, () => {
      const diagnostic = buildAuthDiagnostic({
        resolveAgyInfo: () => ({ found: false, candidates: [] }),
        platform: 'darwin',
        keychainReader: () => ({
          accessToken: 'secret-keychain-token',
          sourceFormat: 'macos-keychain',
          email: 'secret-user@example.com',
          all: [{ accessToken: 'secret-keychain-token' }],
        }),
        keychainProbe: () => ({ available: true, security: '/usr/bin/security' }),
      });
      const serialized = JSON.stringify(diagnostic);

      assert.equal(diagnostic.readToken.found, true);
      assert.equal(diagnostic.readToken.sourceFormat, 'macos-keychain');
      assert.equal(diagnostic.readToken.hasEmail, true);
      assert.equal(diagnostic.keychainProbe.available, true);
      assert.doesNotMatch(serialized, /secret-keychain-token/);
      assert.doesNotMatch(serialized, /secret-user@example.com/);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('auth diagnostic forwards credentialReader, enforces noWriteMirror, and reports mirror + google_accounts existence without leaking secrets', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-diagnose-readonly-'));
  try {
    const home = path.join(tmp, 'home');
    const geminiDir = path.join(home, '.gemini');
    const cliDir = path.join(geminiDir, 'antigravity-cli');
    fs.mkdirSync(cliDir, { recursive: true });

    const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');
    fs.writeFileSync(mirrorPath, JSON.stringify({
      tokens: [{ accessToken: 'secret-mirror-value' }],
      writtenAt: Date.now() - 10 * 60 * 1000,
    }));
    fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({
      active: 'private-user@example.com',
    }));

    withEnv({
      HOME: home,
      USERPROFILE: home,
      XDG_DATA_HOME: undefined,
      APPDATA: undefined,
      LOCALAPPDATA: undefined,
    }, () => {
      let receivedNoWriteMirror = null;
      const winDiag = buildAuthDiagnostic({
        resolveAgyInfo: () => ({ found: false, candidates: [] }),
        platform: 'win32',
        credentialReader: (_platform, _roots, opts) => {
          receivedNoWriteMirror = opts?.noWriteMirror;
          return {
            accessToken: 'secret-win-cred-value',
            sourceFormat: 'windows-credential',
          };
        },
      });
      const serialized = JSON.stringify(winDiag);
      assert.equal(receivedNoWriteMirror, true);
      assert.equal(winDiag.readToken.found, true);
      assert.equal(winDiag.readToken.sourceFormat, 'windows-credential');
      assert.equal(
        winDiag.tokenCandidates.some(c =>
          c.path.endsWith('agy-pulse-token.json') && c.exists === true && c.sourceFormat === 'token-mirror'
        ),
        true
      );
      assert.equal(
        winDiag.tokenCandidates.some(c =>
          c.path.endsWith('google_accounts.json') && c.exists === true && c.sourceFormat === 'google-accounts' && c.hasActiveAccount === true
        ),
        true
      );
      assert.doesNotMatch(serialized, /secret-mirror-value/);
      assert.doesNotMatch(serialized, /secret-win-cred-value/);
      assert.doesNotMatch(serialized, /private-user@example\.com/);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('sanitizeReadTokenResult reports isExpired and buildAuthDiagnostic surfaces quotaCache summary with masked email', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-diagnose-cache-'));
  try {
    const dataDir = path.join(tmp, 'data');
    fs.mkdirSync(dataDir, { recursive: true });
    fs.writeFileSync(path.join(dataDir, 'agy-pulse-quota-cache.json'), JSON.stringify({
      version: 3,
      expiresAt: 0,
      lastRefreshed: 1000,
      lastRefreshAttempt: 2000,
      lastRefreshError: 'quota_fetch_failed',
      unavailableReason: 'auth_failed',
      accountEmail: 'sensitive.person@example.com',
      data: [],
    }));

    assert.equal(sanitizeReadTokenResult({
      accessToken: 'tok',
      expiry: '2000-01-01T00:00:00.000Z',
    }).isExpired, true);
    assert.equal(sanitizeReadTokenResult({
      accessToken: 'tok',
      expiry: new Date(Date.now() + 3600_000).toISOString(),
    }).isExpired, false);

    withEnv({
      AGY_PULSE_DATA_DIR: dataDir,
    }, () => {
      const diagnostic = buildAuthDiagnostic({
        resolveAgyInfo: () => ({ found: false, candidates: [] }),
        platform: 'linux',
        keyringReader: () => ({
          accessToken: 'expired-kr',
          expiry: '2000-01-01T00:00:00.000Z',
          sourceFormat: 'linux-keyring',
        }),
        keyringProbe: () => ({ available: true }),
      });
      const serialized = JSON.stringify(diagnostic);

      assert.equal(diagnostic.readToken.isExpired, true);
      assert.equal(diagnostic.quotaCache.exists, true);
      assert.equal(diagnostic.quotaCache.lastRefreshError, 'quota_fetch_failed');
      assert.equal(diagnostic.quotaCache.unavailableReason, 'auth_failed');
      assert.equal(diagnostic.quotaCache.hasAccountEmail, true);
      assert.match(diagnostic.quotaCache.accountEmail, /^se\*\*\*@ex\*\*\*\.com$/);
      assert.doesNotMatch(serialized, /sensitive\.person@example\.com/);
    });
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
