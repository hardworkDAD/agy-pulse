'use strict';

const fs = require('fs');
const http = require('http');
const https = require('https');
const path = require('path');
const { resolveAntigravityPath } = require('./paths.js');
const { resolveRepoSlug, getRawGitHubUrl } = require('./brand.js');
const { loadConfig } = require('./config.js');

const DEFAULT_TIMEOUT_MS = 10000;

function getStatusFilePath(env = process.env) {
  return resolveAntigravityPath('agy-pulse-update-status.json', env);
}

function parseSemver(version) {
  if (typeof version !== 'string') return null;
  const cleaned = version.trim().replace(/^v/i, '');
  const match = cleaned.match(/^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?/);
  if (!match) return null;
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    prerelease: match[4] || '',
  };
}

function compareSemver(a, b) {
  const pa = parseSemver(a);
  const pb = parseSemver(b);
  if (!pa || !pb) return null;

  if (pa.major !== pb.major) return pa.major > pb.major ? 1 : -1;
  if (pa.minor !== pb.minor) return pa.minor > pb.minor ? 1 : -1;
  if (pa.patch !== pb.patch) return pa.patch > pb.patch ? 1 : -1;

  if (pa.prerelease === pb.prerelease) return 0;
  if (!pa.prerelease) return 1;
  if (!pb.prerelease) return -1;

  const partsA = pa.prerelease.split('.');
  const partsB = pb.prerelease.split('.');
  const maxLen = Math.max(partsA.length, partsB.length);
  for (let i = 0; i < maxLen; i++) {
    if (i >= partsA.length) return -1;
    if (i >= partsB.length) return 1;
    const ai = partsA[i];
    const bi = partsB[i];
    if (ai === bi) continue;

    const aNumeric = /^\d+$/.test(ai);
    const bNumeric = /^\d+$/.test(bi);
    if (aNumeric && bNumeric) {
      return Number(ai) > Number(bi) ? 1 : -1;
    }
    if (aNumeric !== bNumeric) {
      return aNumeric ? -1 : 1;
    }
    return ai > bi ? 1 : -1;
  }
  return 0;
}

function isNewerVersion(remoteVersion, localVersion) {
  const cmp = compareSemver(remoteVersion, localVersion);
  if (cmp !== null) return cmp > 0;
  return false;
}

function resolveUpdateUrl(config, env = process.env) {
  const safeEnv = env || process.env;
  const explicit = safeEnv.AGY_PULSE_UPDATE_URL;
  if (explicit && explicit.trim()) return explicit.trim();

  const repoSlug = resolveRepoSlug(config, safeEnv);
  if (!repoSlug) return null;
  return getRawGitHubUrl(repoSlug, 'main', 'package.json');
}

function fetchRemoteVersion(url, timeoutMs = DEFAULT_TIMEOUT_MS, redirectsLeft = 5) {
  if (!url) return Promise.resolve(null);
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    const client = parsed.protocol === 'http:' ? http : https;
    const req = client.get(parsed, { timeout: timeoutMs }, response => {
      const status = response.statusCode || 0;
      if (status >= 300 && status < 400 && response.headers.location && redirectsLeft > 0) {
        response.resume();
        const nextUrl = new URL(response.headers.location, parsed);
        if (parsed.protocol === 'https:' && nextUrl.protocol === 'http:') {
          return reject(new Error(`Insecure redirect from https to http forbidden: ${url} -> ${nextUrl.toString()}`));
        }
        return resolve(fetchRemoteVersion(nextUrl.toString(), timeoutMs, redirectsLeft - 1));
      }
      if (status !== 200) {
        response.resume();
        return reject(new Error(`Failed to fetch version: ${status}`));
      }
      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.on('end', () => {
        try {
          const body = Buffer.concat(chunks).toString('utf8');
          const pkg = JSON.parse(body);
          resolve(typeof pkg.version === 'string' ? pkg.version : null);
        } catch (err) {
          reject(err);
        }
      });
    });

    req.setTimeout(timeoutMs, () => {
      req.destroy(new Error(`Version check timed out after ${timeoutMs}ms`));
    });
    req.on('error', reject);
  });
}

function findLocalPackageJson(startDir = __dirname) {
  const candidates = [
    path.join(startDir, '..', 'package.json'),
    path.join(startDir, '..', '..', 'package.json'),
  ];
  return candidates.find(candidate => fs.existsSync(candidate)) || null;
}

function writeStatusFile(statusFile, payload) {
  fs.mkdirSync(path.dirname(statusFile), { recursive: true });
  const tmpPath = `${statusFile}.tmp.${process.pid}`;
  fs.writeFileSync(tmpPath, JSON.stringify(payload, null, 2), 'utf8');
  fs.renameSync(tmpPath, statusFile);
}

async function checkForUpdates(options = {}) {
  const env = options.env || process.env;
  const statusFile = options.statusFile || getStatusFilePath(env);
  const now = typeof options.now === 'number' ? options.now : Date.now();
  const fetchFn = options.fetchVersion || fetchRemoteVersion;

  let localVersion = null;
  try {
    const localPkgPath = options.localPkgPath || findLocalPackageJson();
    if (!localPkgPath || !fs.existsSync(localPkgPath)) {
      return null;
    }
    const localPkg = JSON.parse(fs.readFileSync(localPkgPath, 'utf8'));
    localVersion = localPkg.version;

    const argvCwd = process.argv[2] && !process.argv[2].startsWith('-') ? process.argv[2] : null;
    const cwd = options.cwd || argvCwd || process.cwd();
    const config = options.config !== undefined ? options.config : await loadConfig(cwd);
    const updateUrl = options.updateUrl !== undefined
      ? options.updateUrl
      : resolveUpdateUrl({ ...localPkg, ...config, repository: config?.repository || localPkg.repository }, env);

    if (!updateUrl) {
      const status = {
        lastCheck: now,
        updateAvailable: false,
        latestVersion: localVersion,
        localVersion,
        reason: 'no_remote_repo',
      };
      writeStatusFile(statusFile, status);
      return status;
    }

    const remoteVersion = await fetchFn(updateUrl, options.timeoutMs || DEFAULT_TIMEOUT_MS);
    const updateAvailable = Boolean(remoteVersion && isNewerVersion(remoteVersion, localVersion));

    const status = {
      lastCheck: now,
      updateAvailable,
      latestVersion: remoteVersion || localVersion,
      localVersion,
    };

    writeStatusFile(statusFile, status);
    return status;
  } catch {
    try {
      const current = fs.existsSync(statusFile)
        ? JSON.parse(fs.readFileSync(statusFile, 'utf8'))
        : {};
      current.lastCheck = now;
      if (localVersion) {
        current.localVersion = localVersion;
        if (current.updateAvailable && (!current.latestVersion || !isNewerVersion(current.latestVersion, localVersion))) {
          current.updateAvailable = false;
        }
      }
      writeStatusFile(statusFile, current);
      return current;
    } catch {
      return null;
    }
  }
}

async function main() {
  await checkForUpdates();
  process.exit(0);
}

if (require.main === module) {
  main();
}

module.exports = {
  parseSemver,
  compareSemver,
  isNewerVersion,
  resolveUpdateUrl,
  fetchRemoteVersion,
  checkForUpdates,
  getStatusFilePath,
};
