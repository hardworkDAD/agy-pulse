'use strict';

const fs = require('fs');
const path = require('path');
const { CONFIG_FILENAMES } = require('./brand.js');

const FULL_DEFAULT_CONFIG = {
  enabled: true,
  theme: {
    primary: 'green',
    secondary: 'gray',
    warning: 'yellow',
    critical: 'red',
  },
  display: {
    showContextMonitor: true,
    showModelCache: true,
    showCurrentDir: true,
    showCycleMode: true,
    showSpeed: true,
    showModel: false,
    showTokenBar: false,
    showContextBar: false,
    showTurnCacheHitRate: false,
    showBreadcrumbs: false,
    showGitBranch: false,
    showUsername: false,
    showCost: false,
    showDailyCost: false,
    showWeeklyCost: false,
    breadcrumbCount: 3,
    useNerdFonts: false,
  },
  thresholds: {
    warning: 0.7,
    critical: 0.9,
  },
  language: 'auto',
};

function getLocalConfigPath(cwd = process.cwd()) {
  for (const name of CONFIG_FILENAMES) {
    const candidate = path.join(cwd, name);
    if (fs.existsSync(candidate)) return candidate;
  }
  return path.join(cwd, CONFIG_FILENAMES[0]);
}

function getGlobalConfigDir() {
  const override = process.env.AGY_PULSE_GLOBAL_CONFIG_DIR;
  return typeof override === 'string' && override.trim() ? override.trim() : __dirname;
}

function getGlobalConfigPath() {
  const globalDir = getGlobalConfigDir();
  for (const name of CONFIG_FILENAMES) {
    const candidate = path.join(globalDir, name);
    if (fs.existsSync(candidate)) return candidate;
  }
  return path.join(globalDir, CONFIG_FILENAMES[0]);
}

function readJsonFile(filePath) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch {
    return null;
  }
}

function findExistingConfigPath(cwd = process.cwd()) {
  for (const name of CONFIG_FILENAMES) {
    const localCandidate = path.join(cwd, name);
    if (fs.existsSync(localCandidate)) return localCandidate;
  }
  const globalDir = getGlobalConfigDir();
  for (const name of CONFIG_FILENAMES) {
    const globalCandidate = path.join(globalDir, name);
    if (fs.existsSync(globalCandidate)) return globalCandidate;
  }
  return null;
}

function isPlainObject(value) {
  return Boolean(value && typeof value === 'object' && !Array.isArray(value));
}

function mergeConfig(base, override) {
  const safeBase = isPlainObject(base) ? base : {};
  const safeOverride = isPlainObject(override) ? override : {};
  const baseDisplay = isPlainObject(safeBase.display) ? safeBase.display : {};
  const overrideDisplay = isPlainObject(safeOverride.display) ? safeOverride.display : {};
  const mergedDisplay = {
    ...baseDisplay,
    ...overrideDisplay,
  };
  if (isPlainObject(baseDisplay.cacheHitThresholds) || isPlainObject(overrideDisplay.cacheHitThresholds)) {
    mergedDisplay.cacheHitThresholds = {
      ...(isPlainObject(baseDisplay.cacheHitThresholds) ? baseDisplay.cacheHitThresholds : {}),
      ...(isPlainObject(overrideDisplay.cacheHitThresholds) ? overrideDisplay.cacheHitThresholds : {}),
    };
  }
  const merged = {
    ...safeBase,
    ...safeOverride,
    theme: typeof safeOverride.theme === 'string'
      ? safeOverride.theme
      : { ...(isPlainObject(safeBase.theme) ? safeBase.theme : {}), ...(isPlainObject(safeOverride.theme) ? safeOverride.theme : {}) },
    display: mergedDisplay,
    thresholds: {
      ...(isPlainObject(safeBase.thresholds) ? safeBase.thresholds : {}),
      ...(isPlainObject(safeOverride.thresholds) ? safeOverride.thresholds : {}),
    },
  };
  if (isPlainObject(safeBase.icons) || isPlainObject(safeOverride.icons)) {
    merged.icons = {
      ...(isPlainObject(safeBase.icons) ? safeBase.icons : {}),
      ...(isPlainObject(safeOverride.icons) ? safeOverride.icons : {}),
    };
  }
  return merged;
}

async function loadConfig(cwd = process.cwd()) {
  const globalConfigPath = getGlobalConfigPath();
  const bundledConfig = fs.existsSync(globalConfigPath) ? readJsonFile(globalConfigPath) : null;
  const resolvedPath = findExistingConfigPath(cwd);
  const fileConfig = resolvedPath && resolvedPath !== globalConfigPath ? readJsonFile(resolvedPath) : null;

  let merged = mergeConfig(FULL_DEFAULT_CONFIG, bundledConfig);
  if (fileConfig) {
    merged = mergeConfig(merged, fileConfig);
  }

  if (merged.enabled === undefined) {
    merged.enabled = true;
  }
  return merged;
}

async function saveConfig(config, isGlobal = true, cwd = process.cwd()) {
  const targetPath = isGlobal ? getGlobalConfigPath() : getLocalConfigPath(cwd);
  fs.mkdirSync(path.dirname(targetPath), { recursive: true });
  const tmpPath = `${targetPath}.tmp.${process.pid}.${Date.now()}`;
  try {
    fs.writeFileSync(tmpPath, JSON.stringify(config, null, 2), { encoding: 'utf8', mode: 0o600 });
    fs.renameSync(tmpPath, targetPath);
  } catch (err) {
    try {
      if (fs.existsSync(tmpPath)) fs.unlinkSync(tmpPath);
    } catch {}
    throw err;
  }
}

module.exports = {
  FULL_DEFAULT_CONFIG,
  mergeConfig,
  loadConfig,
  getLocalConfigPath,
  getGlobalConfigPath,
  saveConfig,
};
