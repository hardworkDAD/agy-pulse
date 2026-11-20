'use strict';

const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');
const {
  getAntigravityRoots,
  resolveAntigravityPath,
  resolveSafeExecutable,
} = require('../paths.js');

// ─── Cross-platform token discovery ──────────────────────────────────────────
// agy stores its OAuth token in different locations depending on the environment.
// We search in priority order; first readable file wins.
const ANTIGRAVITY_TOKEN_FILENAME = 'antigravity-oauth-token';
const OAUTH_CREDS_FILENAME = 'oauth_creds.json';
const GOOGLE_ACCOUNTS_FILENAME = 'google_accounts.json';

const WINDOWS_TOKEN_TEMP_TTL_MS = 5 * 60 * 1000;
const WINDOWS_TOKEN_EXPIRY_SKEW_MS = 60 * 1000;
const WINDOWS_CREDENTIAL_TARGETS = [
  'gemini:antigravity',
  'LegacyGeneric:target=gemini:antigravity',
];

// agy stores its OAuth token in system keyrings (macOS Keychain, Linux
// Secret Service via python keyring or go-keyring) under this service/account pair.
const KEYCHAIN_SERVICE = 'gemini';
const KEYCHAIN_ACCOUNT = 'antigravity';
const LINUX_KEYRING_SERVICE = KEYCHAIN_SERVICE;
const LINUX_KEYRING_USERNAME = KEYCHAIN_ACCOUNT;

function decodeKeyringRaw(raw) {
  if (!raw || typeof raw !== 'string') return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  let decoded = trimmed;
  if (trimmed.startsWith('go-keyring-base64:')) {
    try {
      decoded = Buffer.from(trimmed.slice(18), 'base64').toString('utf8');
    } catch {
      return null;
    }
  }
  try {
    return JSON.parse(decoded);
  } catch {
    return decoded;
  }
}

function extractEmailFromIdToken(idToken) {
  if (!idToken || typeof idToken !== 'string') return undefined;
  const parts = idToken.split('.');
  if (parts.length !== 3) return undefined;
  try {
    const payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
    if (payload && typeof payload.email === 'string' && payload.email.includes('@')) {
      return payload.email;
    }
  } catch {}
  return undefined;
}

function getGoogleAccountsCandidates(roots = getAntigravityRoots()) {
  const candidates = [];
  for (const root of roots) {
    if (!root) continue;
    if (path.basename(root) === 'antigravity-cli') {
      candidates.push(path.join(path.dirname(root), GOOGLE_ACCOUNTS_FILENAME));
    }
    candidates.push(path.join(root, GOOGLE_ACCOUNTS_FILENAME));
  }
  return [...new Set(candidates)];
}

function readGoogleAccountsState(roots = getAntigravityRoots()) {
  for (const candidate of getGoogleAccountsCandidates(roots)) {
    try {
      const stat = fs.statSync(candidate);
      const raw = JSON.parse(fs.readFileSync(candidate, 'utf8'));
      const activeEmail = typeof raw?.active === 'string' && raw.active.includes('@')
        ? raw.active
        : (typeof raw?.activeAccount === 'string' && raw.activeAccount.includes('@')
          ? raw.activeAccount
          : (typeof raw?.email === 'string' && raw.email.includes('@') ? raw.email : null));
      return {
        activeEmail,
        mtimeMs: stat.mtimeMs,
        path: candidate,
      };
    } catch { /* try next */ }
  }
  return null;
}

function getTokenCandidates(roots = getAntigravityRoots()) {
  const candidates = [];
  for (const root of roots) {
    candidates.push(path.join(root, ANTIGRAVITY_TOKEN_FILENAME));
    if (path.basename(root) === 'antigravity-cli') {
      candidates.push(path.join(path.dirname(root), OAUTH_CREDS_FILENAME));
    }
  }
  return [...new Set(candidates)];
}

function isUsableAccessToken(token, writtenAt = 0, now = Date.now()) {
  if (!token || !token.accessToken) return false;

  if (token.expiry) {
    const expiresAt = new Date(token.expiry).getTime();
    if (Number.isFinite(expiresAt)) {
      return expiresAt - WINDOWS_TOKEN_EXPIRY_SKEW_MS > now;
    }
  }

  return Boolean(writtenAt && now - writtenAt < WINDOWS_TOKEN_TEMP_TTL_MS);
}

function selectUsableTokens(tokens, writtenAt = 0, now = Date.now()) {
  return (Array.isArray(tokens) ? tokens : [])
    .filter(token => isUsableAccessToken(token, writtenAt, now));
}

function isTokenExpired(token, now = Date.now()) {
  if (!token || !token.expiry) return false;
  const expiresAt = new Date(token.expiry).getTime();
  return Number.isFinite(expiresAt) && expiresAt - WINDOWS_TOKEN_EXPIRY_SKEW_MS <= now;
}

function tokenResultFromTokens(tokens) {
  if (!tokens || tokens.length === 0) return null;
  return {
    accessToken: tokens[0].accessToken,
    expiry: tokens[0].expiry,
    sourceFormat: tokens[0].sourceFormat,
    email: tokens[0].email,
    all: tokens,
  };
}

function normalizeKeyringTokenResult(token) {
  if (!token) return null;
  if (typeof token === 'string') {
    return {
      accessToken: token,
      sourceFormat: 'linux-keyring',
      all: [{ accessToken: token, sourceFormat: 'linux-keyring' }],
    };
  }
  if (typeof token === 'object') {
    return {
      ...token,
      sourceFormat: token.sourceFormat || 'linux-keyring',
    };
  }
  return null;
}

function resolveTokenTempPath(roots = getAntigravityRoots()) {
  const candidates = (Array.isArray(roots) ? roots : [])
    .filter(Boolean)
    .map(root => path.join(root, 'agy-pulse-token.json'));

  // Always use the first candidate as the canonical path so that read and
  // write operations always target the same file, regardless of whether it
  // already exists.
  if (candidates.length > 0) return candidates[0];
  return resolveAntigravityPath('agy-pulse-token.json');
}

function normalizeExpiryDate(value) {
  if (value === undefined || value === null || value === '') return undefined;
  if (typeof value === 'number' && Number.isFinite(value)) {
    return new Date(value).toISOString();
  }
  if (typeof value === 'string') {
    if (/^\d+$/.test(value)) {
      return new Date(Number(value)).toISOString();
    }
    return value;
  }
  return undefined;
}

function parseTokenPayload(raw) {
  if (!raw || typeof raw !== 'object') return null;

  const rawEmail = (typeof raw.id_token_email === 'string' && raw.id_token_email.includes('@'))
    ? raw.id_token_email
    : ((typeof raw.email === 'string' && raw.email.includes('@'))
      ? raw.email
      : extractEmailFromIdToken(raw.id_token || (raw.token && raw.token.id_token)));

  if (raw.token && typeof raw.token === 'object' && raw.token.access_token) {
    return {
      accessToken: raw.token.access_token,
      expiry: normalizeExpiryDate(raw.token.expiry),
      sourceFormat: 'antigravity-cli',
      ...(rawEmail ? { email: rawEmail } : {}),
    };
  }

  if (raw.access_token) {
    return {
      accessToken: raw.access_token,
      expiry: normalizeExpiryDate(raw.expiry || raw.expiry_date),
      sourceFormat: 'oauth-creds',
      ...(rawEmail ? { email: rawEmail } : {}),
    };
  }

  return null;
}

function parseKeyringSecret(raw) {
  if (!raw || typeof raw !== 'string') return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const parsed = decodeKeyringRaw(trimmed);
  if (!parsed) return null;
  return typeof parsed === 'string'
    ? (!/\s/.test(parsed) ? { accessToken: parsed } : null)
    : parseTokenPayload(parsed);
}

function writeWindowsTokenTemp(tokens, roots = getAntigravityRoots()) {
  if (!Array.isArray(tokens) || tokens.length === 0) return;
  const tmp = resolveTokenTempPath(roots);
  const staging = `${tmp}.tmp.${process.pid}.${Date.now()}.${Math.random().toString(36).slice(2, 8)}`;
  try {
    fs.mkdirSync(path.dirname(tmp), { recursive: true, mode: 0o700 });
    // mode 0o600 — the file holds OAuth access tokens, so keep permissions
    // narrow even though this is only a short-lived mirror.
    fs.writeFileSync(staging, JSON.stringify({ tokens, writtenAt: Date.now() }), { mode: 0o600, flag: 'wx' });
    try {
      if (fs.lstatSync(tmp).isSymbolicLink()) {
        fs.unlinkSync(tmp);
      }
    } catch { /* target does not exist */ }
    fs.renameSync(staging, tmp);
  } catch {
    try { fs.unlinkSync(staging); } catch {}
  }
}

function readWindowsTokenTemp(roots = getAntigravityRoots()) {
  try {
    const tmp = resolveTokenTempPath(roots);
    const raw = JSON.parse(fs.readFileSync(tmp, 'utf8'));
    return tokenResultFromTokens(selectUsableTokens(raw.tokens, raw.writtenAt));
  } catch {
    return null;
  }
}

function readDiskCandidateTokens(roots = getAntigravityRoots(), prioritizeAfterMs = null) {
  const candidates = getTokenCandidates(roots);
  const entries = [];
  for (const candidate of candidates) {
    try {
      const stat = fs.statSync(candidate);
      entries.push({ candidate, mtimeMs: stat.mtimeMs });
    } catch { /* missing */ }
  }
  if (prioritizeAfterMs !== null) {
    entries.sort((a, b) => {
      const aNew = a.mtimeMs > prioritizeAfterMs ? 1 : 0;
      const bNew = b.mtimeMs > prioritizeAfterMs ? 1 : 0;
      if (aNew !== bNew) return bNew - aNew;
      if (aNew && bNew) return b.mtimeMs - a.mtimeMs;
      return 0;
    });
  }
  const results = [];
  for (const entry of entries) {
    try {
      const raw = JSON.parse(fs.readFileSync(entry.candidate, 'utf8'));
      const token = parseTokenPayload(raw);
      if (token) results.push({ ...token, sourcePath: entry.candidate, mtimeMs: entry.mtimeMs });
    } catch { /* try next */ }
  }
  return results;
}

/**
 * Checks the short-lived JSON token mirror against on-disk credential files and
 * google_accounts.json so account switches on disk are never masked by a stale mirror.
 */
function readFreshMirrorOrDiskToken(roots, defaultSourceFormat, options = {}) {
  const { noWriteMirror = false } = options;
  const tmp = resolveTokenTempPath(roots);
  let raw;
  let mirrorStat;
  try {
    mirrorStat = fs.statSync(tmp);
    raw = JSON.parse(fs.readFileSync(tmp, 'utf8'));
  } catch {
    return { hit: false, token: null };
  }

  const tempToken = tokenResultFromTokens(selectUsableTokens(raw.tokens, raw.writtenAt));
  if (!tempToken) {
    return { hit: false, token: null };
  }

  const mirrorTime = Number.isFinite(raw.writtenAt) ? raw.writtenAt : mirrorStat.mtimeMs;
  const accountsState = readGoogleAccountsState(roots);
  const accountMismatch = Boolean(
    accountsState?.activeEmail &&
    tempToken.email &&
    accountsState.activeEmail.toLowerCase() !== tempToken.email.toLowerCase()
  );
  const accountsNewer = Boolean(accountsState && accountsState.mtimeMs > mirrorTime);

  const diskTokens = readDiskCandidateTokens(roots, mirrorTime);
  const usableDiskTokens = diskTokens.filter(t => !isTokenExpired(t));
  const diskNewer = usableDiskTokens.some(t => t.mtimeMs > mirrorTime);

  if (!accountMismatch && !accountsNewer && !diskNewer) {
    return {
      hit: true,
      token: { ...tempToken, sourceFormat: tempToken.sourceFormat || defaultSourceFormat },
    };
  }

  // Mirror is stale or shadowed by an account switch / newer disk credential.
  // Only allow non-expired disk tokens that are strictly newer than the mirror
  // OR explicitly match the active account email, preventing stale email-less
  // oauth_creds.json files from masking the system keyring.
  const activeEmailLower = accountsState?.activeEmail ? accountsState.activeEmail.toLowerCase() : null;
  const matchingDisk = usableDiskTokens.find(t => {
    const isNewerThanMirror = t.mtimeMs > mirrorTime;
    const tokenEmailLower = t.email ? t.email.toLowerCase() : null;
    const matchesActiveEmail = Boolean(activeEmailLower && tokenEmailLower && tokenEmailLower === activeEmailLower);
    if (!isNewerThanMirror && !matchesActiveEmail) return false;
    if (activeEmailLower && tokenEmailLower) {
      return tokenEmailLower === activeEmailLower;
    }
    return true;
  });

  if (matchingDisk) {
    const { mtimeMs: _mtimeMs, ...cleanToken } = matchingDisk;
    if (!noWriteMirror) {
      const mirrorEntry = {
        accessToken: cleanToken.accessToken,
        expiry: cleanToken.expiry,
        sourceFormat: cleanToken.sourceFormat,
        ...(cleanToken.email ? { email: cleanToken.email } : {}),
      };
      writeWindowsTokenTemp([mirrorEntry], roots);
    }
    return { hit: true, token: cleanToken };
  }

  if (!noWriteMirror) {
    clearTokenTemp(roots);
  }
  return { hit: false, token: null };
}

function buildWindowsCredentialScript() {
  const targets = WINDOWS_CREDENTIAL_TARGETS
    .map(target => `'${target.replace(/'/g, "''")}'`)
    .join(',');

  return [
    '$ErrorActionPreference = "SilentlyContinue"',
    'Add-Type -Language CSharp -TypeDefinition @"',
    'using System; using System.Runtime.InteropServices; using System.Text;',
    'public class WC {',
    '  [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)]',
    '  public struct CRED { public uint Flags,Type; public string Target,Comment;',
    '    public long LastWritten; public uint BlobSize; public IntPtr Blob;',
    '    public uint Persist,AttrCount; public IntPtr Attrs; public string Alias,User; }',
    '  [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)]',
    '  public static extern bool CredRead(string target,uint type,int reservedFlag,out IntPtr credentialPtr);',
    '  [DllImport("advapi32.dll")] public static extern void CredFree(IntPtr p);',
    '}',
    '"@',
    `$targets=@(${targets})`,
    '$tokens=@()',
    'function Get-JwtEmail($jwt){',
    '  if(-not $jwt -or ($jwt -isnot [string])){ return $null }',
    '  $parts=$jwt.Split(".")',
    '  if($parts.Length -ne 3){ return $null }',
    '  try {',
    '    $p=$parts[1].Replace("-","+").Replace("_","/")',
    '    switch($p.Length % 4){ 2 { $p+="==" } 3 { $p+="=" } }',
    '    $json=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($p)) | ConvertFrom-Json',
    '    if($json.email -and $json.email.Contains("@")){ return $json.email }',
    '  } catch {}',
    '  return $null',
    '}',
    'foreach($target in $targets){',
    '  $p=[IntPtr]::Zero',
    '  if([WC]::CredRead($target,1,0,[ref]$p)){',
    '    try {',
    '      $c=[Runtime.InteropServices.Marshal]::PtrToStructure($p,[type][WC+CRED])',
    '      if($c.BlobSize -gt 0){',
    '        $bytes=New-Object byte[] $c.BlobSize',
    '        [Runtime.InteropServices.Marshal]::Copy($c.Blob,$bytes,0,$c.BlobSize)',
    '        foreach($enc in @([Text.Encoding]::UTF8,[Text.Encoding]::Unicode)){',
    '          try {',
    '            $rawText=$enc.GetString($bytes).Trim()',
    '            if($rawText.StartsWith("go-keyring-base64:")){',
    '              $rawText=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($rawText.Substring(18)))',
    '            }',
    '            $o=$rawText|ConvertFrom-Json',
    '            $tok=$o.token',
    '            $email=$null',
    '            if($o.id_token_email -and $o.id_token_email.Contains("@")){ $email=$o.id_token_email }',
    '            elseif($o.email -and $o.email.Contains("@")){ $email=$o.email }',
    '            elseif($tok -and $tok.email -and $tok.email.Contains("@")){ $email=$tok.email }',
    '            else {',
    '              $idTok=if($o.id_token){ $o.id_token } elseif($tok -and $tok.id_token){ $tok.id_token } else { $null }',
    '              $email=Get-JwtEmail $idTok',
    '            }',
    '            if($tok -and $tok.access_token){',
    '              $exp=if($tok.expiry){ $tok.expiry } elseif($tok.expiry_date){ $tok.expiry_date } elseif($o.expiry){ $o.expiry } else { $o.expiry_date }',
    '              $entry=[ordered]@{ accessToken=$tok.access_token; expiry=$exp }',
    '              if($email){ $entry.email=$email }',
    '              $tokens += [pscustomobject]$entry',
    '              break',
    '            } elseif($o.access_token){',
    '              $exp=if($o.expiry){ $o.expiry } else { $o.expiry_date }',
    '              $entry=[ordered]@{ accessToken=$o.access_token; expiry=$exp }',
    '              if($email){ $entry.email=$email }',
    '              $tokens += [pscustomobject]$entry',
    '              break',
    '            }',
    '          } catch {}',
    '        }',
    '      }',
    '    } finally { [WC]::CredFree($p) }',
    '  }',
    '}',
    '$dedup=@{}',
    'foreach($t in $tokens){ if($t.accessToken -and -not $dedup.ContainsKey($t.accessToken)){ $dedup[$t.accessToken]=$t } }',
    'if($dedup.Count -gt 0){ [pscustomobject]@{ tokens=@($dedup.Values) } | ConvertTo-Json -Compress -Depth 4 }',
    'else { Write-Output "{`"tokens`":[]}" }',
  ].join('\n');
}

function readWindowsCredentialTokens(platform = process.platform, roots = getAntigravityRoots(), options = {}) {
  if (platform !== 'win32') return null;

  try {
    const powershellPath = resolveSafeExecutable('powershell');
    if (!powershellPath) return null;
    const raw = execFileSync(powershellPath, [
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-Command',
      buildWindowsCredentialScript(),
    ], {
      encoding: 'utf8',
      timeout: 5000,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const parsed = JSON.parse(raw.trim() || '{"tokens":[]}');
    const normalizedTokens = (Array.isArray(parsed.tokens) ? parsed.tokens : []).map(token => ({
      ...token,
      expiry: normalizeExpiryDate(token?.expiry ?? token?.expiry_date),
    }));
    const valid = selectUsableTokens(normalizedTokens, Date.now())
      .map(token => ({
        accessToken: token.accessToken,
        expiry: token.expiry,
        sourceFormat: 'windows-credential',
        ...(typeof token.email === 'string' && token.email.includes('@') ? { email: token.email } : {}),
      }));
    if (valid.length === 0) return null;
    if (!options.noWriteMirror) {
      writeWindowsTokenTemp(valid, roots);
    }
    return { ...tokenResultFromTokens(valid), sourceFormat: 'windows-credential' };
  } catch {
    return null;
  }
}

/**
 * Reads the OAuth token stored in the Linux Keyring (D-Bus Secret Service)
 * by delegating to python3. agy stores its token via the third-party `keyring`
 * pip package, so the python3 resolved from PATH must be able to `import keyring`
 * — typically true when agy was installed via pipx / system pip / an activated
 * venv, but NOT guaranteed for isolated venvs. On failure this returns null and
 * readToken falls back to disk files; see probeLinuxKeyringAvailability() for a
 * diagnostic that distinguishes "no token" from "keyring unreadable".
 *
 * To avoid spawning a python3 subprocess on every statusline refresh, a
 * successful read is written to the same short-lived JSON cache that the
 * Windows Credential Manager path uses.
 *
 * @param {string} [platform]
 * @returns {{ accessToken: string, expiry?: string, sourceFormat?: string, all: Array<{ accessToken: string, expiry?: string }> } | null}
 */
const LINUX_SECRET_TOOL_CANDIDATES = [
  [LINUX_KEYRING_SERVICE, LINUX_KEYRING_USERNAME],
];

function readLinuxKeyringViaPython(execFn, resolveExe) {
  try {
    const pythonPath = resolveExe('python3') || resolveExe('python');
    if (!pythonPath) return null;

    const raw = execFn(pythonPath, [
      '-c',
      `import keyring; print(keyring.get_password(${JSON.stringify(LINUX_KEYRING_SERVICE)}, ${JSON.stringify(LINUX_KEYRING_USERNAME)}) or '')`,
    ], {
      encoding: 'utf8',
      timeout: 5000,
      stdio: ['ignore', 'pipe', 'ignore'],
    });

    return parseKeyringSecret(raw);
  } catch {
    return null;
  }
}

function readLinuxKeyringViaSecretTool(execFn, resolveExe) {
  try {
    const secretTool = resolveExe('secret-tool');
    if (!secretTool) return null;

    for (const [svc, acc] of LINUX_SECRET_TOOL_CANDIDATES) {
      try {
        const raw = execFn(secretTool, ['lookup', 'service', svc, 'account', acc], {
          encoding: 'utf8',
          timeout: 2000,
          stdio: ['ignore', 'pipe', 'ignore'],
        });
        const token = parseKeyringSecret(raw);
        if (token) return token;
      } catch { /* try next candidate */ }
    }
    return null;
  } catch {
    return null;
  }
}

function readLinuxKeyringTokens(platform = process.platform, roots = getAntigravityRoots(), options = {}) {
  if (platform !== 'linux') return null;

  const execFn = options._execFileSync || execFileSync;
  const resolveExe = options._resolveSafeExecutable || resolveSafeExecutable;

  const token =
    readLinuxKeyringViaPython(execFn, resolveExe) ||
    readLinuxKeyringViaSecretTool(execFn, resolveExe);
  if (!token) return null;

  // Mirror into the shared temp cache so subsequent statusline renders
  // do not need to re-spawn python3/secret-tool (skip mirroring already-expired tokens).
  const tokens = [{
    accessToken: token.accessToken,
    expiry: token.expiry,
    sourceFormat: 'linux-keyring',
    ...(token.email ? { email: token.email } : {}),
  }];
  if (!options.noWriteMirror && !isTokenExpired(token)) {
    writeWindowsTokenTemp(tokens, roots);
  }

  return { ...token, sourceFormat: 'linux-keyring', all: tokens };
}

/**
 * Reads the OAuth token stored in macOS Keychain by delegating to /usr/bin/security.
 * agy stores its token under service "gemini" and account "antigravity" via go-keyring.
 *
 * @param {string} [platform]
 * @param {string[]} [roots]
 * @returns {{ accessToken: string, expiry?: string, sourceFormat?: string, email?: string, all: Array<{ accessToken: string, expiry?: string, email?: string }> } | null}
 */
function readMacosKeychainTokens(platform = process.platform, roots = getAntigravityRoots(), options = {}) {
  if (platform !== 'darwin') return null;

  try {
    const execFn = options._execFileSync || execFileSync;
    const resolveExe = options._resolveSafeExecutable || resolveSafeExecutable;
    const securityPath = resolveExe('security') || '/usr/bin/security';
    if (!options._execFileSync && !fs.existsSync(securityPath)) return null;

    const raw = execFn(securityPath, [
      'find-generic-password',
      '-s',
      KEYCHAIN_SERVICE,
      '-a',
      KEYCHAIN_ACCOUNT,
      '-w',
    ], {
      encoding: 'utf8',
      timeout: 5000,
      stdio: ['ignore', 'pipe', 'ignore'],
    });

    const token = parseKeyringSecret(raw);
    if (!token) return null;

    const tokens = [{
      accessToken: token.accessToken,
      expiry: token.expiry,
      sourceFormat: 'macos-keychain',
      ...(token.email ? { email: token.email } : {}),
    }];
    if (!options.noWriteMirror && !isTokenExpired(token)) {
      writeWindowsTokenTemp(tokens, roots);
    }

    return { ...token, sourceFormat: 'macos-keychain', all: tokens };
  } catch {
    return null;
  }
}

/**
 * Probes whether the macOS security CLI is available.
 *
 * @param {string} [platform]
 * @returns {{ available: boolean, security?: string } | null}
 */
function probeMacosKeychainAvailability(platform = process.platform) {
  if (platform !== 'darwin') return null;
  const securityPath = resolveSafeExecutable('security') || '/usr/bin/security';
  return {
    available: fs.existsSync(securityPath),
    security: securityPath,
  };
}

/**
 * Probes whether the Linux keyring read path is wired up, WITHOUT reading the
 * token itself. Returns null on non-linux platforms. On linux, reports whether
 * a python interpreter is resolvable on PATH and can `import keyring` (the
 * third-party pip package agy uses to store its OAuth token). Used by
 * diagnose-auth so users can tell "not logged in" apart from "system python3
 * is missing the keyring package".
 *
 * @param {string} [platform]
 * @returns {{ available: boolean, reason?: string, python?: string, status?: number, stderr?: string } | null}
 */
function probeLinuxKeyringAvailability(platform = process.platform, options = {}) {
  if (platform !== 'linux') return null;

  const execFn = options._execFileSync || execFileSync;
  const resolveExe = options._resolveSafeExecutable || resolveSafeExecutable;
  const pythonPath = resolveExe('python3') || resolveExe('python');
  let pythonFailure = null;

  if (pythonPath) {
    try {
      execFn(pythonPath, ['-c', 'import keyring'], {
        encoding: 'utf8',
        timeout: 3000,
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      return { available: true, python: pythonPath };
    } catch (error) {
      pythonFailure = {
        available: false,
        reason: 'keyring-not-importable',
        python: pythonPath,
        status: typeof error.status === 'number' ? error.status : undefined,
        stderr: error.stderr ? String(error.stderr).trim().slice(0, 200) : undefined,
      };
    }
  } else {
    pythonFailure = { available: false, reason: 'python-not-found' };
  }

  const secretToolPath = resolveExe('secret-tool');
  if (secretToolPath) {
    try {
      execFn(secretToolPath, ['--version'], {
        encoding: 'utf8',
        timeout: 3000,
        stdio: ['ignore', 'pipe', 'pipe'],
      });
      return {
        available: true,
        secretTool: secretToolPath,
        ...(pythonPath ? { python: pythonPath } : {}),
      };
    } catch { /* fall through to failure details */ }
  }

  return pythonFailure;
}

function clearStaleMirrorIfMismatched(roots, liveToken, noWriteMirror) {
  if (noWriteMirror || !liveToken || !liveToken.accessToken) return;
  const tmp = resolveTokenTempPath(roots);
  try {
    if (!fs.existsSync(tmp)) return;
    const raw = JSON.parse(fs.readFileSync(tmp, 'utf8'));
    const primary = Array.isArray(raw?.tokens) ? raw.tokens[0] : null;
    if (!primary || primary.accessToken !== liveToken.accessToken) {
      clearTokenTemp(roots);
    }
  } catch {
    clearTokenTemp(roots);
  }
}

/**
 * @returns {{ accessToken: string, expiry?: string, sourceFormat?: string, sourcePath?: string, email?: string, all?: Array<{ accessToken: string, expiry?: string, email?: string }> } | null}
 */
function readToken(options = {}) {
  const {
    platform = process.platform,
    roots = getAntigravityRoots(),
    fast = false,
    noWriteMirror = false,
    skipWindowsCredential = false,
    skipLinuxKeyring = false,
    skipKeychain = false,
    credentialReader = readWindowsCredentialTokens,
    keyringReader = readLinuxKeyringTokens,
    keychainReader = readMacosKeychainTokens,
  } = options;

  let expiredFallback = null;
  const hasCustomKeychainReader = Boolean(options.keychainReader);
  const hasCustomCredentialReader = Boolean(options.credentialReader);
  const hasCustomKeyringReader = Boolean(options.keyringReader);

  // macOS: agy stores its OAuth token in macOS Keychain (login keychain).
  // In fast mode (without an injected keychainReader), check the short-lived
  // JSON mirror first to avoid spawning /usr/bin/security on every render.
  if (platform === 'darwin') {
    if (fast && !hasCustomKeychainReader) {
      const mirrorResult = readFreshMirrorOrDiskToken(roots, 'macos-keychain', { noWriteMirror });
      if (mirrorResult.hit) {
        if (!isTokenExpired(mirrorResult.token)) return mirrorResult.token;
        expiredFallback = mirrorResult.token;
      }
    }

    if (!skipKeychain) {
      const keychainToken = keychainReader(platform, roots, { noWriteMirror });
      if (keychainToken) {
        if (!isTokenExpired(keychainToken)) return keychainToken;
        expiredFallback = expiredFallback || keychainToken;
      }
    }
  }

  // Windows: agy stores its OAuth token in Credential Manager. Only check the
  // short-lived JSON mirror first in fast mode without an injected reader;
  // non-fast paths (--refresh, --diagnose) read live credentials first.
  if (platform === 'win32') {
    if (fast && !hasCustomCredentialReader) {
      const mirrorResult = readFreshMirrorOrDiskToken(roots, 'windows-credential', { noWriteMirror });
      if (mirrorResult.hit) {
        if (!isTokenExpired(mirrorResult.token)) return mirrorResult.token;
        expiredFallback = expiredFallback || mirrorResult.token;
      }
    }

    if (!skipWindowsCredential) {
      const credentialToken = credentialReader(platform, roots, { noWriteMirror });
      if (credentialToken) {
        if (!isTokenExpired(credentialToken)) {
          clearStaleMirrorIfMismatched(roots, credentialToken, noWriteMirror);
          return credentialToken;
        }
        expiredFallback = expiredFallback || credentialToken;
      }
    }

    if (!fast || hasCustomCredentialReader) {
      const mirrorResult = readFreshMirrorOrDiskToken(roots, 'windows-credential', { noWriteMirror });
      if (mirrorResult.hit) {
        if (!isTokenExpired(mirrorResult.token)) return mirrorResult.token;
        expiredFallback = expiredFallback || mirrorResult.token;
      }
    }
  }

  // Linux: agy stores its OAuth token in the system keyring (D-Bus Secret
  // Service). Only check the short-lived JSON cache first in fast mode without
  // an injected reader; non-fast paths read live keyring first so account switches
  // take effect immediately.
  if (platform === 'linux') {
    if (fast && !hasCustomKeyringReader) {
      const mirrorResult = readFreshMirrorOrDiskToken(roots, 'linux-keyring', { noWriteMirror });
      if (mirrorResult.hit) {
        if (!isTokenExpired(mirrorResult.token)) return mirrorResult.token;
        expiredFallback = expiredFallback || mirrorResult.token;
      }
    }

    if (!skipLinuxKeyring) {
      const keyringToken = normalizeKeyringTokenResult(keyringReader(platform, roots, { noWriteMirror }));
      if (keyringToken) {
        if (!isTokenExpired(keyringToken)) {
          clearStaleMirrorIfMismatched(roots, keyringToken, noWriteMirror);
          return keyringToken;
        }
        expiredFallback = expiredFallback || keyringToken;
      }
    }

    if (!fast || hasCustomKeyringReader) {
      const mirrorResult = readFreshMirrorOrDiskToken(roots, 'linux-keyring', { noWriteMirror });
      if (mirrorResult.hit) {
        if (!isTokenExpired(mirrorResult.token)) return mirrorResult.token;
        expiredFallback = expiredFallback || mirrorResult.token;
      }
    }
  }

  for (const candidate of getTokenCandidates(roots)) {
    try {
      const raw = JSON.parse(fs.readFileSync(candidate, 'utf8'));
      const token = parseTokenPayload(raw);
      if (token) {
        const result = { ...token, sourcePath: candidate };
        if (!isTokenExpired(result)) return result;
        expiredFallback = expiredFallback || result;
      }
    } catch { /* try next */ }
  }
  return expiredFallback;
}

function anyTokenFileExists(roots = getAntigravityRoots()) {
  for (const candidate of getTokenCandidates(roots)) {
    try {
      if (fs.existsSync(candidate)) return true;
    } catch { /* ignore */ }
  }
  return false;
}

function anyTokenMirrorExists(roots = getAntigravityRoots()) {
  for (const root of (Array.isArray(roots) ? roots : [])) {
    if (!root) continue;
    try {
      if (fs.existsSync(path.join(root, 'agy-pulse-token.json'))) return true;
    } catch { /* ignore */ }
  }
  return false;
}

function clearTokenTemp(roots = getAntigravityRoots()) {
  try {
    const tmp = resolveTokenTempPath(roots);
    if (fs.existsSync(tmp)) fs.unlinkSync(tmp);
  } catch { /* best effort */ }
}

module.exports = {
  ANTIGRAVITY_TOKEN_FILENAME,
  OAUTH_CREDS_FILENAME,
  GOOGLE_ACCOUNTS_FILENAME,
  WINDOWS_TOKEN_TEMP_TTL_MS,
  WINDOWS_TOKEN_EXPIRY_SKEW_MS,
  WINDOWS_CREDENTIAL_TARGETS,
  KEYCHAIN_SERVICE,
  KEYCHAIN_ACCOUNT,
  LINUX_KEYRING_SERVICE,
  LINUX_KEYRING_USERNAME,
  getTokenCandidates,
  getGoogleAccountsCandidates,
  readGoogleAccountsState,
  isUsableAccessToken,
  selectUsableTokens,
  isTokenExpired,
  tokenResultFromTokens,
  normalizeExpiryDate,
  parseTokenPayload,
  decodeKeyringRaw,
  extractEmailFromIdToken,
  writeWindowsTokenTemp,
  readWindowsTokenTemp,
  clearTokenTemp,
  buildWindowsCredentialScript,
  readWindowsCredentialTokens,
  readLinuxKeyringTokens,
  probeLinuxKeyringAvailability,
  readMacosKeychainTokens,
  probeMacosKeychainAvailability,
  readToken,
  anyTokenFileExists,
  anyTokenMirrorExists,
};
