import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import { mkdtemp, rm } from 'node:fs/promises';
import path from 'node:path';
import { tmpdir } from 'node:os';
import {
  getCostTotals,
  getDailyCostLedgerPath,
  DAILY_COST_WRITE_THROTTLE_MS,
} from '../../runtime/daily-cost.js';

let tempDir;

function deps(now) {
  return { rootDir: () => tempDir, now: () => now };
}

function localDateKey(now) {
  const date = new Date(now);
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${date.getFullYear()}${month}${day}`;
}

function readLedger() {
  return JSON.parse(fs.readFileSync(getDailyCostLedgerPath(tempDir), 'utf8'));
}

const todayUsd = (...args) => getCostTotals(...args)?.todayUsd ?? null;
const NOON = new Date(2026, 6, 15, 12, 0, 0).getTime();
const DAY_MS = 24 * 60 * 60 * 1000;

beforeEach(async () => {
  tempDir = await mkdtemp(path.join(tmpdir(), 'agy-hud-daily-test-'));
});

afterEach(async () => {
  await rm(tempDir, { recursive: true, force: true });
});

test('returns null when nothing has been recorded', () => {
  assert.equal(todayUsd({}, undefined, deps(NOON)), null);
  assert.equal(todayUsd({ session_id: 'a' }, undefined, deps(NOON)), null);
  assert.equal(todayUsd({ cost: { total_cost_usd: 1.5 } }, undefined, deps(NOON)), null);
  assert.equal(todayUsd({ session_id: 42, cost: { total_cost_usd: 1.5 } }, undefined, deps(NOON)), null);
});

test('first sighting records a baseline so only increments count', () => {
  const stdin = { session_id: 'a', cost: { total_cost_usd: 2.5 } };
  assert.equal(todayUsd(stdin, undefined, deps(NOON)), 0);

  stdin.cost.total_cost_usd = 4.0;
  assert.equal(todayUsd(stdin, undefined, deps(NOON + 1000)), 1.5);
});

test('total never regresses when stdin reports a lower cost', () => {
  const stdin = { session_id: 'a', cost: { total_cost_usd: 3.0 } };
  todayUsd(stdin, undefined, deps(NOON));
  stdin.cost.total_cost_usd = 5.0;
  assert.equal(todayUsd(stdin, undefined, deps(NOON + 1000)), 2.0);
  stdin.cost.total_cost_usd = 1.0;
  assert.equal(todayUsd(stdin, undefined, deps(NOON + 2000)), 2.0);
});

test('sums increments across sessions', () => {
  todayUsd({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, undefined, deps(NOON));
  todayUsd({ session_id: 'a', cost: { total_cost_usd: 3.0 } }, undefined, deps(NOON + 1000));
  todayUsd({ session_id: 'b', cost: { total_cost_usd: 0.5 } }, undefined, deps(NOON + 2000));
  const total = todayUsd({ session_id: 'b', cost: { total_cost_usd: 1.5 } }, undefined, deps(NOON + 3000));
  assert.equal(total, 3.0);
});

test('day rollover carries active sessions with baseline reset to last total', () => {
  const yesterday = NOON - DAY_MS;
  todayUsd({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, undefined, deps(yesterday));
  todayUsd({ session_id: 'a', cost: { total_cost_usd: 6.0 } }, undefined, deps(yesterday + 1000));

  // First render today: yesterday increment no longer counts in todayUsd
  assert.equal(todayUsd({ session_id: 'a', cost: { total_cost_usd: 6.0 } }, undefined, deps(NOON)), 0);
  assert.equal(readLedger().date, localDateKey(NOON));

  // The session keeps accruing today from its carried-over baseline
  assert.equal(todayUsd({ session_id: 'a', cost: { total_cost_usd: 7.25 } }, undefined, deps(NOON + 1000)), 1.25);
});

test('drops sessions unseen for more than 24 hours', () => {
  const twoDaysAgo = NOON - 2 * DAY_MS;
  todayUsd({ session_id: 'stale', cost: { total_cost_usd: 9.0 } }, undefined, deps(twoDaysAgo));

  assert.equal(todayUsd({ session_id: 'fresh', cost: { total_cost_usd: 0.5 } }, undefined, deps(NOON)), 0);
  assert.deepEqual(Object.keys(readLedger().sessions), ['fresh']);
});

test('recovers from a corrupt ledger file', () => {
  const ledgerPath = getDailyCostLedgerPath(tempDir);
  fs.mkdirSync(path.dirname(ledgerPath), { recursive: true });
  fs.writeFileSync(ledgerPath, 'not json{');

  assert.equal(todayUsd({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, undefined, deps(NOON)), 0);
  assert.equal(readLedger().date, localDateKey(NOON));
});

test('ignores malformed session entries in the ledger', () => {
  const ledgerPath = getDailyCostLedgerPath(tempDir);
  fs.mkdirSync(path.dirname(ledgerPath), { recursive: true });
  fs.writeFileSync(ledgerPath, JSON.stringify({
    date: localDateKey(NOON),
    sessions: {
      good: { baseline: 1.0, total: 2.5, ts: NOON - 1000 },
      bad: { baseline: 'x', total: -1, ts: null },
    },
  }));

  assert.equal(todayUsd({}, undefined, deps(NOON)), 1.5);
});

test('persists content changes immediately and throttles ts-only refreshes', () => {
  todayUsd({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, undefined, deps(NOON));
  const ledgerPath = getDailyCostLedgerPath(tempDir);
  const firstMtime = fs.statSync(ledgerPath).mtimeMs;

  todayUsd({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, undefined, deps(NOON + 1000));
  assert.equal(fs.statSync(ledgerPath).mtimeMs, firstMtime);

  todayUsd({ session_id: 'a', cost: { total_cost_usd: 2.0 } }, undefined, deps(NOON + 2000));
  assert.equal(readLedger().sessions.a.total, 2.0);

  todayUsd({ session_id: 'a', cost: { total_cost_usd: 2.0 } }, undefined, deps(NOON + DAILY_COST_WRITE_THROTTLE_MS + 3000));
  assert.equal(readLedger().sessions.a.ts, NOON + DAILY_COST_WRITE_THROTTLE_MS + 3000);
});

test('weekly total spans days and follows the quota window', () => {
  const opts = { sevenDayResetAt: new Date(NOON + 2 * DAY_MS) };
  const yesterday = NOON - DAY_MS;

  getCostTotals({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, opts, deps(yesterday));
  getCostTotals({ session_id: 'a', cost: { total_cost_usd: 4.0 } }, opts, deps(yesterday + 1000));

  let totals = getCostTotals({ session_id: 'a', cost: { total_cost_usd: 6.0 } }, opts, deps(NOON));
  assert.equal(totals.todayUsd, 2.0);
  assert.equal(totals.weekUsd, 5.0);

  totals = getCostTotals({ session_id: 'b', cost: { total_cost_usd: 0.5 } }, opts, deps(NOON + DAY_MS + 1000));
  assert.equal(totals.todayUsd, 0);
  assert.equal(totals.weekUsd, 5.0);

  const next = { sevenDayResetAt: new Date(NOON + 8 * DAY_MS) };
  totals = getCostTotals({ session_id: 'b', cost: { total_cost_usd: 2.5 } }, next, deps(NOON + DAY_MS + 2000));
  assert.equal(totals.weekUsd, 2.0);
});

test('computes weekly total using local 7-day rolling window when sevenDayResetAt is null/offline', () => {
  getCostTotals({ session_id: 'a', cost: { total_cost_usd: 1.0 } }, { sevenDayResetAt: null }, deps(NOON));
  let totals = getCostTotals({ session_id: 'a', cost: { total_cost_usd: 3.5 } }, { sevenDayResetAt: null }, deps(NOON + 1000));
  assert.equal(totals.todayUsd, 2.5);
  assert.equal(totals.weekUsd, 2.5);

  // Keep session 'a' active within 24h across days 1..6 so SESSION_MAX_AGE_MS does not evict it
  for (let d = 1; d <= 6; d += 1) {
    getCostTotals({ session_id: 'a', cost: { total_cost_usd: 3.5 } }, { sevenDayResetAt: null }, deps(NOON + d * DAY_MS + 1000));
  }

  // Rolling 7-day window resets after 7 days
  totals = getCostTotals({ session_id: 'a', cost: { total_cost_usd: 4.5 } }, { sevenDayResetAt: null }, deps(NOON + 7 * DAY_MS + 1000));
  assert.equal(totals.weekUsd, 1.0);
});

test('continues accumulating spend after context compaction causes estimated cost to drop', () => {
  const s = { session_id: 'compact-1', cost: { total_cost_usd: 3.0 } };
  getCostTotals(s, undefined, deps(NOON));
  s.cost.total_cost_usd = 5.0; // +2.0
  assert.equal(todayUsd(s, undefined, deps(NOON + 1000)), 2.0);

  // Compaction resets estimated cost down to 1.0
  s.cost.total_cost_usd = 1.0;
  assert.equal(todayUsd(s, undefined, deps(NOON + 2000)), 2.0);

  // Subsequent growth from 1.0 to 2.5 (+1.5) must be added on top of prior 2.0 -> 3.5
  s.cost.total_cost_usd = 2.5;
  const totals = getCostTotals(s, undefined, deps(NOON + 3000));
  assert.equal(totals.todayUsd, 3.5);
  assert.equal(totals.weekUsd, 3.5);
});
