import { test } from 'node:test';
import assert from 'node:assert/strict';
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
  assert.deepEqual(parseDashboardArgs([]), {
    port: DEFAULT_DASHBOARD_PORT,
    open: true,
    host: '127.0.0.1',
    lang: 'auto',
    noSync: false,
  });

  assert.deepEqual(
    parseDashboardArgs(['--dashboard', '--port', '4123', '--no-open', '--no-sync', '--lang', 'zh']),
    {
      port: 4123,
      open: false,
      host: '127.0.0.1',
      lang: 'zh',
      noSync: true,
    }
  );

  assert.deepEqual(
    parseDashboardArgs(['-p', '0', '--lang=en']),
    {
      port: 0,
      open: true,
      host: '127.0.0.1',
      lang: 'en',
      noSync: false,
    }
  );

  assert.equal(parseDashboardArgs(['--port=invalid']).port, DEFAULT_DASHBOARD_PORT);
  assert.equal(parseDashboardArgs(['--port', '70000']).port, DEFAULT_DASHBOARD_PORT);

  assert.deepEqual(
    parseDashboardArgs(['dashboard', '--port', '4123', '--no-open', '--no-sync', '--lang', 'zh']),
    parseDashboardArgs(['--dashboard', '--port', '4123', '--no-open', '--no-sync', '--lang', 'zh'])
  );
  assert.deepEqual(parseDashboardArgs(['dash', '-p', '0']), parseDashboardArgs(['--dashboard', '-p', '0']));
  assert.deepEqual(parseDashboardArgs(['ui', '--lang=en']), parseDashboardArgs(['--dashboard', '--lang=en']));
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
