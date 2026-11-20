import { describe, test } from 'node:test';
import assert from 'node:assert';
import { abbreviateDisplayName, compactModelName, formatQuotaPercent } from '../../runtime/renderer.js';
import {
  modelIncludesCacheInInput,
  getDisplayWidth,
  stripAnsi,
  getAnsiDisplayWidth,
  truncateAnsiToDisplayWidth,
  truncateToDisplayWidth,
  sanitizeTerminalText,
} from '../../runtime/renderer/format.js';

describe('renderer / format helpers', () => {
  describe('abbreviateDisplayName', () => {
    test('handles all known agent model patterns including 2026 models', () => {
      assert.equal(abbreviateDisplayName('Gemini 3.5 Flash (High)'), 'Gemini 3.5 Flash(H)');
      assert.equal(abbreviateDisplayName('Gemini 3.5 Flash (Medium)'), 'Gemini 3.5 Flash(M)');
      assert.equal(abbreviateDisplayName('Gemini 3.5 Flash (Low)'), 'Gemini 3.5 Flash(L)');
      assert.equal(abbreviateDisplayName('Gemini 3.8 Flash (High)'), 'Gemini 3.8 Flash(H)');
      assert.equal(abbreviateDisplayName('Gemini 3.1 Pro (High)'), 'Gemini 3.1 Pro(H)');
      assert.equal(abbreviateDisplayName('Gemini 3.1 Pro (Low)'), 'Gemini 3.1 Pro(L)');
      assert.equal(abbreviateDisplayName('Gemini 4 Argon (Medium)'), 'Gemini 4 Argon(M)');
      assert.equal(abbreviateDisplayName('Gemini 4 Argon (High)'), 'Gemini 4 Argon(H)');
      assert.equal(abbreviateDisplayName('Gemini 4 Argon (Low)'), 'Gemini 4 Argon(L)');
      assert.equal(abbreviateDisplayName('Gemini 4 Carbon (High)'), 'Gemini 4 Carbon(H)');
      assert.equal(abbreviateDisplayName('Gemini 4 Hydrogen (XHigh)'), 'Gemini 4 Hydrogen(XH)');
      assert.equal(abbreviateDisplayName('Gemini 4 Hydrogen (Sonar)'), 'Gemini 4 Hydrogen(S)');
      assert.equal(abbreviateDisplayName('Gemini 4 Neon'), 'Gemini 4 Neon');
      assert.equal(abbreviateDisplayName('Gemini 4 Neon (XHigh)'), 'Gemini 4 Neon(XH)');
      assert.equal(abbreviateDisplayName('Gemini 4 Neon (High)'), 'Gemini 4 Neon(H)');
      assert.equal(abbreviateDisplayName('Gemini 4 Neon (Medium)'), 'Gemini 4 Neon(M)');
      assert.equal(abbreviateDisplayName('Gemini 4 Neon (Low)'), 'Gemini 4 Neon(L)');
      assert.equal(abbreviateDisplayName('Orchestrator (Carbon)'), 'Orch (Carbon)');
      assert.equal(abbreviateDisplayName('Orchestrator (Argon)'), 'Orch (Argon)');
      assert.equal(abbreviateDisplayName('Orchestrator (Neon)'), 'Orch (Neon)');
      assert.equal(abbreviateDisplayName('Claude Sonnet 4.6 (Thinking)'), 'Sonnet 4.6(Th)');
      assert.equal(abbreviateDisplayName('Claude Opus 4.6 (Thinking)'), 'Opus 4.6(Th)');
      assert.equal(abbreviateDisplayName('GPT-OSS 120B (Medium)'), 'GPT-OSS 120B');
    });

    test('passes through unknown names unchanged', () => {
      assert.equal(abbreviateDisplayName('Some Future Model 9.0'), 'Some Future Model 9.0');
      assert.equal(abbreviateDisplayName(''), '');
    });

    test('handles hypothetical future models', () => {
      assert.equal(abbreviateDisplayName('Gemini 4.0 Flash (High)'), 'Gemini 4.0 Flash(H)');
      assert.equal(abbreviateDisplayName('Claude Haiku 5.0 (Thinking)'), 'Haiku 5.0(Th)');
      assert.equal(abbreviateDisplayName('GPT-OSS 200B (Low)'), 'GPT-OSS 200B');
    });
  });

  describe('compactModelName', () => {
    test('produces ultra-short collision-free names for provider summary', () => {
      assert.equal(compactModelName('Gemini 3.5 Flash (High)'), 'Flash');
      assert.equal(compactModelName('Gemini 3.5 Flash (Medium)'), 'Flash');
      assert.equal(compactModelName('Gemini 3.1 Pro (Low)'), 'Pro');
      assert.equal(compactModelName('Gemini 4 Argon (Medium)'), 'Argon');
      assert.equal(compactModelName('Gemini 4 Carbon (High)'), 'Carbon');
      assert.equal(compactModelName('Gemini 4 Hydrogen (XHigh)'), 'Hydro');
      assert.equal(compactModelName('Gemini 4 Hydrogen (Sonar)'), 'Hydro');
      assert.equal(compactModelName('Gemini 4 Neon'), 'Neon');
      assert.equal(compactModelName('Gemini 4 Neon (XHigh)'), 'Neon');
      assert.equal(compactModelName('Gemini 4 Neon (High)'), 'Neon');
      assert.equal(compactModelName('Orchestrator (Carbon)'), 'Orch-C');
      assert.equal(compactModelName('Orchestrator (Argon)'), 'Orch-A');
      assert.equal(compactModelName('Orchestrator (Neon)'), 'Orch-N');
      assert.equal(compactModelName('Claude Sonnet 4.6 (Thinking)'), 'Sonnet');
      assert.equal(compactModelName('Claude Opus 4.6 (Thinking)'), 'Opus');
      assert.equal(compactModelName('GPT-OSS 120B (Medium)'), 'GPT');
      assert.equal(compactModelName('gemini-3.8-flash-tiered'), 'Flash');
      assert.equal(compactModelName('gemini-3.1-pro-low'), 'Pro');
      assert.equal(compactModelName('gemini-4-argon'), 'Argon');
      assert.equal(compactModelName('gemini-4-argon-high'), 'Argon');
      assert.equal(compactModelName('gemini-4-carbon-high'), 'Carbon');
      assert.equal(compactModelName('gemini-4-hydrogen-xhigh'), 'Hydro');
      assert.equal(compactModelName('gemini-4-hydrogen-sonar'), 'Hydro');
      assert.equal(compactModelName('gemini-4-neon'), 'Neon');
      assert.equal(compactModelName('gemini-4-neon-high'), 'Neon');
      assert.equal(compactModelName('gemini-4-neon-xhigh'), 'Neon');
      assert.equal(compactModelName('gemini-pro-orchestrator'), 'Orch-C');
      assert.equal(compactModelName('gemini-pro-orchestrator-argon'), 'Orch-A');
      assert.equal(compactModelName('gemini-pro-orchestrator-neon'), 'Orch-N');
      assert.equal(compactModelName('neon'), 'Neon');
      assert.equal(compactModelName('Orch-N'), 'Orch-N');
      assert.equal(compactModelName('claude-sonnet-4-6'), 'Sonnet');
      assert.equal(compactModelName('claude-opus-4-6-thinking'), 'Opus');
    });

    test('handles null, undefined, empty, and non-string inputs gracefully', () => {
      assert.equal(compactModelName(null), '');
      assert.equal(compactModelName(undefined), '');
      assert.equal(compactModelName(''), '');
      assert.equal(compactModelName(123), '');
    });
  });

  describe('modelIncludesCacheInInput', () => {
    test('identifies 2026 cache-inclusive models including Orchestrator, Carbon, Hydrogen, and Neon', () => {
      assert.equal(modelIncludesCacheInInput('Orchestrator (Carbon)'), true);
      assert.equal(modelIncludesCacheInInput('Orchestrator (Argon)'), true);
      assert.equal(modelIncludesCacheInInput('Orchestrator (Neon)'), true);
      assert.equal(modelIncludesCacheInInput('Gemini 4 Carbon (High)'), true);
      assert.equal(modelIncludesCacheInInput('Gemini 4 Hydrogen (XHigh)'), true);
      assert.equal(modelIncludesCacheInInput('Gemini 4 Neon (High)'), true);
      assert.equal(modelIncludesCacheInInput('Neon'), true);
      assert.equal(modelIncludesCacheInInput('Orch-N'), true);
      assert.equal(modelIncludesCacheInInput('Claude Sonnet 4.6 (Thinking)'), true);
      assert.equal(modelIncludesCacheInInput('CustomExclusiveModel'), false);
    });
  });

  describe('formatQuotaPercent', () => {
    test('formats percentage accurately without rounding 0.995 up to 100%', () => {
      assert.equal(formatQuotaPercent(1.0), 100);
      assert.equal(formatQuotaPercent(0.9953883), 99);
      assert.equal(formatQuotaPercent(0.9999), 99);
      assert.equal(formatQuotaPercent(0.5), 50);
      assert.equal(formatQuotaPercent(0.0), 0);
      assert.equal(formatQuotaPercent(null), 0);
    });
  });

  describe('2026 extended tiers, Claude version-first names, emoji width, and surrogate safety', () => {
    test('abbreviates Extra-Low tier and version-first Claude names', () => {
      assert.equal(abbreviateDisplayName('Gemini 3.5 Flash (Extra-Low)'), 'Gemini 3.5 Flash(XL)');
      assert.equal(abbreviateDisplayName('Claude 4.6 Sonnet (Thinking)'), 'Sonnet 4.6(Th)');
      assert.equal(abbreviateDisplayName('Claude 4.6 Sonnet'), 'Sonnet 4.6');
      assert.equal(compactModelName('Claude 4.6 Sonnet (Thinking)'), 'Sonnet');
      assert.equal(compactModelName('Claude 4.6 Sonnet'), 'Sonnet');
    });

    test('treats variation selectors, ZWJ, and skin tone modifiers as zero-width for emoji display width', () => {
      assert.equal(getDisplayWidth('🖼️'), 2);
      assert.equal(getDisplayWidth('⚠️'), 2);
      assert.equal(getDisplayWidth('👍🏽'), 2);
      assert.equal(getDisplayWidth('a\u200db'), 2);
      assert.equal(truncateToDisplayWidth('🖼️abc', 2, ''), '🖼️');
      assert.equal(truncateToDisplayWidth('👍🏽abc', 2, ''), '👍🏽');
    });

    test('truncates by code point without splitting UTF-16 surrogate pairs', () => {
      const text = 'abcde🚀xyz';
      const sanitized = sanitizeTerminalText(text, 6);
      assert.equal(sanitized, 'abcde🚀');
      assert.equal(compactModelName('🚀🔥✨🎉🌟💡⚡'), '🚀🔥✨🎉🌟💡');
    });
  });

  describe('ANSI-safe width utilities and generalized compactModelName rules', () => {
    test('stripAnsi removes CSI and OSC sequences while keeping visible text', () => {
      assert.equal(stripAnsi('\x1b[32mHello\x1b[0m \x1b]8;;https://example.com\x07link\x1b]8;;\x1b\\'), 'Hello link');
      assert.equal(stripAnsi(null), '');
    });

    test('getAnsiDisplayWidth counts CJK as 2, ASCII as 1, and ANSI codes as 0', () => {
      const colored = '\x1b[36m中文AB\x1b[0m';
      assert.equal(getAnsiDisplayWidth(colored), 6);
      assert.equal(getAnsiDisplayWidth('\x1b[31m输出: 80.0 tok/s\x1b[0m'), 16);
    });

    test('truncateAnsiToDisplayWidth preserves ANSI, respects CJK width, appends reset without padding', () => {
      const within = '\x1b[32m中文\x1b[0m';
      assert.equal(truncateAnsiToDisplayWidth(within, 10), within);

      const coloredCjk = '\x1b[35m中文测试文本\x1b[0m';
      const clipped5 = truncateAnsiToDisplayWidth(coloredCjk, 5, '…');
      assert.equal(clipped5, '\x1b[35m中文…\x1b[0m');
      assert.equal(getAnsiDisplayWidth(clipped5), 5);

      const clipped4 = truncateAnsiToDisplayWidth(coloredCjk, 4, '…');
      assert.equal(clipped4, '\x1b[35m中…\x1b[0m');
      assert.equal(getAnsiDisplayWidth(clipped4), 3);

      assert.equal(truncateAnsiToDisplayWidth('abc', 0), '');
    });

    test('compactModelName matches underscore, hyphen, MODEL_* enums, and versionless family names', () => {
      assert.equal(compactModelName('MODEL_GEMINI_4_CARBON'), 'Carbon');
      assert.equal(compactModelName('MODEL_GEMINI_4_NEON'), 'Neon');
      assert.equal(compactModelName('MODEL_CLAUDE_SONNET_4_6'), 'Sonnet');
      assert.equal(compactModelName('MODEL_ORCHESTRATOR_ARGON'), 'Orch-A');
      assert.equal(compactModelName('MODEL_ORCHESTRATOR_NEON'), 'Orch-N');
      assert.equal(compactModelName('MODEL_ORCHESTRATOR'), 'Orch-C');
      assert.equal(compactModelName('gemini_3_5_flash'), 'Flash');
      assert.equal(compactModelName('gemini_3_1_pro_low'), 'Pro');
      assert.equal(compactModelName('deepseek-v3'), 'DeepSeek');
      assert.equal(compactModelName('claude_haiku'), 'Haiku');
      assert.equal(compactModelName('ProductionModel'), 'Produc');
    });
  });
});
