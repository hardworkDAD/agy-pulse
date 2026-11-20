'use strict';

const {
  syncAllConversationsToLedger,
  queryLedger,
  formatLedgerCsv,
} = require('./ledger.js');
const { formatUsd } = require('./cost.js');
const { loadConfig } = require('./config.js');
const { resolveLanguage } = require('./renderer/lang.js');
const {
  formatTokens,
  getAnsiDisplayWidth,
  getDisplayWidth,
  truncateToDisplayWidth,
} = require('./renderer/format.js');

const VALID_RANGES = new Set(['today', '24h', '7d', '30d', 'month', 'all', 'custom']);
const POSITIONAL_RANGES = new Set(['today', '24h', '7d', '30d', 'month', 'all']);
const VALID_GRANULARITIES = new Set(['auto', 'hour', 'day', 'week']);
const VALID_FORMATS = new Set(['table', 'json', 'csv']);
const VALID_CSV_KINDS = new Set(['timeseries', 'models', 'sessions']);
const VALID_LANGS = new Set(['auto', 'en', 'zh']);
const VALID_SOURCES = new Set(['all', 'cli', 'ide']);

function pad2(n) {
  return String(n).padStart(2, '0');
}

function formatDateTimeShort(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '--';
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

function formatDateShort(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '--';
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}

/**
 * Parses CLI arguments for `agy-pulse --stats`.
 *
 * @param {string[]} [argv]
 * @returns {{
 *   range: string,
 *   from: string|null,
 *   to: string|null,
 *   granularity: string,
 *   source: 'all'|'cli'|'ide',
 *   model: string,
 *   workspace: string,
 *   includeSubagents: boolean,
 *   format: 'table'|'json'|'csv',
 *   csvKind: 'timeseries'|'models'|'sessions',
 *   lang: 'auto'|'en'|'zh',
 *   noSync: boolean,
 *   help: boolean
 * }}
 */
function parseStatsArgs(argv = []) {
  const args = Array.isArray(argv) ? argv : [];
  const result = {
    range: '7d',
    from: null,
    to: null,
    granularity: 'auto',
    source: 'all',
    model: 'all',
    workspace: 'all',
    includeSubagents: true,
    format: 'table',
    csvKind: 'timeseries',
    lang: 'auto',
    noSync: false,
    help: false,
  };
  let explicitRange = false;
  let startIdx = 0;
  while (startIdx < args.length && !String(args[startIdx] ?? '')) {
    startIdx += 1;
  }
  if (startIdx < args.length) {
    const first = String(args[startIdx]);
    if (first === 'stats' || first === 'report' || first === '--stats') {
      startIdx += 1;
    }
  }

  for (let i = startIdx; i < args.length; i += 1) {
    const raw = String(args[i] ?? '');
    if (!raw || raw === '--stats') continue;

    const eqIdx = raw.indexOf('=');
    const flag = eqIdx > 0 ? raw.slice(0, eqIdx) : raw;
    const inlineVal = eqIdx > 0 ? raw.slice(eqIdx + 1) : null;

    const consumeValue = () => {
      if (inlineVal !== null) return inlineVal;
      if (i + 1 < args.length && !String(args[i + 1]).startsWith('-')) {
        i += 1;
        return String(args[i]);
      }
      return null;
    };

    switch (flag) {
      case '--help':
      case '-h':
        result.help = true;
        break;
      case '--no-sync':
        result.noSync = true;
        break;
      case '--no-subagents':
        result.includeSubagents = false;
        break;
      case '--include-subagents':
        result.includeSubagents = true;
        break;
      case '--json':
        result.format = 'json';
        break;
      case '--csv': {
        result.format = 'csv';
        if (inlineVal && VALID_CSV_KINDS.has(inlineVal.toLowerCase())) {
          result.csvKind = inlineVal.toLowerCase();
        }
        break;
      }
      case '--csv-kind':
      case '--kind': {
        const val = consumeValue();
        if (val && VALID_CSV_KINDS.has(val.toLowerCase())) {
          result.csvKind = val.toLowerCase();
        }
        break;
      }
      case '--format': {
        const val = consumeValue();
        if (val && VALID_FORMATS.has(val.toLowerCase())) {
          result.format = val.toLowerCase();
        }
        break;
      }
      case '--range': {
        const val = consumeValue();
        if (val && VALID_RANGES.has(val.toLowerCase())) {
          result.range = val.toLowerCase();
          explicitRange = true;
        }
        break;
      }
      case '--today':
      case '--24h':
      case '--7d':
      case '--30d':
      case '--month':
      case '--all':
        result.range = flag.slice(2);
        explicitRange = true;
        break;
      case '--from': {
        const val = consumeValue();
        if (val) result.from = val;
        break;
      }
      case '--to': {
        const val = consumeValue();
        if (val) result.to = val;
        break;
      }
      case '--granularity':
      case '--by': {
        const val = consumeValue();
        if (val && VALID_GRANULARITIES.has(val.toLowerCase())) {
          result.granularity = val.toLowerCase();
        }
        break;
      }
      case '--model': {
        const val = consumeValue();
        if (val) result.model = val;
        break;
      }
      case '--source': {
        const val = consumeValue();
        if (val !== null) {
          const norm = String(val).trim().toLowerCase();
          result.source = VALID_SOURCES.has(norm) ? norm : 'all';
        }
        break;
      }
      case '--workspace':
      case '--ws': {
        const val = consumeValue();
        if (val) result.workspace = val;
        break;
      }
      case '--lang': {
        const val = consumeValue();
        if (val && VALID_LANGS.has(val.toLowerCase())) {
          result.lang = val.toLowerCase();
        }
        break;
      }
      default:
        if (!raw.startsWith('-') && POSITIONAL_RANGES.has(raw.toLowerCase())) {
          result.range = raw.toLowerCase();
          explicitRange = true;
        }
        break;
    }
  }

  if ((result.from || result.to) && !explicitRange) {
    result.range = 'custom';
  }
  return result;
}

const STATS_TEXT = {
  en: {
    title: 'agy-pulse Telemetry Stats',
    range: 'Range',
    granularity: 'Granularity',
    source: 'Source',
    sourceSummary: 'Sources',
    sourceLabels: { all: 'All', cli: 'CLI', ide: 'IDE' },
    backend: 'Ledger',
    recorded: 'Recorded',
    kpiTitle: 'Summary KPIs',
    totalCost: 'Total Cost',
    mainSubSplit: (main, sub) => `Main ${main} + Sub ${sub}`,
    cacheHit: 'Cache Hit',
    tokens: 'Tokens',
    tokensDetail: (inp, read, out) => `New ${inp} | Read ${read} | Out ${out}`,
    avgSpeed: 'Avg Speed',
    avgTtft: 'Avg TTFT',
    samples: (n) => `${n} samples`,
    sessionsSteps: 'Sessions',
    sessionsDetail: (roots, subs, steps) => `${roots} root + ${subs} subagents (${steps} steps)`,
    modelsTitle: 'By Model',
    trendTitle: 'Spend Trend',
    topSessionsTitle: 'Top 10 Sessions',
    empty: 'No telemetry records found for the selected filter.',
    cols: {
      model: 'Model',
      cost: 'Cost',
      share: 'Share',
      cache: 'Cache%',
      input: 'In(New)',
      cacheRead: 'CacheRead',
      output: 'Out',
      speed: 'Speed',
      ttft: 'TTFT',
      steps: 'Steps',
      workspace: 'Workspace / Session',
      branch: 'Branch',
      subs: 'Subs',
      lastActive: 'Last Active',
    },
  },
  zh: {
    title: 'agy-pulse 遥测统计报表',
    range: '区间',
    granularity: '粒度',
    source: '来源',
    sourceSummary: '来源分布',
    sourceLabels: { all: '全部', cli: 'CLI', ide: 'IDE' },
    backend: '账本',
    recorded: '历史累计',
    kpiTitle: '核心指标总览',
    totalCost: '总费用',
    mainSubSplit: (main, sub) => `主会话 ${main} + 子代理 ${sub}`,
    cacheHit: '缓存命中率',
    tokens: 'Token 用量',
    tokensDetail: (inp, read, out) => `未缓存入 ${inp} | 缓存读 ${read} | 输出 ${out}`,
    avgSpeed: '加权均速',
    avgTtft: '平均首字',
    samples: (n) => `${n} 样本`,
    sessionsSteps: '会话规模',
    sessionsDetail: (roots, subs, steps) => `${roots} 根会话 + ${subs} 子代理 (${steps} 步)`,
    modelsTitle: '模型明细',
    trendTitle: '费用趋势',
    topSessionsTitle: 'Top 10 会话明细',
    empty: '当前筛选条件下暂无遥测记录。',
    cols: {
      model: '模型',
      cost: '费用',
      share: '占比',
      cache: '缓存%',
      input: '未缓存入',
      cacheRead: '缓存读',
      output: '输出',
      speed: '均速',
      ttft: '首字',
      steps: '步数',
      workspace: '工作区 / 会话',
      branch: '分支',
      subs: '子代理',
      lastActive: '最近活跃',
    },
  },
};

function makeAnsiPalette(useColor) {
  if (!useColor) {
    return {
      reset: '',
      bold: '',
      dim: '',
      cyan: '',
      green: '',
      yellow: '',
      red: '',
      gray: '',
      magenta: '',
      blue: '',
    };
  }
  return {
    reset: '\x1b[0m',
    bold: '\x1b[1m',
    dim: '\x1b[2m',
    cyan: '\x1b[36m',
    green: '\x1b[32m',
    yellow: '\x1b[33m',
    red: '\x1b[31m',
    gray: '\x1b[90m',
    magenta: '\x1b[35m',
    blue: '\x1b[34m',
  };
}

function padCell(str, width, align = 'left') {
  const s = String(str ?? '');
  const displayW = getAnsiDisplayWidth(s);
  const pad = Math.max(0, width - displayW);
  if (pad === 0) return s;
  return align === 'right' ? `${' '.repeat(pad)}${s}` : `${s}${' '.repeat(pad)}`;
}

function formatTable(columns, rows, palette) {
  const widths = columns.map(col => getDisplayWidth(col.label));
  for (const row of rows) {
    for (let i = 0; i < columns.length; i += 1) {
      const w = getAnsiDisplayWidth(row[i] ?? '');
      if (w > widths[i]) widths[i] = w;
    }
  }

  const headerLine = columns
    .map((col, idx) => `${palette.bold}${palette.gray}${padCell(col.label, widths[idx], col.align)}${palette.reset}`)
    .join('  ');
  const sepLine = `${palette.gray}${widths.map(w => '─'.repeat(w)).join('  ')}${palette.reset}`;
  const bodyLines = rows.map(row =>
    columns.map((col, idx) => padCell(row[idx] ?? '', widths[idx], col.align)).join('  ')
  );
  return [headerLine, sepLine, ...bodyLines].join('\n');
}

function colorCacheRate(rate, palette) {
  const pct = Number.isFinite(rate) ? rate : 0;
  const text = `${pct.toFixed(1)}%`;
  if (pct >= 80) return `${palette.green}${text}${palette.reset}`;
  if (pct >= 50) return `${palette.yellow}${text}${palette.reset}`;
  if (pct > 0) return `${palette.cyan}${text}${palette.reset}`;
  return `${palette.gray}${text}${palette.reset}`;
}

/**
 * Renders a `LedgerQueryResult` as an aligned, colored terminal report in English or Chinese.
 *
 * @param {Object} queryResult
 * @param {Object} [options]
 * @returns {string}
 */
function renderStatsText(queryResult, options = {}) {
  const env = options.env || process.env;
  const lang = resolveLanguage({ language: options.lang || 'auto' }, env);
  const t = STATS_TEXT[lang] || STATS_TEXT.en;
  const useColor = options.color !== undefined
    ? Boolean(options.color)
    : !(options.noColor || env.NO_COLOR);
  const c = makeAnsiPalette(useColor);

  const meta = queryResult?.meta || {};
  const summary = queryResult?.summary || {};
  const byModel = Array.isArray(queryResult?.byModel) ? queryResult.byModel : [];
  const bySource = Array.isArray(queryResult?.bySource) ? queryResult.bySource : [];
  const timeseries = Array.isArray(queryResult?.timeseries) ? queryResult.timeseries : [];
  const sessions = Array.isArray(queryResult?.sessions) ? queryResult.sessions : [];
  const activeSourceFilter = String(meta.source || 'all').toLowerCase();
  const hasIdeSource = bySource.some(item => String(item?.source || '').toLowerCase() === 'ide');
  const showSourceTags = activeSourceFilter === 'ide' || bySource.length > 1 || hasIdeSource || sessions.some(s => String(s?.source || '').toLowerCase() === 'ide');

  const lines = [];
  const rule = `${c.gray}${'═'.repeat(72)}${c.reset}`;
  lines.push(rule);
  lines.push(`${c.bold}${c.cyan}${t.title}${c.reset}`);
  const sourceFilterPart = activeSourceFilter && activeSourceFilter !== 'all'
    ? `${c.gray}| ${t.source}: ${c.reset}${t.sourceLabels?.[activeSourceFilter] || activeSourceFilter.toUpperCase()}  `
    : '';
  lines.push(
    `${c.gray}${t.range}: ${c.reset}${meta.range || '7d'} (${formatDateShort(meta.fromMs)} ~ ${formatDateShort(meta.toMs)})  ` +
    `${c.gray}| ${t.granularity}: ${c.reset}${meta.granularity || 'day'}  ` +
    sourceFilterPart +
    `${c.gray}| ${t.backend}: ${c.reset}${meta.backend || 'sqlite'}  ` +
    `${c.gray}| ${t.recorded}: ${c.reset}${meta.totalRecordedSteps ?? 0} steps / ${meta.totalRecordedSessions ?? 0} sessions`
  );
  lines.push(rule);

  if (!summary.totalSteps) {
    lines.push('');
    lines.push(`${c.yellow}${t.empty}${c.reset}`);
    lines.push('');
    return `${lines.join('\n')}\n`;
  }

  // 1. KPI Summary
  lines.push('');
  lines.push(`${c.bold}${c.magenta}▌ ${t.kpiTitle}${c.reset}`);
  const speedText = Number.isFinite(summary.avgSpeed)
    ? `${summary.avgSpeed.toFixed(1)} tok/s ${c.gray}(${t.samples(summary.streamSamples || 0)})${c.reset}`
    : `${c.gray}--${c.reset}`;
  const ttftText = Number.isFinite(summary.avgTtftSec)
    ? `${summary.avgTtftSec.toFixed(2)}s ${c.gray}(${t.samples(summary.ttftSamples || 0)})${c.reset}`
    : `${c.gray}--${c.reset}`;

  lines.push(
    `  ${padCell(t.totalCost, 14)}: ${c.bold}${c.green}${formatUsd(summary.totalCostUsd || 0)}${c.reset} ` +
    `${c.gray}(${t.mainSubSplit(formatUsd(summary.mainCostUsd || 0), formatUsd(summary.subagentCostUsd || 0))})${c.reset}`
  );
  lines.push(
    `  ${padCell(t.cacheHit, 14)}: ${colorCacheRate(summary.cacheHitRate, c)} ` +
    `${c.gray}(${formatTokens(summary.cacheReadTokens || 0)} / ${formatTokens(summary.totalPromptTokens || 0)})${c.reset}`
  );
  lines.push(
    `  ${padCell(t.tokens, 14)}: ${c.bold}${formatTokens(summary.totalTokens || 0)}${c.reset} ` +
    `${c.gray}(${t.tokensDetail(formatTokens(summary.inputTokens || 0), formatTokens(summary.cacheReadTokens || 0), formatTokens(summary.outputTokens || 0))})${c.reset}`
  );
  lines.push(`  ${padCell(t.avgSpeed, 14)}: ${c.cyan}${speedText}${c.reset}`);
  lines.push(`  ${padCell(t.avgTtft, 14)}: ${c.blue}${ttftText}${c.reset}`);
  lines.push(
    `  ${padCell(t.sessionsSteps, 14)}: ` +
    `${t.sessionsDetail(summary.rootSessionCount || 0, summary.subagentSessionCount || 0, summary.totalSteps || 0)}`
  );
  if (bySource.length > 1 || hasIdeSource) {
    const sourceParts = bySource.map(item => {
      const srcNorm = String(item?.source || 'cli').toLowerCase() === 'ide' ? 'IDE' : 'CLI';
      const srcColor = srcNorm === 'IDE' ? c.magenta : c.cyan;
      return `${srcColor}${srcNorm}${c.reset} ${c.green}${formatUsd(item?.costUsd || 0)}${c.reset} ${c.gray}(${(item?.costSharePct || 0).toFixed(1)}%)${c.reset}`;
    });
    if (sourceParts.length > 0) {
      lines.push(`  ${padCell(t.sourceSummary, 14)}: ${sourceParts.join(` ${c.gray}·${c.reset} `)}`);
    }
  }

  // 2. By Model Table
  if (byModel.length > 0) {
    lines.push('');
    lines.push(`${c.bold}${c.magenta}▌ ${t.modelsTitle}${c.reset}`);
    const cols = [
      { label: t.cols.model, align: 'left' },
      { label: t.cols.cost, align: 'right' },
      { label: t.cols.share, align: 'right' },
      { label: t.cols.cache, align: 'right' },
      { label: t.cols.input, align: 'right' },
      { label: t.cols.cacheRead, align: 'right' },
      { label: t.cols.output, align: 'right' },
      { label: t.cols.speed, align: 'right' },
      { label: t.cols.ttft, align: 'right' },
      { label: t.cols.steps, align: 'right' },
    ];
    const rows = byModel.map(m => [
      `${c.bold}${m.model}${c.reset}`,
      `${c.green}${formatUsd(m.costUsd)}${c.reset}`,
      `${(m.costSharePct || 0).toFixed(1)}%`,
      colorCacheRate(m.cacheHitRate, c),
      formatTokens(m.inputTokens || 0),
      formatTokens(m.cacheReadTokens || 0),
      formatTokens(m.outputTokens || 0),
      Number.isFinite(m.avgSpeed) ? `${m.avgSpeed.toFixed(1)}/s` : `${c.gray}--${c.reset}`,
      Number.isFinite(m.avgTtftSec) ? `${m.avgTtftSec.toFixed(2)}s` : `${c.gray}--${c.reset}`,
      String(m.steps || 0),
    ]);
    lines.push(formatTable(cols, rows, c));
  }

  // 3. Timeseries ASCII Bar Chart
  if (timeseries.length > 0) {
    lines.push('');
    lines.push(`${c.bold}${c.magenta}▌ ${t.trendTitle}${c.reset}`);
    const maxCost = timeseries.reduce((mx, b) => Math.max(mx, b.costUsd || 0), 0);
    const barWidth = 24;
    const labelWidth = timeseries.reduce((mx, b) => Math.max(mx, getDisplayWidth(b.label || b.bucketKey || '')), 8);
    for (const b of timeseries) {
      const ratio = maxCost > 0 ? Math.min(1, (b.costUsd || 0) / maxCost) : 0;
      const filled = (b.costUsd || 0) > 0 ? Math.max(1, Math.round(ratio * barWidth)) : 0;
      const bar = `${c.cyan}${'█'.repeat(filled)}${c.gray}${'░'.repeat(barWidth - filled)}${c.reset}`;
      const lbl = padCell(b.label || b.bucketKey || '', labelWidth);
      const costStr = padCell(formatUsd(b.costUsd || 0), 8, 'right');
      lines.push(
        `  ${c.gray}${lbl}${c.reset} │ ${bar} ${c.green}${costStr}${c.reset} ` +
        `${c.gray}(${String(b.steps || 0).padStart(3)} steps, ${(b.cacheHitRate || 0).toFixed(0)}% cache)${c.reset}`
      );
    }
  }

  // 4. Top 10 Sessions
  if (sessions.length > 0) {
    lines.push('');
    lines.push(`${c.bold}${c.magenta}▌ ${t.topSessionsTitle}${c.reset}`);
    const cols = [
      { label: t.cols.workspace, align: 'left' },
      { label: t.cols.branch, align: 'left' },
      { label: t.cols.cost, align: 'right' },
      { label: t.cols.subs, align: 'right' },
      { label: t.cols.steps, align: 'right' },
      { label: t.cols.cache, align: 'right' },
      { label: t.cols.model, align: 'left' },
      { label: t.cols.lastActive, align: 'right' },
    ];
    const rows = sessions.slice(0, 10).map(s => {
      const srcNorm = String(s.source || 'cli').toLowerCase() === 'ide' ? 'IDE' : 'CLI';
      const srcPrefix = showSourceTags
        ? `${srcNorm === 'IDE' ? c.magenta : c.cyan}[${srcNorm}]${c.reset} `
        : '';
      const wsName = s.workspaceLabel || s.workspace || s.rootConversationId.slice(0, 12);
      const wsText = truncateToDisplayWidth(wsName, showSourceTags ? 18 : 20, '…').trimEnd();
      const wsCell = `${srcPrefix}${wsText}`;
      const brCell = truncateToDisplayWidth(s.branch || '--', 16, '…').trimEnd();
      const modelsCell = truncateToDisplayWidth((s.models || []).join(', ') || '--', 18, '…').trimEnd();
      return [
        wsCell,
        `${c.gray}${brCell}${c.reset}`,
        `${c.green}${formatUsd(s.totalCostUsd || 0)}${c.reset}`,
        String(s.subagentCount || 0),
        String(s.steps || 0),
        colorCacheRate(s.cacheHitRate, c),
        modelsCell,
        `${c.gray}${formatDateTimeShort(s.lastActivityMs)}${c.reset}`,
      ];
    });
    lines.push(formatTable(cols, rows, c));
  }

  lines.push('');
  return `${lines.join('\n')}\n`;
}

function buildStatsHelpText(lang) {
  if (lang === 'zh') {
    return [
      '用法: agypl stats [选项]  (兼容: agy-pulse --stats [选项])',
      '',
      '选项:',
      '  --range <today|24h|7d|30d|month|all|custom>  统计时间区间（默认: 7d）',
      '  --from <YYYY-MM-DD|ms>                       起始日期/时间戳',
      '  --to <YYYY-MM-DD|ms>                         结束日期/时间戳',
      '  --granularity <auto|hour|day|week>           时间聚合粒度（默认: auto）',
      '  --source <all|cli|ide>                     按数据来源过滤（默认: all）',
      '  --model <name|all>                           按模型短名过滤',
      '  --workspace <path|name|all>                  按工作区过滤',
      '  --no-subagents                               排除蜂群/子代理统计',
      '  --json                                       输出 JSON 格式',
      '  --csv[=timeseries|models|sessions]           输出 CSV 格式',
      '  --csv-kind <timeseries|models|sessions>      指定 CSV 导出明细类型',
      '  --lang <auto|en|zh>                          显示语言（默认: auto）',
      '  --no-sync                                    跳过同步直接查询现有账本',
      '  -h, --help                                   显示此帮助信息',
      '',
    ].join('\n');
  }
  return [
    'Usage: agypl stats [options]  (alias: agy-pulse --stats [options])',
    '',
    'Options:',
    '  --range <today|24h|7d|30d|month|all|custom>  Time range (default: 7d)',
    '  --from <YYYY-MM-DD|ms>                       Start date or epoch ms',
    '  --to <YYYY-MM-DD|ms>                         End date or epoch ms',
    '  --granularity <auto|hour|day|week>           Bucket granularity (default: auto)',
    '  --source <all|cli|ide>                     Filter by data source (default: all)',
    '  --model <name|all>                           Filter by model short name',
    '  --workspace <path|name|all>                  Filter by workspace path or name',
    '  --no-subagents                               Exclude swarm/subagent sessions',
    '  --json                                       Output JSON',
    '  --csv[=timeseries|models|sessions]           Output UTF-8 CSV',
    '  --csv-kind <timeseries|models|sessions>      CSV export kind',
    '  --lang <auto|en|zh>                          Output language (default: auto)',
    '  --no-sync                                    Skip DB scan and query existing ledger',
    '  -h, --help                                   Show this help message',
    '',
  ].join('\n');
}

/**
 * Executes the `--stats` CLI command.
 *
 * @param {string[]} [argv]
 * @param {Object} [options]
 * @returns {Promise<{ exitCode: number, output: string }>}
 */
async function runStatsCli(argv = process.argv.slice(2), options = {}) {
  const args = parseStatsArgs(argv);
  const env = options.env || process.env;
  const write = typeof options.write === 'function'
    ? options.write
    : (text) => process.stdout.write(text);

  let effectiveLang = args.lang;
  if (effectiveLang === 'auto') {
    try {
      const cfg = await loadConfig(options.cwd || process.cwd());
      if (cfg && (cfg.language === 'en' || cfg.language === 'zh')) {
        effectiveLang = cfg.language;
      }
    } catch {
      // Ignore config load failures in CLI mode
    }
  }
  const resolvedLang = resolveLanguage({ language: effectiveLang }, env);

  if (args.help) {
    const helpOut = buildStatsHelpText(resolvedLang);
    write(helpOut);
    return { exitCode: 0, output: helpOut };
  }

  const ledgerImpl = options.ledger || {
    syncAllConversationsToLedger,
    queryLedger,
    formatLedgerCsv,
  };
  const ledgerOpts = options.ledgerOptions || options;

  if (!args.noSync && typeof ledgerImpl.syncAllConversationsToLedger === 'function') {
    ledgerImpl.syncAllConversationsToLedger(ledgerOpts);
  }

  const queryResult = ledgerImpl.queryLedger({
    range: args.range,
    from: args.from,
    to: args.to,
    granularity: args.granularity,
    source: args.source,
    model: args.model,
    workspace: args.workspace,
    includeSubagents: args.includeSubagents,
    ...(Number.isFinite(options.now) ? { now: options.now } : {}),
  }, ledgerOpts);

  let output;
  if (args.format === 'json') {
    output = `${JSON.stringify(queryResult, null, 2)}\n`;
  } else if (args.format === 'csv') {
    output = ledgerImpl.formatLedgerCsv(queryResult, args.csvKind);
  } else {
    output = renderStatsText(queryResult, {
      lang: resolvedLang,
      color: options.color,
      noColor: options.noColor,
      env,
    });
  }

  write(output);
  return { exitCode: 0, output };
}

module.exports = {
  parseStatsArgs,
  renderStatsText,
  runStatsCli,
};
