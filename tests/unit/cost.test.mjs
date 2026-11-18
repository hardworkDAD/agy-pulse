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
  // Gemini 3.8 Flash: input $0.075/1M, output $0.30/1M, cache read $0.01875/1M
  const flashTokens = {
    input_tokens: 1_000_000,
    output_tokens: 1_000_000,
    cache_read_input_tokens: 1_000_000,
  };
  const flashCost = calculateTokenCost('Gemini 3.8 Flash (Medium)', flashTokens);
  assert.equal(flashCost?.toFixed(5), (0.075 + 0.30 + 0.01875).toFixed(5));

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
  // Flash: (100k/1M * 0.075) + (10k/1M * 0.30) = 0.0075 + 0.003 = 0.0105
  assert.equal(computed?.toFixed(4), (0.0105).toFixed(4));
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
  assert.equal(modelIncludesCacheInInput('Orchestrator (Argon)'), true);
  assert.equal(modelIncludesCacheInInput('gpt-oss-120b-medium'), true);
  assert.equal(modelIncludesCacheInInput('claude-opus-4-6-thinking'), false);
  assert.equal(modelIncludesCacheInInput('claude-sonnet-4-6'), false);
  assert.equal(modelIncludesCacheInInput(null), false);
});

test('getSessionCostUsd deducts cache for cache-inclusive models and includes cacheWrite', () => {
  // Gemini 4 Carbon: input $1.25/1M, output $10.00/1M, cacheRead $0.125/1M, cacheWrite $1.25/1M
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
  const expectedCarbon = (40_000 / 1e6) * 1.25 + (10_000 / 1e6) * 10.0 + (150_000 / 1e6) * 0.125 + (10_000 / 1e6) * 1.25;
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
  assert.equal(orchArgonCost?.toFixed(4), (0.50 + 3.00).toFixed(4));

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
