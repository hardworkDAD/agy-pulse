import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import fs from 'node:fs';
import { getOutputSpeed, formatSpeed, formatTtft, formatChildModelSpeeds, getSpeedCachePath } from '../../runtime/speed.js';

let tempDir;

const frame = (output, totalOutput, apiMs) => ({
  transcript_path: path.join(tempDir, 'session.jsonl'),
  context_window: {
    total_output_tokens: totalOutput,
    current_usage: { input_tokens: 1000, output_tokens: output },
  },
  cost: { total_api_duration_ms: apiMs },
});

beforeEach(async () => {
  tempDir = await mkdtemp(path.join(tmpdir(), 'agy-speed-test-'));
});

afterEach(async () => {
  await rm(tempDir, { recursive: true, force: true });
});

test('uses matching cumulative deltas across requests and holds speed on repeated snapshots', () => {
  assert.equal(getOutputSpeed(frame(500, 500, 10000), tempDir), null);
  assert.equal(getOutputSpeed(frame(300, 1100, 16000), tempDir), 100);
  assert.equal(getOutputSpeed(frame(0, 1100, 16000), tempDir), 100);
  assert.equal(getOutputSpeed(frame(0, 900, 17000), tempDir), null);
  assert.equal(getOutputSpeed(frame(400, 1300, 21000), tempDir), 100);
  assert.equal(getOutputSpeed(frame(0, 1400, 100), tempDir), null);
  assert.equal(getOutputSpeed({ ...frame(300, 1400, 100), transcript_path: undefined }, tempDir), null);
});

test('rebases on one-sided updates and ignores intervals shorter than 500ms', () => {
  assert.equal(getOutputSpeed(frame(500, 500, 10000), tempDir), null);
  assert.equal(getOutputSpeed(frame(300, 800, 13000), tempDir), 100);

  // Time advances ahead of tokens. Neither update by itself yields a speed.
  assert.equal(getOutputSpeed(frame(300, 800, 16000), tempDir), null);
  assert.equal(getOutputSpeed(frame(300, 1100, 16000), tempDir), null);
  assert.equal(getOutputSpeed(frame(400, 1500, 18000), tempDir), 200);

  // The inverse arrival order also rebases. A later valid interval recovers.
  assert.equal(getOutputSpeed(frame(200, 1700, 18000), tempDir), null);
  assert.equal(getOutputSpeed(frame(200, 1700, 20000), tempDir), null);
  assert.equal(getOutputSpeed(frame(300, 2000, 20200), tempDir), null);
  assert.equal(getOutputSpeed(frame(600, 2600, 23200), tempDir), 200);
});

test('falls back to a timed planner response when the native counters cannot be paired', () => {
  const partial = frame(500, undefined, 10000);
  assert.equal(getOutputSpeed(partial, tempDir, { totalOutputTokens: 500 }), null);
  assert.equal(getOutputSpeed(frame(300, undefined, 13000), tempDir, { totalOutputTokens: 800 }), null);
  assert.equal(getOutputSpeed(partial, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 14000, outputTokens: 400 },
  }), 100);

  const wallClock = { ...frame(500, 500, 10000), cost: { total_duration_ms: 10000 } };
  const nextWallClock = { ...frame(300, 800, 13000), cost: { total_duration_ms: 13000 } };
  assert.equal(getOutputSpeed(wallClock, tempDir), null);
  assert.equal(getOutputSpeed(nextWallClock, tempDir), null);
  assert.equal(getOutputSpeed({ ...wallClock, total_api_duration_ms: 10000 }, tempDir), null);
  assert.equal(getOutputSpeed(frame(300, 800, 13000), tempDir), 100);
});

test('uses transcript lastPlannerResponse closed and trailing durations without tool-time inflation', () => {
  const transcriptPath = path.join(tempDir, 'session.jsonl');
  const agyTurn = (input, output, totalOutput) => ({
    transcript_path: transcriptPath,
    context_window: {
      total_output_tokens: totalOutput,
      current_usage: {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 50000,
      },
    },
  });

  // Legacy cache entry without plannerStartMs (e.g. bogus 2774 tok/s) is replaced by closed turn speed
  const cacheFile = getSpeedCachePath(tempDir, transcriptPath);
  fs.mkdirSync(path.dirname(cacheFile), { recursive: true });
  fs.writeFileSync(cacheFile, JSON.stringify({ apiDurationMs: 11000, usageKey: '100:200:0:0', speed: 2774 }));

  // Closed PLANNER_RESPONSE: 5548 tokens generated in 40s (preceding tool took 2s, which must NOT be used)
  const closedSpeed = getOutputSpeed(agyTurn(2352, 5548, 5548), tempDir, {
    totalApiDurationMs: 42000,
    lastPlannerResponse: { startMs: 10000, endMs: 50000, outputTokens: 5548 },
  }, 50500);
  assert.equal(closedSpeed.toFixed(1), (5548 / 40).toFixed(1));

  // Trailing final-text PLANNER_RESPONSE (no subsequent step yet): uses now - startMs
  const trailingSpeed = getOutputSpeed(agyTurn(2500, 600, 6148), tempDir, {
    totalApiDurationMs: 42000,
    lastPlannerResponse: { startMs: 60000, endMs: null, outputTokens: 600 },
  }, 66000);
  assert.equal(trailingSpeed, 100); // 600 / 6s

  // Idle re-render holds speed instead of decaying as now increases
  const heldSpeed = getOutputSpeed(agyTurn(2500, 600, 6148), tempDir, {
    totalApiDurationMs: 42000,
    lastPlannerResponse: { startMs: 60000, endMs: null, outputTokens: 600 },
  }, 180000);
  assert.equal(heldSpeed, 100);
});

test('uses delta of total_output_tokens when per-turn output_tokens is absent', () => {
  const transcriptPath = path.join(tempDir, 'cumulative.jsonl');
  const frameOnlyTotal = (totalOutput, apiMs) => ({
    transcript_path: transcriptPath,
    context_window: {
      total_output_tokens: totalOutput,
      current_usage: { input_tokens: 1000, cache_read_input_tokens: 2000 },
    },
    cost: { total_api_duration_ms: apiMs },
  });

  assert.equal(getOutputSpeed(frameOnlyTotal(1000, 2000), tempDir), null);
  assert.equal(getOutputSpeed(frameOnlyTotal(1300, 5000), tempDir), 100); // (1300 - 1000) / 3s
  assert.equal(getOutputSpeed(frameOnlyTotal(1800, 10000), tempDir), 100); // (1800 - 1300) / 5s
});

test('formatSpeed formats output speed in en and zh', () => {
  assert.equal(formatSpeed(null, 'en'), null);
  assert.equal(formatSpeed(42.56, 'en'), 'out: 42.6 tok/s');
  assert.equal(formatSpeed(42.56, 'zh'), '输出: 42.6 tok/s');
});

test('computes and latches speed for first-turn plain-text reply within 500ms..120s window, and returns null when stale or too short', () => {
  const activeTranscript = path.join(tempDir, 'first-turn-active.jsonl');
  const agyTurn = {
    transcript_path: activeTranscript,
    context_window: {
      total_output_tokens: 500,
      current_usage: {
        input_tokens: 1500,
        output_tokens: 500,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
      },
    },
  };

  // Active first turn (endMs === null, elapsed = 5000ms within [500, 120000])
  const firstSpeed = getOutputSpeed(agyTurn, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 15000);
  assert.equal(firstSpeed, 100); // 500 / 5s

  // Subsequent idle re-render holds the latched speed
  const heldSpeed = getOutputSpeed(agyTurn, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 300000);
  assert.equal(heldSpeed, 100);

  // Attached historical session (> 120000ms old, !previous) returns null
  const staleTranscript = path.join(tempDir, 'first-turn-stale.jsonl');
  const staleSpeed = getOutputSpeed({ ...agyTurn, transcript_path: staleTranscript }, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 135000);
  assert.equal(staleSpeed, null);

  // Under 500ms noise threshold (!previous) returns null
  const shortTranscript = path.join(tempDir, 'first-turn-short.jsonl');
  const shortSpeed = getOutputSpeed({ ...agyTurn, transcript_path: shortTranscript }, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 10300);
  assert.equal(shortSpeed, null);
});

test('a reopened transcript needs the timed response’s own output count', () => {
  const agy = { session_id: 'reopened', context_window: { total_output_tokens: 9000 } };
  const planner = { startMs: 1000, endMs: 11000 };
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { ...planner, outputTokens: null },
  }), null);
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { ...planner, outputTokens: 300 },
  }), 30);
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { startMs: 12000, endMs: 16000, outputTokens: null },
  }), null);
});

test('Path A recovers from < 500ms pending state and updates speed when unclosed turn closes', () => {
  const transcriptPath = path.join(tempDir, 'pending-recovery.jsonl');
  const agy = {
    transcript_path: transcriptPath,
    context_window: {
      total_output_tokens: 500,
      current_usage: {
        input_tokens: 1000,
        output_tokens: 500,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
      },
    },
  };

  // 1. First render at 300ms (< 500ms) -> returns null and marks pending
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 10300), null);

  // 2. Second render at 2000ms (still open, same usageKey & startMs) -> recovers and computes 250 tok/s
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 12000), 250);

  // 3. Turn closes at 15000ms (5000ms duration, same usageKey & startMs) -> recalculates to 100 tok/s
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 15000, outputTokens: 500 },
  }, 15500), 100);

  // 4. Subsequent render on closed turn holds 100 tok/s
  assert.equal(getOutputSpeed(agy, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 15000, outputTokens: 500 },
  }, 20000), 100);
});

test('prunes speed-cache directory to at most 32 files', () => {
  for (let i = 0; i < 36; i += 1) {
    getOutputSpeed({
      transcript_path: path.join(tempDir, `sess-${i}.jsonl`),
      context_window: { total_output_tokens: 200 },
      cost: { total_api_duration_ms: 1000 },
    }, tempDir);
  }
  const cacheDir = path.join(tempDir, 'speed-cache');
  const files = fs.readdirSync(cacheDir).filter(f => f.endsWith('.json'));
  assert.ok(files.length <= 32, `expected <= 32 speed cache files, got ${files.length}`);
});

test('non-first-turn open turn > 120s or interrupted holds previous speed instead of using now - startMs', () => {
  const transcriptPath = path.join(tempDir, 'open-turn-guard.jsonl');
  const makeAgy = (input, output, totalOutput) => ({
    transcript_path: transcriptPath,
    context_window: {
      total_output_tokens: totalOutput,
      current_usage: {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
      },
    },
  });

  // Turn 1 closes cleanly at 100 tok/s (500 tokens / 5s)
  const turn1Speed = getOutputSpeed(makeAgy(1000, 500, 500), tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 15000, outputTokens: 500 },
  }, 15500);
  assert.equal(turn1Speed, 100);

  // Turn 2 has endMs: null and now - startMs > 120s (e.g. user idle/reconnect): must hold 100, NOT 600 / 200 = 3.0!
  const staleTurn2Speed = getOutputSpeed(makeAgy(1500, 600, 1100), tempDir, {
    lastPlannerResponse: { startMs: 20000, endMs: null, outputTokens: 600 },
  }, 220000);
  assert.equal(staleTurn2Speed, 100);

  const cacheStateAfterStale = JSON.parse(fs.readFileSync(getSpeedCachePath(tempDir, transcriptPath), 'utf8'));
  assert.equal(cacheStateAfterStale.pending, false);

  // Turn 3 has endMs: null and interrupted: true within 120s: must hold 100, NOT 400 / 10 = 40!
  const interruptedSpeed = getOutputSpeed(makeAgy(2000, 400, 1500), tempDir, {
    lastPlannerResponse: { startMs: 230000, endMs: null, outputTokens: 400, interrupted: true },
  }, 240000);
  assert.equal(interruptedSpeed, 100);

  const cacheStateAfterInterrupt = JSON.parse(fs.readFileSync(getSpeedCachePath(tempDir, transcriptPath), 'utf8'));
  assert.equal(cacheStateAfterInterrupt.pending, false);

  // Fresh session with interrupted open turn returns null and sets pending: false
  const freshInterruptedPath = path.join(tempDir, 'fresh-interrupted.jsonl');
  const freshSpeed = getOutputSpeed({ ...makeAgy(1000, 300, 300), transcript_path: freshInterruptedPath }, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 300, interrupted: true },
  }, 15000);
  assert.equal(freshSpeed, null);
  const freshCache = JSON.parse(fs.readFileSync(getSpeedCachePath(tempDir, freshInterruptedPath), 'utf8'));
  assert.equal(freshCache.pending, false);
});

test('planner response counts survive short open samples and open-to-closed recalculation', () => {
  const transcriptPath = path.join(tempDir, 'cumulative-baseline.jsonl');
  const cumAgy = (input, totalOutput) => ({
    transcript_path: transcriptPath,
    context_window: {
      total_output_tokens: totalOutput,
      current_usage: {
        input_tokens: input,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
      },
    },
  });

  // Turn 1: the log supplies 500 tokens for this response.
  // t = 200ms -> null (pending: true)
  assert.equal(getOutputSpeed(cumAgy(1000, 500), tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 10200), null);

  // t = 2200ms -> recovers 500 / 2.2s
  const openSpeed = getOutputSpeed(cumAgy(1000, 500), tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: null, outputTokens: 500 },
  }, 12200);
  assert.equal(openSpeed?.toFixed(2), (500 / 2.2).toFixed(2));

  // t = 5000ms closed -> recalculates cleanly to 500 / 5s = 100 tok/s (not 0!)
  assert.equal(getOutputSpeed(cumAgy(1000, 500), tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 15000, outputTokens: 500 },
  }, 15500), 100);

  // Turn 2: the log supplies 600 tokens for this response.
  // First check < 500ms (pending: true, holds 100)
  assert.equal(getOutputSpeed(cumAgy(2000, 1100), tempDir, {
    lastPlannerResponse: { startMs: 20000, endMs: null, outputTokens: 600 },
  }, 20200), 100);

  // Closes with 3000ms duration -> uses the logged 600 tokens => 200 tok/s
  assert.equal(getOutputSpeed(cumAgy(2000, 1100), tempDir, {
    lastPlannerResponse: { startMs: 20000, endMs: 23000, outputTokens: 600 },
  }, 23500), 200);
});

test('formatChildModelSpeeds formats unicode, Nerd Font, and ASCII outputs cleanly', () => {
  assert.equal(formatChildModelSpeeds([]), null);
  assert.equal(formatChildModelSpeeds(null), null);
  assert.equal(formatChildModelSpeeds([{ model: 'Hydro', speed: 0 }]), null);

  assert.equal(
    formatChildModelSpeeds([{ model: 'Hydro', speed: 215.44 }], { unicode: true, useNerd: false }),
    '⚡ Hydro: 215.4 tok/s'
  );
  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 215.4 }, { model: 'Flash', speed: 310.2 }],
      { unicode: true, useNerd: false }
    ),
    '⚡ Hydro: 215.4 tok/s · Flash: 310.2 tok/s'
  );
  assert.equal(
    formatChildModelSpeeds([{ model: 'Hydro', speed: 215.4 }], { unicode: false, useNerd: true }),
    '\uf0e7 Hydro: 215.4 tok/s'
  );
  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 215.4 }, { model: 'Flash', speed: 310.2 }],
      { unicode: false, useNerd: false }
    ),
    '[sub] Hydro: 215.4 tok/s / Flash: 310.2 tok/s'
  );
});

test('formatTtft formats sub-second ms and >=1s seconds in en and zh, and handles invalid inputs', () => {
  assert.equal(formatTtft(null), null);
  assert.equal(formatTtft(undefined), null);
  assert.equal(formatTtft(-0.5), null);
  assert.equal(formatTtft(Number.NaN), null);

  assert.equal(formatTtft(0.42, 'en'), 'ttft 420ms');
  assert.equal(formatTtft(0.42, 'zh'), '首字 420ms');
  assert.equal(formatTtft(0, 'en'), 'ttft 0ms');
  assert.equal(formatTtft(10.82, 'en'), 'ttft 10.8s');
  assert.equal(formatTtft(10.82, 'zh'), '首字 10.8s');
});

test('formatChildModelSpeeds enriches with modelPerf avgSpeed + avgTtftSec and falls back cleanly', () => {
  const modelPerf = {
    Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.528 },
    Flash: { model: 'Flash', avgSpeed: 340.1, avgTtftSec: 0.42 },
  };

  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 180.0 }, { model: 'Flash', speed: 300.0 }],
      { unicode: true, useNerd: false, modelPerf, lang: 'zh' }
    ),
    '⚡ Hydro: 260.9 tok/s (首字 5.5s) · Flash: 340.1 tok/s (首字 420ms)'
  );

  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 180.0 }, { model: 'Opus', speed: 95.2 }],
      { unicode: false, useNerd: false, modelPerf, lang: 'en' }
    ),
    '[sub] Hydro: 260.9 tok/s (ttft 5.5s) / Opus: 95.2 tok/s'
  );

  // Even when child instantaneous speed is 0 (newly started), modelPerf avgSpeed surfaces it
  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 0 }],
      { unicode: true, useNerd: true, modelPerf, lang: 'en' }
    ),
    '\uf0e7 Hydro: 260.9 tok/s (ttft 5.5s)'
  );

  // includeTtft: false omits TTFT suffix for narrow-terminal degradation
  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 180.0 }, { model: 'Flash', speed: 300.0 }],
      { unicode: true, useNerd: false, modelPerf, lang: 'zh', includeTtft: false }
    ),
    '⚡ Hydro: 260.9 tok/s · Flash: 340.1 tok/s'
  );

  // Filters dirty child speeds > 5000 tok/s
  assert.equal(
    formatChildModelSpeeds(
      [{ model: 'Hydro', speed: 18000 }, { model: 'Flash', speed: 300.0 }],
      { unicode: true, useNerd: false }
    ),
    '⚡ Flash: 300.0 tok/s'
  );
});

test('getOutputSpeed invalidates stale speed on model switch, rejects >5000 dirty cache, and skips steady-state disk writes', () => {
  const transcriptPath = path.join(tempDir, 'model-switch.jsonl');
  const makeFrame = (model, input, output, totalOut) => ({
    transcript_path: transcriptPath,
    model: { display_name: model },
    context_window: {
      total_output_tokens: totalOut,
      current_usage: { input_tokens: input, output_tokens: output },
    },
  });

  // Seed dirty > 5000 cache entry -> rejected and recomputed cleanly
  const cacheFile = getSpeedCachePath(tempDir, transcriptPath);
  fs.mkdirSync(path.dirname(cacheFile), { recursive: true });
  fs.writeFileSync(cacheFile, JSON.stringify({
    version: 1,
    source: 'transcript',
    modelKey: 'gemini 4 carbon (high)',
    speed: 25000,
    plannerStartMs: 10000,
    plannerEndMs: 15000,
    plannerOutputTokens: 500,
    pending: false,
  }));

  const s1 = getOutputSpeed(makeFrame('Gemini 4 Carbon (High)', 1000, 500, 500), tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 15000, outputTokens: 500 },
  }, 15500);
  assert.equal(s1, 100);

  // Switching to Opus on an interrupted/open turn must NOT reuse Carbon's 100 tok/s
  const sSwitch = getOutputSpeed(makeFrame('Claude Opus 4.6', 1200, 200, 700), tempDir, {
    lastPlannerResponse: { startMs: 20000, endMs: null, outputTokens: 200, interrupted: true },
  }, 25000);
  assert.equal(sSwitch, null);

  // Steady-state > 120s open turn writes once then skips writeState on repeated identical renders
  const stalePath = path.join(tempDir, 'steady-120s.jsonl');
  const staleAgy = {
    transcript_path: stalePath,
    model: { display_name: 'Gemini 4 Carbon (High)' },
    context_window: { total_output_tokens: 400, current_usage: { input_tokens: 500, output_tokens: 400 } },
  };
  getOutputSpeed(staleAgy, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 14000, outputTokens: 400 },
  }, 14500);
  // First >120s open turn transitions pending: false and writes
  assert.equal(getOutputSpeed({
    ...staleAgy,
    context_window: { total_output_tokens: 700, current_usage: { input_tokens: 800, output_tokens: 300 } },
  }, tempDir, {
    lastPlannerResponse: { startMs: 20000, endMs: null, outputTokens: 300 },
  }, 200000), 100);
  const staleCacheFile = getSpeedCachePath(tempDir, stalePath);
  const tOld = new Date(Date.now() - 60000);
  fs.utimesSync(staleCacheFile, tOld, tOld);
  const oldMtime = fs.statSync(staleCacheFile).mtimeMs;

  // Repeated idle render with same usageKey and pending === false must skip writeState
  assert.equal(getOutputSpeed({
    ...staleAgy,
    context_window: { total_output_tokens: 700, current_usage: { input_tokens: 800, output_tokens: 300 } },
  }, tempDir, {
    lastPlannerResponse: { startMs: 20000, endMs: null, outputTokens: 300 },
  }, 250000), 100);
  assert.equal(fs.statSync(staleCacheFile).mtimeMs, oldMtime);
});

test('rebases cumulative counters after switching between model variants', () => {
  const sample = (id, output, totalOutput, apiMs) => ({
    ...frame(output, totalOutput, apiMs),
    model: { id },
  });
  assert.equal(getOutputSpeed(sample('gemini-4-carbon-high', 0, 0, 1000), tempDir), null);
  assert.equal(getOutputSpeed(sample('gemini-4-carbon-high', 400, 400, 5000), tempDir), 100);
  assert.equal(getOutputSpeed(sample('gemini-4-carbon-low', 400, 800, 9000), tempDir), null);
  assert.equal(getOutputSpeed(sample('gemini-4-carbon-low', 600, 1400, 12000), tempDir), 200);
});

test('discards an old speed cache even when both counters match', () => {
  const oldFrame = frame(300, 1100, 16000);
  const cacheFile = getSpeedCachePath(tempDir, oldFrame.transcript_path);
  fs.mkdirSync(path.dirname(cacheFile), { recursive: true });
  fs.writeFileSync(cacheFile, JSON.stringify({ apiDurationMs: 16000, usageKey: 'old', totalOutput: 1100, speed: 50 }));
  assert.equal(getOutputSpeed(oldFrame, tempDir), null);
  assert.equal(getOutputSpeed(frame(300, 1400, 19000), tempDir), 100);
});

