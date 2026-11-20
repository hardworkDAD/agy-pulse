import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { quotaModule, withEnv } from './_helpers/quota-test-utils.mjs';

const {
  selectUsableTokens,
  isTokenExpired,
  parseTokenPayload,
  readToken,
  clearTokenTemp,
} = quotaModule;
const tokenModule = quotaModule.tokenMod || (await import('../../runtime/quota/token.js'));
const { decodeKeyringRaw, extractEmailFromIdToken } = tokenModule;

describe('quota / token', () => {
  describe('selectUsableTokens', () => {
    test('keeps Windows temp tokens until token expiry', () => {
      const now = Date.parse('2026-05-20T20:00:00Z');
      const oldWrittenAt = now - 60 * 60 * 1000;

      const tokens = selectUsableTokens([
        { accessToken: 'valid-with-expiry', expiry: '2026-05-20T20:10:00Z' },
        { accessToken: 'nearly-expired', expiry: '2026-05-20T20:00:30Z' },
        { accessToken: 'no-expiry-old-cache' },
        { accessToken: 'no-expiry-fresh-cache' },
      ], oldWrittenAt, now);

      assert.deepEqual(tokens.map(t => t.accessToken), ['valid-with-expiry']);

      const freshNoExpiry = selectUsableTokens([
        { accessToken: 'no-expiry-fresh-cache' },
      ], now - 1000, now);
      assert.deepEqual(freshNoExpiry.map(t => t.accessToken), ['no-expiry-fresh-cache']);
    });
  });

  describe('isTokenExpired', () => {
    test('detects expired file tokens with expiry skew', () => {
      const now = Date.parse('2026-05-21T20:00:00Z');

      assert.equal(
        isTokenExpired({ accessToken: 'old-token', expiry: '2026-05-21T19:59:30Z' }, now),
        true
      );
      assert.equal(
        isTokenExpired({ accessToken: 'fresh-token', expiry: '2026-05-21T20:05:00Z' }, now),
        false
      );
      assert.equal(
        isTokenExpired({ accessToken: 'no-expiry-token' }, now),
        false
      );
    });
  });

  describe('parseTokenPayload', () => {
    test('supports antigravity-cli and oauth_creds token shapes', () => {
      assert.deepEqual(
        parseTokenPayload({
          token: {
            access_token: 'cli-token',
            expiry: '2026-05-20T20:10:00Z'
          }
        }),
        {
          accessToken: 'cli-token',
          expiry: '2026-05-20T20:10:00Z',
          sourceFormat: 'antigravity-cli'
        }
      );

      assert.deepEqual(
        parseTokenPayload({
          access_token: 'oauth-creds-token',
          expiry_date: Date.parse('2026-05-20T12:10:00Z')
        }),
        {
          accessToken: 'oauth-creds-token',
          expiry: '2026-05-20T12:10:00.000Z',
          sourceFormat: 'oauth-creds'
        }
      );
    });

    test('extracts email from id_token in token payload', () => {
      const mockIdToken = 'eyJhbGciOiJSUzI1NiJ9.' +
        Buffer.from(JSON.stringify({ email: 'active@example.com' })).toString('base64') +
        '.signature';
      assert.deepEqual(
        parseTokenPayload({
          token: {
            access_token: 'cli-token',
            expiry: '2026-05-20T20:10:00Z',
          },
          id_token: mockIdToken,
        }),
        {
          accessToken: 'cli-token',
          expiry: '2026-05-20T20:10:00Z',
          sourceFormat: 'antigravity-cli',
          email: 'active@example.com',
        }
      );
    });
  });

  describe('readToken', () => {
    test('only accepts Antigravity token files from configured data roots', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-roots-'));
      try {
        const home = path.join(tmp, 'home');
        const xdg = path.join(tmp, 'xdg');
        fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
        fs.mkdirSync(path.join(xdg, 'antigravity-cli'), { recursive: true });
        fs.writeFileSync(
          path.join(home, '.gemini', 'jetski-standalone-oauth-token'),
          JSON.stringify({ access_token: 'legacy-token' })
        );
        const antigravityTokenPath = path.join(xdg, 'antigravity-cli', 'antigravity-oauth-token');
        fs.writeFileSync(
          antigravityTokenPath,
          JSON.stringify({ token: { access_token: 'antigravity-token' } })
        );

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: xdg,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          assert.equal(readToken({ platform: 'linux', keyringReader: () => null }).accessToken, 'antigravity-token');

          fs.rmSync(antigravityTokenPath);
          assert.equal(readToken({ platform: 'linux', keyringReader: () => null }), null);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('falls back to ~/.gemini/oauth_creds.json when antigravity-cli token is missing', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-oauth-creds-'));
      try {
        const home = path.join(tmp, 'home');
        fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
        fs.writeFileSync(
          path.join(home, '.gemini', 'oauth_creds.json'),
          JSON.stringify({
            access_token: 'oauth-creds-token',
            expiry_date: Date.parse('2026-05-20T12:10:00Z'),
            refresh_token: 'refresh-token'
          })
        );

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          const token = readToken({ platform: 'linux', keyringReader: () => null });
          assert.equal(token.accessToken, 'oauth-creds-token');
          assert.equal(token.sourceFormat, 'oauth-creds');
          assert.equal(token.sourcePath.endsWith(path.join('.gemini', 'oauth_creds.json')), true);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('can skip Windows Credential Manager for statusline fast path', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-fast-'));
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
          assert.equal(readToken({ platform: 'win32', skipWindowsCredential: true }), null);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('can read Windows Credential Manager when the fast path is not requested', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-credential-'));
      try {
        const home = path.join(tmp, 'home');
        fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
        let credentialReads = 0;

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          const token = readToken({
            platform: 'win32',
            credentialReader: () => {
              credentialReads += 1;
              return { accessToken: 'credential-token', sourceFormat: 'windows-credential' };
            },
          });

          assert.equal(token.accessToken, 'credential-token');
          assert.equal(token.sourceFormat, 'windows-credential');
          assert.equal(credentialReads, 1);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
    test('reads token from Linux Keyring when no token file exists', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-linux-keyring-'));
      try {
        const home = path.join(tmp, 'home');
        fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
        let keyringReads = 0;

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          const token = readToken({
            platform: 'linux',
            keyringReader: () => {
              keyringReads += 1;
              return {
                accessToken: 'linux-keyring-token',
                sourceFormat: 'linux-keyring',
                all: [{ accessToken: 'linux-keyring-token' }],
              };
            },
          });

          assert.equal(token.accessToken, 'linux-keyring-token');
          assert.equal(token.sourceFormat, 'linux-keyring');
          assert.equal(keyringReads, 1);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('falls back to token file when Linux Keyring returns null', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-linux-fallback-'));
      try {
        const home = path.join(tmp, 'home');
        fs.mkdirSync(path.join(home, '.gemini', 'antigravity-cli'), { recursive: true });
        fs.writeFileSync(
          path.join(home, '.gemini', 'antigravity-cli', 'antigravity-oauth-token'),
          JSON.stringify({ token: { access_token: 'file-token' } })
        );

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          const token = readToken({
            platform: 'linux',
            keyringReader: () => null,
          });

          assert.equal(token.accessToken, 'file-token');
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('accepts raw Linux Keyring token strings', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-linux-raw-'));
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
          const token = readToken({
            platform: 'linux',
            keyringReader: () => 'linux-raw-token',
          });

          assert.equal(token.accessToken, 'linux-raw-token');
          assert.equal(token.sourceFormat, 'linux-keyring');
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('passes platform and roots arguments to keyringReader on Linux', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-linux-args-'));
      try {
        const home = path.join(tmp, 'home');
        fs.mkdirSync(path.join(home, '.gemini'), { recursive: true });
        let passedPlatform = null;
        let passedRoots = null;

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          readToken({
            platform: 'linux',
            keyringReader: (platform, roots) => {
              passedPlatform = platform;
              passedRoots = roots;
              return null;
            },
          });

          assert.equal(passedPlatform, 'linux');
          assert.equal(Array.isArray(passedRoots), true);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('prefers macOS Keychain reader over file tokens on darwin', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-darwin-'));
      try {
        const home = path.join(tmp, 'home');
        fs.mkdirSync(path.join(home, '.gemini', 'antigravity-cli'), { recursive: true });
        fs.writeFileSync(
          path.join(home, '.gemini', 'antigravity-cli', 'antigravity-oauth-token'),
          JSON.stringify({ token: { access_token: 'stale-file-token' } })
        );

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          const token = readToken({
            platform: 'darwin',
            keychainReader: () => ({
              accessToken: 'active-keychain-token',
              sourceFormat: 'macos-keychain',
              email: 'active@example.com',
              all: [{ accessToken: 'active-keychain-token', email: 'active@example.com' }],
            }),
          });

          assert.equal(token.accessToken, 'active-keychain-token');
          assert.equal(token.sourceFormat, 'macos-keychain');
          assert.equal(token.email, 'active@example.com');
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('macOS Keychain reader is not shadowed by stale temp token file', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-darwin-temp-'));
      try {
        const home = path.join(tmp, 'home');
        const cliDir = path.join(home, '.gemini', 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        fs.writeFileSync(
          path.join(cliDir, 'agy-pulse-token.json'),
          JSON.stringify({
            tokens: [{
              accessToken: 'stale-temp-token',
              expiry: '2026-10-30T22:00:00Z',
              sourceFormat: 'macos-keychain',
              email: 'stale@example.com',
            }],
            writtenAt: Date.now(),
          })
        );

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          const token = readToken({
            platform: 'darwin',
            roots: [cliDir],
            keychainReader: () => ({
              accessToken: 'live-keychain-token',
              sourceFormat: 'macos-keychain',
              email: 'live@example.com',
              all: [{ accessToken: 'live-keychain-token', email: 'live@example.com' }],
            }),
          });

          assert.equal(token.accessToken, 'live-keychain-token');
          assert.equal(token.sourceFormat, 'macos-keychain');
          assert.equal(token.email, 'live@example.com');
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('clearTokenTemp removes existing temporary token cache file', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-clear-'));
      try {
        const home = path.join(tmp, 'home');
        const cliDir = path.join(home, '.gemini', 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        const tokenFile = path.join(cliDir, 'agy-pulse-token.json');
        fs.writeFileSync(tokenFile, JSON.stringify({ tokens: [{ accessToken: 'stale' }] }));

        withEnv({
          HOME: home,
          USERPROFILE: home,
          XDG_DATA_HOME: undefined,
          APPDATA: undefined,
          LOCALAPPDATA: undefined,
        }, () => {
          clearTokenTemp([cliDir]);
          assert.equal(fs.existsSync(tokenFile), false);
        });
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });
  });

  describe('decodeKeyringRaw', () => {
    test('decodes go-keyring-base64 prefixed JSON string', () => {
      const payload = { token: { access_token: 'go-token' } };
      const raw = 'go-keyring-base64:' + Buffer.from(JSON.stringify(payload)).toString('base64');
      assert.deepEqual(decodeKeyringRaw(raw), payload);
    });

    test('decodes standard JSON string', () => {
      const payload = { token: { access_token: 'json-token' } };
      assert.deepEqual(decodeKeyringRaw(JSON.stringify(payload)), payload);
    });

    test('returns plain trimmed string when not JSON', () => {
      assert.equal(decodeKeyringRaw('  plain-token  '), 'plain-token');
    });

    test('returns null on empty or invalid inputs', () => {
      assert.equal(decodeKeyringRaw(''), null);
      assert.equal(decodeKeyringRaw(null), null);
      assert.equal(decodeKeyringRaw('   '), null);
    });
  });

  describe('base64url JWT, fast skip flags, and mirror freshness', () => {
    test('extractEmailFromIdToken decodes unpadded base64url payloads with - and _', () => {
      const payloadJson = JSON.stringify({ sub: '123?>?', email: 'user+test_1@example.com' });
      const b64url = Buffer.from(payloadJson).toString('base64url');
      assert.equal(b64url.includes('='), false);
      assert.equal(extractEmailFromIdToken(`hdr.${b64url}.sig`), 'user+test_1@example.com');
    });

    test('readToken skips Linux keyring and macOS keychain when skip flags are set', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-skip-'));
      try {
        const cliDir = path.join(tmp, '.gemini', 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        let keyringCalls = 0;
        let keychainCalls = 0;

        assert.equal(readToken({
          platform: 'linux',
          roots: [cliDir],
          skipLinuxKeyring: true,
          keyringReader: () => {
            keyringCalls += 1;
            return { accessToken: 'should-not-read' };
          },
        }), null);
        assert.equal(keyringCalls, 0);

        assert.equal(readToken({
          platform: 'darwin',
          roots: [cliDir],
          skipKeychain: true,
          keychainReader: () => {
            keychainCalls += 1;
            return { accessToken: 'should-not-read' };
          },
        }), null);
        assert.equal(keychainCalls, 0);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('disk credential newer than agy-pulse-token.json mirror overrides stale mirror and refreshes it', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-mirror-fresh-'));
      try {
        const geminiDir = path.join(tmp, '.gemini');
        const cliDir = path.join(geminiDir, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });

        const oldTime = Date.now() - 60_000;
        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');
        fs.writeFileSync(mirrorPath, JSON.stringify({
          tokens: [{
            accessToken: 'stale-account-a',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'oauth-creds',
            email: 'a@example.com',
          }],
          writtenAt: oldTime,
        }));

        const oauthPath = path.join(geminiDir, 'oauth_creds.json');
        fs.writeFileSync(oauthPath, JSON.stringify({
          access_token: 'fresh-account-b',
          expiry_date: Date.now() + 3600_000,
          email: 'b@example.com',
        }));

        const tok = readToken({
          platform: 'linux',
          roots: [cliDir],
          skipLinuxKeyring: true,
        });
        assert.equal(tok?.accessToken, 'fresh-account-b');
        assert.equal(tok?.email, 'b@example.com');

        const refreshedMirror = JSON.parse(fs.readFileSync(mirrorPath, 'utf8'));
        assert.equal(refreshedMirror.tokens[0].accessToken, 'fresh-account-b');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('google_accounts.json account switch invalidates mirror even without disk token file', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-mirror-switch-'));
      try {
        const geminiDir = path.join(tmp, '.gemini');
        const cliDir = path.join(geminiDir, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });

        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');
        fs.writeFileSync(mirrorPath, JSON.stringify({
          tokens: [{
            accessToken: 'stale-account-a',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'linux-keyring',
            email: 'a@example.com',
          }],
          writtenAt: Date.now(),
        }));

        fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({
          active: 'b@example.com',
          old: ['a@example.com'],
        }));

        const tok = readToken({
          platform: 'linux',
          roots: [cliDir],
          keyringReader: () => ({
            accessToken: 'keyring-account-b',
            sourceFormat: 'linux-keyring',
            email: 'b@example.com',
          }),
        });
        assert.equal(tok?.accessToken, 'keyring-account-b');
        assert.equal(fs.existsSync(mirrorPath), false);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('expired Linux keyring token does not shadow a valid non-expired disk token', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-expired-keyring-'));
      try {
        const cliDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        fs.writeFileSync(
          path.join(cliDir, 'antigravity-oauth-token'),
          JSON.stringify({
            token: {
              access_token: 'fresh-disk-token',
              expiry: new Date(Date.now() + 3600_000).toISOString(),
            },
          })
        );

        const tok = readToken({
          platform: 'linux',
          roots: [cliDir],
          keyringReader: () => ({
            accessToken: 'stale-expired-keyring-token',
            expiry: '2026-09-28T18:47:55.000Z',
            sourceFormat: 'linux-keyring',
            all: [{
              accessToken: 'stale-expired-keyring-token',
              expiry: '2026-09-28T18:47:55.000Z',
            }],
          }),
        });

        assert.equal(tok?.accessToken, 'fresh-disk-token');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('buildWindowsCredentialScript unwraps go-keyring-base64, expiry_date fallback, and email extraction', () => {
      const script = quotaModule.tokenMod
        ? quotaModule.tokenMod.buildWindowsCredentialScript()
        : tokenModule.buildWindowsCredentialScript();
      assert.match(script, /go-keyring-base64:/);
      assert.match(script, /expiry_date/);
      assert.match(script, /id_token_email/);
      assert.match(script, /Get-JwtEmail/);
    });

    test('stale email-less oauth_creds.json does not shadow keyring when google_accounts.json switches account', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-stale-disk-mask-'));
      try {
        const geminiDir = path.join(tmp, '.gemini');
        const cliDir = path.join(geminiDir, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });

        // Old email-less oauth_creds.json from an older session
        const oauthPath = path.join(geminiDir, 'oauth_creds.json');
        fs.writeFileSync(oauthPath, JSON.stringify({
          access_token: 'very-old-oauth-token',
          expiry_date: Date.now() + 3600_000,
        }));
        const pastSec = (Date.now() - 120_000) / 1000;
        fs.utimesSync(oauthPath, pastSec, pastSec);

        // Mirror written more recently for account A
        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');
        fs.writeFileSync(mirrorPath, JSON.stringify({
          tokens: [{
            accessToken: 'mirror-a',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'linux-keyring',
            email: 'a@example.com',
          }],
          writtenAt: Date.now() - 10_000,
        }));

        // google_accounts.json switched to b@example.com
        fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({
          active: 'b@example.com',
        }));

        const tok = readToken({
          platform: 'linux',
          roots: [cliDir],
          keyringReader: () => ({
            accessToken: 'keyring-b',
            sourceFormat: 'linux-keyring',
            email: 'b@example.com',
          }),
        });
        assert.equal(tok?.accessToken, 'keyring-b', 'stale email-less oauth_creds must not shadow keyring');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('readLinuxKeyringTokens falls back to secret-tool when python3 lacks keyring', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-secret-tool-'));
      try {
        const cliDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        const payload = {
          token: { access_token: 'secret-tool-token', expiry: new Date(Date.now() + 3600_000).toISOString() },
          id_token_email: 'secret-tool@example.com',
        };
        const encoded = 'go-keyring-base64:' + Buffer.from(JSON.stringify(payload)).toString('base64');

        const calls = [];
        const tok = tokenModule.readLinuxKeyringTokens('linux', [cliDir], {
          _resolveSafeExecutable: (name) => (name === 'secret-tool' ? '/usr/bin/secret-tool' : null),
          _execFileSync: (cmd, args) => {
            calls.push({ cmd, args });
            return encoded;
          },
        });
        assert.equal(calls.length, 1);
        assert.equal(calls[0].cmd, '/usr/bin/secret-tool');
        assert.deepEqual(calls[0].args, ['lookup', 'service', 'gemini', 'account', 'antigravity']);
        assert.equal(tok?.accessToken, 'secret-tool-token');
        assert.equal(tok?.email, 'secret-tool@example.com');
        assert.equal(tok?.sourceFormat, 'linux-keyring');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('noWriteMirror: true prevents creating or deleting agy-pulse-token.json', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-nowrite-mirror-'));
      try {
        const geminiDir = path.join(tmp, '.gemini');
        const cliDir = path.join(geminiDir, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');

        // 1) secret-tool / python keyring read with noWriteMirror: true must not write mirror
        tokenModule.readLinuxKeyringTokens('linux', [cliDir], {
          noWriteMirror: true,
          _resolveSafeExecutable: (name) => (name === 'secret-tool' ? '/usr/bin/secret-tool' : null),
          _execFileSync: () => JSON.stringify({ access_token: 'readonly-tok' }),
        });
        assert.equal(fs.existsSync(mirrorPath), false);

        // 2) Stale mirror must NOT be deleted when readToken runs with noWriteMirror: true
        fs.writeFileSync(mirrorPath, JSON.stringify({
          tokens: [{
            accessToken: 'existing-mirror-a',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            email: 'a@example.com',
          }],
          writtenAt: Date.now() - 60_000,
        }));
        fs.writeFileSync(path.join(geminiDir, 'google_accounts.json'), JSON.stringify({
          active: 'b@example.com',
        }));

        const tok = readToken({
          platform: 'linux',
          roots: [cliDir],
          noWriteMirror: true,
          keyringReader: () => ({ accessToken: 'live-b', sourceFormat: 'linux-keyring', email: 'b@example.com' }),
        });
        assert.equal(tok?.accessToken, 'live-b');
        assert.equal(fs.existsSync(mirrorPath), true, 'noWriteMirror must not delete existing mirror');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('macOS fast: true hits fresh mirror without calling security, while injected keychainReader stays live', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-darwin-fast-'));
      try {
        const cliDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');

        // Populate mirror via readMacosKeychainTokens
        const fromSecurity = tokenModule.readMacosKeychainTokens('darwin', [cliDir], {
          _resolveSafeExecutable: () => '/usr/bin/security',
          _execFileSync: () => JSON.stringify({
            token: { access_token: 'darwin-live-tok', expiry: new Date(Date.now() + 3600_000).toISOString() },
            id_token_email: 'mac@example.com',
          }),
        });
        assert.equal(fromSecurity?.accessToken, 'darwin-live-tok');
        assert.equal(fs.existsSync(mirrorPath), true, 'macOS keychain read should populate mirror');

        // fast: true without custom keychainReader returns mirrored token directly
        const fastTok = readToken({
          platform: 'darwin',
          roots: [cliDir],
          fast: true,
        });
        assert.equal(fastTok?.accessToken, 'darwin-live-tok');
        assert.equal(fastTok?.sourceFormat, 'macos-keychain');

        // fast: true WITH injected keychainReader still uses injected reader
        const injectedTok = readToken({
          platform: 'darwin',
          roots: [cliDir],
          fast: true,
          keychainReader: () => ({
            accessToken: 'injected-live-tok',
            sourceFormat: 'macos-keychain',
          }),
        });
        assert.equal(injectedTok?.accessToken, 'injected-live-tok');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('writeWindowsTokenTemp unlinks symlinked agy-pulse-token.json before atomic rename', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-token-symlink-'));
      try {
        const cliDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        const targetFile = path.join(tmp, 'outside-target.txt');
        fs.writeFileSync(targetFile, 'KEEP_INTACT');
        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');
        fs.symlinkSync(targetFile, mirrorPath);

        tokenModule.writeWindowsTokenTemp([{ accessToken: 'safe-tok' }], [cliDir]);
        assert.equal(fs.readFileSync(targetFile, 'utf8'), 'KEEP_INTACT');
        assert.equal(fs.lstatSync(mirrorPath).isSymbolicLink(), false);
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('non-fast readToken reads live Linux keyring and Windows credential manager before mirror', () => {
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-live-first-'));
      try {
        const cliDir = path.join(tmp, 'antigravity-cli');
        fs.mkdirSync(cliDir, { recursive: true });
        const mirrorPath = path.join(cliDir, 'agy-pulse-token.json');

        fs.writeFileSync(mirrorPath, JSON.stringify({
          tokens: [{
            accessToken: 'stale-mirror-token',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'linux-keyring',
            email: 'old@example.com',
          }],
          writtenAt: Date.now(),
        }));

        const linuxLive = readToken({
          platform: 'linux',
          fast: false,
          roots: [cliDir],
          keyringReader: () => ({
            accessToken: 'live-linux-token',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'linux-keyring',
            email: 'new@example.com',
          }),
        });
        assert.equal(linuxLive?.accessToken, 'live-linux-token');
        assert.equal(linuxLive?.email, 'new@example.com');

        fs.writeFileSync(mirrorPath, JSON.stringify({
          tokens: [{
            accessToken: 'stale-win-mirror',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'windows-credential',
            email: 'old@example.com',
          }],
          writtenAt: Date.now(),
        }));

        const winLive = readToken({
          platform: 'win32',
          fast: false,
          roots: [cliDir],
          credentialReader: () => ({
            accessToken: 'live-win-token',
            expiry: new Date(Date.now() + 3600_000).toISOString(),
            sourceFormat: 'windows-credential',
            email: 'new@example.com',
          }),
        });
        assert.equal(winLive?.accessToken, 'live-win-token');
        assert.equal(winLive?.email, 'new@example.com');
      } finally {
        fs.rmSync(tmp, { recursive: true, force: true });
      }
    });

    test('probeLinuxKeyringAvailability falls back to secret-tool --version when python keyring fails', () => {
      const okSecretTool = tokenModule.probeLinuxKeyringAvailability('linux', {
        _resolveSafeExecutable: (name) => (name === 'secret-tool' ? '/usr/bin/secret-tool' : '/usr/bin/python3'),
        _execFileSync: (cmd, args) => {
          if (cmd === '/usr/bin/python3') {
            const err = new Error('No module named keyring');
            err.status = 1;
            err.stderr = 'ModuleNotFoundError: No module named keyring';
            throw err;
          }
          if (cmd === '/usr/bin/secret-tool' && args[0] === '--version') {
            return 'secret-tool 0.21';
          }
          throw new Error('unexpected');
        },
      });
      assert.equal(okSecretTool.available, true);
      assert.equal(okSecretTool.secretTool, '/usr/bin/secret-tool');

      const bothMissing = tokenModule.probeLinuxKeyringAvailability('linux', {
        _resolveSafeExecutable: () => null,
      });
      assert.equal(bothMissing.available, false);
      assert.equal(bothMissing.reason, 'python-not-found');
    });
  });
});
