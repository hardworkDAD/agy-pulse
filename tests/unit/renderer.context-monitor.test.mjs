import { describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { renderHUD } from '../../runtime/renderer.js';

const require = createRequire(import.meta.url);
const {
  formatMonitorTokens,
  buildSmoothBar,
  renderContextMonitorLine,
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

  test('renderHUD appends context monitor line only when display.showContextMonitor is true', () => {
    const state = { currentDir: 'agy-pulse', serverCompactionLimit: 512000, compactionCount: 0 };
    const agyData = { model: { display_name: 'Gemini 4 Carbon (High)' } };

    const withoutMonitor = renderHUD(state, agyData, { display: { unicode: false } }).replace(ANSI_RE, '');
    assert.doesNotMatch(withoutMonitor, /Ctx /);

    const withMonitor = renderHUD(state, agyData, {
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
});
