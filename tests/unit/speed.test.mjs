import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import fs from 'node:fs';
import { getOutputSpeed, formatSpeed, getSpeedCachePath } from '../../runtime/speed.js';

let tempDir;

const frame = (output, apiMs, input = 1000) => ({
  transcript_path: path.join(tempDir, 'session.jsonl'),
  context_window: {
    current_usage: {
      input_tokens: input,
      output_tokens: output,
      cache_creation_input_tokens: 0,
      cache_read_input_tokens: 0,
    },
  },
  cost: { total_api_duration_ms: apiMs },
});

beforeEach(async () => {
  tempDir = await mkdtemp(path.join(tmpdir(), 'agy-speed-test-'));
});

afterEach(async () => {
  await rm(tempDir, { recursive: true, force: true });
});

test('speed is the finished response over API time added, held until next response', () => {
  // 1. Initial 0 output tokens returns null
  assert.equal(getOutputSpeed(frame(0, 1000), tempDir), null);

  // 2. First response: 400 tokens in (5000 - 1000 = 4000ms) => 100 tok/s
  assert.equal(getOutputSpeed(frame(400, 5000), tempDir), 100);

  // 3. Same frame keeps the reading
  assert.equal(getOutputSpeed(frame(400, 5000), tempDir), 100);

  // 4. API time increased but usage unchanged (subagents) => folds into baseline
  assert.equal(getOutputSpeed(frame(400, 9000), tempDir), 100);

  // 5. Under 500ms of API time (9200 - 9000 = 200ms) is too noisy => holds previous speed
  assert.equal(getOutputSpeed(frame(300, 9200, 2000), tempDir), 100);

  // 6. Valid new response: 300 tokens in (12000 - 9200 = 2800ms) => 300 / 2.8 tok/s
  assert.equal(
    getOutputSpeed(frame(300, 12000, 3000), tempDir)?.toFixed(1),
    (300 / 2.8).toFixed(1)
  );

  // 7. Counter reset starts over
  assert.equal(getOutputSpeed(frame(300, 100, 4000), tempDir), null);

  // 8. Missing transcript_path returns null
  assert.equal(getOutputSpeed({ ...frame(300, 100), transcript_path: undefined }, tempDir), null);
});

test('falls back to state.totalApiDurationMs when agyData.cost is missing', () => {
  const baseData = {
    transcript_path: path.join(tempDir, 'session.jsonl'),
    context_window: {
      current_usage: { input_tokens: 1000, output_tokens: 0 },
    },
  };
  getOutputSpeed(baseData, tempDir, { totalApiDurationMs: 1000 });

  const agyData = {
    transcript_path: path.join(tempDir, 'session.jsonl'),
    context_window: {
      current_usage: {
        input_tokens: 1000,
        output_tokens: 400,
      },
    },
  };
  const state = {
    totalApiDurationMs: 5000,
  };

  const speed = getOutputSpeed(agyData, tempDir, state);
  assert.equal(speed, 100); // 400 tokens / 4s = 100 tok/s
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

test('allows 5% (min 5 tokens) alignment tolerance and supports first-turn speed from cumulative total_output_tokens', () => {
  const tolTranscript = path.join(tempDir, 'tolerance.jsonl');
  const makeAgy = (output, totalOutput) => ({
    transcript_path: tolTranscript,
    context_window: {
      total_output_tokens: totalOutput,
      current_usage: {
        input_tokens: 1000,
        output_tokens: output,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
      },
    },
  });

  // 1000 vs 1045 -> diff 45 <= max(5, round(1000 * 0.05) = 50) -> accepted
  const alignedSpeed = getOutputSpeed(makeAgy(1000, 1000), tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 20000, outputTokens: 1045 },
  }, 20500);
  assert.equal(alignedSpeed, 100);

  // 100 vs 500 -> diff 400 > 5 -> rejected, falls back to previous speed (100)
  const misalignedSpeed = getOutputSpeed(makeAgy(100, 1100), tempDir, {
    lastPlannerResponse: { startMs: 25000, endMs: 27000, outputTokens: 500 },
  }, 27500);
  assert.equal(misalignedSpeed, 100);

  // First turn with ONLY cumulative total_output_tokens (no per-turn output_tokens)
  const cumTranscript = path.join(tempDir, 'cum-first-turn.jsonl');
  const cumAgy = {
    transcript_path: cumTranscript,
    context_window: {
      total_output_tokens: 600,
      current_usage: {
        input_tokens: 2000,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
      },
    },
  };
  const cumSpeed = getOutputSpeed(cumAgy, tempDir, {
    lastPlannerResponse: { startMs: 10000, endMs: 14000, outputTokens: 600 },
  }, 14500);
  assert.equal(cumSpeed, 150); // 600 / 4s
});

