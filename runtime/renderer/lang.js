'use strict';

const PROVIDER_LABELS = {
  MODEL_PROVIDER_GOOGLE: 'Google',
  MODEL_PROVIDER_ANTHROPIC: 'Anthropic',
  MODEL_PROVIDER_OPENAI: 'OpenAI',
};

const LANGUAGE_TEXT = {
  en: {
    cacheLabel: 'cache',
    costLabel: 'Cost',
    todayLabel: 'Today',
    weekLabel: 'Week',
    outLabel: 'out',
    tokPerSec: 'tok/s',
    ttftLabel: 'ttft',
    ctxLabel: 'Ctx',
    limitLabel: 'limit',
    compactedLabel: 'Compacted',
    peakLabel: 'peak',
    cacheLabelMonitor: 'Cache',
    modelCacheLabel: 'Session Cache',
    cacheHitLabel: 'hit',
    cacheWriteLabel: 'write',
    cacheInLabel: 'in',
    outLabelMonitor: 'Out',
    costLabelMonitor: 'Cost',
    compactionImminent: 'Compaction imminent',
    quotaUnavailable: 'Quota unavailable',
    quotaLoading: 'Quota loading',
    quotaReasons: {
      not_logged_in: 'not logged into Antigravity',
      expired_token: 'Antigravity token expired',
      auth_failed: 'Antigravity auth failed',
      quota_fetch_failed: 'quota fetch failed',
    },
  },
  zh: {
    cacheLabel: 'cache',
    costLabel: '费用',
    todayLabel: '今日',
    weekLabel: '本周',
    outLabel: '输出',
    tokPerSec: 'tok/s',
    ttftLabel: '首字',
    ctxLabel: '上下文',
    limitLabel: '上限',
    compactedLabel: '压缩',
    peakLabel: '峰',
    cacheLabelMonitor: '缓存',
    modelCacheLabel: '累计缓存',
    cacheHitLabel: '命中',
    cacheWriteLabel: '写入',
    cacheInLabel: '总入',
    outLabelMonitor: '输出',
    costLabelMonitor: '费用',
    compactionImminent: '即将触发压缩',
    quotaUnavailable: '额度不可用',
    quotaLoading: '额度加载中',
    quotaReasons: {
      not_logged_in: '未登录 Antigravity',
      expired_token: 'Antigravity token 已过期',
      auth_failed: 'Antigravity 认证失败',
      quota_fetch_failed: '额度获取失败',
    },
  },
};

function resolveLanguage(config, env = process.env) {
  const language = config?.language;
  if (language === 'en' || language === 'zh') return language;
  const locale = env.LC_ALL || env.LC_CTYPE || env.LANG || '';
  return /^zh(?:_|-|$)/i.test(locale) ? 'zh' : 'en';
}

module.exports = {
  PROVIDER_LABELS,
  LANGUAGE_TEXT,
  resolveLanguage,
};
