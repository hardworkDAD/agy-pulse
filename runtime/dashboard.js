'use strict';

const os = require('os');
const http = require('http');
const { URL } = require('url');
const { spawn } = require('child_process');
const { loadConfig } = require('./config.js');
const { resolveLanguage } = require('./renderer/lang.js');
const { renderDashboardHtml } = require('./dashboard-ui.js');
const launcher = require('./dashboard-launcher.js');

const DEFAULT_DASHBOARD_PORT = 3789;
const LOOPBACK_HOST = '127.0.0.1';
const VALID_RANGES = new Set(['today', '24h', '7d', '30d', 'month', 'all', 'custom']);
const VALID_GRANULARITIES = new Set(['auto', 'hour', 'day', 'week']);
const VALID_SOURCES = new Set(['all', 'cli', 'ide']);
const VALID_CSV_KINDS = new Set(['timeseries', 'models', 'sessions']);
const SUBCOMMAND_ALIASES = {
  open: 'open',
  stop: 'stop',
  status: 'status',
  'install-shortcut': 'install-shortcut',
  'install-app': 'install-shortcut',
  'uninstall-shortcut': 'uninstall-shortcut',
  'uninstall-app': 'uninstall-shortcut',
};

/**
 * Parse CLI arguments for `agy-pulse --dashboard` and `agy-pulse dashboard <action>`.
 *
 * @param {string[]} [argv]
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {object}
 */
function parseDashboardArgs(argv = [], env = process.env) {
  const args = Array.isArray(argv) ? argv : [];
  let port = DEFAULT_DASHBOARD_PORT;
  let open = true;
  let lang = 'auto';
  let noSync = false;
  let action = 'serve';
  let appModeExplicit = null;
  let backgroundServe = false;
  let idleTimeoutExplicit = null;
  let includeDesktop = false;

  let startIdx = 0;
  while (startIdx < args.length && !String(args[startIdx] ?? '')) {
    startIdx += 1;
  }
  if (startIdx < args.length) {
    const first = String(args[startIdx]);
    if (first === 'dashboard' || first === 'dash' || first === 'ui' || first === '--dashboard') {
      startIdx += 1;
    }
  }
  while (startIdx < args.length && !String(args[startIdx] ?? '')) {
    startIdx += 1;
  }
  if (startIdx < args.length) {
    const maybeAction = String(args[startIdx]);
    if (!maybeAction.startsWith('-') && SUBCOMMAND_ALIASES[maybeAction]) {
      action = SUBCOMMAND_ALIASES[maybeAction];
      startIdx += 1;
    }
  }

  for (let i = startIdx; i < args.length; i++) {
    const arg = String(args[i] || '');
    if (!arg || arg === '--dashboard') continue;
    if (arg === '--no-open') {
      open = false;
    } else if (arg === '--open') {
      open = true;
    } else if (arg === '--no-sync') {
      noSync = true;
    } else if (arg === '--zh') {
      lang = 'zh';
    } else if (arg === '--en') {
      lang = 'en';
    } else if (arg === '--app') {
      appModeExplicit = true;
    } else if (arg === '--browser') {
      appModeExplicit = false;
    } else if (arg === '--background' || arg === '--serve-background') {
      backgroundServe = true;
    } else if (arg === '--desktop') {
      includeDesktop = true;
    } else if (arg === '--idle-timeout') {
      const next = args[i + 1];
      if (next !== undefined) {
        const parsed = Number(next);
        if (Number.isInteger(parsed) && parsed >= 0) {
          idleTimeoutExplicit = parsed;
        }
        i += 1;
      }
    } else if (arg.startsWith('--idle-timeout=')) {
      const raw = arg.slice('--idle-timeout='.length);
      const parsed = Number(raw);
      if (Number.isInteger(parsed) && parsed >= 0) {
        idleTimeoutExplicit = parsed;
      }
    } else if (arg === '--port' || arg === '-p') {
      const next = args[i + 1];
      if (next !== undefined) {
        const parsed = Number(next);
        if (Number.isInteger(parsed) && parsed >= 0 && parsed <= 65535) {
          port = parsed;
        }
        i += 1;
      }
    } else if (arg.startsWith('--port=') || arg.startsWith('-p=')) {
      const raw = arg.slice(arg.indexOf('=') + 1);
      const parsed = Number(raw);
      if (Number.isInteger(parsed) && parsed >= 0 && parsed <= 65535) {
        port = parsed;
      }
    } else if (arg === '--lang') {
      const next = String(args[i + 1] || '').toLowerCase();
      if (next === 'zh' || next === 'en' || next === 'auto') {
        lang = next;
      }
      i += 1;
    } else if (arg.startsWith('--lang=')) {
      const raw = arg.slice('--lang='.length).toLowerCase();
      if (raw === 'zh' || raw === 'en' || raw === 'auto') {
        lang = raw;
      }
    }
  }

  const appMode = appModeExplicit !== null ? appModeExplicit : action === 'open';
  const envIdleRaw = env && env.AGY_PULSE_DASHBOARD_IDLE_SEC !== undefined && env.AGY_PULSE_DASHBOARD_IDLE_SEC !== ''
    ? Number(env.AGY_PULSE_DASHBOARD_IDLE_SEC)
    : NaN;
  const envIdleSec = Number.isInteger(envIdleRaw) && envIdleRaw >= 0 ? envIdleRaw : null;
  const defaultIdleSec = (backgroundServe || action === 'open')
    ? (envIdleSec !== null ? envIdleSec : launcher.DEFAULT_IDLE_TIMEOUT_SEC)
    : 0;
  const idleTimeoutSec = idleTimeoutExplicit !== null ? idleTimeoutExplicit : defaultIdleSec;

  return {
    port,
    open,
    host: LOOPBACK_HOST,
    lang,
    noSync,
    action,
    appMode,
    backgroundServe,
    idleTimeoutSec,
    includeDesktop,
  };
}

function isAllowedHost(hostHeader, actualPort) {
  if (!hostHeader || typeof hostHeader !== 'string') return false;
  const normalized = hostHeader.trim().toLowerCase();
  if (normalized === `127.0.0.1:${actualPort}` || normalized === `localhost:${actualPort}`) {
    return true;
  }
  if (actualPort === 80 && (normalized === '127.0.0.1' || normalized === 'localhost')) {
    return true;
  }
  return false;
}

function isAllowedOrigin(originHeader, actualPort) {
  if (originHeader === undefined || originHeader === null || originHeader === '') {
    return true;
  }
  if (typeof originHeader !== 'string') return false;
  try {
    const parsed = new URL(originHeader.trim());
    if (parsed.protocol !== 'http:') return false;
    const hostname = parsed.hostname.toLowerCase();
    if (hostname !== '127.0.0.1' && hostname !== 'localhost') return false;
    const expectedPort = String(actualPort);
    if (parsed.port === expectedPort) return true;
    if (actualPort === 80 && parsed.port === '') return true;
    return false;
  } catch {
    return false;
  }
}

function parseDateOrEpoch(raw) {
  if (!raw || typeof raw !== 'string') return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return undefined;
  if (/^\d{10,}$/.test(trimmed)) {
    return Number(trimmed);
  }
  return trimmed;
}

function parseOverviewFilter(searchParams) {
  const rawRange = String(searchParams.get('range') || '7d').trim();
  const range = VALID_RANGES.has(rawRange) ? rawRange : '7d';

  const rawGran = String(searchParams.get('granularity') || 'auto').trim();
  const granularity = VALID_GRANULARITIES.has(rawGran) ? rawGran : 'auto';

  const rawSource = String(searchParams.get('source') || 'all').trim().toLowerCase();
  const source = VALID_SOURCES.has(rawSource) ? rawSource : 'all';

  const model = String(searchParams.get('model') || 'all').trim() || 'all';
  const workspace = String(searchParams.get('workspace') || 'all').trim() || 'all';

  const subRaw = searchParams.get('includeSubagents');
  const includeSubagents = !(subRaw === 'false' || subRaw === '0');

  const filter = {
    range,
    granularity,
    source,
    model,
    workspace,
    includeSubagents,
  };

  const fromVal = parseDateOrEpoch(searchParams.get('from'));
  if (fromVal !== undefined) filter.from = fromVal;

  const toVal = parseDateOrEpoch(searchParams.get('to'));
  if (toVal !== undefined) filter.to = toVal;

  return filter;
}

function setSecurityHeaders(res) {
  res.setHeader('X-Content-Type-Options', 'nosniff');
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('X-Frame-Options', 'DENY');
  res.setHeader('Referrer-Policy', 'no-referrer');
}

function sendJson(res, statusCode, payload, extraHeaders = {}) {
  const body = JSON.stringify(payload);
  setSecurityHeaders(res);
  res.writeHead(statusCode, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(body, 'utf8'),
    ...extraHeaders,
  });
  res.end(body);
}

function sendText(res, statusCode, body, contentType, extraHeaders = {}) {
  setSecurityHeaders(res);
  res.writeHead(statusCode, {
    'Content-Type': contentType,
    'Content-Length': Buffer.byteLength(body, 'utf8'),
    ...extraHeaders,
  });
  res.end(body);
}

function listenWithFallback(server, requestedPort, host) {
  return new Promise((resolve, reject) => {
    const attemptListen = (targetPort, allowFallback) => {
      const onError = (err) => {
        server.removeListener('listening', onListening);
        if (err && err.code === 'EADDRINUSE' && allowFallback && targetPort !== 0) {
          attemptListen(0, false);
          return;
        }
        reject(err);
      };
      const onListening = () => {
        server.removeListener('error', onError);
        const addr = server.address();
        resolve(addr && typeof addr === 'object' ? addr.port : targetPort);
      };
      server.once('error', onError);
      server.once('listening', onListening);
      server.listen(targetPort, host);
    };

    attemptListen(requestedPort, true);
  });
}

function resolveLedgerModule(injectedLedger) {
  if (injectedLedger) return injectedLedger;
  return require('./ledger.js');
}

/**
 * Create the local 127.0.0.1 dashboard HTTP server.
 *
 * @param {object} [options]
 * @returns {{ server: http.Server, start: () => Promise<{ url: string, port: number, host: string, syncStats: object|null }>, close: () => Promise<void> }}
 */
function createDashboardServer(options = {}) {
  if (options.host && options.host !== LOOPBACK_HOST) {
    throw new Error(`Dashboard server must bind exclusively to ${LOOPBACK_HOST}`);
  }

  const rawPort = options.port !== undefined && options.port !== null ? Number(options.port) : DEFAULT_DASHBOARD_PORT;
  const requestedPort = Number.isInteger(rawPort) && rawPort >= 0 && rawPort <= 65535 ? rawPort : DEFAULT_DASHBOARD_PORT;
  const noSync = Boolean(options.noSync);
  const deferInitialSync = Boolean(options.deferInitialSync);
  const idleTimeoutMs = Number(options.idleTimeoutMs) > 0 ? Number(options.idleTimeoutMs) : 0;
  const autoSyncIntervalMs = options.autoSyncIntervalMs !== undefined
    ? Number(options.autoSyncIntervalMs)
    : 10000;
  const onIdleClose = typeof options.onIdleClose === 'function' ? options.onIdleClose : null;
  const ledgerOptions = options.ledgerOptions || {};
  const renderHtml = typeof options.renderHtml === 'function' ? options.renderHtml : renderDashboardHtml;
  const defaultLang = options.lang === 'zh' || options.lang === 'en'
    ? options.lang
    : resolveLanguage({ language: options.lang || 'auto' }, options.env || process.env);

  let actualPort = requestedPort;
  let startedInfo = null;
  let startedAt = null;
  let lastSyncAtMs = 0;
  let syncInFlight = null;
  let idleTimer = null;

  function clearIdleTimer() {
    if (idleTimer) {
      clearTimeout(idleTimer);
      idleTimer = null;
    }
  }

  function touchIdleTimer() {
    if (!(idleTimeoutMs > 0) || !server.listening) return;
    clearIdleTimer();
    idleTimer = setTimeout(() => {
      idleTimer = null;
      close().then(() => {
        if (onIdleClose) {
          try {
            onIdleClose();
          } catch {
            // ignore idle callback error
          }
        }
      });
    }, idleTimeoutMs);
    if (typeof idleTimer.unref === 'function') {
      idleTimer.unref();
    }
  }

  async function maybeAutoSync() {
    if (noSync || !(autoSyncIntervalMs > 0)) return;
    if (lastSyncAtMs > 0 && Date.now() - lastSyncAtMs < autoSyncIntervalMs) return;
    if (syncInFlight) {
      await syncInFlight;
      return;
    }
    const ledger = resolveLedgerModule(options.ledger);
    if (typeof ledger.syncAllConversationsToLedger !== 'function') return;
    syncInFlight = (async () => {
      try {
        await ledger.syncAllConversationsToLedger(ledgerOptions);
        lastSyncAtMs = Date.now();
      } catch {
        // Do not block overview rendering on incremental sync failures
      } finally {
        syncInFlight = null;
      }
    })();
    await syncInFlight;
  }

  const server = http.createServer(async (req, res) => {
    try {
      const boundPort = actualPort || (server.address() && server.address().port) || requestedPort;
      if (!isAllowedHost(req.headers.host, boundPort)) {
        return sendJson(res, 403, { ok: false, error: 'Forbidden: invalid Host header' });
      }

      const method = String(req.method || 'GET').toUpperCase();
      const parsedUrl = new URL(req.url || '/', `http://${LOOPBACK_HOST}:${boundPort}`);
      const pathname = parsedUrl.pathname;

      if (pathname === '/api/health') {
        if (method !== 'GET') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        return sendJson(res, 200, {
          ok: true,
          app: launcher.DASHBOARD_APP_ID,
          pid: process.pid,
          port: boundPort,
          idleTimeoutMs,
          startedAt,
        });
      }

      if (pathname === '/api/ping') {
        if (method !== 'POST') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        if (!isAllowedOrigin(req.headers.origin, boundPort)) {
          return sendJson(res, 403, { ok: false, error: 'Forbidden: invalid Origin header' });
        }
        touchIdleTimer();
        return sendJson(res, 200, { ok: true, active: true, ts: Date.now() });
      }

      if (pathname === '/icon.svg') {
        if (method !== 'GET' && method !== 'HEAD') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        setSecurityHeaders(res);
        res.writeHead(200, {
          'Content-Type': 'image/svg+xml; charset=utf-8',
          'Content-Length': Buffer.byteLength(launcher.DASHBOARD_ICON_SVG, 'utf8'),
        });
        res.end(method === 'HEAD' ? undefined : launcher.DASHBOARD_ICON_SVG);
        return;
      }

      if (pathname === '/manifest.webmanifest') {
        if (method !== 'GET' && method !== 'HEAD') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        const manifestBody = JSON.stringify({
          name: 'agy-pulse Telemetry Dashboard',
          short_name: 'agy-pulse',
          start_url: '/',
          display: 'standalone',
          background_color: '#0b1120',
          theme_color: '#0b1120',
          icons: [
            {
              src: '/icon.svg',
              sizes: 'any',
              type: 'image/svg+xml',
              purpose: 'any',
            },
          ],
        }, null, 2);
        setSecurityHeaders(res);
        res.writeHead(200, {
          'Content-Type': 'application/manifest+json; charset=utf-8',
          'Content-Length': Buffer.byteLength(manifestBody, 'utf8'),
        });
        res.end(method === 'HEAD' ? undefined : manifestBody);
        return;
      }

      if (pathname === '/' || pathname === '/index.html') {
        if (method !== 'GET' && method !== 'HEAD') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        touchIdleTimer();
        const queryLang = parsedUrl.searchParams.get('lang');
        const initialLang = queryLang === 'zh' || queryLang === 'en' ? queryLang : defaultLang;
        const html = renderHtml({ initialLang });
        setSecurityHeaders(res);
        res.setHeader(
          'Content-Security-Policy',
          "default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:;"
        );
        res.writeHead(200, {
          'Content-Type': 'text/html; charset=utf-8',
          'Content-Length': Buffer.byteLength(html, 'utf8'),
        });
        res.end(method === 'HEAD' ? undefined : html);
        return;
      }

      if (pathname === '/api/overview') {
        if (method !== 'GET') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        touchIdleTimer();
        await maybeAutoSync();
        const ledger = resolveLedgerModule(options.ledger);
        const filter = parseOverviewFilter(parsedUrl.searchParams);
        const data = await ledger.queryLedger(filter, ledgerOptions);
        return sendJson(res, 200, data);
      }

      if (pathname === '/api/sync') {
        if (method !== 'POST') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        if (!isAllowedOrigin(req.headers.origin, boundPort)) {
          return sendJson(res, 403, { ok: false, error: 'Forbidden: invalid Origin header' });
        }
        touchIdleTimer();
        const ledger = resolveLedgerModule(options.ledger);
        const syncStats = await ledger.syncAllConversationsToLedger(ledgerOptions);
        lastSyncAtMs = Date.now();
        return sendJson(res, 200, {
          ok: syncStats && syncStats.ok !== undefined ? Boolean(syncStats.ok) : true,
          ...(syncStats || {}),
        });
      }

      if (pathname === '/api/export.csv') {
        if (method !== 'GET') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        touchIdleTimer();
        const ledger = resolveLedgerModule(options.ledger);
        const filter = parseOverviewFilter(parsedUrl.searchParams);
        const rawKind = String(parsedUrl.searchParams.get('kind') || '').trim();
        const kind = VALID_CSV_KINDS.has(rawKind) ? rawKind : '';
        const queryResult = await ledger.queryLedger(filter, ledgerOptions);
        const csv = kind
          ? ledger.formatLedgerCsv(queryResult, { kind })
          : ledger.formatLedgerCsv(queryResult);
        const safeRange = filter.range.replace(/[^a-z0-9_-]/gi, '');
        return sendText(res, 200, csv, 'text/csv; charset=utf-8', {
          'Content-Disposition': `attachment; filename="agy-pulse-telemetry-${safeRange}.csv"`,
        });
      }

      if (pathname === '/api/export.json') {
        if (method !== 'GET') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
        touchIdleTimer();
        const ledger = resolveLedgerModule(options.ledger);
        const filter = parseOverviewFilter(parsedUrl.searchParams);
        const queryResult = await ledger.queryLedger(filter, ledgerOptions);
        const safeRange = filter.range.replace(/[^a-z0-9_-]/gi, '');
        return sendText(res, 200, JSON.stringify(queryResult, null, 2), 'application/json; charset=utf-8', {
          'Content-Disposition': `attachment; filename="agy-pulse-telemetry-${safeRange}.json"`,
        });
      }

      return sendJson(res, 404, { ok: false, error: 'Not Found' });
    } catch (err) {
      return sendJson(res, 500, { ok: false, error: err && err.message ? err.message : 'Internal Server Error' });
    }
  });

  async function start() {
    if (startedInfo) return startedInfo;

    let syncStats = null;
    if (!noSync && !deferInitialSync) {
      try {
        const ledger = resolveLedgerModule(options.ledger);
        if (typeof ledger.syncAllConversationsToLedger === 'function') {
          syncStats = await ledger.syncAllConversationsToLedger(ledgerOptions);
          lastSyncAtMs = Date.now();
        }
      } catch (err) {
        syncStats = { ok: false, error: err && err.message ? err.message : String(err) };
      }
    }

    actualPort = await listenWithFallback(server, requestedPort, LOOPBACK_HOST);
    startedAt = new Date().toISOString();
    touchIdleTimer();
    startedInfo = {
      url: `http://${LOOPBACK_HOST}:${actualPort}`,
      port: actualPort,
      host: LOOPBACK_HOST,
      syncStats,
      startedAt,
    };
    return startedInfo;
  }

  function close() {
    clearIdleTimer();
    return new Promise((resolve) => {
      if (!server.listening) {
        startedInfo = null;
        resolve();
        return;
      }
      if (typeof server.closeAllConnections === 'function') {
        server.closeAllConnections();
      }
      server.close(() => {
        startedInfo = null;
        resolve();
      });
    });
  }

  return { server, start, close };
}

function openBrowser(url, platform = process.platform, spawner = spawn) {
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

/**
 * CLI entrypoint for `agy-pulse --dashboard` and `agy-pulse dashboard <action>`.
 *
 * @param {string[]} [argv]
 * @param {object} [options]
 */
async function startDashboardCli(argv = [], options = {}) {
  const env = options.env || process.env;
  const homeDir = options.homeDir || os.homedir();
  const platform = options.platform || process.platform;
  const parsed = parseDashboardArgs(argv, env);
  const write = typeof options.write === 'function' ? options.write : (msg) => console.log(msg);
  const launcherApi = options.launcher ? { ...launcher, ...options.launcher } : launcher;

  let lang = parsed.lang;
  if (lang === 'auto') {
    let config = {};
    try {
      config = await loadConfig(options.cwd || process.cwd());
    } catch {
      config = {};
    }
    lang = resolveLanguage(config, env);
  }
  const isZh = lang === 'zh';

  if (parsed.action === 'open') {
    const running = await launcherApi.ensureBackgroundDashboard({
      port: parsed.port,
      host: LOOPBACK_HOST,
      idleTimeoutSec: parsed.idleTimeoutSec,
      lang,
      noSync: parsed.noSync,
      env,
      homeDir,
      execPath: options.execPath,
      runtimeEntry: options.runtimeEntry,
      spawn: options.spawn,
      probeHealth: options.probeHealth,
      waitTimeoutMs: options.waitTimeoutMs,
      pollIntervalMs: options.pollIntervalMs,
    });

    if (parsed.open && options.open !== false && env.AGY_PULSE_NO_OPEN !== '1') {
      launcherApi.openDashboardWindow(running.url, {
        appMode: parsed.appMode,
        platform,
        env,
        homeDir,
        spawn: options.spawn,
        existsSync: options.existsSync,
        resolveExecutable: options.resolveExecutable,
        fallbackOpenBrowser: options.fallbackOpenBrowser || openBrowser,
      });
    }

    write(isZh
      ? `agy-pulse 本地看板${running.reused ? '已在运行' : '已在后台启动'}: ${running.url} (PID ${running.pid || '?'})`
      : `agy-pulse dashboard ${running.reused ? 'already running' : 'started in background'}: ${running.url} (PID ${running.pid || '?'})`);

    return {
      ...running,
      action: 'open',
    };
  }

  if (parsed.action === 'stop') {
    const res = await launcherApi.stopBackgroundDashboard({
      port: parsed.port,
      host: LOOPBACK_HOST,
      env,
      homeDir,
      probeHealth: options.probeHealth,
      kill: options.kill,
      waitTimeoutMs: options.waitTimeoutMs,
      pollIntervalMs: options.pollIntervalMs,
    });

    if (res.stopped) {
      write(isZh
        ? `已停止 agy-pulse 后台看板 (PID ${res.pid}, ${res.url || `http://${LOOPBACK_HOST}:${res.port}`})`
        : `Stopped background agy-pulse dashboard (PID ${res.pid}, ${res.url || `http://${LOOPBACK_HOST}:${res.port}`})`);
    } else {
      write(isZh
        ? '未检测到正在运行的 agy-pulse 后台看板。'
        : 'No running background agy-pulse dashboard found.');
    }

    return {
      ...res,
      action: 'stop',
    };
  }

  if (parsed.action === 'status') {
    const running = await launcherApi.findRunningDashboard({
      port: parsed.port,
      host: LOOPBACK_HOST,
      env,
      homeDir,
      probeHealth: options.probeHealth,
    });
    const shortcut = launcherApi.getDesktopShortcutStatus({
      platform,
      env,
      homeDir,
    });

    if (running) {
      write(isZh
        ? `看板状态: 运行中 | URL: ${running.url} | PID: ${running.pid || '?'}`
        : `Dashboard: running | URL: ${running.url} | PID: ${running.pid || '?'}`);
    } else {
      write(isZh
        ? '看板状态: 未运行'
        : 'Dashboard: not running');
    }

    if (shortcut.installed) {
      write(isZh
        ? `桌面快捷方式: 已安装 (${shortcut.path})`
        : `Desktop shortcut: installed (${shortcut.path})`);
    } else {
      write(isZh
        ? '桌面快捷方式: 未安装'
        : 'Desktop shortcut: not installed');
    }

    return {
      action: 'status',
      running,
      shortcut,
    };
  }

  if (parsed.action === 'install-shortcut') {
    const res = launcherApi.installDesktopShortcut({
      platform,
      env,
      homeDir,
      execPath: options.execPath,
      runtimeEntry: options.runtimeEntry,
      includeDesktop: parsed.includeDesktop,
      spawnSync: options.spawnSync,
      port: parsed.port !== DEFAULT_DASHBOARD_PORT ? parsed.port : undefined,
    });

    write(isZh
      ? `已安装 agy-pulse Dashboard 桌面快捷方式: ${res.primaryPath}`
      : `Installed agy-pulse Dashboard desktop shortcut: ${res.primaryPath}`);
    write(isZh
      ? '现在可直接在系统应用菜单/桌面点击图标打开看板（空闲自动退出）。'
      : 'You can now launch the dashboard directly from your app menu or desktop icon.');

    return {
      ...res,
      action: 'install-shortcut',
    };
  }

  if (parsed.action === 'uninstall-shortcut') {
    const res = launcherApi.uninstallDesktopShortcut({
      platform,
      env,
      homeDir,
      spawnSync: options.spawnSync,
    });

    if (res.removed.length > 0) {
      write(isZh
        ? `已移除 agy-pulse Dashboard 快捷方式 (${res.removed.length} 项): ${res.removed.join(', ')}`
        : `Removed agy-pulse Dashboard shortcut (${res.removed.length}): ${res.removed.join(', ')}`);
    } else {
      write(isZh
        ? '未发现由 agy-pulse 创建的桌面快捷方式。'
        : 'No agy-pulse Dashboard desktop shortcuts found to remove.');
    }

    return {
      ...res,
      action: 'uninstall-shortcut',
    };
  }

  if (parsed.backgroundServe) {
    const idleTimeoutMs = parsed.idleTimeoutSec * 1000;
    const ownerPid = process.pid;
    let closeServerFn = null;

    const instance = createDashboardServer({
      port: parsed.port,
      host: LOOPBACK_HOST,
      lang,
      noSync: parsed.noSync,
      deferInitialSync: true,
      idleTimeoutMs,
      autoSyncIntervalMs: options.autoSyncIntervalMs,
      ledger: options.ledger,
      ledgerOptions: options.ledgerOptions,
      env,
      onIdleClose: () => {
        launcherApi.clearDashboardStateIfOwner(ownerPid, { env, homeDir });
        if (typeof options.onIdleClose === 'function') {
          options.onIdleClose();
        }
        if (options.exitOnIdle !== false && options.registerSignals !== false) {
          process.exit(0);
        }
      },
    });

    const info = await instance.start();
    launcherApi.writeDashboardState(
      {
        app: launcher.DASHBOARD_APP_ID,
        pid: ownerPid,
        port: info.port,
        host: info.host,
        url: info.url,
        idleTimeoutMs,
        startedAt: info.startedAt,
      },
      { env, homeDir }
    );

    closeServerFn = async () => {
      launcherApi.clearDashboardStateIfOwner(ownerPid, { env, homeDir });
      await instance.close();
    };

    if (options.registerSignals !== false) {
      const shutdown = async () => {
        await closeServerFn();
        process.exit(0);
      };
      process.once('SIGINT', shutdown);
      process.once('SIGTERM', shutdown);
    }

    return {
      ...info,
      pid: ownerPid,
      backgroundServe: true,
      close: closeServerFn,
    };
  }

  const idleTimeoutMs = parsed.idleTimeoutSec > 0 ? parsed.idleTimeoutSec * 1000 : 0;
  const instance = createDashboardServer({
    port: parsed.port,
    host: LOOPBACK_HOST,
    lang,
    noSync: parsed.noSync,
    idleTimeoutMs,
    autoSyncIntervalMs: options.autoSyncIntervalMs,
    ledger: options.ledger,
    ledgerOptions: options.ledgerOptions,
    env,
    onIdleClose: options.onIdleClose,
  });

  const info = await instance.start();

  if (info.syncStats && info.syncStats.ok !== false && !parsed.noSync) {
    const totalDbs = info.syncStats.totalDbs ?? 0;
    const stepsUpserted = info.syncStats.stepsUpserted ?? 0;
    const durationMs = info.syncStats.durationMs ?? 0;
    write(isZh
      ? `agy-pulse 遥测账本已同步: 扫描 ${totalDbs} 个库，归档 ${stepsUpserted} 步 (${durationMs}ms)`
      : `agy-pulse ledger synced: ${totalDbs} DBs scanned, ${stepsUpserted} steps upserted (${durationMs}ms)`);
  }

  write(isZh
    ? `agy-pulse 本地看板已启动: ${info.url}（按 Ctrl+C 退出）`
    : `agy-pulse dashboard listening at ${info.url} (press Ctrl+C to stop)`);

  if (parsed.open && options.open !== false && env.AGY_PULSE_NO_OPEN !== '1') {
    if (parsed.appMode) {
      launcherApi.openDashboardWindow(info.url, {
        appMode: true,
        platform,
        env,
        homeDir,
        spawn: options.spawn,
        existsSync: options.existsSync,
        resolveExecutable: options.resolveExecutable,
        fallbackOpenBrowser: options.fallbackOpenBrowser || openBrowser,
      });
    } else {
      openBrowser(info.url, platform, options.spawn || spawn);
    }
  }

  if (options.registerSignals !== false) {
    const shutdown = async () => {
      await instance.close();
      process.exit(0);
    };
    process.once('SIGINT', shutdown);
    process.once('SIGTERM', shutdown);
  }

  return {
    ...info,
    close: instance.close,
  };
}

module.exports = {
  DEFAULT_DASHBOARD_PORT,
  parseDashboardArgs,
  createDashboardServer,
  startDashboardCli,
  openBrowser,
};
