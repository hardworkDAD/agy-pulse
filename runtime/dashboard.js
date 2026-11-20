'use strict';

const http = require('http');
const { URL } = require('url');
const { spawn } = require('child_process');
const { loadConfig } = require('./config.js');
const { resolveLanguage } = require('./renderer/lang.js');
const { renderDashboardHtml } = require('./dashboard-ui.js');

const DEFAULT_DASHBOARD_PORT = 3789;
const LOOPBACK_HOST = '127.0.0.1';
const VALID_RANGES = new Set(['today', '24h', '7d', '30d', 'month', 'all', 'custom']);
const VALID_GRANULARITIES = new Set(['auto', 'hour', 'day', 'week']);
const VALID_SOURCES = new Set(['all', 'cli', 'ide']);
const VALID_CSV_KINDS = new Set(['timeseries', 'models', 'sessions']);

/**
 * Parse CLI arguments for `agy-pulse --dashboard`.
 *
 * @param {string[]} [argv]
 * @returns {{ port: number, open: boolean, host: string, lang: string, noSync: boolean }}
 */
function parseDashboardArgs(argv = []) {
  const args = Array.isArray(argv) ? argv : [];
  let port = DEFAULT_DASHBOARD_PORT;
  let open = true;
  let lang = 'auto';
  let noSync = false;

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

  return {
    port,
    open,
    host: LOOPBACK_HOST,
    lang,
    noSync,
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
  const ledgerOptions = options.ledgerOptions || {};
  const renderHtml = typeof options.renderHtml === 'function' ? options.renderHtml : renderDashboardHtml;
  const defaultLang = options.lang === 'zh' || options.lang === 'en'
    ? options.lang
    : resolveLanguage({ language: options.lang || 'auto' }, options.env || process.env);

  let actualPort = requestedPort;
  let startedInfo = null;

  const server = http.createServer(async (req, res) => {
    try {
      const boundPort = actualPort || (server.address() && server.address().port) || requestedPort;
      if (!isAllowedHost(req.headers.host, boundPort)) {
        return sendJson(res, 403, { ok: false, error: 'Forbidden: invalid Host header' });
      }

      const method = String(req.method || 'GET').toUpperCase();
      const parsedUrl = new URL(req.url || '/', `http://${LOOPBACK_HOST}:${boundPort}`);
      const pathname = parsedUrl.pathname;

      if (pathname === '/' || pathname === '/index.html') {
        if (method !== 'GET' && method !== 'HEAD') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
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
        const ledger = resolveLedgerModule(options.ledger);
        const syncStats = await ledger.syncAllConversationsToLedger(ledgerOptions);
        return sendJson(res, 200, {
          ok: syncStats && syncStats.ok !== undefined ? Boolean(syncStats.ok) : true,
          ...(syncStats || {}),
        });
      }

      if (pathname === '/api/export.csv') {
        if (method !== 'GET') {
          return sendJson(res, 405, { ok: false, error: 'Method Not Allowed' });
        }
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
    if (!noSync) {
      try {
        const ledger = resolveLedgerModule(options.ledger);
        if (typeof ledger.syncAllConversationsToLedger === 'function') {
          syncStats = await ledger.syncAllConversationsToLedger(ledgerOptions);
        }
      } catch (err) {
        syncStats = { ok: false, error: err && err.message ? err.message : String(err) };
      }
    }

    actualPort = await listenWithFallback(server, requestedPort, LOOPBACK_HOST);
    startedInfo = {
      url: `http://${LOOPBACK_HOST}:${actualPort}`,
      port: actualPort,
      host: LOOPBACK_HOST,
      syncStats,
    };
    return startedInfo;
  }

  function close() {
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
 * CLI entrypoint for `agy-pulse --dashboard`.
 *
 * @param {string[]} [argv]
 * @param {object} [options]
 */
async function startDashboardCli(argv = [], options = {}) {
  const parsed = parseDashboardArgs(argv);
  const env = options.env || process.env;
  const write = typeof options.write === 'function' ? options.write : (msg) => console.log(msg);

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

  const instance = createDashboardServer({
    port: parsed.port,
    host: LOOPBACK_HOST,
    lang,
    noSync: parsed.noSync,
    ledger: options.ledger,
    ledgerOptions: options.ledgerOptions,
    env,
  });

  const info = await instance.start();
  const isZh = lang === 'zh';

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
    openBrowser(info.url, options.platform || process.platform, options.spawn || spawn);
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
