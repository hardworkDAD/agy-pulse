import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
  parseSemver,
  compareSemver,
  isNewerVersion,
  resolveUpdateUrl,
  fetchRemoteVersion,
  checkForUpdates,
} = require('../../runtime/update-checker.js');

test('compareSemver and isNewerVersion prevent downgrade notifications and follow SemVer 2.0 prerelease rules', () => {
  assert.deepEqual(parseSemver('v1.2.3'), { major: 1, minor: 2, patch: 3, prerelease: '' });
  assert.equal(compareSemver('1.1.0', '1.0.9'), 1);
  assert.equal(compareSemver('1.0.0', '1.0.0'), 0);
  assert.equal(compareSemver('0.3.3', '1.0.0'), -1);
  assert.equal(compareSemver('1.0.0-beta.10', '1.0.0-beta.2'), 1);
  assert.equal(compareSemver('1.0.0-1', '1.0.0-alpha'), -1);
  assert.equal(compareSemver('1.0.0-alpha', '1.0.0-alpha.1'), -1);

  assert.equal(isNewerVersion('1.0.1', '1.0.0'), true);
  assert.equal(isNewerVersion('1.0.0', '1.0.0'), false);
  assert.equal(isNewerVersion('0.3.3', '1.0.0'), false);
  assert.equal(isNewerVersion('1.0.0', '1.0.0-beta.1'), true);
  assert.equal(isNewerVersion('1.0.0-beta.10', '1.0.0-beta.2'), true);
});

test('resolveUpdateUrl returns null when no own repository or override is configured', () => {
  assert.equal(resolveUpdateUrl({ repository: '' }, {}), null);
  assert.equal(resolveUpdateUrl({}, {}), null);
  assert.equal(
    resolveUpdateUrl({}, { AGY_PULSE_REPO: 'custom-user/agy-pulse' }),
    'https://raw.githubusercontent.com/custom-user/agy-pulse/main/package.json'
  );
  assert.equal(
    resolveUpdateUrl({}, { AGY_PULSE_UPDATE_URL: 'https://example.com/pkg.json' }),
    'https://example.com/pkg.json'
  );
});

test('checkForUpdates writes no_remote_repo without fetching when repository is unconfigured', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-update-none-'));
  try {
    const pkgPath = path.join(tmp, 'package.json');
    const statusPath = path.join(tmp, 'status.json');
    fs.writeFileSync(pkgPath, JSON.stringify({ name: 'agy-pulse', version: '1.0.0' }));

    let fetched = false;
    const status = await checkForUpdates({
      localPkgPath: pkgPath,
      statusFile: statusPath,
      config: {},
      env: {},
      now: 123456,
      fetchVersion: async () => {
        fetched = true;
        return '9.9.9';
      },
    });

    assert.equal(fetched, false);
    assert.equal(status.updateAvailable, false);
    assert.equal(status.reason, 'no_remote_repo');
    assert.equal(status.lastCheck, 123456);
    assert.equal(JSON.parse(fs.readFileSync(statusPath, 'utf8')).reason, 'no_remote_repo');
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('checkForUpdates detects newer remote version and handles fetch errors cleanly', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-update-ok-'));
  try {
    const pkgPath = path.join(tmp, 'package.json');
    const statusPath = path.join(tmp, 'status.json');
    fs.writeFileSync(pkgPath, JSON.stringify({ name: 'agy-pulse', version: '1.0.0' }));

    const upgraded = await checkForUpdates({
      localPkgPath: pkgPath,
      statusFile: statusPath,
      config: { repository: 'owner/agy-pulse' },
      env: {},
      now: 1000,
      fetchVersion: async () => '1.2.0',
    });
    assert.equal(upgraded.updateAvailable, true);
    assert.equal(upgraded.latestVersion, '1.2.0');

    const downgraded = await checkForUpdates({
      localPkgPath: pkgPath,
      statusFile: statusPath,
      config: { repository: 'owner/agy-pulse' },
      env: {},
      now: 2000,
      fetchVersion: async () => '0.9.0',
    });
    assert.equal(downgraded.updateAvailable, false);

    const errored = await checkForUpdates({
      localPkgPath: pkgPath,
      statusFile: statusPath,
      config: { repository: 'owner/agy-pulse' },
      env: {},
      now: 3000,
      fetchVersion: async () => {
        throw new Error('network down');
      },
    });
    assert.equal(errored.lastCheck, 3000);
    assert.equal(JSON.parse(fs.readFileSync(statusPath, 'utf8')).lastCheck, 3000);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('fetchRemoteVersion follows http redirects up to 5 hops', async () => {
  const server = http.createServer((req, res) => {
    if (req.url === '/redirect-1') {
      res.writeHead(302, { Location: '/redirect-2' });
      res.end();
      return;
    }
    if (req.url === '/redirect-2') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ version: '2.1.0' }));
      return;
    }
    res.writeHead(404);
    res.end();
  });

  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  try {
    const port = server.address().port;
    const version = await fetchRemoteVersion(`http://127.0.0.1:${port}/redirect-1`);
    assert.equal(version, '2.1.0');
  } finally {
    await new Promise(resolve => server.close(resolve));
  }
});

test('checkForUpdates loads workspace config using options.cwd', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-update-cwd-'));
  try {
    const pkgPath = path.join(tmp, 'package.json');
    const statusPath = path.join(tmp, 'status.json');
    const workspaceDir = path.join(tmp, 'workspace');
    fs.mkdirSync(workspaceDir, { recursive: true });
    fs.writeFileSync(pkgPath, JSON.stringify({ name: 'agy-pulse', version: '1.0.0' }));
    fs.writeFileSync(path.join(workspaceDir, 'agy-pulse.config.json'), JSON.stringify({
      repository: 'workspace-owner/agy-pulse',
    }));

    let requestedUrl = null;
    const status = await checkForUpdates({
      cwd: workspaceDir,
      localPkgPath: pkgPath,
      statusFile: statusPath,
      env: {},
      now: 5000,
      fetchVersion: async (url) => {
        requestedUrl = url;
        return '1.3.0';
      },
    });

    assert.equal(requestedUrl, 'https://raw.githubusercontent.com/workspace-owner/agy-pulse/main/package.json');
    assert.equal(status.updateAvailable, true);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('checkForUpdates clears stale updateAvailable on fetch error when localVersion >= latestVersion', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-pulse-update-stale-'));
  try {
    const pkgPath = path.join(tmp, 'package.json');
    const statusPath = path.join(tmp, 'status.json');
    fs.writeFileSync(pkgPath, JSON.stringify({ name: 'agy-pulse', version: '1.0.0' }));
    fs.writeFileSync(statusPath, JSON.stringify({
      lastCheck: 100,
      updateAvailable: true,
      latestVersion: '1.0.0',
      localVersion: '0.9.0',
    }));

    const result = await checkForUpdates({
      localPkgPath: pkgPath,
      statusFile: statusPath,
      config: { repository: 'owner/agy-pulse' },
      env: {},
      now: 9000,
      fetchVersion: async () => {
        throw new Error('offline');
      },
    });

    assert.equal(result.lastCheck, 9000);
    assert.equal(result.localVersion, '1.0.0');
    assert.equal(result.updateAvailable, false);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});
