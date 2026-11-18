'use strict';

// Model pricing per 1,000,000 tokens (USD)
const MODEL_PRICING = [
  {
    pattern: /opus-?4[.-]?6|opus\s+4\.6/i,
    input: 5.00,
    output: 25.00,
    cacheRead: 0.50,
    cacheWrite: 6.25,
  },
  {
    pattern: /opus/i,
    input: 15.00,
    output: 75.00,
    cacheRead: 1.50,
    cacheWrite: 18.75,
  },
  {
    pattern: /sonnet/i,
    input: 3.00,
    output: 15.00,
    cacheRead: 0.30,
    cacheWrite: 3.75,
  },
  {
    pattern: /haiku/i,
    input: 0.80,
    output: 4.00,
    cacheRead: 0.08,
    cacheWrite: 1.00,
  },
  {
    pattern: /orchestrator.*argon/i,
    input: 0.50,
    output: 3.00,
    cacheRead: 0.05,
    cacheWrite: 0.50,
  },
  {
    pattern: /orchestrator/i,
    input: 1.25,
    output: 10.00,
    cacheRead: 0.125,
    cacheWrite: 1.25,
  },
  {
    pattern: /hydrogen/i,
    input: 2.00,
    output: 12.00,
    cacheRead: 0.20,
    cacheWrite: 2.00,
  },
  {
    pattern: /carbon/i,
    input: 1.25,
    output: 10.00,
    cacheRead: 0.125,
    cacheWrite: 1.25,
  },
  {
    pattern: /argon/i,
    input: 0.50,
    output: 3.00,
    cacheRead: 0.05,
    cacheWrite: 0.50,
  },
  {
    pattern: /flash/i,
    input: 0.075,
    output: 0.30,
    cacheRead: 0.01875,
    cacheWrite: 0.075,
  },
  {
    pattern: /pro/i,
    input: 1.25,
    output: 5.00,
    cacheRead: 0.3125,
    cacheWrite: 1.25,
  },
  {
    pattern: /gpt-oss/i,
    input: 0.15,
    output: 0.60,
    cacheRead: 0.075,
    cacheWrite: 0.15,
  },
  {
    pattern: /gpt-?4/i,
    input: 2.50,
    output: 10.00,
    cacheRead: 1.25,
    cacheWrite: 2.50,
  },
];

const DEFAULT_PRICING = {
  input: 0.075,
  output: 0.30,
  cacheRead: 0.01875,
  cacheWrite: 0.075,
};

function modelIncludesCacheInInput(modelNameOrId) {
  if (!modelNameOrId || typeof modelNameOrId !== 'string') return false;
  if (/claude|anthropic|sonnet|opus|haiku/i.test(modelNameOrId)) return false;
  return /gemini|carbon|hydrogen|argon|orchestrator|gpt|deepseek|o1-|o3-|o4-/i.test(modelNameOrId);
}

function firstFiniteToken(...values) {
  for (const v of values) {
    if (typeof v === 'number' && Number.isFinite(v) && v >= 0) return v;
  }
  return 0;
}

function isBedrockModelId(id) {
  return typeof id === 'string' && (id.startsWith('anthropic.') || id.startsWith('us.anthropic.') || id.startsWith('eu.anthropic.'));
}

function isVertexModelId(id) {
  return typeof id === 'string' && (id.startsWith('claude-') && id.includes('@'));
}

/**
 * Native session cost reported directly by agy/Claude CLI.
 *
 * @param {Object} agyData
 * @param {Object} [options]
 * @returns {number|null}
 */
function getNativeCostUsd(agyData, options = {}) {
  const cost = agyData?.cost?.total_cost_usd ?? agyData?.cost?.total_usd ?? agyData?.total_usd;
  if (typeof cost !== 'number' || !Number.isFinite(cost) || cost < 0) {
    return null;
  }
  const modelId = agyData?.model?.id || '';
  const routed = isBedrockModelId(modelId) || isVertexModelId(modelId);
  if (routed && (!options?.allowRoutedCost || cost === 0)) {
    return null;
  }
  return cost;
}

/**
 * Calculates estimated cost from token counts and model pricing.
 *
 * @param {string} modelNameOrId
 * @param {Object} tokens
 * @param {Object} [options]
 * @returns {number|null}
 */
function calculateTokenCost(modelNameOrId, tokens, options = {}) {
  if (!tokens || typeof tokens !== 'object') return null;

  const modelStr = String(modelNameOrId || '');
  const pricing = MODEL_PRICING.find(p => p.pattern.test(modelStr)) || DEFAULT_PRICING;

  const rawInTokens = Number(tokens.input_tokens ?? tokens.total_input_tokens ?? 0);
  const outTokens = Number(tokens.output_tokens ?? tokens.total_output_tokens ?? 0);
  const cacheReadTokens = Number(tokens.cache_read_input_tokens ?? tokens.cache_read_tokens ?? 0);
  const cacheWriteTokens = Number(tokens.cache_creation_input_tokens ?? tokens.cache_creation_tokens ?? 0);
  const inTokens = options?.cacheInclusive
    ? Math.max(0, rawInTokens - cacheReadTokens - cacheWriteTokens)
    : rawInTokens;
  const cacheWriteRate = pricing.cacheWrite ?? pricing.input;

  const cost = (
    (inTokens / 1_000_000) * pricing.input +
    (outTokens / 1_000_000) * pricing.output +
    (cacheReadTokens / 1_000_000) * pricing.cacheRead +
    (cacheWriteTokens / 1_000_000) * cacheWriteRate
  );

  return Number.isFinite(cost) && cost >= 0 ? cost : null;
}

/**
 * Returns session cost: native cost when available, or calculates from token usage.
 *
 * @param {Object} agyData
 * @param {Object} [state]
 * @param {Object} [options]
 * @returns {number|null}
 */
function getSessionCostUsd(agyData, state = null, options = {}) {
  const native = getNativeCostUsd(agyData, options);
  if (native !== null) {
    return native;
  }

  const cw = agyData?.context_window;
  const cwCur = cw?.current_usage;
  const stateUsage = state?.usage;
  const stateCur = stateUsage?.current_usage;
  const stateLast = state?.lastTurnUsage;
  if (!cw && !stateUsage && !stateLast) return null;

  const modelName = agyData?.model?.display_name || agyData?.model?.id || stateLast?.model || '';
  const tokens = {
    input_tokens: firstFiniteToken(
      cw?.total_input_tokens,
      cwCur?.input_tokens,
      cw?.input_tokens,
      stateUsage?.total_input_tokens,
      stateCur?.input_tokens,
      stateUsage?.input_tokens,
      stateLast?.input_tokens,
      0
    ),
    output_tokens: firstFiniteToken(
      cw?.total_output_tokens,
      cwCur?.output_tokens,
      cw?.output_tokens,
      state?.totalOutputTokens,
      stateUsage?.total_output_tokens,
      stateCur?.output_tokens,
      stateUsage?.output_tokens,
      stateLast?.output_tokens,
      0
    ),
    cache_read_input_tokens: firstFiniteToken(
      cwCur?.cache_read_input_tokens,
      cwCur?.cache_read_tokens,
      cw?.cache_read_input_tokens,
      cw?.cache_read_tokens,
      stateCur?.cache_read_input_tokens,
      stateCur?.cache_read_tokens,
      stateUsage?.cache_read_input_tokens,
      stateUsage?.cache_read_tokens,
      stateLast?.cache_read_input_tokens,
      0
    ),
    cache_creation_input_tokens: firstFiniteToken(
      cwCur?.cache_creation_input_tokens,
      cwCur?.cache_creation_tokens,
      cw?.cache_creation_input_tokens,
      cw?.cache_creation_tokens,
      stateCur?.cache_creation_input_tokens,
      stateCur?.cache_creation_tokens,
      stateUsage?.cache_creation_input_tokens,
      stateUsage?.cache_creation_tokens,
      stateLast?.cache_creation_input_tokens,
      0
    ),
  };

  const cacheTotal = tokens.cache_read_input_tokens + tokens.cache_creation_input_tokens;
  const cacheInclusive = modelIncludesCacheInInput(modelName) ||
    (/claude|anthropic|sonnet|opus|haiku/i.test(modelName) && cacheTotal > 0 && tokens.input_tokens >= cacheTotal);

  return calculateTokenCost(modelName, tokens, {
    cacheInclusive,
  });
}

/**
 * Formats a USD amount according to standard tiered decimal rules.
 *
 * @param {number} amount
 * @returns {string}
 */
function formatUsd(amount) {
  if (typeof amount !== 'number' || !Number.isFinite(amount) || amount <= 0) return '$0.00';
  if (amount >= 1) return `$${amount.toFixed(2)}`;
  if (amount >= 0.1) return `$${amount.toFixed(3)}`;
  return `$${amount.toFixed(4)}`;
}

module.exports = {
  MODEL_PRICING,
  DEFAULT_PRICING,
  modelIncludesCacheInInput,
  getNativeCostUsd,
  calculateTokenCost,
  getSessionCostUsd,
  formatUsd,
  isBedrockModelId,
  isVertexModelId,
};
