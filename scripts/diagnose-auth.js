#!/usr/bin/env node
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');
const { getAntigravityRoots, resolveSafeExecutable } = require('../runtime/paths.js');
const {
  getTokenCandidates,
  getGoogleAccountsCandidates,
  parseTokenPayload,
  readToken,
  isTokenExpired,
  probeLinuxKeyringAvailability,
  probeMacosKeychainAvailability,
} = require('../runtime/quota.js');

function compactObject(value) {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined)
  );
}

function pathExists(target) {
  try {
    fs.accessSync(target, fs.constants.F_OK);
    return true;
  } catch {
    return false;
  }
}

function listJsonKeys(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
  return Object.keys(value).sort();
}

function summarizeTokenCandidate(candidatePath) {
  const summary = {
    path: candidatePath,
    exists: false,
    readable: false,
    parseable: false,
  };

  try {
    const stat = fs.statSync(candidatePath);
    summary.exists = stat.isFile();
    summary.size = stat.size;
  } catch {
    summary.reason = 'not_found';
    return summary;
  }

  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(candidatePath, 'utf8'));
    summary.readable = true;
  } catch {
    summary.reason = 'invalid_or_unreadable_json';
    return summary;
  }

  summary.keys = listJsonKeys(parsed);
  if (parsed && typeof parsed === 'object' && parsed.token) {
    summary.tokenKeys = listJsonKeys(parsed.token);
  }

  const baseName = path.basename(candidatePath);
  if (baseName === 'agy-pulse-token.json') {
    const tokens = Array.isArray(parsed?.tokens) ? parsed.tokens : [];
    summary.sourceFormat = 'token-mirror';
    summary.tokenCount = tokens.length;
    summary.parseable = tokens.some(t => Boolean(t && t.accessToken));
    if (!summary.parseable) summary.reason = 'unsupported_token_shape';
    return summary;
  }

  if (baseName === 'google_accounts.json') {
    summary.sourceFormat = 'google-accounts';
    summary.hasActiveAccount = Boolean(
      (typeof parsed?.active === 'string' && parsed.active.includes('@')) ||
      (typeof parsed?.activeAccount === 'string' && parsed.activeAccount.includes('@')) ||
      (typeof parsed?.email === 'string' && parsed.email.includes('@'))
    );
    summary.parseable = Boolean(parsed && typeof parsed === 'object' && !Array.isArray(parsed));
    if (!summary.parseable) summary.reason = 'unsupported_token_shape';
    return summary;
  }

  const token = parseTokenPayload(parsed);
  if (!token) {
    summary.reason = 'unsupported_token_shape';
    return summary;
  }

  summary.parseable = true;
  summary.sourceFormat = token.sourceFormat;
  summary.hasExpiry = Boolean(token.expiry);
  return summary;
}

function getAgyCandidates() {
  const candidates = [process.env.AGY_BIN].filter(Boolean);

  const resolved = resolveSafeExecutable('agy');
  if (resolved) candidates.push(resolved);

  if (process.platform === 'win32') {
    const localAppData = process.env.LOCALAPPDATA ||
      path.join(os.homedir(), 'AppData', 'Local');
    candidates.push(path.join(localAppData, 'agy', 'bin', 'agy.exe'));
  } else {
    candidates.push(
      '/opt/homebrew/bin/agy',
      '/usr/local/bin/agy',
      '/usr/bin/agy'
    );
  }

  return [...new Set(candidates)];
}

function summarizeAgyCandidates(candidates) {
  const existingCandidates = candidates.filter(pathExists);
  return compactObject({
    candidateCount: candidates.length,
    existingCandidates: existingCandidates.length > 0 ? existingCandidates.slice(0, 10) : undefined,
  });
}

function resolveAgyInfo() {
  const candidates = getAgyCandidates();
  let lastError;

  for (const candidate of candidates) {
    if (!pathExists(candidate)) continue;
    try {
      const version = execFileSync(candidate, ['--version'], {
        encoding: 'utf8',
        timeout: 3000,
        stdio: ['ignore', 'pipe', 'pipe'],
      }).trim();
      return {
        found: true,
        path: candidate,
        version: version.split(/\r?\n/)[0] || null,
        candidateCount: candidates.length,
      };
    } catch (error) {
      lastError = error && error.code ? error.code : 'version_check_failed';
      return compactObject({
        found: true,
        path: candidate,
        version: null,
        versionError: lastError,
        candidateCount: candidates.length,
      });
    }
  }

  return compactObject({
    found: false,
    ...summarizeAgyCandidates(candidates),
    lastError,
  });
}

function maskEmail(email) {
  if (typeof email !== 'string' || !email.includes('@')) return undefined;
  const [local, domain] = email.split('@');
  if (!local || !domain) return undefined;
  const maskedLocal = local.length <= 2
    ? `${local[0]}*`
    : `${local.slice(0, 2)}***`;
  const domainParts = domain.split('.');
  const maskedDomain = domainParts.length >= 2
    ? `${domainParts[0].slice(0, 2)}***.${domainParts.slice(1).join('.')}`
    : `${domain.slice(0, 2)}***`;
  return `${maskedLocal}@${maskedDomain}`;
}

function summarizeQuotaCache(roots) {
  for (const root of roots) {
    if (!root) continue;
    const candidate = path.join(root, 'agy-pulse-quota-cache.json');
    if (!pathExists(candidate)) continue;
    try {
      const raw = JSON.parse(fs.readFileSync(candidate, 'utf8'));
      return compactObject({
        exists: true,
        path: candidate,
        version: raw?.version,
        dataCount: Array.isArray(raw?.data) ? raw.data.length : 0,
        lastRefreshed: raw?.lastRefreshed || undefined,
        lastRefreshAttempt: raw?.lastRefreshAttempt || undefined,
        lastRefreshError: raw?.lastRefreshError || undefined,
        unavailableReason: raw?.unavailableReason || undefined,
        hasAccountEmail: Boolean(raw?.accountEmail),
        accountEmail: maskEmail(raw?.accountEmail),
      });
    } catch {
      return {
        exists: true,
        path: candidate,
        parseable: false,
      };
    }
  }
  return { exists: false };
}

function sanitizeReadTokenResult(token) {
  if (!token) return { found: false };

  return compactObject({
    found: true,
    sourceFormat: token.sourceFormat || undefined,
    sourcePath: token.sourcePath || undefined,
    hasExpiry: Boolean(token.expiry),
    isExpired: isTokenExpired(token),
    hasEmail: Boolean(token.email),
    tokenCount: Array.isArray(token.all) ? token.all.length : 1,
  });
}

function buildAuthDiagnostic(options = {}) {
  const roots = getAntigravityRoots();
  const resolveAgy = options.resolveAgyInfo || resolveAgyInfo;
  const platform = options.platform || process.platform;
  const keyringProbe = options.keyringProbe || probeLinuxKeyringAvailability;
  const keychainProbe = options.keychainProbe || probeMacosKeychainAvailability;

  const candidatePaths = [...new Set([
    ...getTokenCandidates(roots),
    ...roots.filter(Boolean).map(root => path.join(root, 'agy-pulse-token.json')),
    ...getGoogleAccountsCandidates(roots),
  ])];

  return {
    schemaVersion: 1,
    platform,
    arch: process.arch,
    node: process.version,
    home: os.homedir(),
    cwd: process.cwd(),
    agy: resolveAgy(),
    antigravityRoots: roots.map(root => ({
      path: root,
      exists: pathExists(root),
    })),
    tokenCandidates: candidatePaths.map(summarizeTokenCandidate),
    readToken: sanitizeReadTokenResult(readToken({
      platform,
      credentialReader: options.credentialReader,
      keyringReader: options.keyringReader,
      keychainReader: options.keychainReader,
      noWriteMirror: true,
    })),
    quotaCache: summarizeQuotaCache(roots),
    keyringProbe: keyringProbe(platform),
    keychainProbe: keychainProbe(platform),
  };
}

function main() {
  if (process.argv.includes('--help') || process.argv.includes('-h')) {
    process.stdout.write('Usage: node scripts/diagnose-auth.js\n');
    process.stdout.write('Prints a JSON auth diagnostic without token values.\n');
    return;
  }

  process.stdout.write(`${JSON.stringify(buildAuthDiagnostic(), null, 2)}\n`);
}

if (require.main === module) main();

module.exports = {
  buildAuthDiagnostic,
  resolveAgyInfo,
  summarizeTokenCandidate,
  summarizeQuotaCache,
  maskEmail,
  sanitizeReadTokenResult,
};
