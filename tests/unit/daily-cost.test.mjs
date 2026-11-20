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
  tempDir = await mkdtemp(path.join(tmpdir(), 'agy-pulse-daily-test-'));
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

test('cleans stale lock files and preserves active locks held by another process', () => {
  const ledgerPath = getDailyCostLedgerPath(tempDir);
  const lockPath = `${ledgerPath}.lock`;
  fs.mkdirSync(path.dirname(ledgerPath), { recursive: true });

  // Stale lock (> 5000ms old) should be cleaned up and released
  fs.writeFileSync(lockPath, '999999');
  const staleSec = (Date.now() - 10_000) / 1000;
  fs.utimesSync(lockPath, staleSec, staleSec);
  const t1 = getCostTotals({ session_id: 'lock-stale', cost: { total_cost_usd: 1.0 } }, undefined, deps(NOON));
  assert.equal(t1.todayUsd, 0);
  assert.equal(fs.existsSync(lockPath), false);

  // Fresh active lock should NOT be deleted by this process after degraded write
  fs.writeFileSync(lockPath, '888888');
  const t2 = getCostTotals({ session_id: 'lock-stale', cost: { total_cost_usd: 2.5 } }, undefined, deps(NOON + 1000));
  assert.equal(t2.todayUsd, 1.5);
  assert.equal(fs.existsSync(lockPath), true);
});

test('tolerates 60s week.start clock jitter without resetting week and avoids double-counting concurrently revived evicted sessions', () => {
  const opts = { sevenDayResetAt: new Date(NOON + 7 * DAY_MS) };
  getCostTotals({ session_id: 'active', cost: { total_cost_usd: 1.0 } }, opts, deps(NOON));

  // Small 10s forward jitter in sevenDayResetAt (< 60s) must NOT reset the week
  const jittered = { sevenDayResetAt: new Date(NOON + 7 * DAY_MS + 10_000) };
  const afterJitter = getCostTotals({ session_id: 'active', cost: { total_cost_usd: 3.0 } }, jittered, deps(NOON + 20_000));
  assert.equal(afterJitter.weekUsd, 2.0);

  // Evicted session concurrently refreshed on disk must not be double-counted in week.carry + sessions
  const ledgerPath = getDailyCostLedgerPath(tempDir);
  const stored = readLedger();
  stored.sessions.staleRace = {
    baseline: 1.0,
    weekBaseline: 1.0,
    total: 5.0, // $4.0 week spend
    lastObservedCost: 5.0,
    ts: NOON - DAY_MS - 1000, // expired (> 24h) when NOON + 30_000 runs
  };
  fs.writeFileSync(ledgerPath, JSON.stringify(stored, null, 2));

  const racingStdin = { session_id: 'active' };
  Object.defineProperty(racingStdin, 'cost', {
    get() {
      // Simulate concurrent process refreshing 'staleRace' on disk before mergeDiskLedgerBeforeWrite
      const disk = readLedger();
      disk.sessions.staleRace.ts = NOON + 29_000;
      fs.writeFileSync(ledgerPath, JSON.stringify(disk, null, 2));
      return { total_cost_usd: 3.0 };
    },
  });

  const totals = getCostTotals(racingStdin, undefined, deps(NOON + 30_000));
  // active ($2.0) + staleRace ($4.0) = $6.0 (NOT $10.0!)
  assert.equal(totals.weekUsd, 6.0);
});

test('normalizes /brain/<uuid>/ sessionId across conversation_id and transcriptPath and rejects shell brain path', () => {
  const uuid = 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee';
  const transcriptPath = `/home/user/.gemini/antigravity/brain/${uuid}/.system_generated/logs/transcript.jsonl`;

  // 1. First call via conversation_id
  getCostTotals(
    { conversation_id: uuid, cost: { total_cost_usd: 1.0 } },
    undefined,
    deps(NOON),
    { transcriptPath }
  );
  // 2. Second call via transcriptPath only (no session_id / conversation_id) -> hits SAME uuid key
  const totals = getCostTotals(
    {},
    undefined,
    deps(NOON + 1000),
    { transcriptPath, costBreakdown: { totalCostUsd: 2.5, mainCostUsd: 2.5, childrenCostUsd: 0 } }
  );
  assert.equal(totals.todayUsd, 1.5);
  assert.deepEqual(Object.keys(readLedger().sessions), [uuid]);

  // 3. Shell path `/brain/.system_generated/logs/transcript.jsonl` must NOT become shared sessionId
  const shellPath = '/home/user/.gemini/antigravity/brain/.system_generated/logs/transcript.jsonl';
  assert.equal(
    getCostTotals(
      { cost: { total_cost_usd: 3.0 } },
      undefined,
      deps(NOON + 2000),
      { transcriptPath: shellPath }
    )?.todayUsd,
    1.5 // unchanged; anonymous shell path ignored
  );

  // With workspace cwd, shell path isolates by cwd:<dir>
  getCostTotals(
    { cwd: '/tmp/ws-a', cost: { total_cost_usd: 1.0 } },
    undefined,
    deps(NOON + 3000),
    { transcriptPath: shellPath }
  );
  const withCwd = getCostTotals(
    { cwd: '/tmp/ws-a', cost: { total_cost_usd: 1.75 } },
    undefined,
    deps(NOON + 4000),
    { transcriptPath: shellPath }
  );
  assert.equal(withCwd.todayUsd, 2.25);
  assert.ok(readLedger().sessions['cwd:/tmp/ws-a']);
  assert.equal(readLedger().sessions[shellPath], undefined);
});

test('prioritizes state.costBreakdown with childrenCostUsd over main-only native cost and ignores <50% jitter dips', () => {
  const s = {
    session_id: 'child-plus-jitter',
    cost: { total_cost_usd: 1.0 },
  };
  const state1 = {
    costBreakdown: { totalCostUsd: 3.0, mainCostUsd: 1.0, childrenCostUsd: 2.0 },
  };
  getCostTotals(s, undefined, deps(NOON), state1);

  // Main + child grows to $5.00 (+2.00)
  const state2 = {
    costBreakdown: { totalCostUsd: 5.0, mainCostUsd: 1.5, childrenCostUsd: 3.5 },
  };
  let totals = getCostTotals({ session_id: 'child-plus-jitter', cost: { total_cost_usd: 1.5 } }, undefined, deps(NOON + 1000), state2);
  assert.equal(totals.todayUsd, 2.0);

  // Minor estimation dip (<50% drop: 5.0 -> 4.2) then rebound back to 5.0 must NOT double-count +0.8
  const stateDip = { costBreakdown: { totalCostUsd: 4.2, mainCostUsd: 1.5, childrenCostUsd: 2.7 } };
  totals = getCostTotals({ session_id: 'child-plus-jitter', cost: { total_cost_usd: 1.5 } }, undefined, deps(NOON + 2000), stateDip);
  assert.equal(totals.todayUsd, 2.0);
  totals = getCostTotals({ session_id: 'child-plus-jitter', cost: { total_cost_usd: 1.5 } }, undefined, deps(NOON + 3000), state2);
  assert.equal(totals.todayUsd, 2.0);
});

test('mergeDiskLedgerBeforeWrite adopts newer disk week window without regressing rolled-over weekBaseline', () => {
  const week1Opts = { sevenDayResetAt: new Date(NOON + 6 * DAY_MS) };
  getCostTotals({ session_id: 'w-sess', cost: { total_cost_usd: 1.0 } }, week1Opts, deps(NOON));
  getCostTotals({ session_id: 'w-sess', cost: { total_cost_usd: 4.0 } }, week1Opts, deps(NOON + 1000));

  const ledgerPath = getDailyCostLedgerPath(tempDir);
  const week2Start = NOON + 200_000;
  const racingStdin = { session_id: 'w-sess' };
  Object.defineProperty(racingStdin, 'cost', {
    get() {
      // Simulate concurrent process rolling over to Week 2 on disk before mergeDiskLedgerBeforeWrite runs
      const disk = readLedger();
      disk.week = { start: week2Start, carry: 0 };
      disk.sessions['w-sess'].weekBaseline = 4.0;
      disk.sessions['w-sess'].weekStart = week2Start;
      disk.sessions['w-sess'].total = 4.0;
      disk.sessions['other-new'] = {
        baseline: 2.0,
        weekBaseline: 2.0,
        weekStart: week2Start,
        total: 3.5,
        lastObservedCost: 3.5,
        ts: NOON + 200_500,
      };
      fs.writeFileSync(ledgerPath, JSON.stringify(disk, null, 2));
      return { total_cost_usd: 4.5 };
    },
  });

  const totals = getCostTotals(racingStdin, week1Opts, deps(NOON + 201_000));
  // In Week 2: w-sess gained +0.5 since rollover (4.5 - 4.0) + other-new gained +1.5 (3.5 - 2.0) = $2.0
  // Must NOT pull w-sess.weekBaseline back to Week 1's 1.0!
  assert.equal(totals.weekUsd, 2.0);
  const merged = readLedger();
  assert.equal(merged.week.start, week2Start);
  assert.equal(merged.sessions['w-sess'].weekBaseline, 4.0);
});
