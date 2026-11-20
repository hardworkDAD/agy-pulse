import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
  THEME_PRESETS,
  QUOTA_STYLES,
  BREADCRUMB_COUNTS,
  mockState,
  mockAgyData,
  mockQuotaData,
  normalizeThemeObject,
  createWizardState,
  buildConfigForScopeSave,
  computeMenuViewport,
  buildMenuItems,
  buildPreviewConfig,
  handleWizardKeypress,
} = require('../../runtime/config-wizard.js');
const { getGlobalConfigPath } = require('../../runtime/config.js');

test('wizard preserves quotaStyle=models and cycles table -> compact -> models -> none -> table', () => {
  assert.deepEqual(QUOTA_STYLES, ['table', 'compact', 'models', 'none']);
  const state = createWizardState({ display: { quotaStyle: 'models' } }, { localExists: false });
  const items = buildMenuItems(state);
  const quotaItem = items.find(i => i.key === 'quotaStyle');

  assert.equal(quotaItem.getValue(), 'Models');
  quotaItem.toggle();
  assert.equal(state.config.display.quotaStyle, 'none');
  assert.equal(quotaItem.getValue(), 'None');
  quotaItem.toggle();
  assert.equal(state.config.display.quotaStyle, 'table');
  quotaItem.toggle();
  assert.equal(state.config.display.quotaStyle, 'compact');
  quotaItem.toggle();
  assert.equal(state.config.display.quotaStyle, 'models');
});

test('wizard supports showContextMonitor, showModel, showContextBar, showTurnCacheHitRate, showImageQuota, and language', () => {
  const state = createWizardState({ display: {} }, { localExists: true });
  const items = buildMenuItems(state);

  assert.equal(state.targetIsGlobal, false);

  const getItem = (key) => items.find(i => i.key === key);
  assert.equal(getItem('showContextMonitor').getValue(), 'Enabled');
  getItem('showContextMonitor').toggle();
  assert.equal(state.config.display.showContextMonitor, false);

  assert.equal(getItem('showModelCache').getValue(), 'Enabled');
  getItem('showModelCache').toggle();
  assert.equal(state.config.display.showModelCache, false);

  assert.equal(getItem('showModel').getValue(), 'Disabled');
  getItem('showModel').toggle();
  assert.equal(state.config.display.showModel, true);

  assert.equal(getItem('showContextBar').getValue(), 'Enabled');
  getItem('showContextBar').toggle();
  assert.equal(state.config.display.showContextBar, false);

  assert.equal(getItem('showTurnCacheHitRate').getValue(), 'Disabled');
  getItem('showTurnCacheHitRate').toggle();
  assert.equal(state.config.display.showTurnCacheHitRate, true);

  assert.equal(getItem('showImageQuota').getValue(), 'Disabled');
  getItem('showImageQuota').toggle();
  assert.equal(state.config.display.showImageQuota, true);

  assert.equal(getItem('language').getValue(), 'auto');
  getItem('language').toggle();
  assert.equal(state.config.language, 'en');
  getItem('language').toggle();
  assert.equal(state.config.language, 'zh');
  getItem('language').toggle();
  assert.equal(state.config.language, 'auto');
});

test('wizard hotkeys support j/k navigation, s to save, and q to cancel', async () => {
  let saved = false;
  let canceled = false;
  const state = createWizardState({}, { localExists: false });
  const menuItems = buildMenuItems(state, {
    onSave: async () => { saved = true; },
    onCancel: async () => { canceled = true; },
  });

  const ctx = {
    selectedIdx: 0,
    menuItems,
    renderMenu: () => {},
    abort: async () => {},
  };

  await handleWizardKeypress('j', { name: 'j' }, ctx);
  assert.equal(ctx.selectedIdx, 1);

  await handleWizardKeypress('k', { name: 'k' }, ctx);
  assert.equal(ctx.selectedIdx, 0);

  await handleWizardKeypress('0', { name: '0' }, ctx);
  assert.equal(ctx.selectedIdx, 9);

  await handleWizardKeypress('s', { name: 's' }, ctx);
  assert.equal(saved, true);

  await handleWizardKeypress('q', { name: 'q' }, ctx);
  assert.equal(canceled, true);
});

test('wizard normalizes string theme without spreading characters on cycle to Custom', () => {
  const singleColorState = createWizardState({ theme: 'yellow' }, { localExists: false });
  assert.deepEqual(singleColorState.originalTheme, {
    primary: 'yellow',
    secondary: 'gray',
    warning: 'yellow',
    critical: 'red',
  });
  assert.equal(singleColorState.themeName, 'Custom');

  const items = buildMenuItems(singleColorState);
  const themeItem = items.find(i => i.key === 'theme');
  for (let i = 0; i < Object.keys(THEME_PRESETS).length + 1; i++) {
    themeItem.toggle();
  }
  assert.equal(singleColorState.themeName, 'Custom');
  assert.deepEqual(singleColorState.config.theme, {
    primary: 'yellow',
    secondary: 'gray',
    warning: 'yellow',
    critical: 'red',
  });
  assert.equal(singleColorState.config.theme['0'], undefined);

  const presetState = createWizardState({ theme: 'Ocean Blue' }, { localExists: false });
  assert.equal(presetState.themeName, 'Ocean Blue');
  assert.deepEqual(presetState.config.theme, THEME_PRESETS['Ocean Blue']);
  assert.deepEqual(normalizeThemeObject('magenta'), {
    primary: 'magenta',
    secondary: 'gray',
    warning: 'yellow',
    critical: 'red',
  });
});

test('wizard supports enabled toggle before save and buildPreviewConfig aligns preview with menu state', () => {
  const state = createWizardState({ display: {} }, { localExists: false });
  const items = buildMenuItems(state);
  const enabledIdx = items.findIndex(i => i.key === 'enabled');
  const saveIdx = items.findIndex(i => i.key === 'save');
  assert.ok(enabledIdx >= 0 && enabledIdx === saveIdx - 1);

  assert.equal(items[enabledIdx].getValue(), 'Enabled');
  items[enabledIdx].toggle();
  assert.equal(state.config.enabled, false);
  assert.equal(items[enabledIdx].getValue(), 'Disabled');
  items[enabledIdx].toggle();
  assert.equal(state.config.enabled, true);

  const previewConfig = buildPreviewConfig(state, items);
  assert.equal(previewConfig.display.showModel, false);
  assert.equal(previewConfig.display.showContextMonitor, true);
  assert.equal(previewConfig.display.showGitBranch, true);
  assert.equal(previewConfig.display.showUsername, false);
  assert.equal(previewConfig.display.showCost, false);

  items.find(i => i.key === 'showModel').toggle();
  const updatedPreview = buildPreviewConfig(state, items);
  assert.equal(updatedPreview.display.showModel, true);
});

test('wizard cycles breadcrumbCount across [0, 1, 2, 3, 5] and populates full preview mock data', () => {
  assert.deepEqual(BREADCRUMB_COUNTS, [0, 1, 2, 3, 5]);
  const state = createWizardState({ display: { breadcrumbCount: 3 } }, { localExists: false });
  const items = buildMenuItems(state);
  const bcItem = items.find(i => i.key === 'breadcrumbCount');

  assert.equal(bcItem.getValue(), 3);
  bcItem.toggle();
  assert.equal(bcItem.getValue(), 5);
  bcItem.toggle();
  assert.equal(bcItem.getValue(), 0);
  bcItem.toggle();
  assert.equal(bcItem.getValue(), 1);
  bcItem.toggle();
  assert.equal(bcItem.getValue(), 2);
  bcItem.toggle();
  assert.equal(bcItem.getValue(), 3);

  assert.ok(mockState.username);
  assert.ok(mockState.modelPerf.Carbon && mockState.modelPerf.Hydro);
  assert.ok(Array.isArray(mockState.childModelSpeeds) && mockState.childModelSpeeds.length > 0);
  assert.ok(mockState.costBreakdown && mockState.costBreakdown.totalCostUsd > 0);
  assert.ok(Number.isFinite(mockAgyData.terminal_width) && mockAgyData.terminal_width > 0);
  assert.ok(mockQuotaData.some(q => q.id === 'gemini-3.1-flash-image'));
});

test('wizard buildConfigForScopeSave prevents cross-contamination between Global and Local configs', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-wizard-scope-'));
  const globalPath = getGlobalConfigPath();
  const globalBackup = fs.existsSync(globalPath) ? fs.readFileSync(globalPath, 'utf8') : null;
  try {
    const workspace = path.join(tmp, 'workspace');
    fs.mkdirSync(workspace, { recursive: true });
    fs.writeFileSync(path.join(workspace, 'agy-pulse.config.json'), JSON.stringify({
      display: { showUsername: true },
    }));
    fs.writeFileSync(globalPath, JSON.stringify({
      language: 'zh',
      display: { showCost: true },
    }));

    // 1) Local workspace -> toggle showModel -> switch Scope to Global -> must NOT leak showUsername into global
    const stateToGlobal = createWizardState({
      language: 'zh',
      display: { showCost: true, showUsername: true },
    }, { cwd: workspace, localExists: true });
    const itemsA = buildMenuItems(stateToGlobal);
    itemsA.find(i => i.key === 'showModel').toggle();
    itemsA.find(i => i.key === 'scope').toggle(); // switch Local -> Global
    assert.equal(stateToGlobal.targetIsGlobal, true);

    const savedGlobal = buildConfigForScopeSave(stateToGlobal);
    assert.equal(savedGlobal.language, 'zh');
    assert.equal(savedGlobal.display.showCost, true);
    assert.equal(savedGlobal.display.showModel, true);
    assert.equal(savedGlobal.display.showUsername, undefined, 'local showUsername must not leak into global');

    // 2) Global -> switch Scope to Local -> toggle showDailyCost -> must NOT dump global keys into local
    const stateToLocal = createWizardState({
      language: 'zh',
      display: { showCost: true },
    }, { cwd: path.join(tmp, 'clean-ws'), localExists: false });
    const itemsB = buildMenuItems(stateToLocal);
    itemsB.find(i => i.key === 'scope').toggle(); // switch Global -> Local
    itemsB.find(i => i.key === 'showDailyCost').toggle();
    const savedLocal = buildConfigForScopeSave(stateToLocal);
    assert.equal(savedLocal.display.showDailyCost, true);
    assert.equal(savedLocal.language, undefined, 'global language must not solidify into local');
    assert.equal(savedLocal.display.showCost, undefined, 'global showCost must not solidify into local');
  } finally {
    if (globalBackup !== null) fs.writeFileSync(globalPath, globalBackup, 'utf8');
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('computeMenuViewport slices menu around selectedIdx for 24-row terminals', () => {
  const topView = computeMenuViewport(23, 0, 24, 6);
  assert.equal(topView.startIdx, 0);
  assert.equal(topView.hasAbove, false);
  assert.equal(topView.hasBelow, true);
  assert.ok(topView.endIdx < 23);

  const midView = computeMenuViewport(23, 12, 24, 6);
  assert.equal(midView.hasAbove, true);
  assert.equal(midView.hasBelow, true);
  assert.ok(12 >= midView.startIdx && 12 < midView.endIdx);

  const bottomView = computeMenuViewport(23, 22, 24, 6);
  assert.equal(bottomView.hasAbove, true);
  assert.equal(bottomView.hasBelow, false);
  assert.equal(bottomView.endIdx, 23);
  assert.ok(22 >= bottomView.startIdx && 22 < bottomView.endIdx);

  const fullView = computeMenuViewport(23, 5, 40, 6);
  assert.equal(fullView.startIdx, 0);
  assert.equal(fullView.endIdx, 23);
  assert.equal(fullView.hasAbove, false);
  assert.equal(fullView.hasBelow, false);
});

test('wizard includes showCycleMode default true, populates mockAgyData.cycle_mode, and reflects toggle in preview & save', () => {
  assert.equal(mockAgyData.cycle_mode, 'auto');

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-wizard-cycle-mode-'));
  const prevGlobalDir = process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
  process.env.AGY_PULSE_GLOBAL_CONFIG_DIR = tmp;
  try {
    const workspace = path.join(tmp, 'workspace');
    fs.mkdirSync(workspace, { recursive: true });
    const state = createWizardState({ display: {} }, { cwd: workspace, localExists: true });
    assert.equal(state.config.display.showCycleMode, true);

    const items = buildMenuItems(state);
    const cycleItem = items.find(i => i.key === 'showCycleMode');
    assert.ok(cycleItem, 'showCycleMode menu item must exist');
    assert.equal(cycleItem.getValue(), 'Enabled');

    const initialPreview = buildPreviewConfig(state, items);
    assert.equal(initialPreview.display.showCycleMode, true);

    cycleItem.toggle();
    assert.equal(cycleItem.getValue(), 'Disabled');
    assert.equal(state.config.display.showCycleMode, false);

    const updatedPreview = buildPreviewConfig(state, items);
    assert.equal(updatedPreview.display.showCycleMode, false);

    const savedConfig = buildConfigForScopeSave(state);
    assert.equal(savedConfig.display.showCycleMode, false);
  } finally {
    if (prevGlobalDir === undefined) delete process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
    else process.env.AGY_PULSE_GLOBAL_CONFIG_DIR = prevGlobalDir;
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
