import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { buildCmdShimContents } = require('../../runtime/statusline-installer.js');

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, '..', '..');

function readJson(relativePath) {
  return JSON.parse(fs.readFileSync(path.join(projectRoot, relativePath), 'utf8'));
}

function readText(relativePath) {
  return fs.readFileSync(path.join(projectRoot, relativePath), 'utf8');
}

function extractConfigurationJson(readme, headingPattern) {
  const normalizedReadme = readme.replace(/\r\n/g, '\n');
  const match = normalizedReadme.match(new RegExp(`${headingPattern}[\\s\\S]*?\`\`\`json\\n([\\s\\S]*?)\\n\`\`\``));
  assert.ok(match, 'README must include a JSON configuration example');
  return JSON.parse(match[1]);
}

test('release manifests keep the same package version', () => {
  const packageJson = readJson('package.json');
  const packageLock = readJson('package-lock.json');
  const geminiExtension = readJson('gemini-extension.json');

  assert.equal(geminiExtension.version, packageJson.version);
  assert.equal(packageLock.version, packageJson.version);
  assert.equal(packageLock.packages[''].version, packageJson.version);
});

test('README configuration examples match supported runtime fields', () => {
  const configs = [
    extractConfigurationJson(readText('README.md'), '## Configuration'),
    extractConfigurationJson(readText('README_zh.md'), '## 配置'),
  ];

  for (const config of configs) {
    assert.equal(config.display.unicode, undefined);
    assert.equal(config.display.nerdFont, undefined);
    assert.equal(typeof config.display.useNerdFonts, 'boolean');
    assert.equal(config.thresholds.warning, 0.7);
    assert.equal(config.thresholds.critical, 0.9);
  }
});

test('CI uses reproducible install and scheduled agy drift checks', () => {
  const workflow = readText('.github/workflows/e2e.yml');

  assert.match(workflow, /^\s+schedule:/m);
  assert.match(workflow, /FORCE_JAVASCRIPT_ACTIONS_TO_NODE24:\s*'true'/);
  assert.match(workflow, /actions\/checkout@v6/);
  assert.match(workflow, /actions\/setup-node@v6/);
  assert.match(workflow, /actions\/upload-artifact@v7/);
  assert.doesNotMatch(workflow, /actions\/(?:checkout|setup-node|upload-artifact)@v4/);
  assert.match(workflow, /npm ci/);
  assert.doesNotMatch(workflow, /npm install/);
});

test('CI E2E detection follows the status line layout without a banner', () => {
  const releaseScript = readText('release.sh');
  const workflow = readText('.github/workflows/e2e.yml');
  const verifyDisplay = readText('scripts/verify-display.js');

  assert.doesNotMatch(releaseScript, /assert AGY-PULSE/);
  assert.doesNotMatch(releaseScript, /AGY-PULSE rendered/);
  assert.doesNotMatch(workflow, /AGY-PULSE banner/);
  assert.doesNotMatch(verifyDisplay, /AGY-PULSE banner/);
  assert.doesNotMatch(verifyDisplay, /literal AGY-PULSE/);
  assert.doesNotMatch(verifyDisplay, /includes\('AGY-PULSE'\)/);
  assert.doesNotMatch(verifyDisplay, /\/AGY-PULSE\//);
  assert.match(verifyDisplay, /detectStatusLineRender/);
});

test('configure-utf8 script fails loudly and is covered by Windows CI', () => {
  const script = readText('scripts/configure-utf8.ps1');
  const workflow = readText('.github/workflows/e2e.yml');

  assert.match(script, /\$ErrorActionPreference\s*=\s*'Stop'/);
  assert.match(script, /function Invoke-CheckedCommand/);
  assert.match(script, /Get-Command git/);
  assert.match(script, /\$env:AGY_PULSE_PROFILE_PATH/);
  assert.match(script, /\$LASTEXITCODE/);

  assert.match(workflow, /Test configure-utf8 script \(Windows\)/);
  assert.match(workflow, /GIT_CONFIG_GLOBAL/);
  assert.match(workflow, /AGY_PULSE_PROFILE_PATH/);
});

test('release script prints valid GitHub release install URLs', () => {
  const releaseScript = readText('release.sh');

  assert.match(releaseScript, /replace\(\/\\\.git\$\/,\s*''\)/);
  assert.doesNotMatch(releaseScript, /replace\(\/\\\.git\\\$\/,\s*''\)/);
});

test('Windows installer fails loudly when native plugin install fails', () => {
  const installScript = readText('scripts/install.ps1');
  const installIndex = installScript.indexOf('agy plugin install $RepoUrl');
  const bootstrapIndex = installScript.indexOf('$BootstrapUrl');

  assert.notEqual(installIndex, -1);
  assert.notEqual(bootstrapIndex, -1);
  assert.match(
    installScript.slice(installIndex, bootstrapIndex),
    /\$LASTEXITCODE\s+-ne\s+0/
  );
});

test('Windows uninstaller falls back when plugin uninstall fails', () => {
  const uninstallScript = readText('uninstall.ps1');

  assert.match(uninstallScript, /\$PluginDir\s*=/);
  assert.match(uninstallScript, /Remove-Item\s+\$PluginDir/);
  assert.match(uninstallScript, /\$LASTEXITCODE\s+-ne\s+0/);
});

test('release package contract ships agent skills and docs', () => {
  const packageJson = readJson('package.json');
  const releaseScript = readText('release.sh');

  assert.deepEqual(packageJson.files, [
    'runtime',
    'scripts',
    'skills',
    'plugin.json',
    'gemini-extension.json',
    'README.md',
    'README_zh.md',
    'LICENSE',
    'uninstall.sh',
    'uninstall.ps1',
    'package.json',
  ]);
  assert.match(releaseScript, /skills/);
  assert.ok(
    fs.existsSync(path.join(projectRoot, 'LICENSE')),
    'LICENSE must exist at repo root'
  );
  assert.ok(
    fs.existsSync(path.join(projectRoot, 'skills', 'pulse-config', 'SKILL.md')),
    'skills/pulse-config/SKILL.md must exist so the agent can load the config skill'
  );
  assert.equal(
    fs.existsSync(path.join(projectRoot, 'skills', 'hud-config')),
    false,
    'legacy skills/hud-config directory must be removed'
  );
});

test('runtime/bin/agy-pulse.cmd stays aligned with buildCmdShimContents', () => {
  const cmdContents = readText('runtime/bin/agy-pulse.cmd');
  assert.equal(cmdContents, buildCmdShimContents('agy-pulse.js'));
});

test('docs/README.md relative links all resolve to existing files', () => {
  const docsDir = path.join(projectRoot, 'docs');
  const docsReadme = readText('docs/README.md');
  const linkMatches = [...docsReadme.matchAll(/\[[^\]]+\]\(([^)#?\s]+)(?:#[^)]*)?\)/g)];
  assert.ok(linkMatches.length > 0, 'docs/README.md should contain relative documentation links');
  for (const match of linkMatches) {
    const target = match[1];
    if (/^[a-z]+:\/\//i.test(target)) continue;
    const resolved = path.resolve(docsDir, target);
    assert.ok(fs.existsSync(resolved), `Broken relative link in docs/README.md: ${target}`);
  }
});

test('package does not keep obsolete local HTTP server helpers', () => {
  const packageJson = readJson('package.json');

  assert.equal(packageJson.scripts['serve:release'], undefined);
  assert.equal(packageJson.scripts['serve:source'], undefined);
  assert.equal(fs.existsSync(path.join(projectRoot, 'scripts', 'serve-release.js')), false);
  assert.equal(fs.existsSync(path.join(projectRoot, 'scripts', 'serve-source.js')), false);
});

test('agy-pulse brand manifests and default runtime config stay aligned', () => {
  const packageJson = readJson('package.json');
  const pluginJson = readJson('plugin.json');
  const defaultConfig = readJson('runtime/agy-pulse.config.json');

  assert.equal(packageJson.name, 'agy-pulse');
  assert.equal(packageJson.version, '1.0.0');
  assert.equal(pluginJson.name, 'agy-pulse');
  assert.equal(defaultConfig.display.showContextMonitor, true);
  assert.equal(defaultConfig.display.showModelCache, true);
  assert.equal(defaultConfig.display.unicode, undefined);
});

test('pulse-config skill documents none quotaStyle and deploy-local avoids rm -f', () => {
  const skill = readText('skills/pulse-config/SKILL.md');
  const deployLocal = readText('scripts/deploy-local.sh');

  assert.match(skill, /"none"/);
  assert.match(skill, /--diagnose/);
  assert.match(skill, /--stats/);
  assert.match(skill, /--dashboard/);
  assert.match(skill, /--help/);
  assert.doesNotMatch(deployLocal, /\brm\s+-r?f\b/);
  assert.match(deployLocal, /diagnose-auth\.js/);
  assert.match(deployLocal, /ledger\.js/);
  assert.match(deployLocal, /stats\.js/);
  assert.match(deployLocal, /dashboard\.js/);
  assert.match(deployLocal, /dashboard-ui\.js/);
});

test('uninstall preserves permanent telemetry ledgers by default and purges them with purgeLedger', () => {
  const { uninstall } = require('../../runtime/uninstall.js');
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-contract-uninstall-'));
  try {
    const home = path.join(tmp, 'home');
    const root = path.join(home, '.gemini', 'antigravity-cli');
    const runtimeDir = path.join(root, 'agy-pulse-runtime');
    const settingsPath = path.join(root, 'settings.json');
    const sqliteLedger = path.join(root, 'agy-pulse-ledger.sqlite');
    const sqliteWal = path.join(root, 'agy-pulse-ledger.sqlite-wal');
    const jsonLedger = path.join(root, 'agy-pulse-ledger.json');

    fs.mkdirSync(runtimeDir, { recursive: true });
    fs.writeFileSync(settingsPath, JSON.stringify({}));
    fs.writeFileSync(sqliteLedger, 'sqlite-bytes');
    fs.writeFileSync(sqliteWal, 'wal-bytes');
    fs.writeFileSync(jsonLedger, '{}');

    const preservedRes = uninstall({
      homeDir: home,
      env: { HOME: home, USERPROFILE: home },
      settingsPath,
      tmpDir: path.join(tmp, 'tmp'),
    });

    assert.equal(fs.existsSync(sqliteLedger), true);
    assert.equal(fs.existsSync(jsonLedger), true);
    assert.ok(preservedRes.preservedLedgers.includes(sqliteLedger));
    assert.ok(preservedRes.preservedLedgers.includes(jsonLedger));
    assert.deepEqual(preservedRes.purgedLedgers, []);

    const purgedRes = uninstall({
      homeDir: home,
      env: { HOME: home, USERPROFILE: home },
      settingsPath,
      tmpDir: path.join(tmp, 'tmp'),
      purgeLedger: true,
    });

    assert.equal(fs.existsSync(sqliteLedger), false);
    assert.equal(fs.existsSync(sqliteWal), false);
    assert.equal(fs.existsSync(jsonLedger), false);
    assert.ok(purgedRes.purgedLedgers.includes(sqliteLedger));
    assert.ok(purgedRes.purgedLedgers.includes(jsonLedger));
    assert.deepEqual(purgedRes.preservedLedgers, []);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
