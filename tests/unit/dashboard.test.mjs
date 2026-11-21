import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import net from 'node:net';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
  DEFAULT_DASHBOARD_PORT,
  parseDashboardArgs,
  createDashboardServer,
  startDashboardCli,
  openBrowser,
} = require('../../runtime/dashboard.js');
const { renderDashboardHtml } = require('../../runtime/dashboard-ui.js');
const {
  DASHBOARD_APP_ID,
  DASHBOARD_ICON_SVG,
  readDashboardState,
} = require('../../runtime/dashboard-launcher.js');

function createSampleQueryResult() {
  return {
    meta: {
      range: '7d',
      fromMs: 1700000000000,
      toMs: 1700604800000,
      granularity: 'day',
      ledgerPath: '/tmp/agy-pulse-ledger.sqlite',
      backend: 'sqlite',
      totalRecordedSteps: 42,
      totalRecordedSessions: 5,
      availableModels: ['Carbon', 'Hydro'],
      availableWorkspaces: ['agy-pulse'],
    },
    summary: {
      totalCostUsd: 1.2345,
      mainCostUsd: 1.0,
      subagentCostUsd: 0.2345,
      totalSteps: 42,
      rootSessionCount: 2,
      subagentSessionCount: 3,
      inputTokens: 12000,
      cacheReadTokens: 88000,
      cacheWriteTokens: 4000,
      outputTokens: 6000,
      totalPromptTokens: 104000,
      totalTokens: 110000,
      cacheHitRate: 84.6,
      avgSpeed: 92.4,
      avgTtftSec: 0.85,
      streamSamples: 38,
      ttftSamples: 40,
    },
    timeseries: [
      {
        bucketKey: '2026-11-19',
        bucketStartMs: 1700352000000,
        label: '11-19',
        costUsd: 0.75,
        mainCostUsd: 0.6,
        subagentCostUsd: 0.15,
        inputTokens: 7000,
        cacheReadTokens: 50000,
        cacheWriteTokens: 2000,
        outputTokens: 3500,
        totalPromptTokens: 59000,
        cacheHitRate: 84.7,
        steps: 24,
        avgSpeed: 90.1,
        avgTtftSec: 0.82,
        byModelCost: { Carbon: 0.5, Hydro: 0.25 },
      },
    ],
    byModel: [
      {
        model: 'Carbon',
        costUsd: 0.9,
        costSharePct: 72.9,
        steps: 30,
        inputTokens: 8000,
        cacheReadTokens: 65000,
        cacheWriteTokens: 3000,
        outputTokens: 4200,
        totalPromptTokens: 76000,
        cacheHitRate: 85.5,
        avgSpeed: 95.0,
        avgTtftSec: 0.8,
        streamSamples: 28,
        ttftSamples: 29,
        lastUsedMs: 1700600000000,
      },
    ],
    sessions: [
      {
        rootConversationId: 'root-conv-1234',
        workspace: '/home/user/agy-pulse',
        workspaceLabel: 'agy-pulse',
        branch: 'main',
        startedAtMs: 1700500000000,
        lastActivityMs: 1700600000000,
        totalCostUsd: 1.2345,
        mainCostUsd: 1.0,
        subagentCostUsd: 0.2345,
        subagentCount: 1,
        steps: 42,
        inputTokens: 12000,
        cacheReadTokens: 88000,
        cacheWriteTokens: 4000,
        outputTokens: 6000,
        totalPromptTokens: 104000,
        cacheHitRate: 84.6,
        avgSpeed: 92.4,
        avgTtftSec: 0.85,
        models: ['Carbon', 'Hydro'],
        children: [
          {
            conversationId: 'sub-conv-5678',
            models: ['Hydro'],
            steps: 10,
            costUsd: 0.2345,
            inputTokens: 2000,
            cacheReadTokens: 18000,
            cacheWriteTokens: 500,
            outputTokens: 1200,
            cacheHitRate: 87.8,
            avgSpeed: 110.2,
            avgTtftSec: 0.55,
            startedAtMs: 1700510000000,
            lastActivityMs: 1700590000000,
          },
        ],
      },
    ],
  };
}

function createMockLedger(overrides = {}) {
  const calls = { query: [], sync: [], csv: [] };
  const sample = createSampleQueryResult();
  return {
    calls,
    ledger: {
      queryLedger(filter, options) {
        calls.query.push({ filter, options });
        return overrides.queryResult || sample;
      },
      syncAllConversationsToLedger(options) {
        calls.sync.push({ options });
        return overrides.syncResult || {
          ok: true,
          totalDbs: 12,
          newlySyncedDbs: 3,
          stepsUpserted: 25,
          durationMs: 18,
          ledgerPath: '/tmp/agy-pulse-ledger.sqlite',
        };
      },
      formatLedgerCsv(queryResult, options) {
        calls.csv.push({ queryResult, options });
        const kind = options?.kind || 'timeseries';
        return `kind,bucket,cost\n${kind},2026-11-19,0.75\n`;
      },
    },
  };
}

function rawRequest({ port, method = 'GET', path = '/', headers = {} }) {
  return new Promise((resolve, reject) => {
    const req = http.request({
      host: '127.0.0.1',
      port,
      method,
      path,
      headers,
    }, (res) => {
      const chunks = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => {
        resolve({
          statusCode: res.statusCode,
          headers: res.headers,
          body: Buffer.concat(chunks).toString('utf8'),
        });
      });
    });
    req.on('error', reject);
    req.end();
  });
}

test('parseDashboardArgs parses defaults and flags accurately', () => {
  assert.deepEqual(parseDashboardArgs([], {}), {
    port: DEFAULT_DASHBOARD_PORT,
    open: true,
    host: '127.0.0.1',
    lang: 'auto',
    noSync: false,
    action: 'serve',
    appMode: false,
    backgroundServe: false,
    idleTimeoutSec: 0,
    includeDesktop: false,
  });

  assert.deepEqual(
    parseDashboardArgs(['--dashboard', '--port', '4123', '--no-open', '--no-sync', '--lang', 'zh'], {}),
    {
      port: 4123,
      open: false,
      host: '127.0.0.1',
      lang: 'zh',
      noSync: true,
      action: 'serve',
      appMode: false,
      backgroundServe: false,
      idleTimeoutSec: 0,
      includeDesktop: false,
    }
  );

  assert.deepEqual(
    parseDashboardArgs(['-p', '0', '--lang=en'], {}),
    {
      port: 0,
      open: true,
      host: '127.0.0.1',
      lang: 'en',
      noSync: false,
      action: 'serve',
      appMode: false,
      backgroundServe: false,
      idleTimeoutSec: 0,
      includeDesktop: false,
    }
  );

  assert.equal(parseDashboardArgs(['--port=invalid'], {}).port, DEFAULT_DASHBOARD_PORT);
  assert.equal(parseDashboardArgs(['--port', '70000'], {}).port, DEFAULT_DASHBOARD_PORT);

  assert.deepEqual(
    parseDashboardArgs(['dashboard', '--port', '4123', '--no-open', '--no-sync', '--lang', 'zh'], {}),
    parseDashboardArgs(['--dashboard', '--port', '4123', '--no-open', '--no-sync', '--lang', 'zh'], {})
  );
  assert.deepEqual(parseDashboardArgs(['dash', '-p', '0'], {}), parseDashboardArgs(['--dashboard', '-p', '0'], {}));
  assert.deepEqual(parseDashboardArgs(['ui', '--lang=en'], {}), parseDashboardArgs(['--dashboard', '--lang=en'], {}));

  // Subcommands and launcher flags
  assert.deepEqual(parseDashboardArgs(['open'], {}), {
    port: DEFAULT_DASHBOARD_PORT,
    open: true,
    host: '127.0.0.1',
    lang: 'auto',
    noSync: false,
    action: 'open',
    appMode: true,
    backgroundServe: false,
    idleTimeoutSec: 600,
    includeDesktop: false,
  });
  assert.equal(parseDashboardArgs(['open', '--browser'], {}).appMode, false);
  assert.equal(parseDashboardArgs(['serve', '--app'], {}).appMode, true);
  assert.equal(parseDashboardArgs(['open'], { AGY_PULSE_DASHBOARD_IDLE_SEC: '180' }).idleTimeoutSec, 180);
  assert.equal(parseDashboardArgs(['open', '--idle-timeout', '45'], { AGY_PULSE_DASHBOARD_IDLE_SEC: '180' }).idleTimeoutSec, 45);
  assert.equal(parseDashboardArgs(['--serve-background', '--idle-timeout=90'], {}).backgroundServe, true);
  assert.equal(parseDashboardArgs(['--serve-background', '--idle-timeout=90'], {}).idleTimeoutSec, 90);
  assert.equal(parseDashboardArgs(['stop'], {}).action, 'stop');
  assert.equal(parseDashboardArgs(['status'], {}).action, 'status');
  assert.equal(parseDashboardArgs(['install-app', '--desktop'], {}).action, 'install-shortcut');
  assert.equal(parseDashboardArgs(['install-app', '--desktop'], {}).includeDesktop, true);
  assert.equal(parseDashboardArgs(['uninstall-app'], {}).action, 'uninstall-shortcut');
});

test('renderDashboardHtml outputs self-contained zero-CDN bilingual HTML with XSS protection', () => {
  const enHtml = renderDashboardHtml({ initialLang: 'en' });
  const zhHtml = renderDashboardHtml({ initialLang: 'zh' });
  const sanitized = renderDashboardHtml({ initialLang: '</script><script>alert(1)</script>' });

  assert.match(enHtml, /^<!DOCTYPE html>/);
  assert.match(enHtml, /<html lang="en"/);
  assert.match(zhHtml, /<html lang="zh-CN"/);
  assert.match(sanitized, /var INITIAL_LANG = "en";/);
  assert.doesNotMatch(sanitized, /alert\(1\)/);

  // Zero external CDN links or scripts
  assert.doesNotMatch(enHtml, /https?:\/\//i);

  // Client-side inline script parses cleanly
  const scriptMatch = enHtml.match(/<script>([\s\S]*?)<\/script>/);
  assert.ok(scriptMatch, 'Inline <script> must be present');
  assert.doesNotThrow(() => new Function(scriptMatch[1]));

  // Core UI sections & controls
  for (const id of [
    'range-pills',
    'source-pills',
    'source-summary-bar',
    'filter-from',
    'filter-to',
    'filter-granularity',
    'filter-model',
    'filter-workspace',
    'filter-subagents',
    'kpi-grid',
    'cost-chart-wrap',
    'perf-chart-wrap',
    'model-tbody',
    'session-tbody',
    'session-search',
    'btn-sync',
    'btn-export-csv',
    'btn-export-json',
    'btn-lang',
    'btn-theme',
  ]) {
    assert.ok(enHtml.includes(`id="${id}"`), `Missing UI element #${id}`);
  }
  assert.ok(enHtml.includes('function escHtml('));
  assert.ok(enHtml.includes('data-source="all"'));
  assert.ok(enHtml.includes('data-source="cli"'));
  assert.ok(enHtml.includes('data-source="ide"'));
  assert.ok(enHtml.includes('badge-source-ide'));
  assert.ok(enHtml.includes('badge-source-cli'));
  assert.ok(enHtml.includes('s.source'));
  assert.ok(enHtml.includes('<link rel="icon" type="image/svg+xml" href="/icon.svg">'));
  assert.ok(enHtml.includes('<link rel="manifest" href="/manifest.webmanifest">'));
  assert.ok(enHtml.includes('/api/ping'));
  assert.ok(enHtml.includes('visibilitychange'));
});

test('createDashboardServer rejects non-loopback host binding', () => {
  assert.throws(() => createDashboardServer({ host: '0.0.0.0' }), /127\.0\.0\.1/);
});

test('createDashboardServer serves HTML, overview API, sync, CSV/JSON exports, and enforces Host/Origin security', async () => {
  const { calls, ledger } = createMockLedger();
  const dashboard = createDashboardServer({ port: 0, ledger });
  const info = await dashboard.start();

  try {
    assert.equal(info.host, '127.0.0.1');
    assert.ok(info.port > 0);
    assert.equal(info.url, `http://127.0.0.1:${info.port}`);
    assert.equal(calls.sync.length, 1);

    const validHost = `127.0.0.1:${info.port}`;

    // 1. GET /
    const rootRes = await rawRequest({ port: info.port, path: '/?lang=zh', headers: { Host: validHost } });
    assert.equal(rootRes.statusCode, 200);
    assert.equal(rootRes.headers['content-type'], 'text/html; charset=utf-8');
    assert.equal(rootRes.headers['x-content-type-options'], 'nosniff');
    assert.equal(rootRes.headers['cache-control'], 'no-store');
    assert.match(rootRes.body, /var INITIAL_LANG = "zh";/);

    // 2. GET /api/overview with full filter query
    const ovRes = await rawRequest({
      port: info.port,
      path: '/api/overview?range=custom&from=2026-11-01&to=2026-11-20&granularity=day&model=Carbon&workspace=agy-pulse&includeSubagents=false',
      headers: { Host: validHost },
    });
    assert.equal(ovRes.statusCode, 200);
    assert.equal(ovRes.headers['content-type'], 'application/json; charset=utf-8');
    const ovJson = JSON.parse(ovRes.body);
    assert.equal(ovJson.summary.totalCostUsd, 1.2345);
    assert.deepEqual(calls.query[0].filter, {
      range: 'custom',
      from: '2026-11-01',
      to: '2026-11-20',
      granularity: 'day',
      source: 'all',
      model: 'Carbon',
      workspace: 'agy-pulse',
      includeSubagents: false,
    });

    // 3. GET /api/overview with epoch ms from/to
    const epochRes = await rawRequest({
      port: info.port,
      path: '/api/overview?range=24h&from=1700000000000&to=1700086400000',
      headers: { Host: `localhost:${info.port}` },
    });
    assert.equal(epochRes.statusCode, 200);
    assert.equal(calls.query[1].filter.from, 1700000000000);
    assert.equal(calls.query[1].filter.to, 1700086400000);
    assert.equal(calls.query[1].filter.includeSubagents, true);

    // 4. POST /api/sync (same-origin allowed, no-origin allowed, cross-origin blocked)
    const syncNoOrigin = await rawRequest({
      port: info.port,
      method: 'POST',
      path: '/api/sync',
      headers: { Host: validHost },
    });
    assert.equal(syncNoOrigin.statusCode, 200);
    assert.equal(JSON.parse(syncNoOrigin.body).stepsUpserted, 25);

    const syncSameOrigin = await rawRequest({
      port: info.port,
      method: 'POST',
      path: '/api/sync',
      headers: { Host: validHost, Origin: `http://localhost:${info.port}` },
    });
    assert.equal(syncSameOrigin.statusCode, 200);

    const syncBadOrigin = await rawRequest({
      port: info.port,
      method: 'POST',
      path: '/api/sync',
      headers: { Host: validHost, Origin: 'https://evil.example' },
    });
    assert.equal(syncBadOrigin.statusCode, 403);

    const syncWrongPortOrigin = await rawRequest({
      port: info.port,
      method: 'POST',
      path: '/api/sync',
      headers: { Host: validHost, Origin: `http://127.0.0.1:${info.port + 1}` },
    });
    assert.equal(syncWrongPortOrigin.statusCode, 403);

    // 5. Host header DNS rebinding protection
    const badHostRes = await rawRequest({
      port: info.port,
      path: '/api/overview',
      headers: { Host: `evil.attacker:${info.port}` },
    });
    assert.equal(badHostRes.statusCode, 403);

    // 6. GET /api/export.csv
    const csvRes = await rawRequest({
      port: info.port,
      path: '/api/export.csv?range=7d&kind=models',
      headers: { Host: validHost },
    });
    assert.equal(csvRes.statusCode, 200);
    assert.equal(csvRes.headers['content-type'], 'text/csv; charset=utf-8');
    assert.match(csvRes.headers['content-disposition'], /attachment; filename="agy-pulse-telemetry-7d\.csv"/);
    assert.match(csvRes.body, /^kind,bucket,cost\nmodels,2026-11-19,0\.75/);

    // 7. GET /api/export.json
    const jsonExportRes = await rawRequest({
      port: info.port,
      path: '/api/export.json?range=30d',
      headers: { Host: validHost },
    });
    assert.equal(jsonExportRes.statusCode, 200);
    assert.equal(jsonExportRes.headers['content-type'], 'application/json; charset=utf-8');
    assert.match(jsonExportRes.headers['content-disposition'], /attachment; filename="agy-pulse-telemetry-30d\.json"/);
    assert.equal(JSON.parse(jsonExportRes.body).meta.backend, 'sqlite');

    // 8. 404 & 405
    const notFound = await rawRequest({ port: info.port, path: '/nope', headers: { Host: validHost } });
    assert.equal(notFound.statusCode, 404);
    const methodNotAllowed = await rawRequest({ port: info.port, method: 'GET', path: '/api/sync', headers: { Host: validHost } });
    assert.equal(methodNotAllowed.statusCode, 405);

    // 9. Source filtering on /api/overview, /api/export.csv, /api/export.json
    const qStart = calls.query.length;
    const ideOv = await rawRequest({
      port: info.port,
      path: '/api/overview?source=ide',
      headers: { Host: validHost },
    });
    assert.equal(ideOv.statusCode, 200);
    assert.equal(calls.query[qStart].filter.source, 'ide');

    const cliOv = await rawRequest({
      port: info.port,
      path: '/api/overview?source=CLI',
      headers: { Host: validHost },
    });
    assert.equal(cliOv.statusCode, 200);
    assert.equal(calls.query[qStart + 1].filter.source, 'cli');

    const badSrcOv = await rawRequest({
      port: info.port,
      path: '/api/overview?source=invalid',
      headers: { Host: validHost },
    });
    assert.equal(badSrcOv.statusCode, 200);
    assert.equal(calls.query[qStart + 2].filter.source, 'all');

    const ideSessCsv = await rawRequest({
      port: info.port,
      path: '/api/export.csv?kind=sessions&source=ide',
      headers: { Host: validHost },
    });
    assert.equal(ideSessCsv.statusCode, 200);
    assert.equal(calls.query[qStart + 3].filter.source, 'ide');
    assert.equal(calls.csv[calls.csv.length - 1].options.kind, 'sessions');

    const ideExportJson = await rawRequest({
      port: info.port,
      path: '/api/export.json?source=ide',
      headers: { Host: validHost },
    });
    assert.equal(ideExportJson.statusCode, 200);
    assert.equal(calls.query[qStart + 4].filter.source, 'ide');
  } finally {
    await dashboard.close();
  }
});

test('createDashboardServer automatically falls back to an ephemeral port on EADDRINUSE', async () => {
  const blocker = net.createServer();
  await new Promise((resolve) => blocker.listen(0, '127.0.0.1', resolve));
  const occupiedPort = blocker.address().port;

  const { ledger } = createMockLedger();
  const dashboard = createDashboardServer({ port: occupiedPort, noSync: true, ledger });

  try {
    const info = await dashboard.start();
    assert.equal(info.host, '127.0.0.1');
    assert.notEqual(info.port, occupiedPort);
    assert.ok(info.port > 0);
    assert.equal(info.syncStats, null);

    const res = await rawRequest({
      port: info.port,
      path: '/api/overview',
      headers: { Host: `127.0.0.1:${info.port}` },
    });
    assert.equal(res.statusCode, 200);
  } finally {
    await dashboard.close();
    await new Promise((resolve) => blocker.close(resolve));
  }
});

test('startDashboardCli syncs, prints localized banner, and openBrowser dispatches per-platform', async () => {
  const { calls, ledger } = createMockLedger();
  const logs = [];
  const cliHandle = await startDashboardCli(['--port', '0', '--no-open', '--lang', 'zh'], {
    ledger,
    registerSignals: false,
    write: (line) => logs.push(line),
  });

  try {
    assert.equal(calls.sync.length, 1);
    assert.ok(cliHandle.port > 0);
    assert.ok(logs.some((l) => l.includes('已同步') && l.includes('25')));
    assert.ok(logs.some((l) => l.includes(cliHandle.url)));
  } finally {
    await cliHandle.close();
  }

  // openBrowser platform coverage
  const spawned = [];
  const fakeSpawn = (cmd, args, opts) => {
    spawned.push({ cmd, args, opts });
    return { on() {}, unref() {} };
  };
  assert.equal(openBrowser('http://127.0.0.1:3789', 'linux', fakeSpawn), true);
  assert.equal(openBrowser('http://127.0.0.1:3789', 'darwin', fakeSpawn), true);
  assert.equal(openBrowser('http://127.0.0.1:3789', 'win32', fakeSpawn), true);
  assert.equal(spawned[0].cmd, 'xdg-open');
  assert.equal(spawned[1].cmd, 'open');
  assert.equal(spawned[2].cmd, 'cmd');

  // openBrowser failure is silently tolerated
  assert.equal(openBrowser('http://127.0.0.1:3789', 'linux', () => { throw new Error('boom'); }), false);
});

test('createDashboardServer serves health, ping, icon.svg, manifest.webmanifest, deferred auto-sync, and idle timeout', async () => {
  const { calls, ledger } = createMockLedger();
  let idleClosed = false;
  const dashboard = createDashboardServer({
    port: 0,
    deferInitialSync: true,
    autoSyncIntervalMs: 10000,
    idleTimeoutMs: 120,
    ledger,
    onIdleClose: () => {
      idleClosed = true;
    },
  });

  const info = await dashboard.start();
  try {
    // deferInitialSync skips sync in start()
    assert.equal(calls.sync.length, 0);
    const validHost = `127.0.0.1:${info.port}`;

    // /api/health returns app identity and rejects invalid Host
    const healthRes = await rawRequest({ port: info.port, path: '/api/health', headers: { Host: validHost } });
    assert.equal(healthRes.statusCode, 200);
    const healthJson = JSON.parse(healthRes.body);
    assert.equal(healthJson.ok, true);
    assert.equal(healthJson.app, DASHBOARD_APP_ID);
    assert.equal(healthJson.pid, process.pid);
    assert.equal(healthJson.port, info.port);
    assert.equal(healthJson.idleTimeoutMs, 120);

    const badHealthHost = await rawRequest({ port: info.port, path: '/api/health', headers: { Host: `evil:${info.port}` } });
    assert.equal(badHealthHost.statusCode, 403);

    // /icon.svg & /manifest.webmanifest
    const iconRes = await rawRequest({ port: info.port, path: '/icon.svg', headers: { Host: validHost } });
    assert.equal(iconRes.statusCode, 200);
    assert.equal(iconRes.headers['content-type'], 'image/svg+xml; charset=utf-8');
    assert.equal(iconRes.body, DASHBOARD_ICON_SVG);

    const manifestRes = await rawRequest({ port: info.port, path: '/manifest.webmanifest', headers: { Host: validHost } });
    assert.equal(manifestRes.statusCode, 200);
    assert.equal(manifestRes.headers['content-type'], 'application/manifest+json; charset=utf-8');
    const manifestJson = JSON.parse(manifestRes.body);
    assert.equal(manifestJson.display, 'standalone');
    assert.equal(manifestJson.icons[0].src, '/icon.svg');

    // First /api/overview triggers auto-sync once, second immediate call within interval does not
    const ov1 = await rawRequest({ port: info.port, path: '/api/overview', headers: { Host: validHost } });
    assert.equal(ov1.statusCode, 200);
    assert.equal(calls.sync.length, 1);

    const ov2 = await rawRequest({ port: info.port, path: '/api/overview', headers: { Host: validHost } });
    assert.equal(ov2.statusCode, 200);
    assert.equal(calls.sync.length, 1);

    // /api/ping validates origin and refreshes idle timer
    const pingBadOrigin = await rawRequest({
      port: info.port,
      method: 'POST',
      path: '/api/ping',
      headers: { Host: validHost, Origin: 'https://evil.example' },
    });
    assert.equal(pingBadOrigin.statusCode, 403);

    const pingOk = await rawRequest({
      port: info.port,
      method: 'POST',
      path: '/api/ping',
      headers: { Host: validHost, Origin: `http://127.0.0.1:${info.port}` },
    });
    assert.equal(pingOk.statusCode, 200);
    assert.equal(JSON.parse(pingOk.body).active, true);

    // Keep querying /api/health every 30ms for 180ms: /api/health must NOT reset idle timer, so server closes
    const deadline = Date.now() + 800;
    while (!idleClosed && Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 30));
      try {
        await rawRequest({ port: info.port, path: '/api/health', headers: { Host: validHost } });
      } catch {
        // expected once closed
      }
    }
    assert.equal(idleClosed, true);
  } finally {
    await dashboard.close();
  }
});

test('startDashboardCli dispatches open, stop, status, install-shortcut, uninstall-shortcut, and --serve-background', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-cli-dash-'));
  const env = { AGY_PULSE_DATA_DIR: tmpDir, XDG_DATA_HOME: path.join(tmpDir, 'xdg') };

  try {
    // 1. open action
    const openLogs = [];
    let openedWindow = null;
    const openRes = await startDashboardCli(['open', '--port', '4105', '--lang', 'en'], {
      env,
      homeDir: tmpDir,
      platform: 'linux',
      write: (l) => openLogs.push(l),
      launcher: {
        ensureBackgroundDashboard: async (opts) => ({
          alive: true,
          reused: false,
          url: `http://127.0.0.1:${opts.port}`,
          port: opts.port,
          pid: 9876,
        }),
        openDashboardWindow: (url, opts) => {
          openedWindow = { url, appMode: opts.appMode };
          return { ok: true, mode: 'app', browser: 'chromium' };
        },
      },
    });
    assert.equal(openRes.action, 'open');
    assert.equal(openRes.pid, 9876);
    assert.deepEqual(openedWindow, { url: 'http://127.0.0.1:4105', appMode: true });
    assert.ok(openLogs.some((l) => l.includes('http://127.0.0.1:4105')));

    // 2. install-shortcut -> status -> uninstall-shortcut
    const installLogs = [];
    const installRes = await startDashboardCli(['install-shortcut', '--lang', 'en'], {
      env,
      homeDir: tmpDir,
      platform: 'linux',
      execPath: '/usr/bin/node',
      runtimeEntry: '/app/agy-pulse.js',
      write: (l) => installLogs.push(l),
    });
    assert.equal(installRes.action, 'install-shortcut');
    assert.equal(installRes.installed, true);

    const statusLogs = [];
    const statusRes = await startDashboardCli(['status', '--lang', 'en'], {
      env,
      homeDir: tmpDir,
      platform: 'linux',
      probeHealth: async () => ({ alive: false, occupiedByOther: false }),
      write: (l) => statusLogs.push(l),
    });
    assert.equal(statusRes.action, 'status');
    assert.equal(statusRes.shortcut.installed, true);
    assert.ok(statusLogs.some((l) => l.includes('installed')));

    const uninstallLogs = [];
    const uninstallRes = await startDashboardCli(['uninstall-shortcut', '--lang', 'en'], {
      env,
      homeDir: tmpDir,
      platform: 'linux',
      write: (l) => uninstallLogs.push(l),
    });
    assert.equal(uninstallRes.action, 'uninstall-shortcut');
    assert.ok(uninstallRes.removed.length >= 1);

    // 3. stop action
    const stopLogs = [];
    const stopRes = await startDashboardCli(['stop', '--lang', 'zh'], {
      env,
      homeDir: tmpDir,
      write: (l) => stopLogs.push(l),
      launcher: {
        stopBackgroundDashboard: async () => ({ stopped: true, pid: 9876, port: 4105, url: 'http://127.0.0.1:4105' }),
      },
    });
    assert.equal(stopRes.action, 'stop');
    assert.equal(stopRes.stopped, true);
    assert.ok(stopLogs.some((l) => l.includes('9876')));

    // 4. --serve-background writes state file and cleans up on close()
    const { calls, ledger } = createMockLedger();
    const bgRes = await startDashboardCli(['--serve-background', '--port', '0', '--idle-timeout', '30'], {
      env,
      homeDir: tmpDir,
      ledger,
      registerSignals: false,
      exitOnIdle: false,
      write: () => {},
    });
    try {
      assert.equal(bgRes.backgroundServe, true);
      assert.equal(calls.sync.length, 0);
      const state = readDashboardState({ env, homeDir: tmpDir });
      assert.equal(state.pid, process.pid);
      assert.equal(state.port, bgRes.port);
      assert.equal(state.idleTimeoutMs, 30000);
    } finally {
      await bgRes.close();
    }
    assert.equal(readDashboardState({ env, homeDir: tmpDir }), null);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
});
