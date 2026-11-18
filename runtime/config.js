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
    showCurrentDir: true,
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

function getGlobalConfigPath() {
  for (const name of CONFIG_FILENAMES) {
    const candidate = path.join(__dirname, name);
    if (fs.existsSync(candidate)) return candidate;
  }
  return path.join(__dirname, CONFIG_FILENAMES[1]);
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
  for (const name of CONFIG_FILENAMES) {
    const globalCandidate = path.join(__dirname, name);
    if (fs.existsSync(globalCandidate)) return globalCandidate;
  }
  return null;
}

function mergeConfig(base, override) {
  const safeBase = base && typeof base === 'object' ? base : {};
  const safeOverride = override && typeof override === 'object' ? override : {};
  return {
    ...safeBase,
    ...safeOverride,
    theme: typeof safeOverride.theme === 'string'
      ? safeOverride.theme
      : { ...(safeBase.theme || {}), ...(safeOverride.theme && typeof safeOverride.theme === 'object' ? safeOverride.theme : {}) },
    display: {
      ...(safeBase.display || {}),
      ...(safeOverride.display && typeof safeOverride.display === 'object' ? safeOverride.display : {}),
    },
    thresholds: {
      ...(safeBase.thresholds || {}),
      ...(safeOverride.thresholds && typeof safeOverride.thresholds === 'object' ? safeOverride.thresholds : {}),
    },
  };
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
    fs.writeFileSync(tmpPath, JSON.stringify(config, null, 2), 'utf8');
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
  loadConfig,
  getLocalConfigPath,
  getGlobalConfigPath,
  saveConfig,
};
