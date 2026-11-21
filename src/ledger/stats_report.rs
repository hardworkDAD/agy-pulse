use crate::config::LanguageSetting;
use crate::domain::format_usd;
use crate::render::{
    ansi_display_width, display_width, format_tokens, truncate_plain_to_display_width,
};

use super::query::{
    CsvExportKind, LedgerOverview, LedgerSource, LedgerSourceFilter, LedgerTimeRange,
    LedgerGranularity, format_datetime_short, format_date_short,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatsOutputFormat {
    #[default]
    Table,
    Json,
    Csv,
}

#[derive(Debug, Clone)]
pub struct StatsCliArgs {
    pub range: LedgerTimeRange,
    pub from: Option<String>,
    pub to: Option<String>,
    pub granularity: LedgerGranularity,
    pub source: LedgerSourceFilter,
    pub model: String,
    pub workspace: String,
    pub include_subagents: bool,
    pub format: StatsOutputFormat,
    pub csv_kind: CsvExportKind,
    pub lang: LanguageSetting,
    pub no_sync: bool,
    pub help: bool,
}

impl Default for StatsCliArgs {
    fn default() -> Self {
        Self {
            range: LedgerTimeRange::Last7d,
            from: None,
            to: None,
            granularity: LedgerGranularity::Auto,
            source: LedgerSourceFilter::All,
            model: "all".to_string(),
            workspace: "all".to_string(),
            include_subagents: true,
            format: StatsOutputFormat::Table,
            csv_kind: CsvExportKind::Timeseries,
            lang: LanguageSetting::Auto,
            no_sync: false,
            help: false,
        }
    }
}

pub fn parse_stats_args(argv: &[String]) -> StatsCliArgs {
    let mut result = StatsCliArgs::default();
    let mut explicit_range = false;

    let mut start_idx = 0usize;
    while start_idx < argv.len() && argv[start_idx].is_empty() {
        start_idx += 1;
    }
    if start_idx < argv.len() {
        let first = argv[start_idx].as_str();
        if first == "stats" || first == "report" || first == "--stats" {
            start_idx += 1;
        }
    }

    let mut i = start_idx;
    while i < argv.len() {
        let raw = argv[i].as_str();
        i += 1;
        if raw.is_empty() || raw == "--stats" {
            continue;
        }

        let (flag, inline_val) = match raw.find('=') {
            Some(pos) if pos > 0 => (&raw[..pos], Some(&raw[pos + 1..])),
            _ => (raw, None),
        };

        let mut consume_value = || -> Option<String> {
            if let Some(v) = inline_val {
                return Some(v.to_string());
            }
            if i < argv.len() && !argv[i].starts_with('-') {
                let val = argv[i].clone();
                i += 1;
                return Some(val);
            }
            None
        };

        match flag {
            "--help" | "-h" => result.help = true,
            "--no-sync" => result.no_sync = true,
            "--no-subagents" => result.include_subagents = false,
            "--include-subagents" => result.include_subagents = true,
            "--json" => result.format = StatsOutputFormat::Json,
            "--zh" => result.lang = LanguageSetting::Zh,
            "--en" => result.lang = LanguageSetting::En,
            "--csv" => {
                result.format = StatsOutputFormat::Csv;
                if let Some(val) = inline_val
                    && let Some(kind) = CsvExportKind::from_raw(val)
                {
                    result.csv_kind = kind;
                }
            }
            "--csv-kind" | "--kind" => {
                if let Some(val) = consume_value()
                    && let Some(kind) = CsvExportKind::from_raw(&val)
                {
                    result.csv_kind = kind;
                }
            }
            "--format" => {
                if let Some(val) = consume_value() {
                    match val.trim().to_ascii_lowercase().as_str() {
                        "json" => result.format = StatsOutputFormat::Json,
                        "csv" => result.format = StatsOutputFormat::Csv,
                        "table" => result.format = StatsOutputFormat::Table,
                        _ => {}
                    }
                }
            }
            "--range" => {
                if let Some(val) = consume_value()
                    && let Some(r) = LedgerTimeRange::from_raw(&val)
                {
                    result.range = r;
                    explicit_range = true;
                }
            }
            "--today" => {
                result.range = LedgerTimeRange::Today;
                explicit_range = true;
            }
            "--24h" => {
                result.range = LedgerTimeRange::Last24h;
                explicit_range = true;
            }
            "--7d" => {
                result.range = LedgerTimeRange::Last7d;
                explicit_range = true;
            }
            "--30d" => {
                result.range = LedgerTimeRange::Last30d;
                explicit_range = true;
            }
            "--month" => {
                result.range = LedgerTimeRange::ThisMonth;
                explicit_range = true;
            }
            "--all" => {
                result.range = LedgerTimeRange::All;
                explicit_range = true;
            }
            "--from" => {
                if let Some(val) = consume_value() {
                    result.from = Some(val);
                }
            }
            "--to" => {
                if let Some(val) = consume_value() {
                    result.to = Some(val);
                }
            }
            "--granularity" | "--by" => {
                if let Some(val) = consume_value()
                    && let Some(g) = LedgerGranularity::from_raw(&val)
                {
                    result.granularity = g;
                }
            }
            "--model" => {
                if let Some(val) = consume_value() {
                    result.model = val;
                }
            }
            "--source" => {
                if let Some(val) = consume_value() {
                    result.source = LedgerSourceFilter::from_raw(&val);
                }
            }
            "--workspace" | "--ws" => {
                if let Some(val) = consume_value() {
                    result.workspace = val;
                }
            }
            "--lang" => {
                if let Some(val) = consume_value() {
                    match val.trim().to_ascii_lowercase().as_str() {
                        "zh" => result.lang = LanguageSetting::Zh,
                        "en" => result.lang = LanguageSetting::En,
                        "auto" => result.lang = LanguageSetting::Auto,
                        _ => {}
                    }
                }
            }
            _ => {
                if !raw.starts_with('-')
                    && let Some(r) = LedgerTimeRange::from_raw(raw)
                    && r != LedgerTimeRange::Custom
                {
                    result.range = r;
                    explicit_range = true;
                }
            }
        }
    }

    if (result.from.is_some() || result.to.is_some()) && !explicit_range {
        result.range = LedgerTimeRange::Custom;
    }

    result
}

pub fn build_stats_help_text(lang: LanguageSetting) -> String {
    if lang == LanguageSetting::Zh {
        return [
            "用法: agypl stats [选项]  (兼容: agy-pulse --stats [选项])",
            "",
            "选项:",
            "  --range <today|24h|7d|30d|month|all|custom>  统计时间区间（默认: 7d）",
            "  --from <YYYY-MM-DD|ms>                       起始日期/时间戳",
            "  --to <YYYY-MM-DD|ms>                         结束日期/时间戳",
            "  --granularity <auto|hour|day|week>           时间聚合粒度（默认: auto）",
            "  --source <all|cli|ide>                     按数据来源过滤（默认: all）",
            "  --model <name|all>                           按模型短名过滤",
            "  --workspace <path|name|all>                  按工作区过滤",
            "  --no-subagents                               排除蜂群/子代理统计",
            "  --json                                       输出 JSON 格式",
            "  --csv[=timeseries|models|sessions]           输出 CSV 格式",
            "  --csv-kind <timeseries|models|sessions>      指定 CSV 导出明细类型",
            "  --lang <auto|en|zh>                          显示语言（默认: auto）",
            "  --zh / --en                                  快捷切换中英文输出",
            "  --no-sync                                    跳过同步直接查询现有账本",
            "  -h, --help                                   显示此帮助信息",
            "",
        ]
        .join("\n");
    }
    [
        "Usage: agypl stats [options]  (alias: agy-pulse --stats [options])",
        "",
        "Options:",
        "  --range <today|24h|7d|30d|month|all|custom>  Time range (default: 7d)",
        "  --from <YYYY-MM-DD|ms>                       Start date or epoch ms",
        "  --to <YYYY-MM-DD|ms>                         End date or epoch ms",
        "  --granularity <auto|hour|day|week>           Bucket granularity (default: auto)",
        "  --source <all|cli|ide>                     Filter by data source (default: all)",
        "  --model <name|all>                           Filter by model short name",
        "  --workspace <path|name|all>                  Filter by workspace path or name",
        "  --no-subagents                               Exclude swarm/subagent cascades",
        "  --json                                       Output JSON",
        "  --csv[=timeseries|models|sessions]           Output UTF-8 CSV",
        "  --csv-kind <timeseries|models|sessions>      CSV export kind",
        "  --lang <auto|en|zh>                          Output language (default: auto)",
        "  --zh / --en                                  Shortcut to force Chinese or English",
        "  --no-sync                                    Skip DB scan and query existing ledger",
        "  -h, --help                                   Show this help message",
        "",
    ]
    .join("\n")
}

struct AnsiPalette {
    reset: &'static str,
    bold: &'static str,
    cyan: &'static str,
    green: &'static str,
    yellow: &'static str,
    gray: &'static str,
    magenta: &'static str,
    blue: &'static str,
}

impl AnsiPalette {
    fn new(use_color: bool) -> Self {
        if !use_color {
            Self {
                reset: "",
                bold: "",
                cyan: "",
                green: "",
                yellow: "",
                gray: "",
                magenta: "",
                blue: "",
            }
        } else {
            Self {
                reset: "\x1b[0m",
                bold: "\x1b[1m",
                cyan: "\x1b[36m",
                green: "\x1b[32m",
                yellow: "\x1b[33m",
                gray: "\x1b[90m",
                magenta: "\x1b[35m",
                blue: "\x1b[34m",
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

struct TableCol {
    label: &'static str,
    align: Align,
}

fn pad_cell(s: &str, width: usize, align: Align) -> String {
    let disp_w = ansi_display_width(s);
    let pad = width.saturating_sub(disp_w);
    if pad == 0 {
        return s.to_string();
    }
    match align {
        Align::Right => format!("{}{s}", " ".repeat(pad)),
        Align::Left => format!("{s}{}", " ".repeat(pad)),
    }
}

fn format_table(cols: &[TableCol], rows: &[Vec<String>], c: &AnsiPalette) -> String {
    let mut widths: Vec<usize> = cols.iter().map(|col| display_width(col.label)).collect();
    for row in rows {
        for (idx, cell) in row.iter().enumerate().take(cols.len()) {
            let w = ansi_display_width(cell);
            if w > widths[idx] {
                widths[idx] = w;
            }
        }
    }

    let header_parts: Vec<String> = cols
        .iter()
        .enumerate()
        .map(|(idx, col)| {
            format!(
                "{}{}{}{}",
                c.bold,
                c.gray,
                pad_cell(col.label, widths[idx], col.align),
                c.reset
            )
        })
        .collect();
    let header_line = header_parts.join("  ");

    let sep_parts: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
    let sep_line = format!("{}{}{}", c.gray, sep_parts.join("  "), c.reset);

    let mut out = Vec::with_capacity(rows.len() + 2);
    out.push(header_line);
    out.push(sep_line);
    for row in rows {
        let cells: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(idx, col)| {
                let raw = row.get(idx).map(String::as_str).unwrap_or("");
                pad_cell(raw, widths[idx], col.align)
            })
            .collect();
        out.push(cells.join("  "));
    }
    out.join("\n")
}

fn color_cache_rate(rate: f64, c: &AnsiPalette) -> String {
    let pct = if rate.is_finite() { rate } else { 0.0 };
    let text = format!("{pct:.1}%");
    if pct >= 80.0 {
        format!("{}{text}{}", c.green, c.reset)
    } else if pct >= 50.0 {
        format!("{}{text}{}", c.yellow, c.reset)
    } else if pct > 0.0 {
        format!("{}{text}{}", c.cyan, c.reset)
    } else {
        format!("{}{text}{}", c.gray, c.reset)
    }
}

struct StatsLabels {
    title: &'static str,
    range: &'static str,
    granularity: &'static str,
    source: &'static str,
    source_summary: &'static str,
    source_cli: &'static str,
    source_ide: &'static str,
    source_all: &'static str,
    backend: &'static str,
    recorded: &'static str,
    kpi_title: &'static str,
    total_cost: &'static str,
    cache_hit: &'static str,
    tokens: &'static str,
    avg_speed: &'static str,
    avg_ttft: &'static str,
    cascades_steps: &'static str,
    models_title: &'static str,
    trend_title: &'static str,
    top_cascades_title: &'static str,
    empty: &'static str,
    col_model: &'static str,
    col_cost: &'static str,
    col_share: &'static str,
    col_cache: &'static str,
    col_input: &'static str,
    col_cache_read: &'static str,
    col_output: &'static str,
    col_speed: &'static str,
    col_ttft: &'static str,
    col_steps: &'static str,
    col_workspace: &'static str,
    col_branch: &'static str,
    col_subs: &'static str,
    col_last_active: &'static str,
    is_zh: bool,
}

const EN_LABELS: StatsLabels = StatsLabels {
    title: "agy-pulse Telemetry Stats",
    range: "Range",
    granularity: "Granularity",
    source: "Source",
    source_summary: "Sources",
    source_cli: "CLI",
    source_ide: "IDE",
    source_all: "All",
    backend: "Ledger",
    recorded: "Recorded",
    kpi_title: "Summary KPIs",
    total_cost: "Total Cost",
    cache_hit: "Cache Hit",
    tokens: "Tokens",
    avg_speed: "Avg Speed",
    avg_ttft: "Avg TTFT",
    cascades_steps: "Cascades",
    models_title: "By Model",
    trend_title: "Spend Trend",
    top_cascades_title: "Top 10 Cascades",
    empty: "No telemetry records found for the selected filter.",
    col_model: "Model",
    col_cost: "Cost",
    col_share: "Share",
    col_cache: "Cache%",
    col_input: "In(New)",
    col_cache_read: "CacheRead",
    col_output: "Out",
    col_speed: "Speed",
    col_ttft: "TTFT",
    col_steps: "Steps",
    col_workspace: "Workspace / Cascade",
    col_branch: "Branch",
    col_subs: "Subs",
    col_last_active: "Last Active",
    is_zh: false,
};

const ZH_LABELS: StatsLabels = StatsLabels {
    title: "agy-pulse 遥测统计报表",
    range: "区间",
    granularity: "粒度",
    source: "来源",
    source_summary: "来源分布",
    source_cli: "CLI",
    source_ide: "IDE",
    source_all: "全部",
    backend: "账本",
    recorded: "历史累计",
    kpi_title: "核心指标总览",
    total_cost: "总费用",
    cache_hit: "缓存命中率",
    tokens: "Token 用量",
    avg_speed: "加权均速",
    avg_ttft: "平均首字",
    cascades_steps: "任务树规模",
    models_title: "模型明细",
    trend_title: "费用趋势",
    top_cascades_title: "Top 10 任务树明细",
    empty: "当前筛选条件下暂无遥测记录。",
    col_model: "模型",
    col_cost: "费用",
    col_share: "占比",
    col_cache: "缓存%",
    col_input: "未缓存入",
    col_cache_read: "缓存读",
    col_output: "输出",
    col_speed: "均速",
    col_ttft: "首字",
    col_steps: "步数",
    col_workspace: "工作区 / 任务树",
    col_branch: "分支",
    col_subs: "子代理",
    col_last_active: "最近活跃",
    is_zh: true,
};

pub fn render_stats_text(
    overview: &LedgerOverview,
    lang: LanguageSetting,
    use_color: bool,
) -> String {
    let t = if lang == LanguageSetting::Zh {
        &ZH_LABELS
    } else {
        &EN_LABELS
    };
    let c = AnsiPalette::new(use_color);

    let meta = &overview.meta;
    let summary = &overview.summary;
    let active_src = meta.source.to_ascii_lowercase();
    let has_ide = overview.by_source.iter().any(|s| s.source == LedgerSource::Ide);
    let show_source_tags = active_src == "ide"
        || overview.by_source.len() > 1
        || has_ide
        || overview.cascades.iter().any(|s| s.source == LedgerSource::Ide);

    let mut lines: Vec<String> = Vec::new();
    let rule = format!("{}{}{}", c.gray, "═".repeat(72), c.reset);
    lines.push(rule.clone());
    lines.push(format!("{}{}{}{}", c.bold, c.cyan, t.title, c.reset));

    let source_part = if !active_src.is_empty() && active_src != "all" {
        let lbl = match active_src.as_str() {
            "cli" => t.source_cli,
            "ide" => t.source_ide,
            _ => t.source_all,
        };
        format!("{}| {}: {}{}  ", c.gray, t.source, c.reset, lbl)
    } else {
        String::new()
    };

    lines.push(format!(
        "{}{}: {}{} ({} ~ {})  {}| {}: {}{}  {source_part}{}| {}: {}{}  {}| {}: {}{} steps / {} conversations",
        c.gray,
        t.range,
        c.reset,
        meta.range,
        format_date_short(meta.from_ms),
        format_date_short(meta.to_ms),
        c.gray,
        t.granularity,
        c.reset,
        meta.granularity,
        c.gray,
        t.backend,
        c.reset,
        meta.backend,
        c.gray,
        t.recorded,
        c.reset,
        meta.total_recorded_steps,
        meta.total_recorded_conversations,
    ));
    lines.push(rule);

    if summary.total_steps == 0 {
        lines.push(String::new());
        lines.push(format!("{}{}{}", c.yellow, t.empty, c.reset));
        lines.push(String::new());
        return format!("{}\n", lines.join("\n"));
    }

    lines.push(String::new());
    lines.push(format!("{}{}▌ {}{}", c.bold, c.magenta, t.kpi_title, c.reset));

    let samples_label = |n: u64| -> String {
        if t.is_zh {
            format!("{n} 样本")
        } else {
            format!("{n} samples")
        }
    };

    let speed_text = match summary.avg_speed {
        Some(s) if s.is_finite() => format!(
            "{s:.1} tok/s {}({}){}",
            c.gray,
            samples_label(summary.stream_samples),
            c.reset
        ),
        _ => format!("{}--{}", c.gray, c.reset),
    };
    let ttft_text = match summary.avg_ttft_sec {
        Some(v) if v.is_finite() => format!(
            "{v:.2}s {}({}){}",
            c.gray,
            samples_label(summary.ttft_samples),
            c.reset
        ),
        _ => format!("{}--{}", c.gray, c.reset),
    };

    let main_sub_split = if t.is_zh {
        format!(
            "主对话 {} + 子代理 {}",
            format_usd(summary.main_cost_usd),
            format_usd(summary.subagent_cost_usd)
        )
    } else {
        format!(
            "Main {} + Sub {}",
            format_usd(summary.main_cost_usd),
            format_usd(summary.subagent_cost_usd)
        )
    };

    let tokens_detail = if t.is_zh {
        format!(
            "未缓存入 {} | 缓存读 {} | 输出 {}",
            format_tokens(summary.input_tokens),
            format_tokens(summary.cache_read_tokens),
            format_tokens(summary.output_tokens)
        )
    } else {
        format!(
            "New {} | Read {} | Out {}",
            format_tokens(summary.input_tokens),
            format_tokens(summary.cache_read_tokens),
            format_tokens(summary.output_tokens)
        )
    };

    let cascades_detail = if t.is_zh {
        format!(
            "{} 根任务树 + {} 子代理 ({} 步)",
            summary.root_cascade_count,
            summary.subagent_conversation_count,
            summary.total_steps
        )
    } else {
        format!(
            "{} root + {} subagents ({} steps)",
            summary.root_cascade_count,
            summary.subagent_conversation_count,
            summary.total_steps
        )
    };

    lines.push(format!(
        "  {}: {}{}{}{} {}({main_sub_split}){}",
        pad_cell(t.total_cost, 14, Align::Left),
        c.bold,
        c.green,
        format_usd(summary.total_cost_usd),
        c.reset,
        c.gray,
        c.reset,
    ));
    lines.push(format!(
        "  {}: {} {}({} / {}){}",
        pad_cell(t.cache_hit, 14, Align::Left),
        color_cache_rate(summary.cache_hit_rate, &c),
        c.gray,
        format_tokens(summary.cache_read_tokens),
        format_tokens(summary.total_prompt_tokens),
        c.reset,
    ));
    lines.push(format!(
        "  {}: {}{}{} {}({tokens_detail}){}",
        pad_cell(t.tokens, 14, Align::Left),
        c.bold,
        format_tokens(summary.total_tokens),
        c.reset,
        c.gray,
        c.reset,
    ));
    lines.push(format!(
        "  {}: {}{speed_text}{}",
        pad_cell(t.avg_speed, 14, Align::Left),
        c.cyan,
        c.reset,
    ));
    lines.push(format!(
        "  {}: {}{ttft_text}{}",
        pad_cell(t.avg_ttft, 14, Align::Left),
        c.blue,
        c.reset,
    ));
    lines.push(format!(
        "  {}: {cascades_detail}",
        pad_cell(t.cascades_steps, 14, Align::Left),
    ));

    if (overview.by_source.len() > 1 || has_ide) && !overview.by_source.is_empty() {
        let src_parts: Vec<String> = overview
            .by_source
            .iter()
            .map(|item| {
                let (lbl, col) = match item.source {
                    LedgerSource::Ide => ("IDE", c.magenta),
                    LedgerSource::Cli => ("CLI", c.cyan),
                };
                format!(
                    "{col}{lbl}{} {}{}{} {}({:.1}%){}",
                    c.reset,
                    c.green,
                    format_usd(item.cost_usd),
                    c.reset,
                    c.gray,
                    item.cost_share_pct,
                    c.reset,
                )
            })
            .collect();
        let joiner = format!(" {}·{} ", c.gray, c.reset);
        lines.push(format!(
            "  {}: {}",
            pad_cell(t.source_summary, 14, Align::Left),
            src_parts.join(&joiner)
        ));
    }

    if !overview.by_model.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "{}{}▌ {}{}",
            c.bold, c.magenta, t.models_title, c.reset
        ));
        let cols = [
            TableCol { label: t.col_model, align: Align::Left },
            TableCol { label: t.col_cost, align: Align::Right },
            TableCol { label: t.col_share, align: Align::Right },
            TableCol { label: t.col_cache, align: Align::Right },
            TableCol { label: t.col_input, align: Align::Right },
            TableCol { label: t.col_cache_read, align: Align::Right },
            TableCol { label: t.col_output, align: Align::Right },
            TableCol { label: t.col_speed, align: Align::Right },
            TableCol { label: t.col_ttft, align: Align::Right },
            TableCol { label: t.col_steps, align: Align::Right },
        ];
        let rows: Vec<Vec<String>> = overview
            .by_model
            .iter()
            .map(|m| {
                let spd = match m.avg_speed {
                    Some(v) if v.is_finite() => format!("{v:.1}/s"),
                    _ => format!("{}--{}", c.gray, c.reset),
                };
                let ttft = match m.avg_ttft_sec {
                    Some(v) if v.is_finite() => format!("{v:.2}s"),
                    _ => format!("{}--{}", c.gray, c.reset),
                };
                vec![
                    format!("{}{}{}", c.bold, m.model, c.reset),
                    format!("{}{}{}", c.green, format_usd(m.cost_usd), c.reset),
                    format!("{:.1}%", m.cost_share_pct),
                    color_cache_rate(m.cache_hit_rate, &c),
                    format_tokens(m.input_tokens),
                    format_tokens(m.cache_read_tokens),
                    format_tokens(m.output_tokens),
                    spd,
                    ttft,
                    m.steps.to_string(),
                ]
            })
            .collect();
        lines.push(format_table(&cols, &rows, &c));
    }

    if !overview.timeseries.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "{}{}▌ {}{}",
            c.bold, c.magenta, t.trend_title, c.reset
        ));
        let max_cost = overview
            .timeseries
            .iter()
            .fold(0.0f64, |acc, b| acc.max(b.cost_usd));
        let bar_width = 24usize;
        let label_width = overview
            .timeseries
            .iter()
            .map(|b| display_width(&b.label))
            .fold(8usize, usize::max);

        for b in &overview.timeseries {
            let ratio = if max_cost > 0.0 {
                (b.cost_usd / max_cost).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let filled = if b.cost_usd > 0.0 {
                ((ratio * bar_width as f64).round() as usize).max(1)
            } else {
                0
            };
            let bar = format!(
                "{}{}{}{}{}",
                c.cyan,
                "█".repeat(filled),
                c.gray,
                "░".repeat(bar_width.saturating_sub(filled)),
                c.reset
            );
            let lbl = pad_cell(&b.label, label_width, Align::Left);
            let cost_str = pad_cell(&format_usd(b.cost_usd), 8, Align::Right);
            lines.push(format!(
                "  {}{lbl}{} │ {bar} {}{cost_str}{} {}({:>3} steps, {:.0}% cache){}",
                c.gray,
                c.reset,
                c.green,
                c.reset,
                c.gray,
                b.steps,
                b.cache_hit_rate,
                c.reset
            ));
        }
    }

    if !overview.cascades.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "{}{}▌ {}{}",
            c.bold, c.magenta, t.top_cascades_title, c.reset
        ));
        let cols = [
            TableCol { label: t.col_workspace, align: Align::Left },
            TableCol { label: t.col_branch, align: Align::Left },
            TableCol { label: t.col_cost, align: Align::Right },
            TableCol { label: t.col_subs, align: Align::Right },
            TableCol { label: t.col_steps, align: Align::Right },
            TableCol { label: t.col_cache, align: Align::Right },
            TableCol { label: t.col_model, align: Align::Left },
            TableCol { label: t.col_last_active, align: Align::Right },
        ];
        let rows: Vec<Vec<String>> = overview
            .cascades
            .iter()
            .take(10)
            .map(|s| {
                let (src_lbl, src_col) = match s.source {
                    LedgerSource::Ide => ("IDE", c.magenta),
                    LedgerSource::Cli => ("CLI", c.cyan),
                };
                let src_prefix = if show_source_tags {
                    format!("{src_col}[{src_lbl}]{} ", c.reset)
                } else {
                    String::new()
                };
                let fallback_id: String = s.root_conversation_id.chars().take(12).collect();
                let ws_name = if !s.workspace_label.is_empty() {
                    s.workspace_label.as_str()
                } else if !s.workspace.is_empty() {
                    s.workspace.as_str()
                } else {
                    fallback_id.as_str()
                };
                let ws_trunc = truncate_plain_to_display_width(
                    ws_name,
                    if show_source_tags { 18 } else { 20 },
                    "…",
                );
                let ws_cell = format!("{src_prefix}{}", ws_trunc.trim_end());
                let br_raw = if s.branch.is_empty() { "--" } else { s.branch.as_str() };
                let br_cell = truncate_plain_to_display_width(br_raw, 16, "…");
                let models_joined = if s.models.is_empty() {
                    "--".to_string()
                } else {
                    s.models.join(", ")
                };
                let models_cell = truncate_plain_to_display_width(&models_joined, 18, "…");
                vec![
                    ws_cell,
                    format!("{}{}{}", c.gray, br_cell.trim_end(), c.reset),
                    format!("{}{}{}", c.green, format_usd(s.total_cost_usd), c.reset),
                    s.subagent_count.to_string(),
                    s.steps.to_string(),
                    color_cache_rate(s.cache_hit_rate, &c),
                    models_cell.trim_end().to_string(),
                    format!("{}{}{}", c.gray, format_datetime_short(s.last_activity_ms), c.reset),
                ]
            })
            .collect();
        lines.push(format_table(&cols, &rows, &c));
    }

    lines.push(String::new());
    format!("{}\n", lines.join("\n"))
}
