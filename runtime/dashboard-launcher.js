'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const childProcess = require('child_process');
const paths = require('./paths.js');

const DASHBOARD_APP_ID = 'agy-pulse-dashboard';
const DASHBOARD_STATE_FILENAME = 'agy-pulse-dashboard.json';
const DASHBOARD_SHORTCUT_MARKER = 'agy-pulse-dashboard-shortcut';
const DEFAULT_IDLE_TIMEOUT_SEC = 600;
const DEFAULT_IDLE_TIMEOUT_MS = DEFAULT_IDLE_TIMEOUT_SEC * 1000;

const DASHBOARD_ICON_SVG = [
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64" fill="none">',
  '  <!-- agy-pulse-dashboard-icon -->',
  '  <rect x="2" y="2" width="60" height="60" rx="14" fill="#0b1120" stroke="#10b981" stroke-width="2.5"/>',
  '  <polyline points="10 34 20 34 26 16 34 48 41 26 46 34 54 34" stroke="#10b981" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>',
  '  <circle cx="54" cy="34" r="3" fill="#38bdf8"/>',
  '</svg>',
].join('\n');

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function joinForPlatform(platform, base, ...segments) {
  if (platform === 'win32' && typeof base === 'string' && (/^[a-zA-Z]:/.test(base) || base.includes('\\')) && !base.startsWith('/')) {
    return path.win32.join(base, ...segments);
  }
  return path.join(base, ...segments);
}

function getDashboardStatePath(options = {}) {
  return paths.resolveAntigravityPath(DASHBOARD_STATE_FILENAME, options.env, options.homeDir);
}

function readDashboardState(options = {}) {
  const statePath = getDashboardStatePath(options);
  try {
    if (!fs.existsSync(statePath)) return null;
    const raw = fs.readFileSync(statePath, 'utf8');
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return null;
    }
    return parsed;
  } catch {
    return null;
  }
}

function writeDashboardState(state, options = {}) {
  const statePath = getDashboardStatePath(options);
  const dir = path.dirname(statePath);
  fs.mkdirSync(dir, { recursive: true });

  const payload = {
    app: DASHBOARD_APP_ID,
    pid: state && state.pid !== undefined ? state.pid : process.pid,
    port: state && state.port,
    host: (state && state.host) || '127.0.0.1',
    url: state && state.url,
    idleTimeoutMs: state && state.idleTimeoutMs !== undefined ? state.idleTimeoutMs : DEFAULT_IDLE_TIMEOUT_MS,
    startedAt: (state && state.startedAt) || new Date().toISOString(),
  };

  const tmpPath = `${statePath}.tmp.${process.pid}`;
  fs.writeFileSync(tmpPath, JSON.stringify(payload, null, 2), 'utf8');
  fs.renameSync(tmpPath, statePath);
  return statePath;
}

function clearDashboardStateIfOwner(ownerPid, options = {}) {
  const statePath = getDashboardStatePath(options);
  const current = readDashboardState(options);
  if (!current || ownerPid === undefined || ownerPid === null) {
    return false;
  }
  if (current.pid !== ownerPid) {
    return false;
  }
  try {
    fs.unlinkSync(statePath);
    return true;
  } catch (err) {
    if (err && err.code === 'ENOENT') {
      return false;
    }
    throw err;
  }
}

function probeDashboardHealth({ host = '127.0.0.1', port = 3789, timeoutMs = 400, httpGet } = {}) {
  const getter = typeof httpGet === 'function' ? httpGet : http.get;
  const targetUrl = `http://${host}:${port}/api/health`;

  return new Promise((resolve) => {
    let settled = false;
    let timer = null;
    let req = null;

    const finish = (result) => {
      if (settled) return;
      settled = true;
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      resolve(result);
    };

    if (timeoutMs > 0) {
      timer = setTimeout(() => {
        if (req && typeof req.destroy === 'function') {
          try {
            req.destroy();
          } catch {
            // ignore destroy error
          }
        }
        finish({ alive: false, occupiedByOther: false });
      }, timeoutMs);
      if (typeof timer.unref === 'function') {
        timer.unref();
      }
    }

    try {
      req = getter(
        targetUrl,
        {
          headers: { Host: `${host}:${port}` },
          timeout: timeoutMs,
        },
        (res) => {
          const statusCode = res && res.statusCode;
          const chunks = [];
          res.on('data', (chunk) => chunks.push(chunk));
          res.on('end', () => {
            if (statusCode !== 200) {
              finish({ alive: false, occupiedByOther: true, statusCode });
              return;
            }
            const raw = Buffer.concat(chunks).toString('utf8');
            let payload;
            try {
              payload = JSON.parse(raw);
            } catch {
              finish({ alive: false, occupiedByOther: false });
              return;
            }
            if (payload && payload.ok === true && payload.app === DASHBOARD_APP_ID) {
              finish({
                alive: true,
                occupiedByOther: false,
                url: `http://${host}:${port}`,
                host,
                port: Number(payload.port) || port,
                pid: payload.pid,
                idleTimeoutMs: payload.idleTimeoutMs,
                startedAt: payload.startedAt,
              });
              return;
            }
            finish({ alive: false, occupiedByOther: true, statusCode });
          });
          res.on('error', () => {
            finish({ alive: false, occupiedByOther: false });
          });
        }
      );

      if (req && typeof req.on === 'function') {
        req.on('error', () => {
          finish({ alive: false, occupiedByOther: false });
        });
        req.on('timeout', () => {
          if (typeof req.destroy === 'function') {
            try {
              req.destroy();
            } catch {
              // ignore
            }
          }
          finish({ alive: false, occupiedByOther: false });
        });
      }
    } catch {
      finish({ alive: false, occupiedByOther: false });
    }
  });
}

async function findRunningDashboard(options = {}) {
  const {
    port = 3789,
    host = '127.0.0.1',
    env,
    homeDir,
    probeHealth = probeDashboardHealth,
  } = options;

  const direct = await probeHealth({ host, port });
  if (direct && direct.alive) {
    return direct;
  }

  const state = readDashboardState({ env, homeDir });
  const statePort = state && Number(state.port);
  if (Number.isInteger(statePort) && statePort > 0 && statePort !== Number(port)) {
    const stateHost = (state && state.host) || host;
    const secondary = await probeHealth({ host: stateHost, port: statePort });
    if (secondary && secondary.alive) {
      return secondary;
    }
  }

  return null;
}

async function ensureBackgroundDashboard(options = {}) {
  const {
    port = 3789,
    host = '127.0.0.1',
    idleTimeoutSec = DEFAULT_IDLE_TIMEOUT_SEC,
    lang = 'auto',
    noSync = false,
    env = process.env,
    homeDir = os.homedir(),
    execPath = process.execPath,
    runtimeEntry = path.join(__dirname, 'bin', 'agy-pulse.js'),
    spawn: spawner = childProcess.spawn,
    probeHealth = probeDashboardHealth,
    waitTimeoutMs = 8000,
    pollIntervalMs = 80,
  } = options;

  const existing = await findRunningDashboard({ port, host, env, homeDir, probeHealth });
  if (existing) {
    return { ...existing, reused: true };
  }

  const args = [
    runtimeEntry,
    'dashboard',
    '--serve-background',
    '--no-open',
    '--port',
    String(port),
    '--idle-timeout',
    String(idleTimeoutSec),
  ];
  if (lang && lang !== 'auto') {
    args.push('--lang', String(lang));
  }
  if (noSync) {
    args.push('--no-sync');
  }

  const child = spawner(execPath, args, {
    detached: true,
    stdio: 'ignore',
    windowsHide: true,
    env,
  });
  if (child && typeof child.on === 'function') {
    child.on('error', () => {});
  }
  if (child && typeof child.unref === 'function') {
    child.unref();
  }

  const deadline = Date.now() + Math.max(0, Number(waitTimeoutMs) || 0);
  const interval = Math.max(1, Number(pollIntervalMs) || 80);

  while (true) {
    const running = await findRunningDashboard({ port, host, env, homeDir, probeHealth });
    if (running) {
      return { ...running, reused: false };
    }
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      break;
    }
    await sleep(Math.min(interval, remaining));
  }

  throw new Error(
    `Timed out after ${waitTimeoutMs}ms waiting for background agy-pulse dashboard on ${host}:${port}`
  );
}

function findAppWindowBrowser(options = {}) {
  const {
    platform = process.platform,
    env = process.env,
    homeDir = os.homedir(),
    existsSync = fs.existsSync,
    resolveExecutable = paths.resolveSafeExecutable,
  } = options;

  if (platform === 'darwin') {
    const appNames = [
      'Google Chrome.app',
      'Microsoft Edge.app',
      'Brave Browser.app',
      'Chromium.app',
    ];
    const appDirs = ['/Applications', path.join(homeDir, 'Applications')];
    for (const appName of appNames) {
      for (const dir of appDirs) {
        const appPath = path.join(dir, appName);
        if (existsSync(appPath)) {
          return {
            command: 'open',
            argsForUrl: (url) => ['-na', appPath, '--args', `--app=${url}`, '--new-window'],
            name: appName.replace(/\.app$/, ''),
          };
        }
      }
    }
    return null;
  }

  if (platform === 'win32') {
    const safeEnv = env || {};
    const roots = [
      safeEnv.PROGRAMFILES || safeEnv.ProgramFiles,
      safeEnv['PROGRAMFILES(X86)'] || safeEnv['ProgramFiles(x86)'],
      safeEnv.LOCALAPPDATA || safeEnv.LocalAppData,
    ].filter(Boolean);
    const browsers = [
      { segments: ['Google', 'Chrome', 'Application', 'chrome.exe'], name: 'chrome' },
      { segments: ['Microsoft', 'Edge', 'Application', 'msedge.exe'], name: 'msedge' },
      { segments: ['BraveSoftware', 'Brave-Browser', 'Application', 'brave.exe'], name: 'brave' },
    ];
    for (const browser of browsers) {
      for (const root of roots) {
        const exePath = joinForPlatform('win32', root, ...browser.segments);
        if (existsSync(exePath)) {
          return {
            command: exePath,
            argsForUrl: (url) => [`--app=${url}`, '--new-window'],
            name: browser.name,
          };
        }
      }
    }
    return null;
  }

  // Linux / POSIX
  const linuxNames = [
    'google-chrome',
    'google-chrome-stable',
    'chromium',
    'chromium-browser',
    'microsoft-edge',
    'microsoft-edge-stable',
    'brave-browser',
    'vivaldi',
  ];
  const makeLinuxResult = (command, name) => ({
    command,
    argsForUrl: (url) => [`--app=${url}`, `--class=${DASHBOARD_APP_ID}`, '--new-window'],
    name,
  });

  for (const name of linuxNames) {
    const resolved = typeof resolveExecutable === 'function' ? resolveExecutable(name, env) : null;
    if (resolved) {
      return makeLinuxResult(resolved, name);
    }
    const usrBinPath = `/usr/bin/${name}`;
    if (existsSync(usrBinPath)) {
      return makeLinuxResult(usrBinPath, name);
    }
  }

  const optFallbacks = [
    { path: '/opt/google/chrome/google-chrome', name: 'google-chrome' },
    { path: '/opt/microsoft/msedge/microsoft-edge', name: 'microsoft-edge' },
  ];
  for (const candidate of optFallbacks) {
    if (existsSync(candidate.path)) {
      return makeLinuxResult(candidate.path, candidate.name);
    }
  }

  return null;
}

function defaultOpenBrowser(url, platform = process.platform, spawner = childProcess.spawn) {
  try {
    let child;
    if (platform === 'darwin') {
      child = spawner('open', [url], { detached: true, stdio: 'ignore' });
    } else if (platform === 'win32') {
      child = spawner('cmd', ['/c', 'start', '', url], { detached: true, stdio: 'ignore', windowsHide: true });
    } else {
      child = spawner('xdg-open', [url], { detached: true, stdio: 'ignore' });
    }
    if (child && typeof child.on === 'function') {
      child.on('error', () => {});
    }
    if (child && typeof child.unref === 'function') {
      child.unref();
    }
    return true;
  } catch {
    return false;
  }
}

function openDashboardWindow(url, options = {}) {
  const {
    appMode = true,
    platform = process.platform,
    env = process.env,
    homeDir = os.homedir(),
    spawn: spawner = childProcess.spawn,
    existsSync = fs.existsSync,
    resolveExecutable = paths.resolveSafeExecutable,
    fallbackOpenBrowser = defaultOpenBrowser,
  } = options;

  if (appMode) {
    const found = findAppWindowBrowser({
      platform,
      env,
      homeDir,
      existsSync,
      resolveExecutable,
    });
    if (found) {
      try {
        const child = spawner(found.command, found.argsForUrl(url), {
          detached: true,
          stdio: 'ignore',
          windowsHide: true,
        });
        if (child && typeof child.on === 'function') {
          child.on('error', () => {});
        }
        if (child && typeof child.unref === 'function') {
          child.unref();
        }
        return { ok: true, mode: 'app', browser: found.name };
      } catch {
        // Fall through to standard browser open
      }
    }
  }

  const ok = Boolean(fallbackOpenBrowser(url, platform, spawner));
  return { ok, mode: 'browser', browser: 'default' };
}

async function stopBackgroundDashboard(options = {}) {
  const {
    port = 3789,
    host = '127.0.0.1',
    env,
    homeDir,
    probeHealth = probeDashboardHealth,
    kill = process.kill,
    waitTimeoutMs = 3000,
    pollIntervalMs = 60,
  } = options;

  const running = await findRunningDashboard({ port, host, env, homeDir, probeHealth });
  if (!running) {
    const stale = readDashboardState({ env, homeDir });
    if (stale && stale.pid) {
      let alive = true;
      try {
        kill(stale.pid, 0);
      } catch (err) {
        if (!err || err.code !== 'EPERM') {
          alive = false;
        }
      }
      if (!alive) {
        clearDashboardStateIfOwner(stale.pid, { env, homeDir });
      }
    }
    return { stopped: false, reason: 'not_running' };
  }

  const targetPid = running.pid;
  if (!(Number(targetPid) > 0)) {
    return { stopped: false, reason: 'not_running' };
  }

  try {
    kill(targetPid, 'SIGTERM');
  } catch (err) {
    if (!err || err.code !== 'ESRCH') {
      throw err;
    }
  }

  const deadline = Date.now() + Math.max(0, Number(waitTimeoutMs) || 0);
  const interval = Math.max(1, Number(pollIntervalMs) || 60);
  const targetHost = running.host || host;
  const targetPort = running.port || port;

  while (Date.now() < deadline) {
    const probe = await probeHealth({ host: targetHost, port: targetPort });
    if (!probe || !probe.alive) {
      break;
    }
    const remaining = deadline - Date.now();
    if (remaining <= 0) break;
    await sleep(Math.min(interval, remaining));
  }

  clearDashboardStateIfOwner(targetPid, { env, homeDir });
  return {
    stopped: true,
    pid: targetPid,
    port: targetPort,
    url: running.url,
  };
}

function getDesktopShortcutPaths(options = {}) {
  const {
    platform = process.platform,
    env = process.env,
    homeDir = os.homedir(),
    includeDesktop = false,
  } = options;
  const safeEnv = env || {};

  if (platform === 'darwin') {
    const appBundleDir = path.join(homeDir, 'Applications', 'agy-pulse Dashboard.app');
    const infoPlistPath = path.join(appBundleDir, 'Contents', 'Info.plist');
    const launcherScriptPath = path.join(appBundleDir, 'Contents', 'MacOS', 'agy-pulse-dashboard');
    const iconPath = path.join(appBundleDir, 'Contents', 'Resources', 'agy-pulse-dashboard.svg');
    return {
      platform,
      appBundleDir,
      infoPlistPath,
      launcherScriptPath,
      iconPath,
      primaryPath: appBundleDir,
      includeDesktop: Boolean(includeDesktop),
    };
  }

  if (platform === 'win32') {
    const localAppData = safeEnv.LOCALAPPDATA || safeEnv.LocalAppData || joinForPlatform('win32', homeDir, 'AppData', 'Local');
    const appData = safeEnv.APPDATA || safeEnv.AppData || joinForPlatform('win32', homeDir, 'AppData', 'Roaming');
    const vbsPath = joinForPlatform('win32', localAppData, 'agy-pulse', 'agy-pulse-dashboard.vbs');
    const iconPath = joinForPlatform('win32', localAppData, 'agy-pulse', 'agy-pulse-dashboard.svg');
    const startMenuLnkPath = joinForPlatform(
      'win32',
      appData,
      'Microsoft',
      'Windows',
      'Start Menu',
      'Programs',
      'agy-pulse Dashboard.lnk'
    );
    const desktopLnkPath = joinForPlatform('win32', homeDir, 'Desktop', 'agy-pulse Dashboard.lnk');
    return {
      platform,
      localAppData,
      appData,
      vbsPath,
      iconPath,
      startMenuLnkPath,
      desktopLnkPath,
      primaryPath: startMenuLnkPath,
      includeDesktop: Boolean(includeDesktop),
    };
  }

  // Linux / POSIX
  const xdgDataHome = safeEnv.XDG_DATA_HOME || path.join(homeDir, '.local', 'share');
  const desktopEntryPath = path.join(xdgDataHome, 'applications', 'agy-pulse-dashboard.desktop');
  const iconPath = path.join(xdgDataHome, 'icons', 'hicolor', 'scalable', 'apps', 'agy-pulse-dashboard.svg');
  const desktopCopyPath = path.join(homeDir, 'Desktop', 'agy-pulse-dashboard.desktop');
  return {
    platform,
    xdgDataHome,
    desktopEntryPath,
    iconPath,
    desktopCopyPath,
    primaryPath: desktopEntryPath,
    includeDesktop: Boolean(includeDesktop),
  };
}

function isOurShortcutContent(content) {
  return typeof content === 'string' && content.includes(DASHBOARD_SHORTCUT_MARKER);
}

function escapePowerShellSingleQuoted(str) {
  return String(str).replace(/'/g, "''");
}

function escapeVbsString(str) {
  return String(str).replace(/"/g, '""');
}

function installDesktopShortcut(options = {}) {
  const {
    platform = process.platform,
    env = process.env,
    homeDir = os.homedir(),
    execPath = process.execPath,
    runtimeEntry = path.join(__dirname, 'bin', 'agy-pulse.js'),
    includeDesktop = false,
    spawnSync = childProcess.spawnSync,
    port,
  } = options;

  const shortcutPaths = getDesktopShortcutPaths({ platform, env, homeDir, includeDesktop });
  const portFlag = Number.isInteger(Number(port)) && Number(port) > 0 ? ` --port ${Number(port)}` : '';
  const files = [];

  if (platform === 'darwin') {
    const { appBundleDir, infoPlistPath, launcherScriptPath, iconPath } = shortcutPaths;
    fs.mkdirSync(path.dirname(launcherScriptPath), { recursive: true });
    fs.mkdirSync(path.dirname(iconPath), { recursive: true });

    const plistContent = [
      '<?xml version="1.0" encoding="UTF-8"?>',
      '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
      `<!-- ${DASHBOARD_SHORTCUT_MARKER} -->`,
      '<plist version="1.0">',
      '<dict>',
      '  <key>CFBundleDevelopmentRegion</key>',
      '  <string>en</string>',
      '  <key>CFBundleExecutable</key>',
      '  <string>agy-pulse-dashboard</string>',
      '  <key>CFBundleIdentifier</key>',
      '  <string>dev.agy-pulse.dashboard</string>',
      '  <key>CFBundleName</key>',
      '  <string>agy-pulse Dashboard</string>',
      '  <key>CFBundleDisplayName</key>',
      '  <string>agy-pulse Dashboard</string>',
      '  <key>CFBundlePackageType</key>',
      '  <string>APPL</string>',
      '  <key>CFBundleVersion</key>',
      '  <string>1.0</string>',
      '  <key>LSUIElement</key>',
      '  <true/>',
      '</dict>',
      '</plist>',
      '',
    ].join('\n');

    const scriptContent = [
      '#!/bin/sh',
      `# ${DASHBOARD_SHORTCUT_MARKER}`,
      `exec "${execPath}" "${runtimeEntry}" dashboard open${portFlag}`,
      '',
    ].join('\n');

    fs.writeFileSync(infoPlistPath, plistContent, 'utf8');
    fs.writeFileSync(launcherScriptPath, scriptContent, { encoding: 'utf8', mode: 0o755 });
    fs.chmodSync(launcherScriptPath, 0o755);
    fs.writeFileSync(iconPath, DASHBOARD_ICON_SVG, 'utf8');

    files.push(infoPlistPath, launcherScriptPath, iconPath);
    return {
      installed: true,
      platform,
      files,
      primaryPath: appBundleDir,
    };
  }

  if (platform === 'win32') {
    const { vbsPath, iconPath, startMenuLnkPath, desktopLnkPath } = shortcutPaths;
    fs.mkdirSync(path.dirname(vbsPath), { recursive: true });

    const vbsCmd = `""${escapeVbsString(execPath)}"" ""${escapeVbsString(runtimeEntry)}"" dashboard open${portFlag}`;
    const vbsContent = [
      `' ${DASHBOARD_SHORTCUT_MARKER}`,
      'Set WshShell = CreateObject("WScript.Shell")',
      `WshShell.Run "${vbsCmd}", 0, False`,
      '',
    ].join('\r\n');

    fs.writeFileSync(vbsPath, vbsContent, 'utf8');
    fs.writeFileSync(iconPath, DASHBOARD_ICON_SVG, 'utf8');
    files.push(vbsPath, iconPath);

    const targets = [startMenuLnkPath];
    if (includeDesktop) {
      targets.push(desktopLnkPath);
    }

    const psCommands = [
      "$WshShell = New-Object -ComObject WScript.Shell;",
    ];
    for (const lnkPath of targets) {
      const parentDir = path.dirname(lnkPath);
      psCommands.push(
        `New-Item -ItemType Directory -Force -Path '${escapePowerShellSingleQuoted(parentDir)}' | Out-Null;`,
        `$S = $WshShell.CreateShortcut('${escapePowerShellSingleQuoted(lnkPath)}');`,
        "$S.TargetPath = 'wscript.exe';",
        `$S.Arguments = '"${escapePowerShellSingleQuoted(vbsPath)}"';`,
        `$S.Description = 'agy-pulse Telemetry Dashboard (${DASHBOARD_SHORTCUT_MARKER})';`,
        '$S.Save();'
      );
      files.push(lnkPath);
    }

    try {
      spawnSync(
        'powershell.exe',
        ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', psCommands.join(' ')],
        { stdio: 'ignore', windowsHide: true }
      );
    } catch {
      // Ignore if powershell is unavailable in non-Windows environments
    }

    return {
      installed: true,
      platform,
      files,
      primaryPath: startMenuLnkPath,
    };
  }

  // Linux / POSIX
  const { desktopEntryPath, iconPath, desktopCopyPath } = shortcutPaths;
  fs.mkdirSync(path.dirname(iconPath), { recursive: true });
  fs.mkdirSync(path.dirname(desktopEntryPath), { recursive: true });

  fs.writeFileSync(iconPath, DASHBOARD_ICON_SVG, 'utf8');
  files.push(iconPath);

  const desktopEntryContent = [
    '[Desktop Entry]',
    `# ${DASHBOARD_SHORTCUT_MARKER}`,
    'Type=Application',
    'Version=1.0',
    'Name=agy-pulse Dashboard',
    'Comment=Local Telemetry Dashboard for agy-pulse',
    `Exec="${execPath}" "${runtimeEntry}" dashboard open${portFlag}`,
    `Icon=${iconPath}`,
    'Terminal=false',
    'Categories=Development;Utility;',
    'StartupNotify=true',
    `StartupWMClass=${DASHBOARD_APP_ID}`,
    '',
  ].join('\n');

  fs.writeFileSync(desktopEntryPath, desktopEntryContent, { encoding: 'utf8', mode: 0o755 });
  fs.chmodSync(desktopEntryPath, 0o755);
  files.push(desktopEntryPath);

  if (includeDesktop) {
    try {
      fs.mkdirSync(path.dirname(desktopCopyPath), { recursive: true });
      fs.writeFileSync(desktopCopyPath, desktopEntryContent, { encoding: 'utf8', mode: 0o755 });
      fs.chmodSync(desktopCopyPath, 0o755);
      files.push(desktopCopyPath);
    } catch {
      // Ignore if Desktop directory cannot be created
    }
  }

  try {
    spawnSync('update-desktop-database', [path.dirname(desktopEntryPath)], { stdio: 'ignore' });
  } catch {
    // Ignore if update-desktop-database is missing
  }

  return {
    installed: true,
    platform,
    files,
    primaryPath: desktopEntryPath,
  };
}

function uninstallDesktopShortcut(options = {}) {
  const {
    platform = process.platform,
    env = process.env,
    homeDir = os.homedir(),
    spawnSync = childProcess.spawnSync,
  } = options;

  const shortcutPaths = getDesktopShortcutPaths({ platform, env, homeDir });
  const removed = [];
  const skipped = [];

  if (platform === 'darwin') {
    const { appBundleDir, infoPlistPath, launcherScriptPath } = shortcutPaths;
    if (fs.existsSync(appBundleDir)) {
      let hasMarker = false;
      for (const candidate of [launcherScriptPath, infoPlistPath]) {
        try {
          if (fs.existsSync(candidate) && isOurShortcutContent(fs.readFileSync(candidate, 'utf8'))) {
            hasMarker = true;
            break;
          }
        } catch {
          // ignore read error
        }
      }
      if (hasMarker) {
        fs.rmSync(appBundleDir, { recursive: true, force: true });
        removed.push(appBundleDir);
      } else {
        skipped.push(appBundleDir);
      }
    }
    return { removed, skipped };
  }

  if (platform === 'win32') {
    const { vbsPath, iconPath, startMenuLnkPath, desktopLnkPath } = shortcutPaths;
    let isOurs = false;
    if (fs.existsSync(vbsPath)) {
      try {
        isOurs = isOurShortcutContent(fs.readFileSync(vbsPath, 'utf8'));
      } catch {
        isOurs = false;
      }
    }
    const candidates = [vbsPath, iconPath, startMenuLnkPath, desktopLnkPath];
    for (const filePath of candidates) {
      if (!fs.existsSync(filePath)) continue;
      if (isOurs) {
        fs.unlinkSync(filePath);
        removed.push(filePath);
      } else {
        skipped.push(filePath);
      }
    }
    return { removed, skipped };
  }

  // Linux / POSIX
  const { desktopEntryPath, desktopCopyPath, iconPath } = shortcutPaths;
  let removedEntry = false;
  for (const entryPath of [desktopEntryPath, desktopCopyPath]) {
    if (!fs.existsSync(entryPath)) continue;
    let content = '';
    try {
      content = fs.readFileSync(entryPath, 'utf8');
    } catch {
      content = '';
    }
    if (isOurShortcutContent(content)) {
      fs.unlinkSync(entryPath);
      removed.push(entryPath);
      removedEntry = true;
    } else {
      skipped.push(entryPath);
    }
  }

  if (fs.existsSync(iconPath)) {
    let iconMarked = false;
    try {
      iconMarked = fs.readFileSync(iconPath, 'utf8').includes('agy-pulse-dashboard-icon');
    } catch {
      iconMarked = false;
    }
    if (removedEntry || iconMarked) {
      fs.unlinkSync(iconPath);
      removed.push(iconPath);
    } else {
      skipped.push(iconPath);
    }
  }

  try {
    spawnSync('update-desktop-database', [path.dirname(desktopEntryPath)], { stdio: 'ignore' });
  } catch {
    // Ignore if update-desktop-database is missing
  }

  return { removed, skipped };
}

function getDesktopShortcutStatus(options = {}) {
  const {
    platform = process.platform,
    env = process.env,
    homeDir = os.homedir(),
  } = options;
  const shortcutPaths = getDesktopShortcutPaths({ platform, env, homeDir });

  if (platform === 'darwin') {
    const { appBundleDir, launcherScriptPath, infoPlistPath } = shortcutPaths;
    for (const candidate of [launcherScriptPath, infoPlistPath]) {
      try {
        if (fs.existsSync(candidate) && isOurShortcutContent(fs.readFileSync(candidate, 'utf8'))) {
          return { installed: true, path: appBundleDir };
        }
      } catch {
        // ignore
      }
    }
    return { installed: false, path: null };
  }

  if (platform === 'win32') {
    const { vbsPath, startMenuLnkPath } = shortcutPaths;
    try {
      if (fs.existsSync(vbsPath) && isOurShortcutContent(fs.readFileSync(vbsPath, 'utf8'))) {
        return {
          installed: true,
          path: fs.existsSync(startMenuLnkPath) ? startMenuLnkPath : vbsPath,
        };
      }
    } catch {
      // ignore
    }
    return { installed: false, path: null };
  }

  const { desktopEntryPath, desktopCopyPath } = shortcutPaths;
  for (const candidate of [desktopEntryPath, desktopCopyPath]) {
    try {
      if (fs.existsSync(candidate) && isOurShortcutContent(fs.readFileSync(candidate, 'utf8'))) {
        return { installed: true, path: candidate };
      }
    } catch {
      // ignore
    }
  }
  return { installed: false, path: null };
}

module.exports = {
  DASHBOARD_APP_ID,
  DASHBOARD_STATE_FILENAME,
  DASHBOARD_SHORTCUT_MARKER,
  DEFAULT_IDLE_TIMEOUT_SEC,
  DEFAULT_IDLE_TIMEOUT_MS,
  DASHBOARD_ICON_SVG,
  getDashboardStatePath,
  readDashboardState,
  writeDashboardState,
  clearDashboardStateIfOwner,
  probeDashboardHealth,
  findRunningDashboard,
  ensureBackgroundDashboard,
  findAppWindowBrowser,
  openDashboardWindow,
  stopBackgroundDashboard,
  getDesktopShortcutPaths,
  isOurShortcutContent,
  installDesktopShortcut,
  uninstallDesktopShortcut,
  getDesktopShortcutStatus,
};
