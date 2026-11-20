'use strict';

const fs = require('fs');
const path = require('path');
const { resolveAntigravityPath } = require('./paths.js');
const { getNativeCostUsd, getSessionCostUsd } = require('./cost.js');

const DAILY_COST_WRITE_THROTTLE_MS = 30_000;
const SESSION_MAX_AGE_MS = 24 * 60 * 60 * 1000;
const SEVEN_DAY_WINDOW_MS = 7 * 24 * 60 * 60 * 1000;
const WEEK_START_JITTER_MS = 60_000;
const LEDGER_LOCK_STALE_MS = 5000;
const LEDGER_LOCK_RETRY_COUNT = 8;
const LEDGER_LOCK_RETRY_MS = 15;
const SLEEP_ARR = new Int32Array(new SharedArrayBuffer(4));

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
    const week = isObject(raw.week) ? raw.week : {};
    const ledgerWeekStart = isAmount(week.start) ? week.start : 0;
    const sessions = {};
    for (const [id, s] of Object.entries(raw.sessions)) {
      if (!isObject(s) || !isAmount(s.baseline) || !isAmount(s.total) || !isAmount(s.ts) || s.ts === 0) continue;
      const weekBaseline = isAmount(s.weekBaseline) ? s.weekBaseline : s.baseline;
      const lastObservedCost = isAmount(s.lastObservedCost) ? s.lastObservedCost : s.total;
      const weekStart = isAmount(s.weekStart) ? s.weekStart : ledgerWeekStart;
      sessions[id] = {
        baseline: s.baseline,
        weekBaseline,
        weekStart,
        total: s.total,
        lastObservedCost,
        ts: s.ts,
      };
    }
    return {
      date: raw.date,
      sessions,
      week: {
        start: ledgerWeekStart,
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

function acquireLedgerLock(ledgerPath) {
  const lockPath = `${ledgerPath}.lock`;
  try {
    fs.mkdirSync(path.dirname(ledgerPath), { recursive: true, mode: 0o700 });
  } catch {
    return false;
  }
  for (let attempt = 0; attempt < LEDGER_LOCK_RETRY_COUNT; attempt += 1) {
    try {
      const fd = fs.openSync(lockPath, 'wx', 0o600);
      try {
        fs.writeFileSync(fd, String(process.pid));
      } finally {
        fs.closeSync(fd);
      }
      return true;
    } catch (err) {
      if (!err || err.code !== 'EEXIST') {
        return false;
      }
      try {
        const st = fs.statSync(lockPath);
        if (Date.now() - st.mtimeMs > LEDGER_LOCK_STALE_MS) {
          fs.unlinkSync(lockPath);
          continue;
        }
      } catch {
        continue;
      }
      if (attempt < LEDGER_LOCK_RETRY_COUNT - 1) {
        try {
          Atomics.wait(SLEEP_ARR, 0, 0, LEDGER_LOCK_RETRY_MS);
        } catch {}
      }
    }
  }
  return false;
}

function releaseLedgerLock(ledgerPath) {
  try {
    fs.unlinkSync(`${ledgerPath}.lock`);
  } catch {}
}

function resolveSessionId(agyData, state) {
  if (agyData && Object.prototype.hasOwnProperty.call(agyData, 'session_id') && agyData.session_id !== undefined && agyData.session_id !== null) {
    if (typeof agyData.session_id !== 'string') return '';
    const trimmed = agyData.session_id.trim();
    if (trimmed) return trimmed;
  }
  if (typeof agyData?.conversation_id === 'string' && agyData.conversation_id.trim()) {
    return agyData.conversation_id.trim();
  }

  const rawPath = (typeof state?.transcriptPath === 'string' && state.transcriptPath.trim())
    ? state.transcriptPath.trim()
    : (typeof agyData?.transcript_path === 'string' && agyData.transcript_path.trim()
      ? agyData.transcript_path.trim()
      : '');
  if (rawPath) {
    const norm = rawPath.replace(/\\/g, '/');
    const marker = '/brain/';
    const idx = norm.lastIndexOf(marker);
    if (idx >= 0) {
      const afterBrain = norm.slice(idx + marker.length);
      const firstSeg = (afterBrain.split('/')[0] || '').trim();
      if (firstSeg && firstSeg !== '.system_generated') {
        return firstSeg;
      }
      // Empty shell path `/brain/.system_generated/...`: never use as shared sessionId
    } else {
      return rawPath;
    }
  }

  const dir = (typeof agyData?.workspace?.current_dir === 'string' && agyData.workspace.current_dir.trim())
    ? agyData.workspace.current_dir.trim()
    : (typeof agyData?.cwd === 'string' && agyData.cwd.trim() ? agyData.cwd.trim() : '');
  return dir ? `cwd:${dir}` : '';
}

function resolveBaselineForDate(curBaseline, diskBaseline, curDate, diskDate) {
  if (diskDate === curDate) return Math.min(curBaseline, diskBaseline);
  if (diskDate > curDate) return diskBaseline;
  return curBaseline;
}

function mergeDiskLedgerBeforeWrite(ledgerPath, ledger, activeSessionId, now, evictedMap = null) {
  const latest = readLedger(ledgerPath);
  if (!latest) {
    return;
  }

  // Case 1: Disk has already rolled over to a newer week window -> adopt disk's week window
  if (latest.week.start > ledger.week.start + WEEK_START_JITTER_MS) {
    ledger.week = { start: latest.week.start, carry: latest.week.carry };
    const prevDate = ledger.date;
    if (latest.date > ledger.date) {
      ledger.date = latest.date;
    }
    if (activeSessionId && ledger.sessions[activeSessionId]) {
      const curActive = ledger.sessions[activeSessionId];
      const diskActive = latest.sessions[activeSessionId];
      const diskSameWeek = Boolean(
        diskActive &&
        Math.abs((isAmount(diskActive.weekStart) ? diskActive.weekStart : latest.week.start) - latest.week.start) < WEEK_START_JITTER_MS
      );
      curActive.weekBaseline = diskSameWeek ? diskActive.weekBaseline : curActive.total;
      curActive.weekStart = latest.week.start;
      if (diskActive) {
        curActive.baseline = resolveBaselineForDate(curActive.baseline, diskActive.baseline, prevDate, latest.date);
      }
    }
    for (const [id, diskSess] of Object.entries(latest.sessions)) {
      if (id === activeSessionId) continue;
      if (now - diskSess.ts > SESSION_MAX_AGE_MS) continue;
      const cur = ledger.sessions[id];
      if (!cur) {
        ledger.sessions[id] = diskSess;
      } else {
        ledger.sessions[id] = {
          baseline: resolveBaselineForDate(cur.baseline, diskSess.baseline, prevDate, latest.date),
          weekBaseline: diskSess.weekBaseline,
          weekStart: isAmount(diskSess.weekStart) ? diskSess.weekStart : latest.week.start,
          total: Math.max(cur.total, diskSess.total),
          lastObservedCost: diskSess.ts > cur.ts ? diskSess.lastObservedCost : cur.lastObservedCost,
          ts: Math.max(cur.ts, diskSess.ts),
        };
      }
    }
    return;
  }

  // Case 2: In-memory ledger has already rolled over to a newer week than disk -> keep new week baselines
  if (ledger.week.start > latest.week.start + WEEK_START_JITTER_MS) {
    const prevDate = ledger.date;
    for (const [id, diskSess] of Object.entries(latest.sessions)) {
      if (id === activeSessionId) continue;
      if (now - diskSess.ts > SESSION_MAX_AGE_MS) continue;
      const cur = ledger.sessions[id];
      if (!cur) {
        ledger.sessions[id] = {
          ...diskSess,
          weekBaseline: diskSess.total,
          weekStart: ledger.week.start,
        };
      } else {
        ledger.sessions[id] = {
          baseline: resolveBaselineForDate(cur.baseline, diskSess.baseline, prevDate, latest.date),
          weekBaseline: cur.weekBaseline,
          weekStart: isAmount(cur.weekStart) ? cur.weekStart : ledger.week.start,
          total: Math.max(cur.total, diskSess.total),
          lastObservedCost: diskSess.ts > cur.ts ? diskSess.lastObservedCost : cur.lastObservedCost,
          ts: Math.max(cur.ts, diskSess.ts),
        };
      }
    }
    return;
  }

  // Case 3: Same week window
  const prevDate = ledger.date;
  if (latest.date > ledger.date) {
    ledger.date = latest.date;
  }
  ledger.week.start = Math.min(ledger.week.start, latest.week.start);
  if (evictedMap && evictedMap.size > 0) {
    for (const [id, carriedSpend] of evictedMap.entries()) {
      if (id === activeSessionId) continue;
      const diskSess = latest.sessions[id];
      if (diskSess && now - diskSess.ts <= SESSION_MAX_AGE_MS) {
        ledger.week.carry = Math.max(0, ledger.week.carry - carriedSpend);
      }
    }
  }
  ledger.week.carry = Math.max(ledger.week.carry, latest.week.carry);
  for (const [id, diskSess] of Object.entries(latest.sessions)) {
    if (id === activeSessionId) continue;
    if (now - diskSess.ts > SESSION_MAX_AGE_MS) continue;
    const cur = ledger.sessions[id];
    if (!cur) {
      ledger.sessions[id] = diskSess;
    } else {
      const curWeekStart = isAmount(cur.weekStart) ? cur.weekStart : ledger.week.start;
      const diskWeekStart = isAmount(diskSess.weekStart) ? diskSess.weekStart : latest.week.start;
      let weekBaseline;
      let weekStart;
      if (Math.abs(curWeekStart - diskWeekStart) < WEEK_START_JITTER_MS) {
        weekBaseline = Math.min(cur.weekBaseline, diskSess.weekBaseline);
        weekStart = Math.min(curWeekStart, diskWeekStart);
      } else if (diskWeekStart > curWeekStart) {
        weekBaseline = diskSess.weekBaseline;
        weekStart = diskWeekStart;
      } else {
        weekBaseline = cur.weekBaseline;
        weekStart = curWeekStart;
      }
      ledger.sessions[id] = {
        baseline: resolveBaselineForDate(cur.baseline, diskSess.baseline, prevDate, latest.date),
        weekBaseline,
        weekStart,
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
  const evictedMap = new Map();
  for (const [id, session] of Object.entries(ledger.sessions)) {
    if (now - session.ts > SESSION_MAX_AGE_MS) {
      const carriedSpend = Math.max(0, session.total - session.weekBaseline);
      ledger.week.carry += carriedSpend;
      evictedMap.set(id, carriedSpend);
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

  if (windowStart !== null && windowStart > ledger.week.start + WEEK_START_JITTER_MS) {
    for (const session of Object.values(ledger.sessions)) {
      session.weekBaseline = session.total;
      session.weekStart = now;
    }
    ledger.week = { start: now, carry: 0 };
    evictedMap.clear();
    changed = true;
  } else if (windowStart === null && now - ledger.week.start >= SEVEN_DAY_WINDOW_MS) {
    for (const session of Object.values(ledger.sessions)) {
      session.weekBaseline = session.total;
      session.weekStart = now;
    }
    ledger.week = { start: now, carry: 0 };
    evictedMap.clear();
    changed = true;
  }

  const sessionId = resolveSessionId(agyData, state);

  const cost = state
    ? getSessionCostUsd(agyData, state, options)
    : getNativeCostUsd(agyData, options);

  let tsRenewed = false;
  if (sessionId && cost !== null && Number.isFinite(cost)) {
    const session = ledger.sessions[sessionId];
    if (!session) {
      ledger.sessions[sessionId] = {
        baseline: cost,
        weekBaseline: cost,
        weekStart: ledger.week.start,
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
      } else if (cost < lastObserved * 0.5) {
        // Significant compaction or counter reset (>50% drop): advance the observation cursor
        // without subtracting previously accrued spend so subsequent growth continues counting.
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
    const lockAcquired = acquireLedgerLock(ledgerPath);
    try {
      mergeDiskLedgerBeforeWrite(ledgerPath, ledger, sessionId, now, evictedMap);
      writeLedger(ledgerPath, ledger, now);
    } finally {
      if (lockAcquired) {
        releaseLedgerLock(ledgerPath);
      }
    }
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
