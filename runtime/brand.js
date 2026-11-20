'use strict';

const BRAND_NAME = 'agy-pulse';
const PACKAGE_VERSION = '1.0.0';
const CONFIG_FILENAMES = ['agy-pulse.config.json'];
const RUNTIME_DIR_NAME = 'agy-pulse-runtime';

function extractRepoSlug(rawValue) {
  if (!rawValue) return null;
  let candidate = rawValue;
  if (typeof rawValue === 'object') {
    candidate = rawValue.url || rawValue.slug || '';
  }
  if (typeof candidate !== 'string') return null;

  const trimmed = candidate.trim();
  if (!trimmed) return null;

  const withoutGitPrefix = trimmed.replace(/^git\+/, '').replace(/\.git$/i, '');
  const githubMatch = withoutGitPrefix.match(/(?:https?:\/\/|ssh:\/\/git@|git@)github\.com[:/]([^/\s]+\/[^/\s#?]+)/i);
  if (githubMatch) {
    return githubMatch[1].replace(/\/+$/, '');
  }

  if (/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(withoutGitPrefix)) {
    return withoutGitPrefix;
  }

  return null;
}

/**
 * Resolve the configured GitHub `owner/repo` slug without falling back to the
 * original upstream repository.
 *
 * @param {object|null|undefined} config
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {string|null}
 */
function resolveRepoSlug(config, env = process.env) {
  const safeEnv = env || process.env;
  const envCandidate = safeEnv.AGY_PULSE_REPO || '';
  const fromEnv = extractRepoSlug(envCandidate);
  if (fromEnv) return fromEnv;

  const fromConfig = extractRepoSlug(config?.repository);
  if (fromConfig) return fromConfig;

  return null;
}

/**
 * Build a raw.githubusercontent.com URL for a given repository slug.
 *
 * @param {string|null|undefined} repoSlug
 * @param {string} [branch]
 * @param {string} [filePath]
 * @returns {string|null}
 */
function getRawGitHubUrl(repoSlug, branch = 'main', filePath = 'package.json') {
  const slug = extractRepoSlug(repoSlug);
  if (!slug) return null;
  const cleanBranch = String(branch || 'main').replace(/^\/+|\/+$/g, '') || 'main';
  const cleanFilePath = String(filePath || 'package.json').replace(/^\/+/, '');
  return `https://raw.githubusercontent.com/${slug}/${cleanBranch}/${cleanFilePath}`;
}

module.exports = {
  BRAND_NAME,
  PACKAGE_VERSION,
  CONFIG_FILENAMES,
  RUNTIME_DIR_NAME,
  resolveRepoSlug,
  getRawGitHubUrl,
};
