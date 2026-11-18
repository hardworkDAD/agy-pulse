'use strict';

const fs = require('fs');
const readline = require('readline');
const { loadConfig, saveConfig, getLocalConfigPath, getGlobalConfigPath } = require('./config.js');
const { renderHUD } = require('./renderer.js');

const THEME_PRESETS = {
  'Emerald Green': { primary: 'green', secondary: 'gray', warning: 'yellow', critical: 'red' },
  'Ocean Blue': { primary: 'blue', secondary: 'gray', warning: 'yellow', critical: 'red' },
  'Cyberpunk Magenta': { primary: 'magenta', secondary: 'gray', warning: 'yellow', critical: 'red' },
  'Amber Gold': { primary: 'yellow', secondary: 'gray', warning: 'cyan', critical: 'red' },
};

const QUOTA_STYLES = ['table', 'compact', 'models'];
const LANGUAGES = ['auto', 'en', 'zh'];

const mockState = {
  branch: 'main',
  currentDir: 'agy-pulse',
  steps: 12,
  breadcrumbs: ['index.js', 'auth.js', 'database.js'],
  rulesCount: 4,
  mcpCount: 2,
  hooksCount: 3,
  outputSpeed: 43.4,
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
  task_count: 5,
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

const _wizardNow = Date.now();
const _wizardFiveHourReset = new Date(_wizardNow + 4 * 60 * 60 * 1000).toISOString();
const _wizardWeeklyReset = new Date(_wizardNow + 5 * 24 * 60 * 60 * 1000).toISOString();
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
];

function detectThemePresetName(theme) {
  if (!theme || typeof theme !== 'object') return 'Custom';
  for (const [name, colors] of Object.entries(THEME_PRESETS)) {
    if (
      theme.primary === colors.primary &&
      theme.secondary === colors.secondary &&
      theme.warning === colors.warning &&
      theme.critical === colors.critical
    ) {
      return name;
    }
  }
  return 'Custom';
}

function createWizardState(currentConfig = {}, options = {}) {
  const config = JSON.parse(JSON.stringify(currentConfig || {}));
  config.display = config.display || {};
  config.theme = config.theme || { ...THEME_PRESETS['Emerald Green'] };
  if (!config.language) config.language = 'auto';

  const cwd = options.cwd || process.cwd();
  const localExists = typeof options.localExists === 'boolean'
    ? options.localExists
    : fs.existsSync(getLocalConfigPath(cwd));

  return {
    config,
    cwd,
    targetIsGlobal: !localExists,
    themeName: detectThemePresetName(config.theme),
    originalTheme: { ...config.theme },
  };
}

function buildMenuItems(state, actions = {}) {
  const { config } = state;
  const boolToggle = (key, defaultValue) => {
    const isEnabled = defaultValue ? config.display[key] !== false : config.display[key] === true;
    return {
      getValue: () => {
        const cur = defaultValue ? config.display[key] !== false : config.display[key] === true;
        return cur ? 'Enabled' : 'Disabled';
      },
      toggle: () => {
        const cur = defaultValue ? config.display[key] !== false : config.display[key] === true;
        config.display[key] = !cur;
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
          ? { ...state.originalTheme }
          : { ...THEME_PRESETS[state.themeName] };
      },
    },
    {
      key: 'quotaStyle',
      label: 'Quota Display Mode',
      getValue: () => {
        const style = config.display.quotaStyle || 'table';
        if (style === 'compact') return 'Compact';
        if (style === 'models') return 'Models';
        return 'Table';
      },
      toggle: () => {
        const current = config.display.quotaStyle || 'table';
        const idx = QUOTA_STYLES.indexOf(current);
        config.display.quotaStyle = QUOTA_STYLES[(idx + 1) % QUOTA_STYLES.length];
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
        } else if (config.display.unicode === undefined) {
          config.display.useNerdFonts = false;
          config.display.unicode = true;
        } else if (config.display.unicode === true) {
          config.display.useNerdFonts = false;
          config.display.unicode = false;
        } else {
          config.display.useNerdFonts = true;
          config.display.unicode = true;
        }
      },
    },
    { key: 'showContextMonitor', label: 'Show Context Monitor', ...boolToggle('showContextMonitor', true) },
    { key: 'showCurrentDir', label: 'Show Current Directory', ...boolToggle('showCurrentDir', true) },
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
        config.display.breadcrumbCount = (val % 5) + 1;
      },
    },
    { key: 'showSpeed', label: 'Show Output Speed', ...boolToggle('showSpeed', false) },
    { key: 'showCost', label: 'Show Session Cost', ...boolToggle('showCost', false) },
    { key: 'showDailyCost', label: 'Show Daily Cost', ...boolToggle('showDailyCost', false) },
    { key: 'showWeeklyCost', label: 'Show Weekly Cost', ...boolToggle('showWeeklyCost', false) },
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
      await saveConfig(curState.config, curState.targetIsGlobal, curState.cwd);
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

      stdout.write('\x1b[1m\x1b[35m=== agy-pulse Configuration Wizard ===\x1b[0m\n\n');
      stdout.write('Use \x1b[1m↑/↓\x1b[0m, \x1b[1mj/k\x1b[0m, or \x1b[1m1-9/0\x1b[0m to navigate, \x1b[1mSpace/Enter\x1b[0m to toggle, \x1b[1ms\x1b[0m to save, \x1b[1mq\x1b[0m to cancel.\n\n');

      for (let i = 0; i < menuItems.length; i++) {
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

      stdout.write('\n\x1b[90m─────────────────────────────────────────────────────────────────────────────────\x1b[0m\n');
      stdout.write('\x1b[1mHUD PREVIEW:\x1b[0m\n');
      try {
        const hudPreview = renderHUD(mockState, mockAgyData, state.config, mockQuotaData, 'Pro');
        stdout.write(`${hudPreview}\n`);
      } catch (err) {
        stdout.write(`\x1b[31mFailed to render preview: ${err.message}\x1b[0m\n`);
      }
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
  LANGUAGES,
  detectThemePresetName,
  createWizardState,
  buildMenuItems,
  handleWizardKeypress,
  startWizard,
};
