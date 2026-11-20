'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');


/**
 * Return all candidate antigravity-cli data roots, ordered by priority:
 *   1. ~/.gemini/antigravity-cli         (standard macOS/Linux install)
 *   2. $XDG_DATA_HOME/antigravity-cli    (Linux XDG override)
 *   3. $APPDATA/antigravity-cli          (Windows roaming)
 *   4. $LOCALAPPDATA/antigravity-cli     (Windows local)
 *
 * Unset env vars are filtered out so the list only contains usable paths.
 *
 * @returns {string[]}
 */
function getAntigravityRoots(env = process.env, homeDir = os.homedir()) {
  const safeEnv = env || process.env;
  const dataDir = safeEnv.AGY_PULSE_DATA_DIR;
  if (dataDir) {
    return [dataDir];
  }

  const resolvedHome = homeDir || os.homedir();
  const candidates = [
    path.join(resolvedHome, '.gemini', 'antigravity-cli'),
    safeEnv.XDG_DATA_HOME
      ? path.join(safeEnv.XDG_DATA_HOME, 'antigravity-cli')
      : null,
    safeEnv.APPDATA
      ? path.join(safeEnv.APPDATA, 'antigravity-cli')
      : null,
    safeEnv.LOCALAPPDATA
      ? path.join(safeEnv.LOCALAPPDATA, 'antigravity-cli')
      : null,
  ].filter(Boolean);

  return candidates;
}

/**
 * Return all candidate Antigravity IDE data roots, ordered by priority:
 *   1. $AGY_PULSE_IDE_DATA_DIR           (explicit IDE data dir override)
 *   2. [] when $AGY_PULSE_DATA_DIR is set (isolates custom sandboxes/tests)
 *   3. ~/.gemini/antigravity              (standard macOS/Linux/Windows install)
 *   4. $XDG_DATA_HOME/antigravity         (Linux XDG override)
 *   5. $APPDATA/antigravity               (Windows roaming)
 *   6. $LOCALAPPDATA/antigravity          (Windows local)
 *
 * Unset env vars and duplicate resolved paths are filtered out.
 *
 * @param {NodeJS.ProcessEnv} [env]
 * @param {string} [homeDir]
 * @returns {string[]}
 */
function getAntigravityIdeRoots(env = process.env, homeDir = os.homedir()) {
  const safeEnv = env || process.env;
  const ideDataDir = typeof safeEnv.AGY_PULSE_IDE_DATA_DIR === 'string'
    ? safeEnv.AGY_PULSE_IDE_DATA_DIR.trim()
    : '';
  if (ideDataDir) {
    return [ideDataDir];
  }

  const dataDir = typeof safeEnv.AGY_PULSE_DATA_DIR === 'string'
    ? safeEnv.AGY_PULSE_DATA_DIR.trim()
    : '';
  if (dataDir) {
    return [];
  }

  const resolvedHome = homeDir || os.homedir();
  const xdg = typeof safeEnv.XDG_DATA_HOME === 'string' ? safeEnv.XDG_DATA_HOME.trim() : '';
  const appData = typeof safeEnv.APPDATA === 'string' ? safeEnv.APPDATA.trim() : '';
  const localAppData = typeof safeEnv.LOCALAPPDATA === 'string' ? safeEnv.LOCALAPPDATA.trim() : '';

  const rawCandidates = [
    path.join(resolvedHome, '.gemini', 'antigravity'),
    xdg ? path.join(xdg, 'antigravity') : null,
    appData ? path.join(appData, 'antigravity') : null,
    localAppData ? path.join(localAppData, 'antigravity') : null,
  ].filter(Boolean);

  const seen = new Set();
  const deduped = [];
  for (const candidate of rawCandidates) {
    const key = path.resolve(candidate);
    if (seen.has(key)) continue;
    seen.add(key);
    deduped.push(candidate);
  }
  return deduped;
}

/**
 * Resolve a relative path under any of the candidate antigravity roots.
 * Returns the first candidate whose joined path exists on disk; if none exist,
 * returns the joined path under the first (highest-priority) candidate so
 * callers can still use it as a write target.
 *
 * @param {string} relativePath
 * @param {NodeJS.ProcessEnv} [env]
 * @param {string} [homeDir]
 * @returns {string}
 */
function resolveAntigravityPath(relativePath, env = process.env, homeDir = os.homedir()) {
  const roots = getAntigravityRoots(env, homeDir);
  for (const root of roots) {
    const candidate = path.join(root, relativePath);
    if (fs.existsSync(candidate)) return candidate;
  }
  return path.join(roots[0], relativePath);
}

/**
 * Safely resolve the absolute path of a system executable without searching CWD.
 * On Windows, it maps standard binaries directly to %SystemRoot%\System32 or WindowsPowerShell,
 * and searches PATH while explicitly ignoring relative path directories.
 *
 * @param {string} name
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {string|null}
 */
function resolveSafeExecutable(name, env = process.env) {
  const safeEnv = env || process.env;
  const platform = process.platform;
  if (platform === 'win32') {
    const systemRoot = safeEnv.SystemRoot || 'C:\\Windows';
    if (name === 'chcp') {
      return path.join(systemRoot, 'System32', 'chcp.com');
    }
    if (name === 'powershell') {
      return path.join(systemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
    }
  }

  const pathEnv = safeEnv.PATH || '';
  const dirs = pathEnv.split(path.delimiter).filter(Boolean);
  const extensions = platform === 'win32' ? ['.exe', '.cmd', '.bat'] : [''];

  for (const dir of dirs) {
    const trimmed = dir.trim();
    if (!trimmed || trimmed === '.' || !path.isAbsolute(trimmed)) continue;

    for (const ext of extensions) {
      const fullPath = path.join(trimmed, name + ext);
      try {
        if (fs.existsSync(fullPath) && fs.statSync(fullPath).isFile()) {
          return fullPath;
        }
      } catch {
        // ignore errors reading path entries
      }
    }
  }
  return null;
}

module.exports = {
  getAntigravityRoots,
  getAntigravityIdeRoots,
  resolveAntigravityPath,
  resolveSafeExecutable,
};
