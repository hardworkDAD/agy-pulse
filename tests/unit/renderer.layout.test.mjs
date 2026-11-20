import { describe, test } from 'node:test';
import assert from 'node:assert';
import { renderStatusLine } from '../../runtime/renderer.js';
import { getAnsiDisplayWidth } from '../../runtime/renderer/format.js';

describe('renderer / line composition', () => {
  test('should contain branch', () => {
    const state = {
      steps: 42,
      branch: 'main'
    };
    const agyData = {
      context_window: {
        total_input_tokens: 15000,
        total_output_tokens: 5000,
        used_percentage: 12.5,
        context_window_size: 1000000
      },
      plan_tier: 'Google AI Pro',
      task_count: 3,
      model: {
        display_name: 'Gemini 3.5 Flash (High)'
      }
    };

    const output = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: true } });
    assert.doesNotMatch(output, /^\n/);
    // Layer 1: branch, model, plan (no AGY-PULSE brand, no labels)
    assert.match(output, /⎇ main/);
    assert.match(output, /Gemini 3\.5 Flash\(H\)/);
    assert.match(output, /Google AI Pro/);
    // Layer 2: compact tokens, context bar with %
    assert.match(output, /Tokens 20k .*?\(.*?in: 15k, out: 5k.*?\)/);
    assert.match(output, /13%/);
    assert.match(output, /│/); // Unicode divider
  });

  test('preserves model name suffixes when applying display aliases', () => {
    const output = renderStatusLine(
      { steps: 1, branch: 'main' },
      {
        context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
        model: { display_name: 'Gemini 3.5 Flash (High) Preview' }
      },
      { display: { useNerdFonts: false, unicode: false } }
    );

    assert.match(output, /Gemini 3\.5 Flash\(H\) Preview/);

    const outputLow = renderStatusLine(
      { steps: 1, branch: 'main' },
      {
        context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 },
        model: { display_name: 'Gemini 3.5 Flash (Low)' }
      },
      { display: { useNerdFonts: false, unicode: false } }
    );

    assert.match(outputLow, /Gemini 3\.5 Flash\(L\)/);
  });

  test('should correctly render update notices', () => {
    const state = { steps: 1, branch: 'main' };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };
    const updateInfo = {
      updateAvailable: true,
      latestVersion: '1.2.3'
    };

    const outputUnicode = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: true } }, [], 'tier', updateInfo);
    assert.match(outputUnicode, /⟳ v1\.2\.3/);

    const outputAscii = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: false } }, [], 'tier', updateInfo);
    assert.match(outputAscii, /\[UP\] v1\.2\.3/);
  });
});

describe('renderer / metadata line', () => {
  test('renders Memory files, rules, MCPs, and hooks count', () => {
    const state = {
      steps: 1,
      branch: 'main',
      memoryFile: 'MEMORY.md',
      rulesCount: 4,
      mcpCount: 1,
      hooksCount: 5
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };
    const output = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: true } });
    assert.match(output, /1 MEMORY.md/);
    assert.match(output, /4 rules/);
    assert.match(output, /1 MCPs/);
    assert.match(output, /5 hooks/);
  });

  test('hides zero-count metadata items', () => {
    const state = {
      steps: 1,
      branch: 'main',
      memoryFile: 'GEMINI.md',
      rulesCount: 0,
      mcpCount: 0,
      hooksCount: 2
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };
    const output = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: true } });
    assert.match(output, /1 GEMINI.md/);
    assert.match(output, /2 hooks/);
    assert.doesNotMatch(output, /0 rules/);
    assert.doesNotMatch(output, /0 MCPs/);
  });

  test('omits metadata line when all counts are zero', () => {
    const state = {
      steps: 0,
      branch: 'main',
      rulesCount: 0,
      mcpCount: 0,
      hooksCount: 0
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };
    const output = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: true } });
    const lines = output.trim().split('\n');
    // Only 2 lines (identity + resources), no metadata line
    assert.equal(lines.length, 2);
  });

  test('limits breadcrumb metadata by breadcrumbCount', () => {
    const state = {
      steps: 1,
      branch: 'main',
      breadcrumbs: ['README.md', 'runtime/renderer.js', 'tests/unit/renderer.test.mjs'],
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    const output = renderStatusLine(state, agyData, {
      display: {
        breadcrumbCount: 2,
        unicode: false
      }
    });

    assert.doesNotMatch(output, /README\.md/);
    assert.match(output, /runtime\/renderer\.js/);
    assert.match(output, /tests\/unit\/renderer\.test\.mjs/);
  });
});

describe('renderer / theme & icons', () => {
  test('falls back to ASCII when config.display.unicode is false', () => {
    const state = { steps: 1, branch: 'main' };
    const agyData = {
      context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 5, context_window_size: 1000000 },
      plan_tier: 'Free',
      task_count: 0,
      model: { display_name: 'GPT-OSS 120B (Medium)' }
    };

    const output = renderStatusLine(state, agyData, { display: { useNerdFonts: false, unicode: false } });

    // Box-drawing characters should NOT appear
    assert.doesNotMatch(output, /│/);
    assert.doesNotMatch(output, /█/);
    assert.doesNotMatch(output, /░/);
    // Ascii substitutes should appear
    assert.match(output, /\|/);
    assert.match(output, /#/);
    // Plain-text icons
    assert.match(output, /\[B\]/);
    assert.match(output, /Tokens/);
    // ASCII labels
    assert.match(output, /in: 1k/);
    assert.match(output, /out: 200/);
  });

  test('renders Nerd Font icons when enabled', () => {
    const state = { steps: 0, branch: 'main' };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };
    const output = renderStatusLine(state, agyData, { display: { useNerdFonts: true } });
    assert.doesNotMatch(output, /AGY-PULSE/);
    assert.match(output, /main/);
    assert.match(output, /0%/);
    assert.doesNotMatch(output, /\[B\]/);
    assert.doesNotMatch(output, /\[Tk\]/);
    assert.doesNotMatch(output, /⎇/);
    assert.doesNotMatch(output, /⚿/);
  });

  test('supports theme custom colors', () => {
    const state = { steps: 5, branch: 'dev' };
    const agyData = {
      context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 95 }
    };
    const config = {
      display: {
        unicode: true
      },
      theme: {
        critical: 'blue'
      }
    };
    const output = renderStatusLine(state, agyData, config);
    assert.match(output, /\x1b\[34m\[█+/);
  });

  test('supports custom warning and critical thresholds', () => {
    const state = { steps: 5, branch: 'dev' };
    const agyData = {
      context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 30 }
    };
    const config = {
      display: {
        unicode: true
      },
      thresholds: {
        warning: 0.2,
        critical: 0.4
      }
    };
    const output = renderStatusLine(state, agyData, config);
    assert.match(output, /\x1b\[33m\[█+/);
  });
});

describe('renderer / display visibility toggles', () => {
  test('respects display visibility toggles', () => {
    const state = {
      steps: 5,
      branch: 'main',
      memoryFile: 'GEMINI.md',
      rulesCount: 2,
      hooksCount: 1
    };
    const agyData = {
      context_window: { total_input_tokens: 1000, total_output_tokens: 200, used_percentage: 5 },
      model: { display_name: 'Gemini 3.5 Flash (High)' }
    };

    const output = renderStatusLine(state, agyData, {
      display: {
        showGitBranch: false,
        showTokenBar: false,
        showBreadcrumbs: false,
        unicode: false
      }
    });

    assert.doesNotMatch(output, /\[B\] main/);
    assert.doesNotMatch(output, /\[Tk\]/);
    assert.doesNotMatch(output, /GEMINI\.md/);
    assert.match(output, /Gemini 3\.5 Flash\(H\)/);
    assert.match(output, /\[C\] 1k\/0/);
    assert.match(output, /2 rules/);
    assert.match(output, /1 hooks/);
  });
});

describe('renderer / current directory', () => {
  test('renders current directory', () => {
    const state = {
      steps: 1,
      branch: 'main',
      currentDir: 'my-project-dir',
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    // Test with showCurrentDir = true
    const output1 = renderStatusLine(state, agyData, {
      display: {
        showCurrentDir: true,
        showGitBranch: true,
        unicode: false
      }
    });
    assert.match(output1, /my-project-dir/);
    assert.match(output1, /my-project-dir.*[|│].*\[B\] main/);

    // Test with showCurrentDir = false
    const output2 = renderStatusLine(state, agyData, {
      display: {
        showCurrentDir: false,
        showGitBranch: true,
        unicode: false
      }
    });
    assert.doesNotMatch(output2, /my-project-dir/);
  });

  test('sanitizes current directory before terminal output', () => {
    const state = {
      steps: 1,
      branch: 'main',
      currentDir: 'repo\x1b]52;c;SGVsbG8=\x07\x1b[5mblink',
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    const output = renderStatusLine(state, agyData, {
      display: {
        showCurrentDir: true,
        unicode: false
      }
    });

    assert.match(output, /repo/);
    assert.doesNotMatch(output, /\x1b\]52/);
    assert.doesNotMatch(output, /\x07/);
    assert.doesNotMatch(output, /\x1b\[5m/);
  });
});

describe('renderer / username', () => {
  test('displays username when showUsername is true', () => {
    const state = {
      steps: 1,
      branch: 'main',
      username: 'shetterelland@gmail.com',
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    // Test with showUsername = true
    const output1 = renderStatusLine(state, agyData, {
      display: {
        showUsername: true,
        unicode: false
      }
    });
    assert.match(output1, /shetterelland@gmail\.com/);

    // Test with showUsername = false (default behavior)
    const output2 = renderStatusLine(state, agyData, {
      display: {
        showUsername: false,
        unicode: false
      }
    });
    assert.doesNotMatch(output2, /shetterelland@gmail\.com/);
  });

  test('respects custom config username over state username', () => {
    const state = {
      steps: 1,
      branch: 'main',
      username: 'shetterelland@gmail.com',
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    const output = renderStatusLine(state, agyData, {
      display: {
        showUsername: true,
        username: 'custom-user',
        unicode: false
      }
    });
    assert.match(output, /custom-user/);
    assert.doesNotMatch(output, /shetterelland@gmail\.com/);
  });

  test('sanitizes username before terminal output', () => {
    const state = {
      steps: 1,
      branch: 'main',
      username: 'user\x1b]52;c;SGVsbG8=\x07\x1b[5mblink',
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    const output = renderStatusLine(state, agyData, {
      display: {
        showUsername: true,
        unicode: false
      }
    });

    assert.match(output, /user/);
    assert.doesNotMatch(output, /\x1b\]52/);
    assert.doesNotMatch(output, /\x07/);
    assert.doesNotMatch(output, /\x1b\[5m/);
  });

  test('sanitizes username with terminated OSC sequence correctly without losing subsequent text', () => {
    const state = {
      steps: 1,
      branch: 'main',
      username: 'user\x1b]52;c;SGVsbG8=\x1b\\safe-suffix',
    };
    const agyData = {
      context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 }
    };

    const output = renderStatusLine(state, agyData, {
      display: {
        showUsername: true,
        unicode: false
      }
    });

    assert.match(output, /user/);
    assert.match(output, /safe-suffix/);
    assert.doesNotMatch(output, /\x1b\]52/);
    assert.doesNotMatch(output, /\x1b\\/);
  });

  test('omits git branch in non-git directories when branch is null', () => {
    const output = renderStatusLine(
      { steps: 1, branch: null, currentDir: 'plain-folder' },
      { context_window: {}, plan_tier: 'Pro' },
      { display: { showGitBranch: true, unicode: true } }
    ).replace(/\x1b\[[0-9;]*m/g, '');

    assert.match(output, /plain-folder/);
    assert.doesNotMatch(output, /⎇/);
    assert.doesNotMatch(output, /unknown/);
  });

  test('returns empty string when config.enabled is false', () => {
    const output = renderStatusLine(
      { steps: 1, branch: 'main', currentDir: 'proj' },
      { context_window: { total_input_tokens: 100, total_output_tokens: 50, used_percentage: 10 } },
      { enabled: false, display: { unicode: false } }
    );
    assert.equal(output, '');
  });

  test('clamps out-of-range and non-finite progress percentages without throwing RangeError', () => {
    for (const usedPct of [-25, 150, NaN, Infinity]) {
      const output = renderStatusLine(
        { steps: 1, branch: 'main' },
        { context_window: { total_input_tokens: 100, total_output_tokens: 50, used_percentage: usedPct } },
        { display: { unicode: false } }
      );
      assert.match(output, /\[[#-]{10}\]/);
    }
  });

  test('supports string config.theme for preset names and single colors', () => {
    const state = { steps: 1, branch: 'main' };
    const agyData = {
      context_window: { total_input_tokens: 100, total_output_tokens: 50, used_percentage: 10 },
      model: { display_name: 'Gemini 3.5 Flash (High)' },
    };

    const presetOut = renderStatusLine(state, agyData, { theme: 'Ocean Blue', display: { unicode: false } });
    assert.match(presetOut, /\x1b\[34mGemini 3\.5 Flash\(H\)/);

    const singleOut = renderStatusLine(state, agyData, { theme: 'yellow', display: { unicode: false } });
    assert.match(singleOut, /\x1b\[33mGemini 3\.5 Flash\(H\)/);
  });

  test('supports string agyData.model', () => {
    const output = renderStatusLine(
      { steps: 1, branch: 'main' },
      { model: 'Claude 4.6 Sonnet (Thinking)', context_window: {} },
      { display: { unicode: false } }
    ).replace(/\x1b\[[0-9;]*m/g, '');
    assert.match(output, /Sonnet 4\.6\(Th\)/);
  });

  test('suppresses stale update badge when latestVersion is equal to or older than local package version', () => {
    const state = { steps: 1, branch: 'main' };
    const agyData = { context_window: { total_input_tokens: 0, total_output_tokens: 0, used_percentage: 0 } };
    const config = { display: { unicode: true, useNerdFonts: false } };

    const sameVer = renderStatusLine(state, agyData, config, [], 'Pro', {
      updateAvailable: true,
      latestVersion: '1.0.0',
    });
    assert.doesNotMatch(sameVer, /v1\.0\.0/);

    const olderVer = renderStatusLine(state, agyData, config, [], 'Pro', {
      updateAvailable: true,
      latestVersion: '0.9.9',
    });
    assert.doesNotMatch(olderVer, /v0\.9\.9/);
  });

  test('gracefully degrades Line 1, Line 2, and Line 3 within explicit 80/100/120 terminal_width (including CJK)', () => {
    const now = Date.now();
    const state = {
      branch: 'feature/very-long-branch-name',
      currentDir: 'my-long-workspace-folder',
      username: 'developer@example.com',
      breadcrumbs: ['src/components/DashboardView.tsx', 'runtime/renderer.js', 'tests/unit/renderer.layout.test.mjs'],
      rulesCount: 4,
      mcpCount: 3,
      hooksCount: 2,
      modelPerf: {
        Carbon: { model: 'Carbon', avgSpeed: 83.6, avgTtftSec: 10.8, lastUpdatedMs: now - 1000 },
        Hydro: { model: 'Hydro', avgSpeed: 260.9, avgTtftSec: 5.5, lastUpdatedMs: now - 2000 },
        Flash: { model: 'Flash', avgSpeed: 320.1, avgTtftSec: 2.1, lastUpdatedMs: now - 3000 },
        Opus: { model: 'Opus', avgSpeed: 72.4, avgTtftSec: 8.2, lastUpdatedMs: now - 4000 },
      },
      childModelSpeeds: [
        { model: 'Hydro', speed: 260.9 },
        { model: 'Flash', speed: 320.1 },
        { model: 'Opus', speed: 72.4 },
      ],
      costTotals: { todayUsd: 3.45, weekUsd: 18.9 },
    };

    for (const width of [80, 100, 120]) {
      const agyData = {
        terminal_width: width,
        model: { display_name: 'Gemini 4 Carbon (High)', id: 'gemini-4-carbon-high' },
        plan_tier: 'Google AI Ultra',
        cost: { total_usd: 1.23 },
        context_window: {
          total_input_tokens: 150000,
          total_output_tokens: 25000,
          used_percentage: 30,
          context_window_size: 512000,
        },
      };
      const output = renderStatusLine(
        state,
        agyData,
        {
          language: 'zh',
          display: {
            unicode: true,
            useNerdFonts: false,
            showUsername: true,
            showCost: true,
            showDailyCost: true,
            showWeeklyCost: true,
            showSpeed: true,
            quotaStyle: 'none',
          },
        },
        [],
        null,
        { updateAvailable: true, latestVersion: '1.2.3' }
      );
      const lines = output.split('\n');
      for (const line of lines) {
        assert.ok(
          getAnsiDisplayWidth(line) <= width,
          `width=${width} exceeded: ${getAnsiDisplayWidth(line)} -> ${line}`
        );
      }
      assert.match(lines[0], /Gemini 4 Carbon\(H\)/);
    }
  });
});
