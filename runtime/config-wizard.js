'use strict';

const fs = require('fs');
const readline = require('readline');
const {
  FULL_DEFAULT_CONFIG,
  mergeConfig,
  loadConfig,
  saveConfig,
  getLocalConfigPath,
  getGlobalConfigPath,
} = require('./config.js');
const { renderStatusLine } = require('./renderer.js');
const { THEME_PRESETS, normalizeThemeObject } = require('./renderer/format.js');

const QUOTA_STYLES = ['table', 'compact', 'models', 'none'];
const BREADCRUMB_COUNTS = [0, 1, 2, 3, 5];
const LANGUAGES = ['auto', 'en', 'zh'];

const _wizardNow = Date.now();
const _wizardFiveHourReset = new Date(_wizardNow + 4 * 60 * 60 * 1000).toISOString();
const _wizardWeeklyReset = new Date(_wizardNow + 5 * 24 * 60 * 60 * 1000).toISOString();

const mockState = {
  username: 'developer@agy.local',
  branch: 'main',
  currentDir: 'agy-pulse',
  steps: 12,
  breadcrumbs: ['index.js', 'auth.js', 'database.js'],
  rulesCount: 4,
  mcpCount: 2,
  hooksCount: 3,
  outputSpeed: 43.4,
  modelPerf: {
    Carbon: {
      model: 'Carbon',
      avgSpeed: 83.6,
      avgTtftSec: 10.8,
      streamSamples: 89,
      ttftSamples: 89,
      lastUpdatedMs: _wizardNow,
    },
    Hydro: {
      model: 'Hydro',
      avgSpeed: 64.2,
      avgTtftSec: 4.3,
      streamSamples: 24,
      ttftSamples: 24,
      lastUpdatedMs: _wizardNow - 60000,
    },
  },
  childModelSpeeds: [
    { model: 'Hydro', speed: 64.2 },
  ],
  costBreakdown: {
    totalCostUsd: 0.125,
    mainCostUsd: 0.085,
    childrenCostUsd: 0.04,
    childSessionCount: 1,
    source: 'sqlite',
  },
  compactionCount: 1,
  totalOutputTokens: 13900,
  serverCompactionLimit: 512000,
  lastTurnUsage: {
    input_tokens: 196600,
    output_tokens: 1200,
    cache_read_input_tokens: 160000,
    cache_creation_input_tokens: 0,
  },
  turnHistoryMaxByMode: {
    inclusivePeak: 480000,
    exclusivePeak: 480000,
  },
  costTotals: {
    todayUsd: 0.45,
    weekUsd: 1.82,
  },
  usage: {
    current_usage: {
      input_tokens: 196600,
      cache_read_input_tokens: 160000,
    },
  },
};

const mockAgyData = {
  plan_tier: 'Pro',
  cycle_mode: 'auto',
  task_count: 5,
  terminal_width: process.stdout?.columns || 120,
  model: {
    display_name: 'Gemini 4 Carbon (High)',
    id: 'gemini-4-carbon-high',
  },
  cost: {
    total_cost_usd: 0.085,
    total_api_duration_ms: 3200,
  },
  context_window: {
    total_input_tokens: 196600,
    total_output_tokens: 13900,
    used_percentage: 38.4,
    context_window_size: 1000000,
  },
};

const mockQuotaData = [
  {
    id: 'gemini-4-carbon-high',
    displayName: 'Gemini 4 Carbon (High)',
    remainingFraction: 0.92,
    resetTime: _wizardFiveHourReset,
    modelProvider: 'MODEL_PROVIDER_GOOGLE',
    windows: {
      fiveHour: { remainingFraction: 0.92, resetTime: _wizardFiveHourReset, observedAt: _wizardNow },
      weekly: { remainingFraction: 0.55, resetTime: _wizardWeeklyReset, observedAt: _wizardNow },
    },
  },
  {
    id: 'claude-sonnet-4-6',
    displayName: 'Claude Sonnet 4.6 (Thinking)',
    remainingFraction: 0.4,
    resetTime: _wizardWeeklyReset,
    modelProvider: 'MODEL_PROVIDER_ANTHROPIC',
    windows: {
      fiveHour: { remainingFraction: 0.7, resetTime: _wizardFiveHourReset, observedAt: _wizardNow },
      weekly: { remainingFraction: 0.4, resetTime: _wizardWeeklyReset, observedAt: _wizardNow },
    },
  },
  {
    id: 'gpt-oss-120b-medium',
    displayName: 'GPT-OSS 120B (Medium)',
    remainingFraction: 0.75,
    resetTime: null,
    modelProvider: 'MODEL_PROVIDER_OPENAI',
    windows: {},
  },
  {
    id: 'gemini-3.1-flash-image',
    displayName: 'Gemini 3.1 Flash Image',
    remainingFraction: 0.85,
    resetTime: _wizardFiveHourReset,
    modelProvider: 'MODEL_PROVIDER_GOOGLE',
    windows: {
      fiveHour: { remainingFraction: 0.85, resetTime: _wizardFiveHourReset, observedAt: _wizardNow },
    },
  },
];

function detectThemePresetName(theme) {
  if (!theme || (typeof theme !== 'object' && typeof theme !== 'string')) return 'Custom';
  const normalized = typeof theme === 'string' ? normalizeThemeObject(theme) : theme;
  for (const [name, colors] of Object.entries(THEME_PRESETS)) {
    if (
      normalized.primary === colors.primary &&
      normalized.secondary === colors.secondary &&
      normalized.warning === colors.warning &&
      normalized.critical === colors.critical
    ) {
      return name;
    }
  }
  return 'Custom';
}

function createWizardState(currentConfig = {}, options = {}) {
  const rawConfig = currentConfig && typeof currentConfig === 'object' ? currentConfig : {};
  const config = JSON.parse(JSON.stringify(rawConfig));
  if (config.enabled === undefined) {
    config.enabled = FULL_DEFAULT_CONFIG.enabled;
  }
  config.thresholds = {
    ...FULL_DEFAULT_CONFIG.thresholds,
    ...(config.thresholds && typeof config.thresholds === 'object' ? config.thresholds : {}),
  };
  const rawDisplay = config.display && typeof config.display === 'object' ? config.display : {};
  config.display = {
    ...rawDisplay,
    breadcrumbCount: rawDisplay.breadcrumbCount ?? FULL_DEFAULT_CONFIG.display.breadcrumbCount,
    useNerdFonts: rawDisplay.useNerdFonts ?? FULL_DEFAULT_CONFIG.display.useNerdFonts,
    showContextMonitor: rawDisplay.showContextMonitor ?? FULL_DEFAULT_CONFIG.display.showContextMonitor,
    showModelCache: rawDisplay.showModelCache ?? FULL_DEFAULT_CONFIG.display.showModelCache,
    showCurrentDir: rawDisplay.showCurrentDir ?? FULL_DEFAULT_CONFIG.display.showCurrentDir,
    showCycleMode: rawDisplay.showCycleMode ?? FULL_DEFAULT_CONFIG.display.showCycleMode,
    showModel: rawDisplay.showModel ?? FULL_DEFAULT_CONFIG.display.showModel,
    showTurnCacheHitRate: rawDisplay.showTurnCacheHitRate ?? FULL_DEFAULT_CONFIG.display.showTurnCacheHitRate,
    showUsername: rawDisplay.showUsername ?? FULL_DEFAULT_CONFIG.display.showUsername,
    showCost: rawDisplay.showCost ?? FULL_DEFAULT_CONFIG.display.showCost,
    showDailyCost: rawDisplay.showDailyCost ?? FULL_DEFAULT_CONFIG.display.showDailyCost,
    showWeeklyCost: rawDisplay.showWeeklyCost ?? FULL_DEFAULT_CONFIG.display.showWeeklyCost,
  };
  const normalizedTheme = normalizeThemeObject(config.theme);
  config.theme = { ...normalizedTheme };
  if (!config.language) config.language = FULL_DEFAULT_CONFIG.language || 'auto';

  const cwd = options.cwd || process.cwd();
  const localExists = typeof options.localExists === 'boolean'
    ? options.localExists
    : fs.existsSync(getLocalConfigPath(cwd));

  return {
    config,
    cwd,
    targetIsGlobal: !localExists,
    themeName: detectThemePresetName(normalizedTheme),
    originalTheme: { ...normalizedTheme },
    changes: {
      display: {},
    },
  };
}

function readRawJsonSafe(filePath) {
  try {
    if (filePath && fs.existsSync(filePath)) {
      const parsed = JSON.parse(fs.readFileSync(filePath, 'utf8'));
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return parsed;
      }
    }
  } catch {}
  return {};
}

function buildConfigForScopeSave(state) {
  const targetPath = state.targetIsGlobal
    ? getGlobalConfigPath()
    : getLocalConfigPath(state.cwd);
  const base = readRawJsonSafe(targetPath);
  const changes = state.changes || {};
  const override = {};
  if (changes.enabled !== undefined) override.enabled = changes.enabled;
  if (changes.language !== undefined) override.language = changes.language;
  if (changes.theme !== undefined) override.theme = changes.theme;
  if (changes.display && Object.keys(changes.display).length > 0) {
    override.display = { ...changes.display };
  }
  const next = mergeConfig(base, override);
  if (next.display && next.display.unicode === null) {
    delete next.display.unicode;
  }
  return next;
}

function buildMenuItems(state, actions = {}) {
  const { config } = state;
  if (!state.changes || typeof state.changes !== 'object') {
    state.changes = { display: {} };
  }
  if (!state.changes.display || typeof state.changes.display !== 'object') {
    state.changes.display = {};
  }
  const boolToggle = (key, defaultValue) => {
    const readEffective = () => (defaultValue ? config.display[key] !== false : config.display[key] === true);
    const isEnabled = readEffective();
    return {
      displayKey: key,
      getEffectiveBool: readEffective,
      getValue: () => (readEffective() ? 'Enabled' : 'Disabled'),
      toggle: () => {
        const nextVal = !readEffective();
        config.display[key] = nextVal;
        state.changes.display[key] = nextVal;
      },
      _default: isEnabled,
    };
  };

  return [
    {
      key: 'scope',
      label: 'Configuration Scope',
      getValue: () => (state.targetIsGlobal ? 'Global (All Projects)' : 'Local (Current Project Workspace)'),
      toggle: () => {
        state.targetIsGlobal = !state.targetIsGlobal;
      },
    },
    {
      key: 'language',
      label: 'Language',
      getValue: () => config.language || 'auto',
      toggle: () => {
        const idx = LANGUAGES.indexOf(config.language || 'auto');
        config.language = LANGUAGES[(idx + 1) % LANGUAGES.length];
        state.changes.language = config.language;
      },
    },
    {
      key: 'theme',
      label: 'Theme Preset',
      getValue: () => state.themeName,
      toggle: () => {
        const keys = Object.keys(THEME_PRESETS).concat(['Custom']);
        const idx = keys.indexOf(state.themeName);
        state.themeName = keys[(idx + 1) % keys.length];
        config.theme = state.themeName === 'Custom'
          ? { ...normalizeThemeObject(state.originalTheme) }
          : { ...THEME_PRESETS[state.themeName] };
        state.changes.theme = { ...config.theme };
      },
    },
    {
      key: 'quotaStyle',
      label: 'Quota Display Mode',
      getValue: () => {
        const style = config.display.quotaStyle || 'table';
        if (style === 'compact') return 'Compact';
        if (style === 'models') return 'Models';
        if (style === 'none') return 'None';
        return 'Table';
      },
      toggle: () => {
        const current = config.display.quotaStyle || 'table';
        const idx = QUOTA_STYLES.indexOf(current);
        config.display.quotaStyle = QUOTA_STYLES[(idx + 1) % QUOTA_STYLES.length];
        state.changes.display.quotaStyle = config.display.quotaStyle;
      },
    },
    {
      key: 'iconSet',
      label: 'Icon & Font Set',
      getValue: () => {
        if (config.display.useNerdFonts) return 'Nerd Fonts';
        if (config.display.unicode === true) return 'Unicode Emoji';
        if (config.display.unicode === false) return 'Plain ASCII';
        return 'Auto (UTF-8 Detect)';
      },
      toggle: () => {
        if (config.display.useNerdFonts) {
          config.display.useNerdFonts = false;
          delete config.display.unicode;
          state.changes.display.useNerdFonts = false;
          state.changes.display.unicode = null;
        } else if (config.display.unicode === undefined) {
          config.display.useNerdFonts = false;
          config.display.unicode = true;
          state.changes.display.useNerdFonts = false;
          state.changes.display.unicode = true;
        } else if (config.display.unicode === true) {
          config.display.useNerdFonts = false;
          config.display.unicode = false;
          state.changes.display.useNerdFonts = false;
          state.changes.display.unicode = false;
        } else {
          config.display.useNerdFonts = true;
          config.display.unicode = true;
          state.changes.display.useNerdFonts = true;
          state.changes.display.unicode = true;
        }
      },
    },
    { key: 'showContextMonitor', label: 'Show Context Monitor', ...boolToggle('showContextMonitor', true) },
    { key: 'showModelCache', label: 'Show Model Cache Line', ...boolToggle('showModelCache', true) },
    { key: 'showCurrentDir', label: 'Show Current Directory', ...boolToggle('showCurrentDir', true) },
    { key: 'showCycleMode', label: 'Show Cycle Mode (Shift+Tab)', ...boolToggle('showCycleMode', true) },
    { key: 'showModel', label: 'Show Model (Line 1)', ...boolToggle('showModel', false) },
    { key: 'showGit', label: 'Show Git Branch', ...boolToggle('showGitBranch', true) },
    { key: 'showUsername', label: 'Show Username', ...boolToggle('showUsername', false) },
    { key: 'showTokens', label: 'Show Token Bar', ...boolToggle('showTokenBar', true) },
    { key: 'showContextBar', label: 'Show Context Bar (Line 2)', ...boolToggle('showContextBar', true) },
    { key: 'showTurnCacheHitRate', label: 'Show Turn Cache Hit Rate', ...boolToggle('showTurnCacheHitRate', false) },
    { key: 'showImageQuota', label: 'Show Image Quota', ...boolToggle('showImageQuota', false) },
    { key: 'showBreadcrumbs', label: 'Show Breadcrumbs', ...boolToggle('showBreadcrumbs', true) },
    {
      key: 'breadcrumbCount',
      label: 'Breadcrumb Limit',
      getValue: () => (config.display.breadcrumbCount !== undefined ? config.display.breadcrumbCount : 3),
      toggle: () => {
        const val = config.display.breadcrumbCount !== undefined ? config.display.breadcrumbCount : 3;
        const idx = BREADCRUMB_COUNTS.indexOf(val);
        const nextVal = idx >= 0
          ? BREADCRUMB_COUNTS[(idx + 1) % BREADCRUMB_COUNTS.length]
          : (BREADCRUMB_COUNTS.find(v => v > val) ?? BREADCRUMB_COUNTS[0]);
        config.display.breadcrumbCount = nextVal;
        state.changes.display.breadcrumbCount = nextVal;
      },
    },
    { key: 'showSpeed', label: 'Show Output Speed', ...boolToggle('showSpeed', false) },
    { key: 'showCost', label: 'Show Session Cost', ...boolToggle('showCost', false) },
    { key: 'showDailyCost', label: 'Show Daily Cost', ...boolToggle('showDailyCost', false) },
    { key: 'showWeeklyCost', label: 'Show Weekly Cost', ...boolToggle('showWeeklyCost', false) },
    {
      key: 'enabled',
      label: 'Enable agy-pulse Status Line',
      getValue: () => (config.enabled !== false ? 'Enabled' : 'Disabled'),
      toggle: () => {
        config.enabled = config.enabled === false;
        state.changes.enabled = config.enabled;
      },
    },
    {
      key: 'save',
      label: 'Save & Exit',
      getValue: () => '',
      toggle: async () => {
        if (actions.onSave) await actions.onSave(state);
      },
    },
    {
      key: 'cancel',
      label: 'Cancel & Exit',
      getValue: () => '',
      toggle: async () => {
        if (actions.onCancel) await actions.onCancel(state);
      },
    },
  ];
}

function buildPreviewConfig(state, menuItems) {
  const baseConfig = state?.config ? JSON.parse(JSON.stringify(state.config)) : {};
  baseConfig.display = baseConfig.display && typeof baseConfig.display === 'object' ? baseConfig.display : {};
  baseConfig.theme = normalizeThemeObject(baseConfig.theme);
  const items = Array.isArray(menuItems) ? menuItems : buildMenuItems(state);
  for (const item of items) {
    if (item && item.displayKey && typeof item.getEffectiveBool === 'function') {
      baseConfig.display[item.displayKey] = item.getEffectiveBool();
    }
  }
  return baseConfig;
}

async function handleWizardKeypress(str, key, wizardCtx) {
  const { menuItems } = wizardCtx;
  if (key?.ctrl && key.name === 'c') {
    await wizardCtx.abort();
    return;
  }

  if (key?.name === 'up' || key?.name === 'k') {
    wizardCtx.selectedIdx = (wizardCtx.selectedIdx - 1 + menuItems.length) % menuItems.length;
    wizardCtx.renderMenu();
  } else if (key?.name === 'down' || key?.name === 'j') {
    wizardCtx.selectedIdx = (wizardCtx.selectedIdx + 1) % menuItems.length;
    wizardCtx.renderMenu();
  } else if (key?.name === 'space' || key?.name === 'return' || key?.name === 'enter') {
    await menuItems[wizardCtx.selectedIdx].toggle();
    wizardCtx.renderMenu();
  } else if (str === 's' || str === 'S') {
    const saveItem = menuItems.find(item => item.key === 'save');
    if (saveItem) await saveItem.toggle();
  } else if (str === 'q' || str === 'Q' || key?.name === 'escape') {
    const cancelItem = menuItems.find(item => item.key === 'cancel');
    if (cancelItem) await cancelItem.toggle();
  } else if (str && /^[0-9]$/.test(str)) {
    const num = parseInt(str, 10);
    const targetIdx = num === 0 ? 9 : num - 1;
    if (targetIdx < menuItems.length) {
      wizardCtx.selectedIdx = targetIdx;
      wizardCtx.renderMenu();
    }
  }
}

function computeMenuViewport(totalItems, selectedIdx, termRows, previewLineCount = 6) {
  if (!Number.isFinite(termRows) || termRows <= 0) {
    return { startIdx: 0, endIdx: totalItems, hasAbove: false, hasBelow: false };
  }
  const reservedLines = 8 + Math.max(1, previewLineCount);
  const maxVisible = Math.max(3, termRows - reservedLines);
  if (totalItems <= maxVisible) {
    return { startIdx: 0, endIdx: totalItems, hasAbove: false, hasBelow: false };
  }
  const windowSize = Math.max(3, maxVisible - 2);
  const startIdx = Math.max(0, Math.min(
    selectedIdx - Math.floor(windowSize / 2),
    totalItems - windowSize
  ));
  const endIdx = Math.min(totalItems, startIdx + windowSize);
  return {
    startIdx,
    endIdx,
    hasAbove: startIdx > 0,
    hasBelow: endIdx < totalItems,
  };
}

async function startWizard(options = {}) {
  const cwd = options.cwd || process.cwd();
  const currentConfig = await loadConfig(cwd);
  const state = createWizardState(currentConfig, { cwd });

  const stdin = options.stdin || process.stdin;
  const stdout = options.stdout || process.stdout;
  const exit = options.exit || ((code) => process.exit(code));

  function cleanup() {
    if (stdin.isTTY && typeof stdin.setRawMode === 'function') {
      stdin.setRawMode(false);
    }
    if (typeof stdin.pause === 'function') {
      stdin.pause();
    }
  }

  const menuItems = buildMenuItems(state, {
    onSave: async (curState) => {
      const nextConfig = buildConfigForScopeSave(curState);
      await saveConfig(nextConfig, curState.targetIsGlobal, curState.cwd);
      cleanup();
      const targetPath = curState.targetIsGlobal
        ? getGlobalConfigPath()
        : getLocalConfigPath(curState.cwd);
      stdout.write(`\n\x1b[32m✔ Configuration saved successfully to ${targetPath}\x1b[0m\n\n`);
      exit(0);
    },
    onCancel: async () => {
      cleanup();
      stdout.write('\n\x1b[90mConfiguration canceled.\x1b[0m\n\n');
      exit(0);
    },
  });

  const wizardCtx = {
    selectedIdx: 0,
    menuItems,
    renderMenu() {
      readline.cursorTo(stdout, 0, 0);
      readline.clearScreenDown(stdout);

      let previewBody = '';
      let previewError = null;
      try {
        const previewConfig = buildPreviewConfig(state, menuItems);
        const previewAgyData = {
          ...mockAgyData,
          terminal_width: stdout?.columns || process.stdout?.columns || 120,
        };
        const statusLinePreview = renderStatusLine(mockState, previewAgyData, previewConfig, mockQuotaData, 'Pro');
        previewBody = previewConfig.enabled === false
          ? '\x1b[90m(agy-pulse status line is disabled)\x1b[0m'
          : statusLinePreview;
      } catch (err) {
        previewError = err;
        previewBody = `\x1b[31mFailed to render preview: ${err.message}\x1b[0m`;
      }

      const previewLineCount = previewBody ? previewBody.split('\n').length : 1;
      const termRows = Number(stdout?.rows || process.stdout?.rows || 0);
      const viewport = computeMenuViewport(menuItems.length, wizardCtx.selectedIdx, termRows, previewLineCount);

      stdout.write('\x1b[1m\x1b[35m=== agy-pulse Configuration Wizard ===\x1b[0m\n\n');
      stdout.write('Use \x1b[1m↑/↓\x1b[0m, \x1b[1mj/k\x1b[0m, or \x1b[1m1-9/0\x1b[0m to navigate, \x1b[1mSpace/Enter\x1b[0m to toggle, \x1b[1ms\x1b[0m to save, \x1b[1mq\x1b[0m to cancel.\n\n');

      if (viewport.hasAbove) {
        stdout.write(`  \x1b[90m▲ (${viewport.startIdx} more above)\x1b[0m\n`);
      }

      for (let i = viewport.startIdx; i < viewport.endIdx; i++) {
        const item = menuItems[i];
        const isSelected = i === wizardCtx.selectedIdx;
        const marker = isSelected ? '\x1b[32m▸ \x1b[0m' : '  ';
        const labelStr = isSelected ? `\x1b[1m${item.label}\x1b[0m` : item.label;
        const valStr = item.getValue() !== '' ? `: \x1b[36m${item.getValue()}\x1b[0m` : '';

        if (item.key === 'save') {
          const actionMarker = isSelected ? '\x1b[32m[ Save (s) ]\x1b[0m' : '[ Save (s) ]';
          stdout.write(`${marker}${actionMarker}\n`);
        } else if (item.key === 'cancel') {
          const actionMarker = isSelected ? '\x1b[31m[ Cancel (q) ]\x1b[0m' : '[ Cancel (q) ]';
          stdout.write(`${marker}${actionMarker}\n`);
        } else {
          stdout.write(`${marker}${labelStr}${valStr}\n`);
        }
      }

      if (viewport.hasBelow) {
        stdout.write(`  \x1b[90m▼ (${menuItems.length - viewport.endIdx} more below)\x1b[0m\n`);
      }

      stdout.write('\n\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n');
      stdout.write('\x1b[1magy-pulse Status Line Preview\x1b[0m\n');
      stdout.write(`${previewBody}${previewError ? '\n' : '\n'}`);
      stdout.write('\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n');
    },
    abort: async () => {
      cleanup();
      exit(1);
    },
  };

  readline.emitKeypressEvents(stdin);
  if (stdin.isTTY && typeof stdin.setRawMode === 'function') {
    stdin.setRawMode(true);
  }

  wizardCtx.renderMenu();

  stdin.on('keypress', async (str, key) => {
    try {
      await handleWizardKeypress(str, key, wizardCtx);
    } catch (err) {
      cleanup();
      process.stderr.write(`\n\x1b[31mError: ${err.message}\x1b[0m\n\n`);
      exit(1);
    }
  });

  return wizardCtx;
}

module.exports = {
  THEME_PRESETS,
  QUOTA_STYLES,
  BREADCRUMB_COUNTS,
  LANGUAGES,
  mockState,
  mockAgyData,
  mockQuotaData,
  normalizeThemeObject,
  detectThemePresetName,
  createWizardState,
  buildConfigForScopeSave,
  computeMenuViewport,
  buildMenuItems,
  buildPreviewConfig,
  handleWizardKeypress,
  startWizard,
};
