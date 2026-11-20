import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { renderStatusLine } from '../../runtime/renderer.js';

const require = createRequire(import.meta.url);
const { getSessionState } = require('../../runtime/parser.js');

test('renderStatusLine includes cost and speed on line 1 when enabled', () => {
  const state = {
    branch: 'main',
    outputSpeed: 65.4,
    costTotals: {
      todayUsd: 1.25,
      weekUsd: 4.5,
    },
  };
  const agyData = {
    model: { display_name: 'Gemini 3.8 Flash (Medium)' },
    cost: { total_cost_usd: 0.15 },
    context_window: {
      total_input_tokens: 1000,
      total_output_tokens: 200,
      used_percentage: 5,
    },
  };

  // 1. With showCost, showDailyCost, showWeeklyCost, showSpeed enabled in en
  const config = {
    display: {
      showCost: true,
      showDailyCost: true,
      showWeeklyCost: true,
      showSpeed: true,
      useNerdFonts: false,
      unicode: true,
    },
    language: 'en',
  };

  const output = renderStatusLine(state, agyData, config);
  const lines = output.split('\n');
  const line1 = lines[0];

  assert.match(line1, /Cost \$0\.150/);
  assert.match(line1, /Today \$1\.25/);
  assert.match(line1, /Week \$4\.50/);
  assert.match(line1, /out: 65\.4 tok\/s/);

  // 2. In zh language
  const configZh = {
    ...config,
    language: 'zh',
  };
  const outputZh = renderStatusLine(state, agyData, configZh);
  const line1Zh = outputZh.split('\n')[0];

  assert.match(line1Zh, /费用 \$0\.150/);
  assert.match(line1Zh, /今日 \$1\.25/);
  assert.match(line1Zh, /本周 \$4\.50/);
  assert.match(line1Zh, /输出: 65\.4 tok\/s/);
});

test('renderStatusLine omits cost and speed when display flags are false', () => {
  const state = {
    branch: 'main',
    outputSpeed: 65.4,
    costTotals: { todayUsd: 1.25, weekUsd: 4.5 },
  };
  const agyData = {
    model: { display_name: 'Gemini 3.8 Flash (Medium)' },
    cost: { total_cost_usd: 0.15 },
    context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 5 },
  };
  const config = {
    display: {
      showCost: false,
      showDailyCost: false,
      showWeeklyCost: false,
      showSpeed: false,
    },
  };

  const output = renderStatusLine(state, agyData, config);
  const line1 = output.split('\n')[0];

  assert.doesNotMatch(line1, /Cost/);
  assert.doesNotMatch(line1, /Today/);
  assert.doesNotMatch(line1, /Week/);
  assert.doesNotMatch(line1, /tok\/s/);
});

test('renderStatusLine hides model, context bar, and empty line2 when disabled for combined mode', () => {
  const state = {
    branch: 'main',
    currentDir: 'proj',
    outputSpeed: 88.2,
  };
  const agyData = {
    model: { display_name: 'Gemini 3.8 Flash (Medium)' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 5 },
  };
  const config = {
    display: {
      showModel: false,
      showContextBar: false,
      showTokenBar: false,
      showGitBranch: false,
      showBreadcrumbs: false,
      showSpeed: true,
    },
    language: 'zh',
  };

  const output = renderStatusLine(state, agyData, config);
  const lines = output.split('\n');
  assert.equal(lines.length, 1);
  assert.doesNotMatch(lines[0], /Gemini/);
  assert.match(lines[0], /proj/);
  assert.match(lines[0], /输出: 88\.2 tok\/s/);
});

test('renderStatusLine keeps main speed pure and displays per-model child speeds when showSpeed is true', () => {
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 5 },
  };
  const state = {
    branch: 'main',
    outputSpeed: 85.0,
    childModelSpeeds: [
      { model: 'Hydro', speed: 215.4 },
      { model: 'Flash', speed: 310.2 },
    ],
  };

  const unicodeOutput = renderStatusLine(state, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'en',
  });
  const unicodeLine1 = unicodeOutput.split('\n')[0];
  assert.match(unicodeLine1, /out: 85\.0 tok\/s/);
  assert.match(unicodeLine1, /⚡ Hydro: 215\.4 tok\/s · Flash: 310\.2 tok\/s/);

  const asciiOutput = renderStatusLine(
    { ...state, outputSpeed: null, childModelSpeeds: [{ model: 'Hydro', speed: 215.4 }] },
    agyData,
    {
      display: { showSpeed: true, unicode: false, useNerdFonts: false },
      language: 'en',
    }
  );
  const asciiLine1 = asciiOutput.split('\n')[0];
  assert.doesNotMatch(asciiLine1, /out:/);
  assert.match(asciiLine1, /\[sub\] Hydro: 215\.4 tok\/s/);

  const hiddenOutput = renderStatusLine(state, agyData, {
    display: { showSpeed: false, unicode: true, useNerdFonts: false },
  });
  assert.doesNotMatch(hiddenOutput, /Hydro/);
  assert.doesNotMatch(hiddenOutput, /tok\/s/);
});

test('renderStatusLine displays 30-DB streaming average speed and TTFT on line 1 (including fresh empty sessions) and falls back cleanly', () => {
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)', id: 'gemini-4-carbon-high' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
  };
  const modelPerf = {
    Carbon: { model: 'Carbon', avgSpeed: 83.6, avgTtftSec: 10.823, streamSamples: 89, ttftSamples: 89 },
    Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.528, streamSamples: 304, ttftSamples: 304 },
  };

  // Fresh session: no state.outputSpeed, no agyData.cost -> uses modelPerf directly
  const freshState = {
    branch: 'main',
    modelPerf,
    childModelSpeeds: [{ model: 'Hydro', speed: 200.0 }],
  };

  const zhLine1 = renderStatusLine(freshState, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];
  assert.match(zhLine1, /输出: 83\.6 tok\/s \(首字 10\.8s\)/);
  assert.match(zhLine1, /⚡ Hydro: 260\.9 tok\/s \(首字 5\.5s\)/);

  const enAsciiLine1 = renderStatusLine(freshState, agyData, {
    display: { showSpeed: true, unicode: false, useNerdFonts: false },
    language: 'en',
  }).split('\n')[0];
  assert.match(enAsciiLine1, /out: 83\.6 tok\/s \(ttft 10\.8s\)/);
  assert.match(enAsciiLine1, /\[sub\] Hydro: 260\.9 tok\/s \(ttft 5\.5s\)/);

  // TTFT-only fallback when neither avgSpeed nor outputSpeed is available
  const ttftOnlyState = {
    branch: 'main',
    outputSpeed: null,
    modelPerf: {
      Carbon: { model: 'Carbon', avgSpeed: null, avgTtftSec: 0.42, streamSamples: 0, ttftSamples: 2 },
    },
  };
  const ttftOnlyLine1 = renderStatusLine(ttftOnlyState, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];
  assert.match(ttftOnlyLine1, /首字 420ms/);
  assert.doesNotMatch(ttftOnlyLine1, /输出:/);

  // Empty modelPerf preserves legacy output verbatim
  const legacyState = {
    branch: 'main',
    outputSpeed: 65.4,
    modelPerf: {},
  };
  const legacyLine1 = renderStatusLine(legacyState, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'en',
  }).split('\n')[0];
  assert.match(legacyLine1, /out: 65\.4 tok\/s/);
  assert.doesNotMatch(legacyLine1, /ttft/);
});

test('getSessionState populates modelPerf and cold-start swarm childModelSpeeds from isolated SQLite DB', async () => {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-perf-e2e-'));
  try {
    const convId = '11111111-1111-1111-1111-111111111111';
    const workerId = '22222222-2222-2222-2222-222222222222';
    const dbPath = path.join(tempRoot, 'conversations', `${convId}.db`);
    const transcriptPath = path.join(tempRoot, 'brain', convId, '.system_generated', 'logs', 'transcript.jsonl');
    fs.mkdirSync(path.dirname(dbPath), { recursive: true });
    fs.mkdirSync(path.dirname(transcriptPath), { recursive: true });

    const encodeVarint = (val) => {
      let v = BigInt(val);
      const out = [];
      while (v >= 0x80n) {
        out.push(Number((v & 0x7fn) | 0x80n));
        v >>= 7n;
      }
      out.push(Number(v));
      return Buffer.from(out);
    };
    const encVar = (fn, val) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 0n), encodeVarint(val)]);
    const encBytes = (fn, buf) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 2n), encodeVarint(buf.length), buf]);
    const encChat = (outTok, ttftSec, streamSec, displayName) => encBytes(1, Buffer.concat([
      encBytes(4, encVar(3, outTok)),
      encBytes(11, encVar(1, ttftSec)),
      encBytes(12, encVar(1, streamSec)),
      encBytes(21, Buffer.from(displayName, 'utf8')),
    ]));

    const { DatabaseSync } = require('node:sqlite');
    const db = new DatabaseSync(dbPath);
    try {
      db.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
      const stmt = db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)');
      stmt.run(1, encChat(800, 10, 10, 'Gemini 4 Carbon (High)'));
      stmt.run(2, encChat(1250, 5, 5, 'Gemini 4 Hydrogen (High)'));
    } finally {
      db.close();
    }

    const nowIso = new Date().toISOString();
    fs.writeFileSync(transcriptPath, JSON.stringify({
      step_index: 1,
      type: 'GENERIC',
      created_at: nowIso,
      content: JSON.stringify({
        status: 'added',
        members: [{ conversation_id: workerId, name: 'worker-h', model_display_name: 'Gemini 4 Hydrogen (High)' }],
      }),
    }) + '\n');

    const state = await getSessionState(transcriptPath, tempRoot, { roots: [tempRoot] });
    assert.equal(state.modelPerf.Carbon.avgSpeed, 80);
    assert.equal(state.modelPerf.Carbon.avgTtftSec, 10);
    assert.equal(state.modelPerf.Hydro.avgSpeed, 250);
    assert.equal(state.modelPerf.Hydro.avgTtftSec, 5);
    assert.deepEqual(state.childModelSpeeds, [{ model: 'Hydro', speed: 250 }]);

    const line1 = renderStatusLine(state, {
      model: { display_name: 'Gemini 4 Carbon (High)' },
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
    }, {
      display: { showSpeed: true, unicode: true, useNerdFonts: false },
      language: 'zh',
    }).split('\n')[0];
    assert.match(line1, /输出: 80\.0 tok\/s \(首字 10\.0s\)/);
    assert.match(line1, /⚡ Hydro: 250\.0 tok\/s \(首字 5\.0s\)/);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test('renderStatusLine renders single unified totalCostUsd from multi-model costBreakdown and feeds daily/weekly ledger', () => {
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)' },
    cost: { total_usd: 0 },
    context_window: { total_input_tokens: 2339, total_output_tokens: 145309 },
  };
  const state = {
    branch: 'main',
    costBreakdown: {
      totalCostUsd: 21.64,
      mainCostUsd: 9.89,
      childrenCostUsd: 11.75,
      childSessionCount: 15,
      byModel: {
        Carbon: { costUsd: 9.89, inputTokens: 2967648, cacheReadTokens: 25052429, cacheWriteTokens: 0, outputTokens: 145309, steps: 117 },
        Hydro: { costUsd: 11.75, inputTokens: 2810844, cacheReadTokens: 35885061, cacheWriteTokens: 0, outputTokens: 644705, steps: 364 },
      },
    },
    costTotals: {
      todayUsd: 21.64,
      weekUsd: 21.64,
    },
  };

  const output = renderStatusLine(state, agyData, {
    language: 'en',
    display: {
      showCost: true,
      showDailyCost: true,
      showWeeklyCost: true,
      showContextMonitor: true,
      unicode: false,
    },
  }).replace(/\x1b\[[0-9;]*m/g, '');
  const lines = output.split('\n');
  assert.match(lines[0], /Cost \$21\.64 \| Today \$21\.64 \| Week \$21\.64/);
  assert.match(lines[lines.length - 1], /Cost \$21\.64/);
  assert.doesNotMatch(output, /Carbon:\s*\$|Hydro:\s*\$/);
});

test('renderStatusLine persists non-main models from 30-DB modelPerf on fresh empty sessions sorted by lastUpdatedMs and capped at 3', async () => {
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)', id: 'gemini-4-carbon-high' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
  };
  const now = Date.now();
  const state = {
    branch: 'main',
    childModelSpeeds: [],
    modelPerf: {
      Carbon: { model: 'Carbon', avgSpeed: 83.6, avgTtftSec: 10.8, streamSamples: 80, ttftSamples: 80, lastUpdatedMs: now - 1000 },
      Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.5, streamSamples: 300, ttftSamples: 300, lastUpdatedMs: now - 2000 },
      Flash: { model: 'Flash', avgSpeed: 320.1, avgTtftSec: 2.1, streamSamples: 50, ttftSamples: 50, lastUpdatedMs: now - 3000 },
      Opus: { model: 'Opus', avgSpeed: 72.4, avgTtftSec: 8.2, streamSamples: 20, ttftSamples: 20, lastUpdatedMs: now - 4000 },
      Sonnet: { model: 'Sonnet', avgSpeed: 95.0, avgTtftSec: 3.0, streamSamples: 10, ttftSamples: 10, lastUpdatedMs: now - 5000 },
    },
  };

  const zhLine1 = renderStatusLine(state, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];
  assert.match(zhLine1, /输出: 83\.6 tok\/s \(首字 10\.8s\)/);
  assert.match(zhLine1, /⚡ Hydro: 260\.9 tok\/s \(首字 5\.5s\) · Flash: 320\.1 tok\/s \(首字 2\.1s\) · Opus: 72\.4 tok\/s \(首字 8\.2s\)/);
  assert.doesNotMatch(zhLine1, /Sonnet/);

  const hiddenOutput = renderStatusLine(state, agyData, {
    display: { showSpeed: false, unicode: true, useNerdFonts: false },
    language: 'zh',
  });
  assert.doesNotMatch(hiddenOutput, /tok\/s/);
  assert.doesNotMatch(hiddenOutput, /Hydro/);

  // End-to-end with isolated SQLite DBs and a brand-new empty transcript (0 childAgents)
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-empty-session-perf-'));
  try {
    const histConvId = '99999999-9999-9999-9999-999999999999';
    const emptyConvId = '00000000-0000-0000-0000-000000000000';
    const dbPath = path.join(tempRoot, 'conversations', `${histConvId}.db`);
    const emptyTranscript = path.join(tempRoot, 'brain', emptyConvId, '.system_generated', 'logs', 'transcript.jsonl');
    fs.mkdirSync(path.dirname(dbPath), { recursive: true });
    fs.mkdirSync(path.dirname(emptyTranscript), { recursive: true });
    fs.writeFileSync(emptyTranscript, '');

    const encodeVarint = (val) => {
      let v = BigInt(val);
      const out = [];
      while (v >= 0x80n) {
        out.push(Number((v & 0x7fn) | 0x80n));
        v >>= 7n;
      }
      out.push(Number(v));
      return Buffer.from(out);
    };
    const encVar = (fn, val) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 0n), encodeVarint(val)]);
    const encBytes = (fn, buf) => Buffer.concat([encodeVarint((BigInt(fn) << 3n) | 2n), encodeVarint(buf.length), buf]);
    const encChat = (outTok, ttftSec, streamSec, displayName) => encBytes(1, Buffer.concat([
      encBytes(4, encVar(3, outTok)),
      encBytes(11, encVar(1, ttftSec)),
      encBytes(12, encVar(1, streamSec)),
      encBytes(21, Buffer.from(displayName, 'utf8')),
    ]));

    const { DatabaseSync } = require('node:sqlite');
    const db = new DatabaseSync(dbPath);
    try {
      db.exec('CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);');
      const stmt = db.prepare('INSERT INTO gen_metadata (idx, data) VALUES (?, ?)');
      stmt.run(1, encChat(800, 10, 10, 'Gemini 4 Carbon (High)'));
      stmt.run(2, encChat(1250, 5, 5, 'Gemini 4 Hydrogen (High)'));
    } finally {
      db.close();
    }

    const emptyState = await getSessionState(emptyTranscript, tempRoot, { roots: [tempRoot] });
    assert.deepEqual(emptyState.childModelSpeeds, []);
    assert.ok(emptyState.modelPerf.Hydro.lastUpdatedMs > 0);
    const e2eLine1 = renderStatusLine(emptyState, agyData, {
      display: { showSpeed: true, unicode: true, useNerdFonts: false },
      language: 'zh',
    }).split('\n')[0];
    assert.match(e2eLine1, /输出: 80\.0 tok\/s \(首字 10\.0s\)/);
    assert.match(e2eLine1, /⚡ Hydro: 250\.0 tok\/s \(首字 5\.0s\)/);
  } finally {
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

test('renderStatusLine deduplicates child model speed when child model matches mainModelKey', () => {
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)', id: 'gemini-4-carbon-high' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
  };
  const state = {
    branch: 'main',
    modelPerf: {
      Carbon: { model: 'Carbon', avgSpeed: 83.6, avgTtftSec: 10.8, streamSamples: 80, ttftSamples: 80 },
      Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.5, streamSamples: 100, ttftSamples: 100 },
    },
    childModelSpeeds: [
      { model: 'Carbon', speed: 83.6 },
      { model: 'Gemini 4 Carbon (High)', speed: 80.0 },
      { model: 'Hydro', speed: 260.9 },
    ],
  };

  const line1 = renderStatusLine(state, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];

  assert.match(line1, /输出: 83\.6 tok\/s \(首字 10\.8s\)/);
  assert.match(line1, /⚡ Hydro: 260\.9 tok\/s \(首字 5\.5s\)/);
  assert.doesNotMatch(line1, /Carbon: /);
});

test('renderStatusLine hides child and fresh main model speeds/TTFT when turnsAgo exceeds 30', () => {
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)', id: 'gemini-4-carbon-high' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
  };
  const state = {
    branch: 'main',
    steps: 0,
    modelPerf: {
      Carbon: { model: 'Carbon', avgSpeed: 83.6, avgTtftSec: 10.8, streamSamples: 80, ttftSamples: 80, turnsAgo: 45 },
      Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.5, streamSamples: 100, ttftSamples: 100, turnsAgo: 0 },
      Flash: { model: 'Flash', avgSpeed: 310.2, avgTtftSec: 2.1, streamSamples: 40, ttftSamples: 40, turnsAgo: 145 },
      Argon: { model: 'Argon', avgSpeed: 195.0, avgTtftSec: 4.0, streamSamples: 15, ttftSamples: 15, turnsAgo: 185 },
    },
    childModelSpeeds: [
      { model: 'Hydro', speed: 260.9, turnsAgo: 0 },
      { model: 'Flash', speed: 310.2 }, // turnsAgo undefined -> falls back to modelPerf.Flash.turnsAgo (145)
    ],
  };

  const line1 = renderStatusLine(state, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];

  assert.doesNotMatch(line1, /输出: 83\.6/);
  assert.doesNotMatch(line1, /首字 10\.8s/);
  assert.match(line1, /⚡ Hydro: 260\.9 tok\/s \(首字 5\.5s\)/);
  assert.doesNotMatch(line1, /Flash/);
  assert.doesNotMatch(line1, /Argon/);

  // Custom modelSpeedExpireTurns=200 keeps them visible
  const customLine1 = renderStatusLine(state, agyData, {
    display: { showSpeed: true, unicode: true, useNerdFonts: false, modelSpeedExpireTurns: 200 },
    language: 'zh',
  }).split('\n')[0];
  assert.match(customLine1, /输出: 83\.6 tok\/s \(首字 10\.8s\)/);
  assert.match(customLine1, /Flash: 310\.2/);
});

test('renderStatusLine hides main and child model speeds/TTFT when idle exceeds 5 minutes (configurable via modelSpeedExpireMinutes)', () => {
  const baseNow = Date.now();
  const agyData = {
    model: { display_name: 'Gemini 4 Carbon (High)', id: 'gemini-4-carbon-high' },
    plan_tier: 'Pro',
    context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
  };
  const state = {
    branch: 'main',
    steps: 4,
    lastPlannerResponse: { startMs: baseNow - 6 * 60 * 1000, endMs: baseNow - 5 * 60 * 1000 - 10_000, outputTokens: 200 },
    modelLastActiveMs: {
      Carbon: baseNow - 5 * 60 * 1000 - 10_000,
      Hydro: baseNow - 2 * 60 * 1000,
      Flash: baseNow - 6 * 60 * 1000,
    },
    modelPerf: {
      Carbon: { model: 'Carbon', avgSpeed: 83.6, avgTtftSec: 10.8, lastUpdatedMs: baseNow - 5 * 60 * 1000 - 10_000 },
      Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.5, lastUpdatedMs: baseNow - 2 * 60 * 1000 },
      Flash: { model: 'Flash', avgSpeed: 310.2, avgTtftSec: 2.1, lastUpdatedMs: baseNow - 6 * 60 * 1000 },
      Opus: { model: 'Opus', avgSpeed: 72.4, avgTtftSec: 8.2, lastUpdatedMs: baseNow - 7 * 60 * 1000 },
    },
    childModelSpeeds: [
      { model: 'Hydro', speed: 260.9, lastActiveMs: baseNow - 2 * 60 * 1000 },
      { model: 'Flash', speed: 310.2, lastActiveMs: baseNow - 6 * 60 * 1000 },
    ],
  };

  // At baseNow: Carbon (>5m), Flash (>5m), Opus (>5m) hidden; Hydro (2m) stays visible!
  const line1 = renderStatusLine(state, agyData, {
    _now: baseNow,
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];
  assert.doesNotMatch(line1, /输出: 83\.6/);
  assert.doesNotMatch(line1, /首字 10\.8s/);
  assert.match(line1, /⚡ Hydro: 260\.9 tok\/s \(首字 5\.5s\)/);
  assert.doesNotMatch(line1, /Flash/);
  assert.doesNotMatch(line1, /Opus/);

  // At baseNow + 4m: Hydro becomes 6m idle -> also hidden!
  const allExpiredLine1 = renderStatusLine(state, agyData, {
    _now: baseNow + 4 * 60 * 1000,
    display: { showSpeed: true, unicode: true, useNerdFonts: false },
    language: 'zh',
  }).split('\n')[0];
  assert.doesNotMatch(allExpiredLine1, /tok\/s/);
  assert.doesNotMatch(allExpiredLine1, /Hydro/);

  // Custom modelSpeedExpireMinutes=10 keeps Carbon, Hydro, Flash, Opus visible at baseNow
  const customLine1 = renderStatusLine(state, agyData, {
    _now: baseNow,
    display: { showSpeed: true, unicode: true, useNerdFonts: false, modelSpeedExpireMinutes: 10 },
    language: 'zh',
  }).split('\n')[0];
  assert.match(customLine1, /输出: 83\.6 tok\/s \(首字 10\.8s\)/);
  assert.match(customLine1, /Hydro: 260\.9/);
  assert.match(customLine1, /Flash: 310\.2/);
});
