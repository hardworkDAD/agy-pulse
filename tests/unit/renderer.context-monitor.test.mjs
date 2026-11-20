import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { renderStatusLine } from '../../runtime/renderer.js';
import { getAnsiDisplayWidth } from '../../runtime/renderer/format.js';

const require = createRequire(import.meta.url);
const {
  formatMonitorTokens,
  buildSmoothBar,
  renderContextMonitorLine,
  renderModelCacheLine,
} = require('../../runtime/renderer/context-monitor.js');

const ANSI_RE = /\x1b\[[0-9;]*m/g;

describe('renderer / context-monitor', () => {
  test('formatMonitorTokens formats base-1000 values with uppercase K/M', () => {
    assert.equal(formatMonitorTokens(0), '0');
    assert.equal(formatMonitorTokens(512), '512');
    assert.equal(formatMonitorTokens(196600), '196.6K');
    assert.equal(formatMonitorTokens(512000), '512K');
    assert.equal(formatMonitorTokens(999999), '1M');
    assert.equal(formatMonitorTokens(1000000), '1M');
    assert.equal(formatMonitorTokens(1048576), '1M');
  });

  test('buildSmoothBar renders 5-level UTF-8 and ASCII bars', () => {
    assert.equal(buildSmoothBar(0, 12, true), '············');
    assert.equal(buildSmoothBar(38.4, 12, true), '████▒·······');
    assert.equal(buildSmoothBar(100, 12, true), '████████████');
    assert.equal(buildSmoothBar(38.4, 12, false), '####=.......');
    assert.equal(buildSmoothBar(50, 8, false), '####....');
  });

  test('renders zh and en monitor lines with cache-inclusive deduplication and lastTurnUsage fallback', () => {
    const state = {
      compactionCount: 1,
      totalOutputTokens: 12400,
      serverCompactionLimit: 512000,
      lastTurnUsage: {
        input_tokens: 196600,
        cache_read_input_tokens: 160000,
        cache_creation_input_tokens: 0,
        output_tokens: 0,
      },
      turnHistoryMaxByMode: {
        inclusivePeak: 480000,
        exclusivePeak: 640000,
      },
      peakRawTurnTokens: 640000,
    };
    const agyData = {
      terminal_width: 120,
      model: { id: 'gemini-4-carbon-high', display_name: 'Gemini 4 Carbon', effort: 'High' },
      context_window: {
        context_window_size: 1000000,
        current_usage: {},
      },
      cost: { total_usd: 0.12 },
    };

    const zhLine = renderContextMonitorLine({
      state,
      agyData,
      unicode: true,
      lang: 'zh',
    }).replace(ANSI_RE, '');

    assert.match(zhLine, /◈ Gemini 4 Carbon \(High\)/);
    assert.match(zhLine, /上下文 .* 38\.4% \(196\.6K\/512K\)/);
    assert.match(zhLine, /压缩×1\(峰480K\)/);
    assert.match(zhLine, /缓存 81% \(160K\)/);
    assert.match(zhLine, /输出 12\.4K/);
    assert.match(zhLine, /费用 \$0\.12/);

    const enLine = renderContextMonitorLine({
      state,
      agyData,
      unicode: true,
      lang: 'en',
    }).replace(ANSI_RE, '');

    assert.match(enLine, /Ctx .* 38\.4% \(196\.6K\/512K\)/);
    assert.match(enLine, /Compacted×1\(peak 480K\)/);
    assert.match(enLine, /Cache 81% \(160K\)/);
    assert.match(enLine, /Out 12\.4K/);
    assert.match(enLine, /Cost \$0\.12/);
  });

  test('renders imminent compaction warning at >=85% and ASCII degradation', () => {
    const raw = renderContextMonitorLine({
      state: {
        compactionCount: 2,
        serverCompactionLimit: 512000,
        lastTurnUsage: {
          input_tokens: 450000,
          cache_read_input_tokens: 300000,
          output_tokens: 5000,
        },
      },
      agyData: {
        terminal_width: 120,
        model: { display_name: 'Claude Opus 4.6 (Thinking)' },
      },
      unicode: false,
      lang: 'zh',
      sessionCostUsd: 0.25,
    });

    assert.match(raw, /\x1b\[31m/);
    const plain = raw.replace(ANSI_RE, '');
    assert.match(plain, /^\* Claude Opus 4\.6 \(Thinking\) \| /);
    assert.match(plain, /压缩x2/);
    assert.match(plain, /费用 \$0\.25/);
    assert.match(plain, /即将触发压缩$/);
    assert.doesNotMatch(plain, /[◈█▓▒░·×]/);
  });

  test('respects narrow terminal_width breakpoints and empty cold-start limit', () => {
    const narrow = renderContextMonitorLine({
      state: { serverCompactionLimit: 512000 },
      agyData: { terminal_width: 75, model: { display_name: 'Gemini 4 Carbon (High)' } },
      unicode: true,
      lang: 'en',
    }).replace(ANSI_RE, '');

    assert.doesNotMatch(narrow, /◈/);
    assert.match(narrow, /Ctx ········ 0\.0% \(limit 512K\)/);
  });

  test('renderStatusLine appends context monitor line only when display.showContextMonitor is true', () => {
    const state = { currentDir: 'agy-pulse', serverCompactionLimit: 512000, compactionCount: 0 };
    const agyData = { model: { display_name: 'Gemini 4 Carbon (High)' } };

    const withoutMonitor = renderStatusLine(state, agyData, { display: { unicode: false } }).replace(ANSI_RE, '');
    assert.doesNotMatch(withoutMonitor, /Ctx /);

    const withMonitor = renderStatusLine(state, agyData, {
      language: 'en',
      display: {
        unicode: false,
        showContextMonitor: true,
        showModel: false,
        showTokenBar: false,
        showContextBar: false,
        showGitBranch: false,
        showBreadcrumbs: false,
      },
    }).replace(ANSI_RE, '');

    const lines = withMonitor.split('\n');
    assert.equal(lines.length, 2);
    assert.match(lines[1], /^\* Gemini 4 Carbon \(High\) \| Ctx /);
  });

  test('uses exclusivePeak when Claude turn reports cache-exclusive input tokens', () => {
    const clean = renderContextMonitorLine({
      state: {
        compactionCount: 1,
        serverCompactionLimit: 512000,
        lastTurnUsage: {
          input_tokens: 500,
          cache_read_input_tokens: 160000,
          cache_creation_input_tokens: 0,
          output_tokens: 1000,
        },
        turnHistoryMaxByMode: {
          inclusivePeak: 120000,
          exclusivePeak: 480000,
        },
      },
      agyData: {
        terminal_width: 120,
        model: { display_name: 'Claude Sonnet 4.6 (Thinking)' },
      },
      unicode: true,
      lang: 'en',
    }).replace(ANSI_RE, '');

    assert.match(clean, /Compacted×1\(peak 480K\)/);
  });

  test('reads top-level agyData.total_usd and falls back to sessionCostUsd when native cost is 0', () => {
    const topLevelCost = renderContextMonitorLine({
      state: { serverCompactionLimit: 512000 },
      agyData: { terminal_width: 120, total_usd: 0.42 },
      unicode: false,
      lang: 'en',
    }).replace(ANSI_RE, '');
    assert.match(topLevelCost, /Cost \$0\.42/);

    const zeroFallbackCost = renderContextMonitorLine({
      state: { serverCompactionLimit: 512000 },
      agyData: { terminal_width: 120, cost: { total_usd: 0 } },
      unicode: false,
      lang: 'en',
      sessionCostUsd: 0.35,
    }).replace(ANSI_RE, '');
    assert.match(zeroFallbackCost, /Cost \$0\.35/);
  });

  test('supports custom config.thresholds while keeping 65/85 defaults', () => {
    const rawCustom = renderContextMonitorLine({
      state: {
        serverCompactionLimit: 100000,
        lastTurnUsage: { input_tokens: 76000, output_tokens: 0, cache_read_input_tokens: 0 },
      },
      agyData: { terminal_width: 120, model: 'Gemini 4 Carbon (High)' },
      config: { thresholds: { warning: 0.5, critical: 0.75 } },
      unicode: false,
      lang: 'en',
    });
    assert.match(rawCustom, /\x1b\[31m/);
    assert.match(rawCustom.replace(ANSI_RE, ''), /Compaction imminent$/);

    const rawDefault = renderContextMonitorLine({
      state: {
        serverCompactionLimit: 100000,
        lastTurnUsage: { input_tokens: 76000, output_tokens: 0, cache_read_input_tokens: 0 },
      },
      agyData: { terminal_width: 120, model: 'Gemini 4 Carbon (High)' },
      config: { thresholds: { warning: 0.7, critical: 0.9 } },
      unicode: false,
      lang: 'en',
    });
    assert.match(rawDefault, /\x1b\[33m/);
    assert.doesNotMatch(rawDefault.replace(ANSI_RE, ''), /Compaction imminent/);
  });

  test('renderStatusLine shares exact single totalCostUsd between line 1 showCost and bottom context monitor even when native cost is 0 or routed', () => {
    const state = {
      currentDir: 'agy-pulse',
      serverCompactionLimit: 512000,
      compactionCount: 0,
      costBreakdown: {
        totalCostUsd: 21.64,
        mainCostUsd: 9.89,
        childrenCostUsd: 11.75,
        childSessionCount: 15,
        byModel: {
          Carbon: { costUsd: 9.89, inputTokens: 2967648, cacheReadTokens: 25052429, cacheWriteTokens: 0, outputTokens: 145309, steps: 117 },
          Hydro: { costUsd: 11.75, inputTokens: 2810844, cacheReadTokens: 35885061, cacheWriteTokens: 0, outputTokens: 644705, steps: 364 },
        },
      },
    };
    const agyData = {
      terminal_width: 120,
      model: { id: 'us.anthropic.claude-sonnet-4-20250514-v1:0', display_name: 'Gemini 4 Carbon (High)' },
      cost: { total_usd: 0.50 },
    };

    const output = renderStatusLine(state, agyData, {
      language: 'zh',
      display: {
        unicode: true,
        showCost: true,
        showContextMonitor: true,
        showGitBranch: false,
        showBreadcrumbs: false,
      },
    }).replace(ANSI_RE, '');

    const lines = output.split('\n');
    const line1 = lines[0];
    const monitorLine = lines[lines.length - 1];
    assert.match(line1, /费用 \$21\.64/);
    assert.match(monitorLine, /费用 \$21\.64/);
    assert.doesNotMatch(output, /Carbon:\s*\$|Hydro:\s*\$/);
  });

  test('degrades losslessly within 80/100/120 terminal_width accounting for CJK double-width columns', () => {
    const state = {
      compactionCount: 1,
      totalOutputTokens: 12400,
      serverCompactionLimit: 512000,
      lastTurnUsage: {
        input_tokens: 196600,
        cache_read_input_tokens: 160000,
        cache_creation_input_tokens: 0,
        output_tokens: 0,
      },
      turnHistoryMaxByMode: { inclusivePeak: 480000 },
    };

    for (const width of [80, 100, 120]) {
      const rendered = renderContextMonitorLine({
        state,
        agyData: {
          terminal_width: width,
          model: { display_name: 'Gemini 4 Carbon (High)' },
          cost: { total_usd: 0.12 },
        },
        unicode: true,
        lang: 'zh',
      });
      assert.ok(
        getAnsiDisplayWidth(rendered) <= width,
        `context monitor zh width=${width} got ${getAnsiDisplayWidth(rendered)}: ${rendered.replace(ANSI_RE, '')}`
      );
      assert.match(rendered.replace(ANSI_RE, ''), /38\.4% \(196\.6K\/512K\)/);
    }
  });

  test('falls back to state.lastTurnUsage.model in monitor and status line when agyData.model is omitted', () => {
    const state = {
      branch: 'main',
      serverCompactionLimit: 512000,
      lastTurnUsage: {
        input_tokens: 120000,
        cache_read_input_tokens: 80000,
        output_tokens: 1000,
        model: 'gemini-4-carbon-high',
      },
    };
    const agyData = { terminal_width: 120, context_window: {} };

    const monitorClean = renderContextMonitorLine({
      state,
      agyData,
      unicode: true,
      lang: 'zh',
    }).replace(ANSI_RE, '');
    assert.match(monitorClean, /◈ Gemini 4 Carbon \(High\)/);
    assert.doesNotMatch(monitorClean, /Unknown Model/);

    const fullClean = renderStatusLine(state, agyData, {
      language: 'zh',
      display: { unicode: true, showContextMonitor: true, quotaStyle: 'none' },
    }).replace(ANSI_RE, '');
    const lines = fullClean.split('\n');
    assert.match(lines[0], /Gemini 4 Carbon\(H\)/);
    assert.match(lines[lines.length - 1], /◈ Gemini 4 Carbon \(High\)/);
    assert.doesNotMatch(fullClean, /Unknown Model/);
  });

  test('renderModelCacheLine renders multi-model cache breakdown in zh/en, sorts main first, caps at 4, and supports cacheWriteTokens', () => {
    const state = {
      costBreakdown: {
        byModel: {
          Hydro: { inputTokens: 6_100_000, cacheReadTokens: 88_600_000, cacheWriteTokens: 0 },
          Sonnet: { inputTokens: 1_300_000, cacheReadTokens: 12_400_000, cacheWriteTokens: 1_100_000 },
          Carbon: { inputTokens: 6_400_000, cacheReadTokens: 38_800_000, cacheWriteTokens: 0 },
          Flash: { inputTokens: 2_000_000, cacheReadTokens: 1_000_000, cacheWriteTokens: 0 },
          Opus: { inputTokens: 900_000, cacheReadTokens: 100_000, cacheWriteTokens: 0 },
        },
      },
    };
    const agyData = {
      model: { display_name: 'Gemini 4 Carbon (High)' },
    };

    const zhRaw = renderModelCacheLine({ state, agyData, unicode: true, lang: 'zh' });
    // Carbon 86% >= 80 -> green+bold; Flash 33% < 50 -> dim
    assert.match(zhRaw, /\x1b\[32m\x1b\[1m86%/);
    const zhPlain = zhRaw.replace(ANSI_RE, '');
    assert.equal(
      zhPlain,
      '⟳ 累计缓存 · Carbon 86% (命中 38.8M / 总入 45.2M) · Hydro 94% (命中 88.6M / 总入 94.7M) · Sonnet 84% (命中 12.4M · 写入 1.1M / 总入 14.8M) · Flash 33% (命中 1M / 总入 3M)'
    );
    assert.doesNotMatch(zhPlain, /Opus/);

    const enPlain = renderModelCacheLine({ state, agyData, unicode: false, lang: 'en' }).replace(ANSI_RE, '');
    assert.match(enPlain, /^~ Session Cache \| Carbon 86% \(hit 38\.8M \/ in 45\.2M\)/);
    assert.match(enPlain, /Sonnet 84% \(hit 12\.4M, write 1\.1M \/ in 14\.8M\)/);
    assert.doesNotMatch(enPlain, /[⟳·]/);
  });

  test('renderModelCacheLine falls back to single-turn usage and returns empty string on 0-token cold start', () => {
    const emptyLine = renderModelCacheLine({
      state: { serverCompactionLimit: 512000 },
      agyData: { model: { display_name: 'Gemini 4 Carbon (High)' } },
      unicode: true,
      lang: 'zh',
    });
    assert.equal(emptyLine, '');

    const fallbackLine = renderModelCacheLine({
      state: {
        lastTurnUsage: {
          input_tokens: 200_000,
          cache_read_input_tokens: 150_000,
          cache_creation_input_tokens: 0,
        },
      },
      agyData: { model: { display_name: 'Gemini 4 Carbon (High)' } },
      unicode: true,
      lang: 'zh',
    }).replace(ANSI_RE, '');
    assert.equal(fallbackLine, '⟳ 累计缓存 · Carbon 75% (命中 150K / 总入 200K)');
  });

  test('renderModelCacheLine degrades progressively within 80/100/120 and narrow widths without exceeding cols', () => {
    const state = {
      costBreakdown: {
        byModel: {
          Carbon: { inputTokens: 6_400_000, cacheReadTokens: 38_800_000, cacheWriteTokens: 0 },
          Hydro: { inputTokens: 6_100_000, cacheReadTokens: 88_600_000, cacheWriteTokens: 0 },
          Sonnet: { inputTokens: 1_300_000, cacheReadTokens: 12_400_000, cacheWriteTokens: 1_100_000 },
          Flash: { inputTokens: 2_000_000, cacheReadTokens: 6_000_000, cacheWriteTokens: 0 },
        },
      },
    };
    for (const width of [35, 60, 80, 100, 120]) {
      const rendered = renderModelCacheLine({
        state,
        agyData: { terminal_width: width, model: { display_name: 'Gemini 4 Carbon (High)' } },
        unicode: true,
        lang: 'zh',
      });
      assert.ok(
        getAnsiDisplayWidth(rendered) <= width,
        `modelCache width=${width} got ${getAnsiDisplayWidth(rendered)}: ${rendered.replace(ANSI_RE, '')}`
      );
    }

    const w80 = renderModelCacheLine({
      state: {
        costBreakdown: {
          byModel: {
            Carbon: { inputTokens: 6_400_000, cacheReadTokens: 38_800_000, cacheWriteTokens: 0 },
            Hydro: { inputTokens: 6_100_000, cacheReadTokens: 88_600_000, cacheWriteTokens: 0 },
          },
        },
      },
      agyData: { terminal_width: 80, model: { display_name: 'Gemini 4 Carbon (High)' } },
      unicode: true,
      lang: 'zh',
    }).replace(ANSI_RE, '');
    assert.equal(w80, '⟳ 累计缓存 · Carbon 86% (38.8M/45.2M) · Hydro 94% (88.6M/94.7M)');
  });

  test('renderStatusLine places modelCacheLine directly above monitorLine and honors showModelCache toggle', () => {
    const state = {
      currentDir: 'agy-pulse',
      serverCompactionLimit: 512000,
      costBreakdown: {
        totalCostUsd: 1.5,
        byModel: {
          Carbon: { inputTokens: 100_000, cacheReadTokens: 400_000, cacheWriteTokens: 0 },
          Hydro: { inputTokens: 50_000, cacheReadTokens: 450_000, cacheWriteTokens: 0 },
        },
      },
    };
    const agyData = { terminal_width: 120, model: { display_name: 'Gemini 4 Carbon (High)' } };

    const withBoth = renderStatusLine(state, agyData, {
      language: 'zh',
      display: { unicode: true, showContextMonitor: true, quotaStyle: 'none' },
    }).replace(ANSI_RE, '').split('\n');
    assert.match(withBoth[withBoth.length - 2], /^⟳ 累计缓存 · Carbon 80%/);
    assert.match(withBoth[withBoth.length - 1], /^◈ Gemini 4 Carbon \(High\)/);

    const disabledCache = renderStatusLine(state, agyData, {
      language: 'zh',
      display: { unicode: true, showContextMonitor: true, showModelCache: false, quotaStyle: 'none' },
    }).replace(ANSI_RE, '');
    assert.doesNotMatch(disabledCache, /累计缓存/);
    assert.match(disabledCache, /◈ Gemini 4 Carbon \(High\)/);

    const cacheOnly = renderStatusLine(state, agyData, {
      language: 'en',
      display: { unicode: true, showContextMonitor: false, showModelCache: true, quotaStyle: 'none' },
    }).replace(ANSI_RE, '');
    assert.match(cacheOnly, /⟳ Session Cache · Carbon 80%/);
    assert.doesNotMatch(cacheOnly, /◈ Gemini 4 Carbon/);
  });

  test('treats lastTurnUsage with cacheExclusive=true as exclusive even when input_tokens >= cache_read_input_tokens', () => {
    const state = {
      serverCompactionLimit: 512000,
      lastTurnUsage: {
        input_tokens: 11375,
        cache_read_input_tokens: 10422,
        cache_creation_input_tokens: 0,
        output_tokens: 0,
        cacheExclusive: true,
      },
    };
    const agyData = {
      terminal_width: 120,
      model: { display_name: 'Gemini 4 Carbon (High)' },
    };

    const monitorLine = renderContextMonitorLine({
      state,
      agyData,
      unicode: true,
      lang: 'zh',
    }).replace(ANSI_RE, '');
    // promptCtx = 11375 + 10422 = 21797 (21.8K), cachePct = 10422 / 21797 = 47.81% -> 48% (not 92%)
    assert.match(monitorLine, /21\.8K\/512K/);
    assert.match(monitorLine, /缓存 48% \(10\.4K\)/);
    assert.doesNotMatch(monitorLine, /缓存 92%/);

    const fallbackCacheLine = renderModelCacheLine({
      state,
      agyData,
      unicode: true,
      lang: 'zh',
    }).replace(ANSI_RE, '');
    assert.equal(fallbackCacheLine, '⟳ 累计缓存 · Carbon 48% (命中 10.4K / 总入 21.8K)');
  });
});
