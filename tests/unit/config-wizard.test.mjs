import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
  createWizardState,
  buildMenuItems,
  handleWizardKeypress,
} = require('../../runtime/config-wizard.js');

test('wizard preserves quotaStyle=models and cycles table -> compact -> models', () => {
  const state = createWizardState({ display: { quotaStyle: 'models' } }, { localExists: false });
  const items = buildMenuItems(state);
  const quotaItem = items.find(i => i.key === 'quotaStyle');

  assert.equal(quotaItem.getValue(), 'Models');
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
