'use strict';

const fs = require('fs');
const path = require('path');
const { resolveAntigravityPath } = require('./paths.js');
const { getNativeCostUsd, getSessionCostUsd } = require('./cost.js');

const DAILY_COST_WRITE_THROTTLE_MS = 30_000;
const SESSION_MAX_AGE_MS = 24 * 60 * 60 * 1000;
const SEVEN_DAY_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;

function isAmount(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function localDateKey(now) {
  const date = new Date(now);
  const month = String(date.getMonth() + 1).padStart(2, '0');
  const day = String(date.getDate()).padStart(2, '0');
  return `${date.getFullYear()}${month}${day}`;
}

function getDailyCostLedgerPath(rootDir = null) {
  if (rootDir) return path.join(rootDir, 'daily-cost.json');
  return resolveAntigravityPath('daily-cost.json');
}

function readLedger(ledgerPath) {
  try {
    const raw = JSON.parse(fs.readFileSync(ledgerPath, 'utf8'));
    if (!isObject(raw) || typeof raw.date !== 'string' || !/^\d{8}$/.test(raw.date) || !isObject(raw.sessions)) {
      return null;
    }
    const sessions = {};
    for (const [id, s] of Object.entries(raw.sessions)) {
      if (!isObject(s) || !isAmount(s.baseline) || !isAmount(s.total) || !isAmount(s.ts) || s.ts === 0) continue;
      const weekBaseline = isAmount(s.weekBaseline) ? s.weekBaseline : s.baseline;
      const lastObservedCost = isAmount(s.lastObservedCost) ? s.lastObservedCost : s.total;
      sessions[id] = { baseline: s.baseline, weekBaseline, total: s.total, lastObservedCost, ts: s.ts };
    }
    const week = isObject(raw.week) ? raw.week : {};
    return {
      date: raw.date,
      sessions,
      week: {
        start: isAmount(week.start) ? week.start : 0,
        carry: isAmount(week.carry) ? week.carry : 0,
      },
    };
  } catch {
    return null;
  }
}

function isThrottled(ledgerPath, now) {
  try {
    return now - fs.statSync(ledgerPath).mtimeMs <= DAILY_COST_WRITE_THROTTLE_MS;
  } catch {
    return false;
  }
}

function writeLedger(ledgerPath, ledger, now) {
  const tmpPath = `${ledgerPath}.${process.pid}.${now}.tmp`;
  try {
    fs.mkdirSync(path.dirname(ledgerPath), { recursive: true, mode: 0o700 });
    fs.writeFileSync(tmpPath, `${JSON.stringify(ledger, null, 2)}\n`, { mode: 0o600 });
    fs.renameSync(tmpPath, ledgerPath);
    try {
      fs.utimesSync(ledgerPath, new Date(now), new Date(now));
    } catch {}
  } catch {
    try {
      fs.rmSync(tmpPath, { force: true });
    } catch {}
  }
}

function mergeDiskLedgerBeforeWrite(ledgerPath, ledger, activeSessionId, now) {
  const latest = readLedger(ledgerPath);
  if (!latest || latest.date !== ledger.date || latest.week.start !== ledger.week.start) {
    return;
  }
  ledger.week.carry = Math.max(ledger.week.carry, latest.week.carry);
  for (const [id, diskSess] of Object.entries(latest.sessions)) {
    if (id === activeSessionId) continue;
    if (now - diskSess.ts > SESSION_MAX_AGE_MS) continue;
    const cur = ledger.sessions[id];
    if (!cur) {
      ledger.sessions[id] = diskSess;
    } else {
      ledger.sessions[id] = {
        baseline: Math.min(cur.baseline, diskSess.baseline),
        weekBaseline: Math.min(cur.weekBaseline, diskSess.weekBaseline),
        total: Math.max(cur.total, diskSess.total),
        lastObservedCost: diskSess.ts > cur.ts ? diskSess.lastObservedCost : cur.lastObservedCost,
        ts: Math.max(cur.ts, diskSess.ts),
      };
    }
  }
}

const defaultDeps = {
  rootDir: () => resolveAntigravityPath(''),
  now: () => Date.now(),
};

/**
 * Folds the session cost into the daily ledger and returns today's and this week's spend.
 * Aligned with claude-hud ledger mechanics.
 *
 * @param {Object} agyData
 * @param {Object} [options]
 * @param {Object} [deps]
 * @param {Object} [state]
 * @returns {{ todayUsd: number, weekUsd: number }|null}
 */
function getCostTotals(agyData, options = {}, deps = defaultDeps, state = null) {
  const now = deps.now();
  const today = localDateKey(now);
  const ledgerPath = getDailyCostLedgerPath(deps.rootDir());
  const stored = readLedger(ledgerPath);
  const ledger = stored ?? { date: today, sessions: {}, week: { start: now, carry: 0 } };
  let changed = stored === null;

  if (!ledger.week.start || ledger.week.start <= 0) {
    ledger.week.start = now;
  }

  // Sessions unseen for a day leave the ledger, but their spend stays inside the week.
  for (const [id, session] of Object.entries(ledger.sessions)) {
    if (now - session.ts > SESSION_MAX_AGE_MS) {
      ledger.week.carry += Math.max(0, session.total - session.weekBaseline);
      delete ledger.sessions[id];
      changed = true;
    }
  }

  // At midnight, active sessions carry over so only today's part of spend counts.
  if (ledger.date !== today) {
    ledger.date = today;
    for (const session of Object.values(ledger.sessions)) {
      session.baseline = session.total;
    }
    changed = true;
  }

  // Restart the week when the quota window behind the weekly bar opened after accumulation began,
  // or fall back to a local 7-day rolling window when sevenDayResetAt is unavailable (offline).
  const resetAt = options?.sevenDayResetAt instanceof Date
    ? options.sevenDayResetAt.getTime()
    : typeof options?.sevenDayResetAt === 'number'
      ? options.sevenDayResetAt
      : typeof options?.sevenDayResetAt === 'string'
        ? Date.parse(options.sevenDayResetAt)
        : null;

  const windowStart = resetAt !== null && Number.isFinite(resetAt)
    ? resetAt - SEVEN_DAY_WINDOW_MS
    : null;

  if (windowStart !== null && ledger.week.start < windowStart) {
    for (const session of Object.values(ledger.sessions)) {
      session.weekBaseline = session.total;
    }
    ledger.week = { start: now, carry: 0 };
    changed = true;
  } else if (windowStart === null && now - ledger.week.start >= SEVEN_DAY_WINDOW_MS) {
    for (const session of Object.values(ledger.sessions)) {
      session.weekBaseline = session.total;
    }
    ledger.week = { start: now, carry: 0 };
    changed = true;
  }

  const rawSessionId = agyData?.session_id || agyData?.conversation_id || state?.transcriptPath || '';
  const sessionId = typeof rawSessionId === 'string' ? rawSessionId.trim() : '';

  const cost = getNativeCostUsd(agyData, options) ?? (state ? getSessionCostUsd(agyData, state, options) : null);

  let tsRenewed = false;
  if (sessionId && cost !== null && Number.isFinite(cost)) {
    const session = ledger.sessions[sessionId];
    if (!session) {
      ledger.sessions[sessionId] = {
        baseline: cost,
        weekBaseline: cost,
        total: cost,
        lastObservedCost: cost,
        ts: now,
      };
      changed = true;
    } else {
      const lastObserved = isAmount(session.lastObservedCost) ? session.lastObservedCost : session.total;
      if (cost > lastObserved) {
        session.total += (cost - lastObserved);
        session.lastObservedCost = cost;
        changed = true;
      } else if (cost < lastObserved) {
        // Compaction or counter reset: advance the observation cursor without
        // subtracting previously accrued spend so subsequent growth continues counting.
        session.lastObservedCost = cost;
        changed = true;
      }
      if (session.ts !== now) {
        session.ts = now;
        tsRenewed = true;
      }
    }
  }

  if (Object.keys(ledger.sessions).length === 0 && ledger.week.carry === 0) {
    return null;
  }

  if (tsRenewed && !isThrottled(ledgerPath, now)) {
    changed = true;
  }
  if (changed || !isThrottled(ledgerPath, now)) {
    mergeDiskLedgerBeforeWrite(ledgerPath, ledger, sessionId, now);
    writeLedger(ledgerPath, ledger, now);
  }

  const sessions = Object.values(ledger.sessions);
  const sum = (pick) => sessions.reduce((total, s) => total + Math.max(0, s.total - pick(s)), 0);

  return {
    todayUsd: sum((s) => s.baseline),
    weekUsd: ledger.week.carry + sum((s) => s.weekBaseline),
  };
}

module.exports = {
  getCostTotals,
  getDailyCostLedgerPath,
  DAILY_COST_WRITE_THROTTLE_MS,
  SESSION_MAX_AGE_MS,
  SEVEN_DAY_WINDOW_MS,
};
