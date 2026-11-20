'use strict';

const fs = require('fs');
const path = require('path');
const { resolveAntigravityPath } = require('./paths.js');

function getSettingsPath() {
  return resolveAntigravityPath('settings.json');
}

/**
 * Build the Windows .cmd shim that the statusLine command points to.
 *
 * Single string commands can't both (a) call `node` from PATH and (b) fall back
 * to `C:\Program Files\nodejs\node.exe` — the second path's space breaks any
 * outer quoting that antigravity-cli applies. A `.cmd` file sidesteps quoting
 * entirely: `%~dp0` resolves to the shim's own dir so the script keeps working
 * when the runtime is relocated.
 *
 * @param {string} scriptPath  absolute path to agy-pulse.js
 */
function buildCmdShimContents(scriptPath) {
  const scriptName = path.win32.basename(scriptPath);
  return [
    '@echo off',
    'setlocal',
    `node "%~dp0${scriptName}" %* 2>nul`,
    'if %ERRORLEVEL%==0 exit /b 0',
    `if exist "%LocalAppData%\\Programs\\nodejs\\node.exe" "%LocalAppData%\\Programs\\nodejs\\node.exe" "%~dp0${scriptName}" %*`,
    'if %ERRORLEVEL%==0 exit /b 0',
    `if exist "%ProgramFiles%\\nodejs\\node.exe" "%ProgramFiles%\\nodejs\\node.exe" "%~dp0${scriptName}" %*`,
    '',
  ].join('\r\n');
}

function buildShShimContents() {
  return [
    '@echo off',
    'setlocal EnableExtensions',
    'if /I "%~1"=="-c" (',
    '  set "CMDLINE=%~2"',
    ') else (',
    '  set "CMDLINE=%*"',
    ')',
    'cmd.exe /d /c "%CMDLINE%"',
    'exit /b %ERRORLEVEL%',
    '',
  ].join('\r\n');
}

function getWindowsAgyBinDirs(env = process.env) {
  const dirs = [];

  if (env.LOCALAPPDATA) {
    dirs.push(path.join(env.LOCALAPPDATA, 'agy', 'bin'));
  }

  const pathEntries = (env.PATH || env.Path || '')
    .split(path.delimiter)
    .map(entry => entry.trim())
    .filter(Boolean);

  for (const dir of pathEntries) {
    if (fs.existsSync(path.join(dir, 'agy.exe'))) {
      dirs.push(dir);
    }
  }

  return [...new Set(dirs.map(dir => path.resolve(dir)))];
}

function ensureWindowsShShim(platform = process.platform, env = process.env) {
  if (platform !== 'win32') return [];

  const body = buildShShimContents();
  const written = [];
  for (const dir of getWindowsAgyBinDirs(env)) {
    try {
      if (!fs.existsSync(dir)) continue;
      if (fs.existsSync(path.join(dir, 'sh.exe'))) continue;

      for (const name of ['sh.cmd', 'sh.bat']) {
        const target = path.join(dir, name);
        let existing = null;
        try { existing = fs.readFileSync(target, 'utf8'); } catch {}
        if (existing === body) continue;
        if (existing !== null) continue;

        fs.writeFileSync(target, body, 'utf8');
        written.push(target);
      }
    } catch {
      // Best-effort: statusline still gets configured, and agy logs the failure.
    }
  }
  return written;
}

function writeCmdShim(scriptPath, platform = process.platform) {
  if (platform !== 'win32') return null;
  const shimPath = scriptPath.replace(/\.js$/i, '.cmd');
  const contents = buildCmdShimContents(scriptPath);
  fs.mkdirSync(path.dirname(shimPath), { recursive: true });
  let existing = null;
  try { existing = fs.readFileSync(shimPath, 'utf8'); } catch {}
  if (existing !== contents) {
    fs.writeFileSync(shimPath, contents, 'utf8');
  }
  return shimPath;
}

function createStatusLineCommand(scriptPath, nodePath = process.execPath || 'node', platform = process.platform) {
  if (platform === 'win32') {
    return scriptPath.replace(/\.js$/i, '.cmd');
  }
  return `"${nodePath}" "${scriptPath}"`;
}

function resolveStatusLineScriptPath(baseDir) {
  return path.resolve(baseDir, 'bin', 'agy-pulse.js');
}

function buildCombinedShimContents(scriptPath, nodePath = process.execPath || 'node') {
  const safeDefaultNode = String(nodePath || 'node').replace(/'/g, `'\\''`);
  const safeScriptPath = String(scriptPath).replace(/"/g, '\\"');
  return [
    '#!/usr/bin/env bash',
    `exec "$(_n="\${AGY_PULSE_NODE:-\${AGY_STATUSLINE_NODE:-}}"; if [ -n "$_n" ] && command -v "$_n" >/dev/null 2>&1; then printf '%s' "$_n"; else command -v node 2>/dev/null || printf '%s' '${safeDefaultNode}'; fi)" "${safeScriptPath}" "$@"`,
    '',
  ].join('\n');
}

function updateLegacyCombinedShim(
  rootDir,
  scriptPath,
  nodePath = process.execPath || '/usr/bin/node',
  platform = process.platform
) {
  if (!rootDir) return null;
  const shimPath = path.join(rootDir, 'statusline-combined.sh');
  let stat;
  try {
    stat = fs.lstatSync(shimPath);
  } catch {
    return null;
  }

  const contents = buildCombinedShimContents(scriptPath, nodePath);
  let existing = null;
  try {
    if (stat.isSymbolicLink()) {
      fs.unlinkSync(shimPath);
    } else {
      existing = fs.readFileSync(shimPath, 'utf8');
    }
  } catch {}

  if (existing !== contents) {
    fs.writeFileSync(shimPath, contents, 'utf8');
  }
  if (platform !== 'win32') {
    try { fs.chmodSync(shimPath, 0o755); } catch {}
  }
  return shimPath;
}

function configureStatusLine(baseDir = __dirname, options = {}) {
  const settingsPath = options.settingsPath || getSettingsPath();
  const scriptPath = resolveStatusLineScriptPath(baseDir);

  writeCmdShim(scriptPath);
  ensureWindowsShShim();
  updateLegacyCombinedShim(path.dirname(settingsPath), scriptPath);

  const targetCommand = createStatusLineCommand(scriptPath);

  let settings = {};
  if (fs.existsSync(settingsPath)) {
    const raw = fs.readFileSync(settingsPath, 'utf8');
    if (raw.trim()) {
      try {
        settings = JSON.parse(raw);
        if (!settings || typeof settings !== 'object' || Array.isArray(settings)) {
          settings = {};
        }
      } catch {
        try {
          fs.copyFileSync(settingsPath, `${settingsPath}.bak`);
        } catch {}
        settings = {};
      }
    }
  }

  const existingStatusLine = (settings.statusLine && typeof settings.statusLine === 'object')
    ? settings.statusLine
    : {};

  if (existingStatusLine.command !== targetCommand || existingStatusLine.type !== 'command') {
    settings.statusLine = {
      ...existingStatusLine,
      type: 'command',
      command: targetCommand,
    };
    fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
    fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2), 'utf8');
  }

  return { settingsPath, command: targetCommand };
}

module.exports = {
  buildCmdShimContents,
  buildShShimContents,
  buildCombinedShimContents,
  writeCmdShim,
  ensureWindowsShShim,
  updateLegacyCombinedShim,
  getWindowsAgyBinDirs,
  createStatusLineCommand,
  resolveStatusLineScriptPath,
  configureStatusLine,
  getSettingsPath,
};
