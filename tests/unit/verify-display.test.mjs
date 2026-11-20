import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { detectStatusLineRender, renderTerminalScreen, stripAnsi } = require('../../scripts/verify-display.js');

test('detectStatusLineRender accepts the current Unicode status line without a banner', () => {
  // Matches current renderer format: ⚿ Tokens X (in: Y, out: Z)
  const statusLine = [
    '\x1b[34m⎇ HEAD\x1b[0m \x1b[90m│\x1b[0m \x1b[32mUnknown Model\x1b[0m \x1b[90m│\x1b[0m \x1b[35mFree\x1b[0m',
    '\x1b[36m⚿ Tokens 0 \x1b[90m(\x1b[0min: 0, out: 0\x1b[90m)\x1b[0m \x1b[90m│\x1b[0m \x1b[36m⛁ 0/0\x1b[0m \x1b[36m[░░░░░░░░░░]\x1b[0m \x1b[36m0%\x1b[0m',
    '  Quota unavailable: not logged into Antigravity',
  ].join('\n');

  assert.equal(statusLine.includes('AGY-PULSE'), false);
  assert.equal(detectStatusLineRender(statusLine, renderTerminalScreen(statusLine), stripAnsi(statusLine)), true);
});

test('detectStatusLineRender accepts the ASCII fallback status line', () => {
  // ASCII mode: tokenIcon '[Tk] ' triggers bare 'Tokens' prefix (no [Tk] in output)
  const statusLine = [
    '[B] fix/audit-findings | Unknown Model | Google AI Pro',
    'Tokens 0 (in: 0, out: 0) | [C] 0/0 [----------] 0%',
    'Quota loading...',
  ].join('\n');

  assert.equal(detectStatusLineRender(statusLine), true);
});

test('detectStatusLineRender rejects empty and unrelated command output', () => {
  assert.equal(detectStatusLineRender('', 'random output', 'Usage of agy:'), false);
  assert.equal(detectStatusLineRender('[B] main only has a branch marker'), false);
});

test('detectStatusLineRender accepts unified agy-pulse status lines with Unicode and ASCII context monitors', () => {
  const unicodePulse = [
    'agy-pulse │ Free',
    '  Quota unavailable: not logged into Antigravity',
    '◈ Unknown Model · Ctx ············ 0.0% (limit 512K)',
  ].join('\n');
  const asciiPulse = [
    'agy-pulse | Free',
    '  配额不可用: 未登录 Antigravity',
    '* Gemini 4 Carbon (H) · 上下文 ######...... 50.0% (256K/512K) · 输出 1.2K',
  ].join('\n');

  assert.equal(detectStatusLineRender(unicodePulse), true);
  assert.equal(detectStatusLineRender(asciiPulse), true);
});
