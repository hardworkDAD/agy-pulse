import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  getNativeCostUsd,
  calculateTokenCost,
  getSessionCostUsd,
  formatUsd,
  modelIncludesCacheInInput,
} from '../../runtime/cost.js';

test('getNativeCostUsd extracts native cost from agyData', () => {
  assert.equal(getNativeCostUsd({ cost: { total_cost_usd: 1.5 } }), 1.5);
  assert.equal(getNativeCostUsd({ cost: { total_usd: 2.25 } }), 2.25);
  assert.equal(getNativeCostUsd({ total_usd: 0.75 }), 0.75);
  assert.equal(getNativeCostUsd({ cost: { total_cost_usd: -1 } }), null);
  assert.equal(getNativeCostUsd({ cost: null }), null);
  assert.equal(getNativeCostUsd({}), null);
});

test('getNativeCostUsd respects routed models unless allowRoutedCost is set', () => {
  const stdin = {
    model: { id: 'us.anthropic.claude-sonnet-4-20250514-v1:0' },
    cost: { total_cost_usd: 2.0 },
  };
  assert.equal(getNativeCostUsd(stdin), null);
  assert.equal(getNativeCostUsd(stdin, { allowRoutedCost: true }), 2.0);
});

test('calculateTokenCost calculates estimated cost from tokens and model rates', () => {
  // Gemini 3.8 Flash: input $0.75/1M, output $3.75/1M, cache read $0.075/1M
  const flashTokens = {
    input_tokens: 1_000_000,
    output_tokens: 1_000_000,
    cache_read_input_tokens: 1_000_000,
  };
  const flashCost = calculateTokenCost('Gemini 3.8 Flash (Medium)', flashTokens);
  assert.equal(flashCost?.toFixed(5), (0.75 + 3.75 + 0.075).toFixed(5));

  // Claude 3.5 Sonnet: input $3/1M, output $15/1M, cache read $0.30/1M
  const sonnetTokens = {
    input_tokens: 100_000,
    output_tokens: 10_000,
    cache_read_input_tokens: 50_000,
  };
  const sonnetCost = calculateTokenCost('Claude Sonnet 3.5', sonnetTokens);
  // (100k/1M * 3) + (10k/1M * 15) + (50k/1M * 0.3) = 0.30 + 0.15 + 0.015 = 0.465
  assert.equal(sonnetCost?.toFixed(4), (0.465).toFixed(4));
});

test('getSessionCostUsd prefers native cost, falls back to calculated token cost', () => {
  // When native cost is present:
  const withNative = {
    cost: { total_cost_usd: 0.42 },
    model: { id: 'gemini-3.8-flash' },
    context_window: { total_input_tokens: 1000, total_output_tokens: 500 },
  };
  assert.equal(getSessionCostUsd(withNative), 0.42);

  // When native cost is absent, computes from context_window tokens:
  const withoutNative = {
    model: { id: 'gemini-3.8-flash' },
    context_window: {
      total_input_tokens: 100_000,
      total_output_tokens: 10_000,
    },
  };
  const computed = getSessionCostUsd(withoutNative);
  // Gemini 3.8 Flash: (100k/1M * 0.75) + (10k/1M * 3.75) = 0.075 + 0.0375 = 0.1125
  assert.equal(computed?.toFixed(4), (0.1125).toFixed(4));
});

test('formatUsd formats currencies across thresholds', () => {
  assert.equal(formatUsd(1.2345), '$1.23');
  assert.equal(formatUsd(0.4567), '$0.457');
  assert.equal(formatUsd(0.01234), '$0.0123');
  assert.equal(formatUsd(0), '$0.00');
});

test('modelIncludesCacheInInput distinguishes cache-inclusive vs cache-exclusive models', () => {
  assert.equal(modelIncludesCacheInInput('Gemini 4 Carbon (High)'), true);
  assert.equal(modelIncludesCacheInInput('gemini-4-hydrogen-xhigh'), true);
  assert.equal(modelIncludesCacheInInput('gemini-4-hydrogen-sonar'), true);
  assert.equal(modelIncludesCacheInInput('gemini-4-neon-high'), true);
  assert.equal(modelIncludesCacheInInput('Gemini 4 Neon (XHigh)'), true);
  assert.equal(modelIncludesCacheInInput('Orchestrator (Argon)'), true);
  assert.equal(modelIncludesCacheInInput('Orchestrator (Neon)'), true);
  assert.equal(modelIncludesCacheInInput('Neon'), true);
  assert.equal(modelIncludesCacheInInput('Orch-N'), true);
  assert.equal(modelIncludesCacheInInput('Orch-C'), true);
  assert.equal(modelIncludesCacheInInput('Orch-A'), true);
  assert.equal(modelIncludesCacheInInput('Hydro'), true);
  assert.equal(modelIncludesCacheInInput('gpt-oss-120b-medium'), true);
  assert.equal(modelIncludesCacheInInput('claude-opus-4-6-thinking'), false);
  assert.equal(modelIncludesCacheInInput('claude-sonnet-4-6'), false);
  assert.equal(modelIncludesCacheInInput(null), false);
});

test('getSessionCostUsd deducts cache for cache-inclusive models and includes cacheWrite', () => {
  // Gemini 4 Carbon: input $2.00/1M, output $10.00/1M, cacheRead $0.10/1M, cacheWrite $2.00/1M
  // input_tokens=200k includes cacheRead=150k and cacheWrite=10k -> nonCachedInput=40k
  const carbonCost = getSessionCostUsd({
    model: { id: 'gemini-4-carbon-high' },
    context_window: {
      current_usage: {
        input_tokens: 200_000,
        output_tokens: 10_000,
        cache_read_input_tokens: 150_000,
        cache_creation_input_tokens: 10_000,
      },
    },
  });
  const expectedCarbon = (40_000 / 1e6) * 2.0 + (10_000 / 1e6) * 10.0 + (150_000 / 1e6) * 0.10 + (10_000 / 1e6) * 2.0;
  assert.equal(carbonCost?.toFixed(6), expectedCarbon.toFixed(6));

  // Claude Opus 4.6 (cache-exclusive): input $5/1M, output $25/1M, cacheRead $0.50/1M, cacheWrite $6.25/1M
  // input_tokens=40k does NOT include cache tokens
  const opusCost = getSessionCostUsd(
    { model: { id: 'claude-opus-4-6-thinking' } },
    {
      usage: {
        input_tokens: 40_000,
        output_tokens: 10_000,
        cache_read_input_tokens: 150_000,
        cache_creation_input_tokens: 10_000,
      },
    }
  );
  const expectedOpus = (40_000 / 1e6) * 5.0 + (10_000 / 1e6) * 25.0 + (150_000 / 1e6) * 0.50 + (10_000 / 1e6) * 6.25;
  assert.equal(opusCost?.toFixed(6), expectedOpus.toFixed(6));

  // GPT-OSS 120B & Orchestrator Argon pricing sanity check
  const gptOssCost = getSessionCostUsd({
    model: { id: 'gpt-oss-120b-medium' },
    context_window: { total_input_tokens: 1_000_000, total_output_tokens: 1_000_000 },
  });
  assert.equal(gptOssCost?.toFixed(4), (0.15 + 0.60).toFixed(4));

  const orchArgonCost = getSessionCostUsd({
    model: { id: 'gemini-pro-orchestrator-argon' },
    context_window: { total_input_tokens: 1_000_000, total_output_tokens: 1_000_000 },
  });
  assert.equal(orchArgonCost?.toFixed(4), (2.00 + 10.00).toFixed(4));

  // Claude Opus 4.6 when AGY CLI reports cache-inclusive input_tokens >= cacheTotal (200k >= 160k -> 40k uncached)
  const opusInclusiveCost = getSessionCostUsd({
    model: { id: 'claude-opus-4-6-thinking' },
    context_window: {
      current_usage: {
        input_tokens: 200_000,
        output_tokens: 10_000,
        cache_read_input_tokens: 150_000,
        cache_creation_input_tokens: 10_000,
      },
    },
  });
  assert.equal(opusInclusiveCost?.toFixed(6), expectedOpus.toFixed(6));
});

test('getSessionCostUsd pairs cumulative total_input_tokens with cumulative or scaled cache tokens', () => {
  // Multi-turn Gemini 4 Carbon: total_input_tokens=1,000,000 while current_usage is a single 100k turn (80k cache read, 5k cache create)
  // Scaled cache: 800k cache read, 50k cache create -> 150k uncached
  const scaledCost = getSessionCostUsd({
    model: 'gemini-4-carbon-high',
    context_window: {
      total_input_tokens: 1_000_000,
      total_output_tokens: 50_000,
      current_usage: {
        input_tokens: 100_000,
        output_tokens: 5_000,
        cache_read_input_tokens: 80_000,
        cache_creation_input_tokens: 5_000,
      },
    },
  });
  const expectedScaled = (150_000 / 1e6) * 2.0 + (50_000 / 1e6) * 10.0 + (800_000 / 1e6) * 0.10 + (50_000 / 1e6) * 2.0;
  assert.equal(scaledCost?.toFixed(6), expectedScaled.toFixed(6));

  // When state provides session-level cumulative cache tokens, prefer them over ratio estimation
  const statePairedCost = getSessionCostUsd(
    {
      model: { id: 'gemini-4-carbon-high' },
      context_window: {
        total_input_tokens: 1_000_000,
        total_output_tokens: 50_000,
        current_usage: {
          input_tokens: 100_000,
          output_tokens: 5_000,
          cache_read_input_tokens: 80_000,
          cache_creation_input_tokens: 5_000,
        },
      },
    },
    { totalCacheReadTokens: 700_000, totalCacheCreationTokens: 100_000 }
  );
  const expectedStatePaired = (200_000 / 1e6) * 2.0 + (50_000 / 1e6) * 10.0 + (700_000 / 1e6) * 0.10 + (100_000 / 1e6) * 2.0;
  assert.equal(statePairedCost?.toFixed(6), expectedStatePaired.toFixed(6));
});

test('calculateTokenCost prices deepseek, o1-, o3-, and o4- models without falling back to Flash', () => {
  const tokens = { input_tokens: 1_000_000, output_tokens: 1_000_000 };
  const legacyFlashCost = calculateTokenCost('gemini-2.0-flash', tokens);
  for (const modelId of ['deepseek-chat', 'o1-preview', 'o1-mini', 'o3-mini', 'o4-mini']) {
    const c = calculateTokenCost(modelId, tokens);
    assert.ok(c > legacyFlashCost, `expected ${modelId} cost (${c}) > legacy flash (${legacyFlashCost})`);
  }
});

test('calculateTokenCost matches updated ACV pricing for Claude 5.5, Gemini 4, and Gemini 3.x Flash', () => {
  const oneM = {
    input_tokens: 1_000_000,
    output_tokens: 1_000_000,
    cache_read_input_tokens: 1_000_000,
    cache_creation_input_tokens: 1_000_000,
  };
  assert.equal(calculateTokenCost('claude-opus-5-5', oneM)?.toFixed(4), (4 + 20 + 0.20 + 5).toFixed(4));
  assert.equal(calculateTokenCost('Claude Sonnet 5.5', oneM)?.toFixed(4), (2 + 10 + 0.10 + 2.50).toFixed(4));
  assert.equal(calculateTokenCost('gemini-4-hydrogen-high', oneM)?.toFixed(4), (1.5 + 7.5 + 0.075 + 1.5).toFixed(4));
  assert.equal(calculateTokenCost('gemini-4-carbon-high', oneM)?.toFixed(4), (2 + 10 + 0.10 + 2).toFixed(4));
  assert.equal(calculateTokenCost('gemini-4-argon-high', oneM)?.toFixed(4), (2 + 10 + 0.10 + 2).toFixed(4));
  assert.equal(calculateTokenCost('gemini-3.5-flash', oneM)?.toFixed(4), (0.75 + 3.75 + 0.075 + 0.75).toFixed(4));
  assert.equal(calculateTokenCost('gemini-2.0-flash', oneM)?.toFixed(5), (0.15 + 0.60 + 0.0375 + 0.15).toFixed(5));
});

test('calculateTokenCost applies >200k tiered pricing for Gemini Pro models unless isCumulative is true', () => {
  // Gemini 3.1 Pro: <=200k ($2/$12/$0.20), >200k ($4/$18/$0.40)
  const underLimit = { input_tokens: 50_000, cache_read_input_tokens: 150_000, output_tokens: 10_000 };
  const overLimit = { input_tokens: 50_001, cache_read_input_tokens: 150_000, output_tokens: 10_000 };
  const expectedUnder31 = (50_000 / 1e6) * 2.0 + (150_000 / 1e6) * 0.20 + (10_000 / 1e6) * 12.0;
  const expectedOver31 = (50_001 / 1e6) * 4.0 + (150_000 / 1e6) * 0.40 + (10_000 / 1e6) * 18.0;
  assert.equal(calculateTokenCost('gemini-3.1-pro-preview', underLimit)?.toFixed(6), expectedUnder31.toFixed(6));
  assert.equal(calculateTokenCost('gemini-3.1-pro-preview', overLimit)?.toFixed(6), expectedOver31.toFixed(6));

  // Cumulative sums must NOT trigger >200k single-turn premium
  const cumulative31 = calculateTokenCost('gemini-3.1-pro-preview', overLimit, { isCumulative: true });
  const expectedCum31 = (50_001 / 1e6) * 2.0 + (150_000 / 1e6) * 0.20 + (10_000 / 1e6) * 12.0;
  assert.equal(cumulative31?.toFixed(6), expectedCum31.toFixed(6));

  // Gemini 2.5 Pro: <=200k ($1.25/$10/$0.125), >200k ($2.50/$15/$0.25)
  const expectedUnder25 = (50_000 / 1e6) * 1.25 + (150_000 / 1e6) * 0.125 + (10_000 / 1e6) * 10.0;
  const expectedOver25 = (50_001 / 1e6) * 2.50 + (150_000 / 1e6) * 0.25 + (10_000 / 1e6) * 15.0;
  assert.ok(Math.abs(calculateTokenCost('gemini-2.5-pro', underLimit) - expectedUnder25) < 1e-12);
  assert.ok(Math.abs(calculateTokenCost('gemini-2.5-pro', overLimit) - expectedOver25) < 1e-12);
});

test('calculateTokenCost protects mutually exclusive input when cacheInclusive=true and rawInTokens < cacheTotal', () => {
  // Real Antigravity turn: input=2,339, cacheRead=369,996, output=3,623
  const cost = calculateTokenCost('gemini-4-carbon-high', {
    input_tokens: 2_339,
    cache_read_input_tokens: 369_996,
    output_tokens: 3_623,
  }, { cacheInclusive: true });

  const expected = (2_339 / 1e6) * 2.0 + (369_996 / 1e6) * 0.10 + (3_623 / 1e6) * 10.0;
  assert.equal(cost?.toFixed(6), expected.toFixed(6));
});

test('getNativeCostUsd returns null for zero placeholders and getSessionCostUsd prioritizes state.costBreakdown', () => {
  assert.equal(getNativeCostUsd({ cost: { total_usd: 0 } }), null);
  assert.equal(getNativeCostUsd({ cost: { total_cost_usd: 0 } }), null);
  assert.equal(getNativeCostUsd({ total_usd: 0 }), null);

  // Zero native placeholder must not shadow state.costBreakdown
  const agyWithZeroPlaceholder = {
    model: { id: 'gemini-4-carbon-high' },
    cost: { total_usd: 0 },
    context_window: { total_input_tokens: 1000, total_output_tokens: 200 },
  };
  const stateWithBreakdown = {
    costBreakdown: {
      totalCostUsd: 21.6365,
      mainCostUsd: 9.8936,
      childrenCostUsd: 11.7429,
      childSessionCount: 15,
      byModel: {
        Carbon: { costUsd: 9.8936, inputTokens: 2967648, cacheReadTokens: 25052429, cacheWriteTokens: 0, outputTokens: 145309, steps: 117 },
        Hydro: { costUsd: 11.7429, inputTokens: 2810844, cacheReadTokens: 35885061, cacheWriteTokens: 0, outputTokens: 644705, steps: 364 },
      },
    },
  };
  assert.equal(getSessionCostUsd(agyWithZeroPlaceholder, stateWithBreakdown), 21.6365);

  // Fresh zero-token session with empty costBreakdown returns 0
  assert.equal(getSessionCostUsd({}, { costBreakdown: { totalCostUsd: 0, byModel: {} } }), 0);
});

test('calculateTokenCost supports version-first Claude 4.6 Opus/Sonnet and 4.5 Haiku naming', () => {
  const oneM = {
    input_tokens: 1_000_000,
    output_tokens: 1_000_000,
    cache_read_input_tokens: 1_000_000,
    cache_creation_input_tokens: 1_000_000,
  };
  const expectedOpus46 = 5 + 25 + 0.50 + 6.25;
  assert.equal(calculateTokenCost('Claude 4.6 Opus', oneM)?.toFixed(4), expectedOpus46.toFixed(4));
  assert.equal(calculateTokenCost('claude-4-6-opus-thinking', oneM)?.toFixed(4), expectedOpus46.toFixed(4));
  assert.equal(calculateTokenCost('Claude Opus 4.6', oneM)?.toFixed(4), expectedOpus46.toFixed(4));

  const expectedSonnet46 = 3 + 15 + 0.30 + 3.75;
  assert.equal(calculateTokenCost('Claude 4.6 Sonnet', oneM)?.toFixed(4), expectedSonnet46.toFixed(4));
  assert.equal(calculateTokenCost('claude-4-6-sonnet', oneM)?.toFixed(4), expectedSonnet46.toFixed(4));

  const expectedHaiku45 = 0.80 + 4.00 + 0.08 + 1.00;
  assert.equal(calculateTokenCost('Claude 4.5 Haiku', oneM)?.toFixed(4), expectedHaiku45.toFixed(4));
  assert.equal(calculateTokenCost('claude-4-5-haiku', oneM)?.toFixed(4), expectedHaiku45.toFixed(4));
});

test('getSessionCostUsd preserves subagent childrenCostUsd when main native cost is positive', () => {
  const agy = {
    model: { id: 'gemini-4-carbon-high' },
    cost: { total_cost_usd: 0.60 },
  };
  const state = {
    costBreakdown: {
      totalCostUsd: 1.90,
      mainCostUsd: 0.40,
      childrenCostUsd: 1.50,
    },
  };
  // max(1.90, 0.60 + 1.50) = 2.10
  assert.ok(Math.abs(getSessionCostUsd(agy, state) - 2.10) < 1e-9);

  const stateHigherBreakdown = {
    costBreakdown: {
      totalCostUsd: 2.50,
      mainCostUsd: 1.00,
      childrenCostUsd: 1.50,
    },
  };
  // max(2.50, 0.60 + 1.50) = 2.50
  assert.ok(Math.abs(getSessionCostUsd(agy, stateHigherBreakdown) - 2.50) < 1e-9);
});

test('calculateTokenCost prices Gemini 4 Neon & Orchestrator (Neon) flat at $3/$15/$0.15 across <=200k and >200k contexts', () => {
  const shortTokens = {
    input_tokens: 100_000,
    cache_read_input_tokens: 50_000,
    cache_creation_input_tokens: 10_000,
    output_tokens: 5_000,
  };
  const expectedShort = (100_000 / 1e6) * 3.00 + (50_000 / 1e6) * 0.15 + (10_000 / 1e6) * 3.00 + (5_000 / 1e6) * 15.00;

  const longTokens = {
    input_tokens: 300_000,
    cache_read_input_tokens: 500_000,
    output_tokens: 10_000,
  };
  // 300K * $3/1M + 500K * $0.15/1M + 10K * $15/1M = $0.90 + $0.075 + $0.15 = $1.125
  const expectedLong = (300_000 / 1e6) * 3.00 + (500_000 / 1e6) * 0.15 + (10_000 / 1e6) * 15.00;

  const neonVariants = [
    'gemini-4-neon',
    'gemini-4-neon-high',
    'Gemini 4 Neon (XHigh)',
    'gemini-pro-orchestrator-neon',
    'Orchestrator (Neon)',
    'Neon',
    'Orch-N',
  ];

  for (const model of neonVariants) {
    assert.ok(
      Math.abs(calculateTokenCost(model, shortTokens) - expectedShort) < 1e-12,
      `${model} short-context cost mismatch`
    );
    assert.ok(
      Math.abs(calculateTokenCost(model, longTokens) - expectedLong) < 1e-12,
      `${model} >200k long-context cost must stay flat at $3/$15/$0.15`
    );
  }

  // Short-name lookups for existing Carbon / Argon / Hydrogen families and Sonar
  assert.ok(Math.abs(calculateTokenCost('Orch-C', longTokens) - ((300_000 / 1e6) * 2.0 + (500_000 / 1e6) * 0.10 + (10_000 / 1e6) * 10.0)) < 1e-12);
  assert.ok(Math.abs(calculateTokenCost('Orch-A', longTokens) - ((300_000 / 1e6) * 2.0 + (500_000 / 1e6) * 0.10 + (10_000 / 1e6) * 10.0)) < 1e-12);
  assert.ok(Math.abs(calculateTokenCost('Hydro', longTokens) - ((300_000 / 1e6) * 1.5 + (500_000 / 1e6) * 0.075 + (10_000 / 1e6) * 7.5)) < 1e-12);
  assert.ok(Math.abs(calculateTokenCost('gemini-4-hydrogen-sonar', longTokens) - ((300_000 / 1e6) * 1.5 + (500_000 / 1e6) * 0.075 + (10_000 / 1e6) * 7.5)) < 1e-12);
});
