'use strict';

// Model pricing per 1,000,000 tokens (USD), ordered from specific to general.
const MODEL_PRICING = [
  {
    pattern: /opus-?5[.-]?5|opus\s+5\.5|5[.-]5[-_\s]+opus/i,
    input: 4.00,
    output: 20.00,
    cacheRead: 0.20,
    cacheWrite: 5.00,
  },
  {
    pattern: /sonnet-?5[.-]?5|sonnet\s+5\.5|5[.-]5[-_\s]+sonnet/i,
    input: 2.00,
    output: 10.00,
    cacheRead: 0.10,
    cacheWrite: 2.50,
  },
  {
    pattern: /opus[-_\s]*4[.-]?6|4[.-]6[-_\s]+opus/i,
    input: 5.00,
    output: 25.00,
    cacheRead: 0.50,
    cacheWrite: 6.25,
  },
  {
    pattern: /sonnet[-_\s]*4[.-]?6|4[.-]6[-_\s]+sonnet/i,
    input: 3.00,
    output: 15.00,
    cacheRead: 0.30,
    cacheWrite: 3.75,
  },
  {
    pattern: /haiku[-_\s]*4[.-]?5|4[.-]5[-_\s]+haiku/i,
    input: 0.80,
    output: 4.00,
    cacheRead: 0.08,
    cacheWrite: 1.00,
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
    pattern: /hydrogen|\bhydro\b/i,
    input: 1.50,
    output: 7.50,
    cacheRead: 0.075,
    cacheWrite: 1.50,
  },
  {
    pattern: /carbon|orchestrator.*carbon|\borch-c\b/i,
    input: 2.00,
    output: 10.00,
    cacheRead: 0.10,
    cacheWrite: 2.00,
  },
  {
    pattern: /argon|orchestrator.*argon|\borch-a\b/i,
    input: 2.00,
    output: 10.00,
    cacheRead: 0.10,
    cacheWrite: 2.00,
  },
  {
    pattern: /neon|orchestrator.*neon|\borch-n\b/i,
    input: 3.00,
    output: 15.00,
    cacheRead: 0.15,
    cacheWrite: 3.00,
  },
  {
    pattern: /orchestrator/i,
    input: 2.00,
    output: 10.00,
    cacheRead: 0.10,
    cacheWrite: 2.00,
  },
  {
    pattern: /gemini[-_\s]*3(?:\.\d+)?[-_\s]*flash/i,
    input: 0.75,
    output: 3.75,
    cacheRead: 0.075,
    cacheWrite: 0.75,
  },
  {
    pattern: /flash/i,
    input: 0.15,
    output: 0.60,
    cacheRead: 0.0375,
    cacheWrite: 0.15,
  },
  {
    pattern: /deepseek/i,
    input: 0.27,
    output: 1.10,
    cacheRead: 0.07,
    cacheWrite: 0.27,
  },
  {
    pattern: /\bo1-mini/i,
    input: 1.10,
    output: 4.40,
    cacheRead: 0.55,
    cacheWrite: 1.10,
  },
  {
    pattern: /\bo1-/i,
    input: 15.00,
    output: 60.00,
    cacheRead: 7.50,
    cacheWrite: 15.00,
  },
  {
    pattern: /\bo3-mini/i,
    input: 1.10,
    output: 4.40,
    cacheRead: 0.55,
    cacheWrite: 1.10,
  },
  {
    pattern: /\bo3-/i,
    input: 2.00,
    output: 8.00,
    cacheRead: 0.50,
    cacheWrite: 2.00,
  },
  {
    pattern: /\bo4-/i,
    input: 1.10,
    output: 4.40,
    cacheRead: 0.275,
    cacheWrite: 1.10,
  },
  {
    pattern: /gemini[-_\s]*3\.1[-_\s]*pro/i,
    input: 2.00,
    output: 12.00,
    cacheRead: 0.20,
    cacheWrite: 2.00,
    longThreshold: 200000,
    longInput: 4.00,
    longOutput: 18.00,
    longCacheRead: 0.40,
    longCacheWrite: 4.00,
  },
  {
    pattern: /pro/i,
    input: 1.25,
    output: 10.00,
    cacheRead: 0.125,
    cacheWrite: 1.25,
    longThreshold: 200000,
    longInput: 2.50,
    longOutput: 15.00,
    longCacheRead: 0.25,
    longCacheWrite: 2.50,
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
  input: 0.15,
  output: 0.60,
  cacheRead: 0.0375,
  cacheWrite: 0.15,
};

function modelIncludesCacheInInput(modelNameOrId) {
  if (!modelNameOrId || typeof modelNameOrId !== 'string') return false;
  if (/claude|anthropic|sonnet|opus|haiku/i.test(modelNameOrId)) return false;
  return /gemini|carbon|hydrogen|\bhydro\b|argon|neon|orchestrator|\borch-[can]\b|gpt|deepseek|o1-|o3-|o4-/i.test(modelNameOrId);
}

function isFiniteNonNeg(value) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0;
}

function pickDefinedFinite(...values) {
  for (const v of values) {
    if (isFiniteNonNeg(v)) return v;
  }
  return null;
}

function firstFiniteToken(...values) {
  for (const v of values) {
    if (isFiniteNonNeg(v)) return v;
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
  if (typeof cost !== 'number' || !Number.isFinite(cost) || cost <= 0) {
    return null;
  }
  const modelId = agyData?.model?.id || (typeof agyData?.model === 'string' ? agyData.model : '') || '';
  const routed = isBedrockModelId(modelId) || isVertexModelId(modelId);
  if (routed && !options?.allowRoutedCost) {
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
  const cacheTotal = cacheReadTokens + cacheWriteTokens;
  // Guard against clamping uncached input to 0 when upstream reports cache-exclusive counts
  const isActuallyInclusive = options?.cacheInclusive === true && rawInTokens >= cacheTotal;
  const inTokens = isActuallyInclusive
    ? rawInTokens - cacheTotal
    : rawInTokens;

  const totalPromptTokens = inTokens + cacheTotal;
  const useLongTier = Boolean(
    pricing.longThreshold &&
    options?.isCumulative !== true &&
    totalPromptTokens > pricing.longThreshold
  );

  const inputRate = useLongTier ? (pricing.longInput ?? pricing.input) : pricing.input;
  const outputRate = useLongTier ? (pricing.longOutput ?? pricing.output) : pricing.output;
  const cacheReadRate = useLongTier ? (pricing.longCacheRead ?? pricing.cacheRead) : pricing.cacheRead;
  const cacheWriteRate = useLongTier
    ? (pricing.longCacheWrite ?? pricing.cacheWrite ?? inputRate)
    : (pricing.cacheWrite ?? inputRate);

  const cost = (
    (inTokens / 1_000_000) * inputRate +
    (outTokens / 1_000_000) * outputRate +
    (cacheReadTokens / 1_000_000) * cacheReadRate +
    (cacheWriteTokens / 1_000_000) * cacheWriteRate
  );

  return Number.isFinite(cost) && cost >= 0 ? cost : null;
}

/**
 * Returns session cost: native cost when available, or precomputed per-turn
 * breakdown from state, or falls back to token usage estimation from agyData.
 *
 * @param {Object} agyData
 * @param {Object} [state]
 * @param {Object} [options]
 * @returns {number|null}
 */
function getSessionCostUsd(agyData, state = null, options = {}) {
  const native = getNativeCostUsd(agyData, options);
  if (native !== null) {
    if (state?.costBreakdown && isFiniteNonNeg(state.costBreakdown.childrenCostUsd) && state.costBreakdown.childrenCostUsd > 0) {
      const breakdownTotal = isFiniteNonNeg(state.costBreakdown.totalCostUsd) ? state.costBreakdown.totalCostUsd : 0;
      return Math.max(breakdownTotal, native + state.costBreakdown.childrenCostUsd);
    }
    return native;
  }

  if (state?.costBreakdown && isFiniteNonNeg(state.costBreakdown.totalCostUsd) && state.costBreakdown.totalCostUsd > 0) {
    return state.costBreakdown.totalCostUsd;
  }

  const cw = agyData?.context_window;
  const cwCur = cw?.current_usage;
  const stateUsage = state?.usage;
  const stateCur = stateUsage?.current_usage;
  const stateLast = state?.lastTurnUsage;
  if (!cw && !stateUsage && !stateLast) {
    if (state?.costBreakdown && isFiniteNonNeg(state.costBreakdown.totalCostUsd)) {
      return state.costBreakdown.totalCostUsd;
    }
    return null;
  }

  const modelName = agyData?.model?.display_name || agyData?.model?.id || (typeof agyData?.model === 'string' ? agyData.model : '') || stateLast?.model || '';
  const hasCwTotalInput = isFiniteNonNeg(cw?.total_input_tokens);
  const inputTokens = firstFiniteToken(
    cw?.total_input_tokens,
    cwCur?.input_tokens,
    cw?.input_tokens,
    stateUsage?.total_input_tokens,
    stateCur?.input_tokens,
    stateUsage?.input_tokens,
    stateLast?.input_tokens,
    0
  );
  const outputTokens = firstFiniteToken(
    cw?.total_output_tokens,
    cwCur?.output_tokens,
    cw?.output_tokens,
    state?.totalOutputTokens,
    stateUsage?.total_output_tokens,
    stateCur?.output_tokens,
    stateUsage?.output_tokens,
    stateLast?.output_tokens,
    0
  );

  let cacheReadTokens;
  let cacheCreateTokens;
  if (hasCwTotalInput) {
    const cumCacheRead = pickDefinedFinite(
      cw?.total_cache_read_input_tokens,
      cw?.total_cache_read_tokens,
      cw?.cache_read_input_tokens,
      cw?.cache_read_tokens,
      state?.totalCacheReadTokens > 0 ? state.totalCacheReadTokens : null
    );
    const cumCacheCreate = pickDefinedFinite(
      cw?.total_cache_creation_input_tokens,
      cw?.total_cache_creation_tokens,
      cw?.cache_creation_input_tokens,
      cw?.cache_creation_tokens,
      state?.totalCacheCreationTokens > 0 ? state.totalCacheCreationTokens : null
    );

    if (cumCacheRead !== null || cumCacheCreate !== null) {
      cacheReadTokens = cumCacheRead ?? 0;
      cacheCreateTokens = cumCacheCreate ?? 0;
    } else if (isFiniteNonNeg(cwCur?.input_tokens) && cwCur.input_tokens > 0 && cw.total_input_tokens > cwCur.input_tokens) {
      const cwCurRead = firstFiniteToken(cwCur?.cache_read_input_tokens, cwCur?.cache_read_tokens, 0);
      const cwCurCreate = firstFiniteToken(cwCur?.cache_creation_input_tokens, cwCur?.cache_creation_tokens, 0);
      cacheReadTokens = Math.min(
        cw.total_input_tokens,
        Math.round((cwCurRead / cwCur.input_tokens) * cw.total_input_tokens)
      );
      cacheCreateTokens = Math.min(
        Math.max(0, cw.total_input_tokens - cacheReadTokens),
        Math.round((cwCurCreate / cwCur.input_tokens) * cw.total_input_tokens)
      );
    } else {
      cacheReadTokens = firstFiniteToken(
        cwCur?.cache_read_input_tokens,
        cwCur?.cache_read_tokens,
        stateCur?.cache_read_input_tokens,
        stateCur?.cache_read_tokens,
        stateUsage?.cache_read_input_tokens,
        stateUsage?.cache_read_tokens,
        stateLast?.cache_read_input_tokens,
        0
      );
      cacheCreateTokens = firstFiniteToken(
        cwCur?.cache_creation_input_tokens,
        cwCur?.cache_creation_tokens,
        stateCur?.cache_creation_input_tokens,
        stateCur?.cache_creation_tokens,
        stateUsage?.cache_creation_input_tokens,
        stateUsage?.cache_creation_tokens,
        stateLast?.cache_creation_input_tokens,
        0
      );
    }
  } else {
    cacheReadTokens = firstFiniteToken(
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
    );
    cacheCreateTokens = firstFiniteToken(
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
    );
  }

  const tokens = {
    input_tokens: inputTokens,
    output_tokens: outputTokens,
    cache_read_input_tokens: cacheReadTokens,
    cache_creation_input_tokens: cacheCreateTokens,
  };

  const cacheTotal = tokens.cache_read_input_tokens + tokens.cache_creation_input_tokens;
  const cacheInclusive = modelIncludesCacheInInput(modelName) ||
    (/claude|anthropic|sonnet|opus|haiku/i.test(modelName) && cacheTotal > 0 && tokens.input_tokens >= cacheTotal);

  return calculateTokenCost(modelName, tokens, {
    cacheInclusive,
    isCumulative: hasCwTotalInput,
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
