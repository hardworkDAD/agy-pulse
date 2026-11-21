use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use rusqlite::Connection;
use serde::Serialize;

use crate::domain::compact_model_name;

use super::LedgerError;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const HOUR_MS: i64 = 60 * 60 * 1000;
const MIN_VALID_STREAM_SEC: f64 = 0.05;
const MAX_VALID_STREAM_SEC: f64 = 600.0;
const MAX_VALID_STREAM_SPEED: f64 = 5000.0;
const MAX_VALID_TTFT_SEC: f64 = 600.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerSource {
    #[default]
    Cli,
    Ide,
}

impl LedgerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Ide => "ide",
        }
    }

    pub fn from_raw(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("ide") {
            Self::Ide
        } else {
            Self::Cli
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LedgerSourceFilter {
    #[default]
    All,
    Cli,
    Ide,
}

impl LedgerSourceFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Cli => "cli",
            Self::Ide => "ide",
        }
    }

    pub fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cli" => Self::Cli,
            "ide" => Self::Ide,
            _ => Self::All,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LedgerTimeRange {
    Today,
    Last24h,
    #[default]
    Last7d,
    Last30d,
    ThisMonth,
    All,
    Custom,
}

impl LedgerTimeRange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Last24h => "24h",
            Self::Last7d => "7d",
            Self::Last30d => "30d",
            Self::ThisMonth => "month",
            Self::All => "all",
            Self::Custom => "custom",
        }
    }

    pub fn from_raw(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "today" => Some(Self::Today),
            "24h" => Some(Self::Last24h),
            "7d" => Some(Self::Last7d),
            "30d" => Some(Self::Last30d),
            "month" => Some(Self::ThisMonth),
            "all" => Some(Self::All),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LedgerGranularity {
    #[default]
    Auto,
    Hour,
    Day,
    Week,
}

impl LedgerGranularity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
        }
    }

    pub fn from_raw(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "hour" => Some(Self::Hour),
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CsvExportKind {
    #[default]
    Timeseries,
    Models,
    Cascades,
}

impl CsvExportKind {
    pub fn from_raw(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "timeseries" => Some(Self::Timeseries),
            "models" => Some(Self::Models),
            "sessions" | "cascades" => Some(Self::Cascades),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerExportFormat {
    Json,
    Csv(CsvExportKind),
}

#[derive(Debug, Clone)]
pub struct LedgerQueryFilter {
    pub range: LedgerTimeRange,
    pub from: Option<String>,
    pub to: Option<String>,
    pub granularity: LedgerGranularity,
    pub source: LedgerSourceFilter,
    pub model: Option<String>,
    pub workspace: Option<String>,
    pub include_subagents: bool,
    pub now_ms: Option<i64>,
}

impl Default for LedgerQueryFilter {
    fn default() -> Self {
        Self {
            range: LedgerTimeRange::Last7d,
            from: None,
            to: None,
            granularity: LedgerGranularity::Auto,
            source: LedgerSourceFilter::All,
            model: None,
            workspace: None,
            include_subagents: true,
            now_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerMeta {
    pub range: String,
    pub source: String,
    #[serde(rename = "fromMs")]
    pub from_ms: i64,
    #[serde(rename = "toMs")]
    pub to_ms: i64,
    pub granularity: String,
    #[serde(rename = "ledgerPath")]
    pub ledger_path: String,
    pub backend: String,
    #[serde(rename = "totalRecordedSteps")]
    pub total_recorded_steps: u64,
    #[serde(rename = "totalRecordedSessions")]
    pub total_recorded_conversations: u64,
    #[serde(rename = "availableModels")]
    pub available_models: Vec<String>,
    #[serde(rename = "availableWorkspaces")]
    pub available_workspaces: Vec<String>,
    #[serde(rename = "availableSources")]
    pub available_sources: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LedgerSummary {
    #[serde(rename = "totalCostUsd")]
    pub total_cost_usd: f64,
    #[serde(rename = "mainCostUsd")]
    pub main_cost_usd: f64,
    #[serde(rename = "subagentCostUsd")]
    pub subagent_cost_usd: f64,
    #[serde(rename = "totalSteps")]
    pub total_steps: u64,
    #[serde(rename = "rootSessionCount")]
    pub root_cascade_count: u64,
    #[serde(rename = "subagentSessionCount")]
    pub subagent_conversation_count: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "totalPromptTokens")]
    pub total_prompt_tokens: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    #[serde(rename = "avgSpeed")]
    pub avg_speed: Option<f64>,
    #[serde(rename = "avgTtftSec")]
    pub avg_ttft_sec: Option<f64>,
    #[serde(rename = "streamSamples")]
    pub stream_samples: u64,
    #[serde(rename = "ttftSamples")]
    pub ttft_samples: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimeseriesBucket {
    #[serde(rename = "bucketKey")]
    pub bucket_key: String,
    #[serde(rename = "bucketStartMs")]
    pub bucket_start_ms: i64,
    pub label: String,
    #[serde(rename = "costUsd")]
    pub cost_usd: f64,
    #[serde(rename = "mainCostUsd")]
    pub main_cost_usd: f64,
    #[serde(rename = "subagentCostUsd")]
    pub subagent_cost_usd: f64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "totalPromptTokens")]
    pub total_prompt_tokens: u64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    pub steps: u64,
    #[serde(rename = "avgSpeed")]
    pub avg_speed: Option<f64>,
    #[serde(rename = "avgTtftSec")]
    pub avg_ttft_sec: Option<f64>,
    #[serde(rename = "byModelCost")]
    pub by_model_cost: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelBreakdown {
    pub model: String,
    #[serde(rename = "costUsd")]
    pub cost_usd: f64,
    #[serde(rename = "costSharePct")]
    pub cost_share_pct: f64,
    pub steps: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "totalPromptTokens")]
    pub total_prompt_tokens: u64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    #[serde(rename = "avgSpeed")]
    pub avg_speed: Option<f64>,
    #[serde(rename = "avgTtftSec")]
    pub avg_ttft_sec: Option<f64>,
    #[serde(rename = "streamSamples")]
    pub stream_samples: u64,
    #[serde(rename = "ttftSamples")]
    pub ttft_samples: u64,
    #[serde(rename = "lastUsedMs")]
    pub last_used_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceBreakdown {
    pub source: LedgerSource,
    #[serde(rename = "costUsd")]
    pub cost_usd: f64,
    #[serde(rename = "costSharePct")]
    pub cost_share_pct: f64,
    pub steps: u64,
    #[serde(rename = "sessionCount")]
    pub conversation_count: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "totalPromptTokens")]
    pub total_prompt_tokens: u64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    #[serde(rename = "avgSpeed")]
    pub avg_speed: Option<f64>,
    #[serde(rename = "avgTtftSec")]
    pub avg_ttft_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChildConversationOverview {
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    pub source: LedgerSource,
    pub models: Vec<String>,
    pub steps: u64,
    #[serde(rename = "costUsd")]
    pub cost_usd: f64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    #[serde(rename = "avgSpeed")]
    pub avg_speed: Option<f64>,
    #[serde(rename = "avgTtftSec")]
    pub avg_ttft_sec: Option<f64>,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: i64,
    #[serde(rename = "lastActivityMs")]
    pub last_activity_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CascadeOverview {
    #[serde(rename = "rootConversationId")]
    pub root_conversation_id: String,
    pub source: LedgerSource,
    pub workspace: String,
    #[serde(rename = "workspaceLabel")]
    pub workspace_label: String,
    pub branch: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: i64,
    #[serde(rename = "lastActivityMs")]
    pub last_activity_ms: i64,
    #[serde(rename = "totalCostUsd")]
    pub total_cost_usd: f64,
    #[serde(rename = "mainCostUsd")]
    pub main_cost_usd: f64,
    #[serde(rename = "subagentCostUsd")]
    pub subagent_cost_usd: f64,
    #[serde(rename = "subagentCount")]
    pub subagent_count: u64,
    pub steps: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "totalPromptTokens")]
    pub total_prompt_tokens: u64,
    #[serde(rename = "cacheHitRate")]
    pub cache_hit_rate: f64,
    #[serde(rename = "avgSpeed")]
    pub avg_speed: Option<f64>,
    #[serde(rename = "avgTtftSec")]
    pub avg_ttft_sec: Option<f64>,
    pub models: Vec<String>,
    pub children: Vec<ChildConversationOverview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerOverview {
    pub meta: LedgerMeta,
    pub summary: LedgerSummary,
    pub timeseries: Vec<TimeseriesBucket>,
    #[serde(rename = "byModel")]
    pub by_model: Vec<ModelBreakdown>,
    #[serde(rename = "bySource")]
    pub by_source: Vec<SourceBreakdown>,
    #[serde(rename = "sessions")]
    pub cascades: Vec<CascadeOverview>,
}

// --- Local timezone helpers (cross-platform via chrono) ---

pub(super) fn local_datetime(ms: i64) -> DateTime<Local> {
    if let Some(dt) = Local.timestamp_millis_opt(ms).single() {
        return dt;
    }
    DateTime::from_timestamp_millis(ms)
        .unwrap_or(DateTime::UNIX_EPOCH)
        .with_timezone(&Local)
}

pub(super) fn local_ymd_hms_to_ms(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
    milli: u32,
) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let naive = date.and_hms_milli_opt(hour, min, sec, milli)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp_millis())
}

pub(super) fn local_date_key(ms: i64) -> String {
    let dt = local_datetime(ms);
    format!("{:04}{:02}{:02}", dt.year(), dt.month(), dt.day())
}

pub(super) fn local_today_bounds(now_ms: i64) -> (i64, i64) {
    let dt = local_datetime(now_ms);
    let start = local_ymd_hms_to_ms(dt.year(), dt.month(), dt.day(), 0, 0, 0, 0).unwrap_or(now_ms);
    let end =
        local_ymd_hms_to_ms(dt.year(), dt.month(), dt.day(), 23, 59, 59, 999).unwrap_or(now_ms);
    (start, end)
}

fn format_local_date_str(ms: i64) -> String {
    let dt = local_datetime(ms);
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}

fn parse_date_input(raw: Option<&str>, is_end: bool) -> Option<i64> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        return None;
    }
    if (10..=13).contains(&trimmed.len()) && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let num = trimmed.parse::<i64>().ok()?;
        return Some(if trimmed.len() == 10 { num * 1000 } else { num });
    }
    if trimmed.len() == 10 && trimmed.as_bytes()[4] == b'-' && trimmed.as_bytes()[7] == b'-' {
        let y = trimmed[0..4].parse::<i32>().ok()?;
        let m = trimmed[5..7].parse::<u32>().ok()?;
        let d = trimmed[8..10].parse::<u32>().ok()?;
        return if is_end {
            local_ymd_hms_to_ms(y, m, d, 23, 59, 59, 999)
        } else {
            local_ymd_hms_to_ms(y, m, d, 0, 0, 0, 0)
        };
    }
    DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

struct ResolvedRange {
    range: LedgerTimeRange,
    from_ms: i64,
    to_ms: i64,
}

fn resolve_time_range(filter: &LedgerQueryFilter, now_ms: i64) -> ResolvedRange {
    let parsed_from = parse_date_input(filter.from.as_deref(), false);
    let parsed_to = parse_date_input(filter.to.as_deref(), true);
    if parsed_from.is_some() || parsed_to.is_some() {
        return ResolvedRange {
            range: LedgerTimeRange::Custom,
            from_ms: parsed_from.unwrap_or(0),
            to_ms: parsed_to.unwrap_or(now_ms),
        };
    }

    let dt = local_datetime(now_ms);
    match filter.range {
        LedgerTimeRange::Today => {
            let (from_ms, to_ms) = local_today_bounds(now_ms);
            ResolvedRange {
                range: LedgerTimeRange::Today,
                from_ms,
                to_ms,
            }
        }
        LedgerTimeRange::Last24h => ResolvedRange {
            range: LedgerTimeRange::Last24h,
            from_ms: now_ms - DAY_MS,
            to_ms: now_ms,
        },
        LedgerTimeRange::Last30d => ResolvedRange {
            range: LedgerTimeRange::Last30d,
            from_ms: now_ms - 30 * DAY_MS,
            to_ms: now_ms,
        },
        LedgerTimeRange::ThisMonth => {
            let from_ms = local_ymd_hms_to_ms(dt.year(), dt.month(), 1, 0, 0, 0, 0).unwrap_or(0);
            ResolvedRange {
                range: LedgerTimeRange::ThisMonth,
                from_ms,
                to_ms: now_ms,
            }
        }
        LedgerTimeRange::All => ResolvedRange {
            range: LedgerTimeRange::All,
            from_ms: 0,
            to_ms: now_ms,
        },
        LedgerTimeRange::Custom => ResolvedRange {
            range: LedgerTimeRange::Custom,
            from_ms: now_ms - 7 * DAY_MS,
            to_ms: now_ms,
        },
        LedgerTimeRange::Last7d => ResolvedRange {
            range: LedgerTimeRange::Last7d,
            from_ms: now_ms - 7 * DAY_MS,
            to_ms: now_ms,
        },
    }
}

// --- Internal row loading & transitive Cascade lineage resolution ---

#[derive(Debug, Clone)]
pub(super) struct ConvLineageMeta {
    pub workspace: Option<String>,
    pub branch: Option<String>,
    pub parent_conversation_id: Option<String>,
    pub root_conversation_id: Option<String>,
    pub source: LedgerSource,
}

#[derive(Debug, Clone)]
pub(super) struct StoredStepRow {
    pub conversation_id: String,
    pub ts_ms: i64,
    pub model: String,
    pub raw_model: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub stream_sec: Option<f64>,
    pub ttft_sec: Option<f64>,
    pub valid_stream: bool,
    pub valid_ttft: bool,
    pub workspace: Option<String>,
    pub branch: Option<String>,
    pub source: LedgerSource,
}

pub(super) fn load_ledger_rows(
    conn: &Connection,
) -> Result<(HashMap<String, ConvLineageMeta>, Vec<StoredStepRow>), LedgerError> {
    let mut conv_meta: HashMap<String, ConvLineageMeta> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT conversation_id, workspace, branch, parent_conversation_id, root_conversation_id, source
             FROM ingested_dbs",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let cid: String = row.get(0)?;
            if cid.is_empty() {
                continue;
            }
            let src_raw: String = row.get(5)?;
            conv_meta.insert(
                cid,
                ConvLineageMeta {
                    workspace: row.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()),
                    branch: row.get::<_, Option<String>>(2)?.filter(|s| !s.is_empty()),
                    parent_conversation_id: row
                        .get::<_, Option<String>>(3)?
                        .filter(|s| !s.is_empty()),
                    root_conversation_id: row.get::<_, Option<String>>(4)?.filter(|s| !s.is_empty()),
                    source: LedgerSource::from_raw(&src_raw),
                },
            );
        }
    }

    let mut steps = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT conversation_id, ts_ms, model, raw_model,
                    input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                    cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
                    parent_conversation_id, root_conversation_id, workspace, branch, source
             FROM telemetry_steps",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let conversation_id: String = row.get(0)?;
            let ts_ms: i64 = row.get(1)?;
            let model: String = row.get(2)?;
            let raw_model: Option<String> = row.get(3)?;
            let input_tokens = row.get::<_, i64>(4)?.max(0) as u64;
            let cache_read_tokens = row.get::<_, i64>(5)?.max(0) as u64;
            let cache_write_tokens = row.get::<_, i64>(6)?.max(0) as u64;
            let output_tokens = row.get::<_, i64>(7)?.max(0) as u64;
            let raw_cost: f64 = row.get(8)?;
            let cost_usd = if raw_cost.is_finite() && raw_cost >= 0.0 {
                raw_cost
            } else {
                0.0
            };
            if input_tokens == 0
                && cache_read_tokens == 0
                && cache_write_tokens == 0
                && output_tokens == 0
                && cost_usd <= 0.0
            {
                continue;
            }
            let stream_sec: Option<f64> = row.get(9)?;
            let ttft_sec: Option<f64> = row.get(10)?;
            let valid_stream = row.get::<_, i64>(11)? != 0;
            let valid_ttft = row.get::<_, i64>(12)? != 0;
            let parent_conversation_id = row.get::<_, Option<String>>(13)?.filter(|s| !s.is_empty());
            let root_conversation_id = row.get::<_, Option<String>>(14)?.filter(|s| !s.is_empty());
            let workspace = row.get::<_, Option<String>>(15)?.filter(|s| !s.is_empty());
            let branch = row.get::<_, Option<String>>(16)?.filter(|s| !s.is_empty());
            let src_raw: String = row.get(17)?;
            let source = LedgerSource::from_raw(&src_raw);

            conv_meta
                .entry(conversation_id.clone())
                .and_modify(|existing| {
                    if existing.workspace.is_none() && workspace.is_some() {
                        existing.workspace = workspace.clone();
                    }
                    if existing.branch.is_none() && branch.is_some() {
                        existing.branch = branch.clone();
                    }
                    if existing.parent_conversation_id.is_none() && parent_conversation_id.is_some() {
                        existing.parent_conversation_id = parent_conversation_id.clone();
                    }
                    if existing.root_conversation_id.is_none() && root_conversation_id.is_some() {
                        existing.root_conversation_id = root_conversation_id.clone();
                    }
                })
                .or_insert_with(|| ConvLineageMeta {
                    workspace: workspace.clone(),
                    branch: branch.clone(),
                    parent_conversation_id: parent_conversation_id.clone(),
                    root_conversation_id: root_conversation_id.clone(),
                    source,
                });

            steps.push(StoredStepRow {
                conversation_id,
                ts_ms,
                raw_model: raw_model.unwrap_or_else(|| model.clone()),
                model,
                input_tokens,
                cache_read_tokens,
                cache_write_tokens,
                output_tokens,
                cost_usd,
                stream_sec,
                ttft_sec,
                valid_stream,
                valid_ttft,
                workspace,
                branch,
                source,
            });
        }
    }

    Ok((conv_meta, steps))
}

pub(super) fn resolve_root_conversation_id(
    cid: &str,
    conv_meta: &HashMap<String, ConvLineageMeta>,
    cache: &mut HashMap<String, String>,
) -> String {
    if let Some(cached) = cache.get(cid) {
        return cached.clone();
    }
    let mut cur = cid.to_string();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(cur.clone());
    for _ in 0..32 {
        let Some(meta) = conv_meta.get(&cur) else {
            break;
        };
        let candidate = meta
            .root_conversation_id
            .as_deref()
            .filter(|id| *id != cur)
            .or_else(|| {
                meta.parent_conversation_id
                    .as_deref()
                    .filter(|id| *id != cur)
            });
        let Some(next) = candidate else {
            break;
        };
        if !visited.insert(next.to_string()) {
            break;
        }
        cur = next.to_string();
    }
    cache.insert(cid.to_string(), cur.clone());
    cur
}

fn get_workspace_label(workspace: Option<&str>) -> String {
    let Some(ws) = workspace else {
        return String::new();
    };
    let trimmed = ws.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return if ws.starts_with('/') {
            "/".to_string()
        } else {
            String::new()
        };
    }
    match trimmed.rfind(['/', '\\']) {
        Some(idx) => trimmed[idx + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

fn resolve_granularity(requested: LedgerGranularity, span_ms: i64) -> LedgerGranularity {
    match requested {
        LedgerGranularity::Hour | LedgerGranularity::Day | LedgerGranularity::Week => requested,
        LedgerGranularity::Auto => {
            if span_ms <= 48 * HOUR_MS {
                LedgerGranularity::Hour
            } else if span_ms <= 90 * DAY_MS {
                LedgerGranularity::Day
            } else {
                LedgerGranularity::Week
            }
        }
    }
}

struct BucketInfo {
    bucket_key: String,
    bucket_start_ms: i64,
    label: String,
}

fn get_bucket_info(ts_ms: i64, granularity: LedgerGranularity) -> BucketInfo {
    let dt = local_datetime(ts_ms);
    let y = dt.year();
    let m = dt.month();
    let d = dt.day();

    match granularity {
        LedgerGranularity::Hour => {
            let hh = dt.hour();
            let start_ms = local_ymd_hms_to_ms(y, m, d, hh, 0, 0, 0).unwrap_or(ts_ms);
            BucketInfo {
                bucket_key: format!("{y:04}-{m:02}-{d:02} {hh:02}:00"),
                bucket_start_ms: start_ms,
                label: format!("{m:02}-{d:02} {hh:02}:00"),
            }
        }
        LedgerGranularity::Week => {
            let offset_days = i64::from(dt.weekday().num_days_from_monday());
            let mon_date = dt.date_naive() - chrono::Duration::days(offset_days);
            let my = mon_date.year();
            let mm = mon_date.month();
            let md = mon_date.day();
            let start_ms = local_ymd_hms_to_ms(my, mm, md, 0, 0, 0, 0).unwrap_or(ts_ms);
            BucketInfo {
                bucket_key: format!("{my:04}-{mm:02}-{md:02}"),
                bucket_start_ms: start_ms,
                label: format!("W {mm:02}-{md:02}"),
            }
        }
        LedgerGranularity::Day | LedgerGranularity::Auto => {
            let start_ms = local_ymd_hms_to_ms(y, m, d, 0, 0, 0, 0).unwrap_or(ts_ms);
            BucketInfo {
                bucket_key: format!("{y:04}-{m:02}-{d:02}"),
                bucket_start_ms: start_ms,
                label: format!("{m:02}-{d:02}"),
            }
        }
    }
}

#[derive(Default)]
struct PerfAcc {
    cost_usd: f64,
    main_cost_usd: f64,
    subagent_cost_usd: f64,
    steps: u64,
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
    stream_samples: u64,
    sum_out: u64,
    sum_stream_sec: f64,
    ttft_samples: u64,
    sum_ttft_sec: f64,
}

impl PerfAcc {
    fn accumulate(&mut self, step: &StoredStepRow, is_subagent: bool) {
        self.cost_usd += step.cost_usd;
        if is_subagent {
            self.subagent_cost_usd += step.cost_usd;
        } else {
            self.main_cost_usd += step.cost_usd;
        }
        self.steps += 1;
        self.input_tokens = self.input_tokens.saturating_add(step.input_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(step.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(step.cache_write_tokens);
        self.output_tokens = self.output_tokens.saturating_add(step.output_tokens);

        if step.valid_stream
            && step.output_tokens > 0
            && let Some(sec) = step.stream_sec
            && sec.is_finite()
            && (MIN_VALID_STREAM_SEC..=MAX_VALID_STREAM_SEC).contains(&sec)
            && (step.output_tokens as f64 / sec) <= MAX_VALID_STREAM_SPEED
        {
            self.stream_samples += 1;
            self.sum_out = self.sum_out.saturating_add(step.output_tokens);
            self.sum_stream_sec += sec;
        }

        if step.valid_ttft
            && let Some(ttft) = step.ttft_sec
            && ttft.is_finite()
            && (0.0..=MAX_VALID_TTFT_SEC).contains(&ttft)
        {
            self.ttft_samples += 1;
            self.sum_ttft_sec += ttft;
        }
    }

    fn finalize(&self) -> FinalizedMetrics {
        let total_prompt_tokens = self
            .input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens);
        let total_tokens = total_prompt_tokens.saturating_add(self.output_tokens);
        let cache_hit_rate = if total_prompt_tokens > 0 {
            ((self.cache_read_tokens as f64 / total_prompt_tokens as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let raw_speed = if self.sum_stream_sec > 0.0 && self.sum_out > 0 {
            Some(self.sum_out as f64 / self.sum_stream_sec)
        } else {
            None
        };
        let avg_speed = raw_speed.filter(|s| s.is_finite() && *s <= MAX_VALID_STREAM_SPEED);
        let avg_ttft_sec = if self.ttft_samples > 0 {
            Some(self.sum_ttft_sec / self.ttft_samples as f64)
        } else {
            None
        };
        FinalizedMetrics {
            total_prompt_tokens,
            total_tokens,
            cache_hit_rate,
            avg_speed,
            avg_ttft_sec,
        }
    }
}

struct FinalizedMetrics {
    total_prompt_tokens: u64,
    total_tokens: u64,
    cache_hit_rate: f64,
    avg_speed: Option<f64>,
    avg_ttft_sec: Option<f64>,
}

struct BucketAcc {
    bucket_key: String,
    bucket_start_ms: i64,
    label: String,
    acc: PerfAcc,
    by_model_cost: BTreeMap<String, f64>,
}

struct ModelAcc {
    model: String,
    acc: PerfAcc,
    last_used_ms: i64,
}

struct SourceAcc {
    source: LedgerSource,
    acc: PerfAcc,
    root_ids: HashSet<String>,
}

struct ChildConvAcc {
    conversation_id: String,
    source: LedgerSource,
    started_at_ms: i64,
    last_activity_ms: i64,
    acc: PerfAcc,
    models_cost: HashMap<String, f64>,
}

struct CascadeAcc {
    root_conversation_id: String,
    source: LedgerSource,
    workspace: String,
    workspace_label: String,
    branch: String,
    started_at_ms: i64,
    last_activity_ms: i64,
    acc: PerfAcc,
    models_cost: HashMap<String, f64>,
    children: HashMap<String, ChildConvAcc>,
}

fn sort_model_list(models_cost: &HashMap<String, f64>) -> Vec<String> {
    let mut entries: Vec<(&String, &f64)> = models_cost.iter().collect();
    entries.sort_by(|a, b| {
        b.1.partial_cmp(a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    entries.into_iter().map(|(name, _)| name.clone()).collect()
}

pub(super) fn query_overview(
    conn: &Connection,
    ledger_path: &Path,
    filter: &LedgerQueryFilter,
    default_now_ms: i64,
) -> Result<LedgerOverview, LedgerError> {
    let now_ms = filter.now_ms.unwrap_or(default_now_ms);
    let (conv_meta, steps) = load_ledger_rows(conn)?;
    let time_range = resolve_time_range(filter, now_ms);
    let mut root_cache: HashMap<String, String> = HashMap::new();

    let mut available_models_set: BTreeSet<String> = BTreeSet::new();
    let mut available_workspaces_set: BTreeSet<String> = BTreeSet::new();
    let mut available_sources_set: HashSet<LedgerSource> = HashSet::new();
    let mut all_convs_set: HashSet<String> = HashSet::new();
    let mut min_recorded_ts = i64::MAX;
    let mut max_recorded_ts = i64::MIN;

    for step in &steps {
        if !step.model.is_empty() {
            available_models_set.insert(step.model.clone());
        }
        if !step.conversation_id.is_empty() {
            all_convs_set.insert(step.conversation_id.clone());
        }
        available_sources_set.insert(step.source);
        let root_id = resolve_root_conversation_id(&step.conversation_id, &conv_meta, &mut root_cache);
        let ws = step
            .workspace
            .as_deref()
            .or_else(|| conv_meta.get(&step.conversation_id).and_then(|m| m.workspace.as_deref()))
            .or_else(|| conv_meta.get(&root_id).and_then(|m| m.workspace.as_deref()));
        if let Some(w) = ws {
            available_workspaces_set.insert(w.to_string());
        }
        if step.ts_ms < min_recorded_ts {
            min_recorded_ts = step.ts_ms;
        }
        if step.ts_ms > max_recorded_ts {
            max_recorded_ts = step.ts_ms;
        }
    }

    let mut available_sources = Vec::new();
    if available_sources_set.contains(&LedgerSource::Cli) {
        available_sources.push("cli".to_string());
    }
    if available_sources_set.contains(&LedgerSource::Ide) {
        available_sources.push("ide".to_string());
    }

    let is_unbounded_all = time_range.range == LedgerTimeRange::All
        && filter.from.is_none()
        && filter.to.is_none();
    let span_ms = if is_unbounded_all {
        if max_recorded_ts > min_recorded_ts {
            max_recorded_ts - min_recorded_ts
        } else {
            7 * DAY_MS
        }
    } else {
        (time_range.to_ms - time_range.from_ms).max(0)
    };
    let granularity = resolve_granularity(filter.granularity, span_ms);

    let model_filter_raw = filter.model.as_deref().map(str::trim).unwrap_or("");
    let has_model_filter =
        !model_filter_raw.is_empty() && !model_filter_raw.eq_ignore_ascii_case("all");
    let model_filter_norm = if has_model_filter {
        let compacted = compact_model_name(model_filter_raw);
        if compacted.is_empty() {
            model_filter_raw.to_ascii_lowercase()
        } else {
            compacted.to_ascii_lowercase()
        }
    } else {
        String::new()
    };

    let ws_filter_raw = filter.workspace.as_deref().map(str::trim).unwrap_or("");
    let has_ws_filter = !ws_filter_raw.is_empty() && !ws_filter_raw.eq_ignore_ascii_case("all");

    let mut summary_acc = PerfAcc::default();
    let mut bucket_map: HashMap<String, BucketAcc> = HashMap::new();
    let mut model_map: HashMap<String, ModelAcc> = HashMap::new();
    let mut source_map: HashMap<LedgerSource, SourceAcc> = HashMap::new();
    let mut cascade_map: HashMap<String, CascadeAcc> = HashMap::new();
    let mut matched_subagent_ids: HashSet<String> = HashSet::new();

    for step in &steps {
        if !is_unbounded_all && (step.ts_ms < time_range.from_ms || step.ts_ms > time_range.to_ms) {
            continue;
        }
        match filter.source {
            LedgerSourceFilter::Cli if step.source != LedgerSource::Cli => continue,
            LedgerSourceFilter::Ide if step.source != LedgerSource::Ide => continue,
            _ => {}
        }

        let root_id = resolve_root_conversation_id(&step.conversation_id, &conv_meta, &mut root_cache);
        let is_subagent = root_id != step.conversation_id;
        if !filter.include_subagents && is_subagent {
            continue;
        }

        if has_model_filter {
            let step_model_lower = step.model.to_ascii_lowercase();
            let raw_model_lower = step.raw_model.to_ascii_lowercase();
            if step_model_lower != model_filter_norm
                && step_model_lower != model_filter_raw.to_ascii_lowercase()
                && raw_model_lower != model_filter_raw.to_ascii_lowercase()
            {
                continue;
            }
        }

        let conv_m = conv_meta.get(&step.conversation_id);
        let root_m = conv_meta.get(&root_id);
        let effective_ws = step
            .workspace
            .as_deref()
            .or_else(|| conv_m.and_then(|m| m.workspace.as_deref()))
            .or_else(|| root_m.and_then(|m| m.workspace.as_deref()));
        let effective_branch = step
            .branch
            .as_deref()
            .or_else(|| conv_m.and_then(|m| m.branch.as_deref()))
            .or_else(|| root_m.and_then(|m| m.branch.as_deref()));

        if has_ws_filter {
            let ws_label = get_workspace_label(effective_ws);
            if effective_ws != Some(ws_filter_raw) && ws_label != ws_filter_raw {
                continue;
            }
        }

        summary_acc.accumulate(step, is_subagent);
        if is_subagent {
            matched_subagent_ids.insert(step.conversation_id.clone());
        }

        let b_info = get_bucket_info(step.ts_ms, granularity);
        let bucket = bucket_map
            .entry(b_info.bucket_key.clone())
            .or_insert_with(|| BucketAcc {
                bucket_key: b_info.bucket_key,
                bucket_start_ms: b_info.bucket_start_ms,
                label: b_info.label,
                acc: PerfAcc::default(),
                by_model_cost: BTreeMap::new(),
            });
        bucket.acc.accumulate(step, is_subagent);
        *bucket.by_model_cost.entry(step.model.clone()).or_insert(0.0) += step.cost_usd;

        let model_entry = model_map
            .entry(step.model.clone())
            .or_insert_with(|| ModelAcc {
                model: step.model.clone(),
                acc: PerfAcc::default(),
                last_used_ms: 0,
            });
        model_entry.acc.accumulate(step, is_subagent);
        if step.ts_ms > model_entry.last_used_ms {
            model_entry.last_used_ms = step.ts_ms;
        }

        let source_entry = source_map
            .entry(step.source)
            .or_insert_with(|| SourceAcc {
                source: step.source,
                acc: PerfAcc::default(),
                root_ids: HashSet::new(),
            });
        source_entry.acc.accumulate(step, is_subagent);
        source_entry.root_ids.insert(root_id.clone());

        let root_cascade = cascade_map.entry(root_id.clone()).or_insert_with(|| {
            let root_ws = root_m.and_then(|m| m.workspace.as_deref()).or(effective_ws);
            let root_br = root_m.and_then(|m| m.branch.as_deref()).or(effective_branch);
            let root_src = root_m.map(|m| m.source).unwrap_or(step.source);
            CascadeAcc {
                root_conversation_id: root_id.clone(),
                source: root_src,
                workspace: root_ws.unwrap_or("").to_string(),
                workspace_label: get_workspace_label(root_ws),
                branch: root_br.unwrap_or("").to_string(),
                started_at_ms: step.ts_ms,
                last_activity_ms: step.ts_ms,
                acc: PerfAcc::default(),
                models_cost: HashMap::new(),
                children: HashMap::new(),
            }
        });
        if step.ts_ms < root_cascade.started_at_ms {
            root_cascade.started_at_ms = step.ts_ms;
        }
        if step.ts_ms > root_cascade.last_activity_ms {
            root_cascade.last_activity_ms = step.ts_ms;
        }
        root_cascade.acc.accumulate(step, is_subagent);
        *root_cascade
            .models_cost
            .entry(step.model.clone())
            .or_insert(0.0) += step.cost_usd;

        if is_subagent {
            let child = root_cascade
                .children
                .entry(step.conversation_id.clone())
                .or_insert_with(|| {
                    let child_src = conv_m.map(|m| m.source).unwrap_or(step.source);
                    ChildConvAcc {
                        conversation_id: step.conversation_id.clone(),
                        source: child_src,
                        started_at_ms: step.ts_ms,
                        last_activity_ms: step.ts_ms,
                        acc: PerfAcc::default(),
                        models_cost: HashMap::new(),
                    }
                });
            if step.ts_ms < child.started_at_ms {
                child.started_at_ms = step.ts_ms;
            }
            if step.ts_ms > child.last_activity_ms {
                child.last_activity_ms = step.ts_ms;
            }
            child.acc.accumulate(step, true);
            *child.models_cost.entry(step.model.clone()).or_insert(0.0) += step.cost_usd;
        }
    }

    let summary_fin = summary_acc.finalize();

    let mut buckets: Vec<BucketAcc> = bucket_map.into_values().collect();
    buckets.sort_by_key(|b| b.bucket_start_ms);
    let timeseries: Vec<TimeseriesBucket> = buckets
        .into_iter()
        .map(|b| {
            let fin = b.acc.finalize();
            TimeseriesBucket {
                bucket_key: b.bucket_key,
                bucket_start_ms: b.bucket_start_ms,
                label: b.label,
                cost_usd: b.acc.cost_usd,
                main_cost_usd: b.acc.main_cost_usd,
                subagent_cost_usd: b.acc.subagent_cost_usd,
                input_tokens: b.acc.input_tokens,
                cache_read_tokens: b.acc.cache_read_tokens,
                cache_write_tokens: b.acc.cache_write_tokens,
                output_tokens: b.acc.output_tokens,
                total_prompt_tokens: fin.total_prompt_tokens,
                cache_hit_rate: fin.cache_hit_rate,
                steps: b.acc.steps,
                avg_speed: fin.avg_speed,
                avg_ttft_sec: fin.avg_ttft_sec,
                by_model_cost: b.by_model_cost,
            }
        })
        .collect();

    let mut models_vec: Vec<ModelAcc> = model_map.into_values().collect();
    models_vec.sort_by(|a, b| {
        b.acc
            .cost_usd
            .partial_cmp(&a.acc.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.acc.steps.cmp(&a.acc.steps))
            .then_with(|| a.model.cmp(&b.model))
    });
    let by_model: Vec<ModelBreakdown> = models_vec
        .into_iter()
        .map(|m| {
            let fin = m.acc.finalize();
            let cost_share_pct = if summary_acc.cost_usd > 0.0 {
                (m.acc.cost_usd / summary_acc.cost_usd) * 100.0
            } else {
                0.0
            };
            ModelBreakdown {
                model: m.model,
                cost_usd: m.acc.cost_usd,
                cost_share_pct,
                steps: m.acc.steps,
                input_tokens: m.acc.input_tokens,
                cache_read_tokens: m.acc.cache_read_tokens,
                cache_write_tokens: m.acc.cache_write_tokens,
                output_tokens: m.acc.output_tokens,
                total_prompt_tokens: fin.total_prompt_tokens,
                cache_hit_rate: fin.cache_hit_rate,
                avg_speed: fin.avg_speed,
                avg_ttft_sec: fin.avg_ttft_sec,
                stream_samples: m.acc.stream_samples,
                ttft_samples: m.acc.ttft_samples,
                last_used_ms: m.last_used_ms,
            }
        })
        .collect();

    let mut sources_vec: Vec<SourceAcc> = source_map.into_values().collect();
    sources_vec.sort_by(|a, b| {
        b.acc
            .cost_usd
            .partial_cmp(&a.acc.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.acc.steps.cmp(&a.acc.steps))
            .then_with(|| a.source.as_str().cmp(b.source.as_str()))
    });
    let by_source: Vec<SourceBreakdown> = sources_vec
        .into_iter()
        .map(|s| {
            let fin = s.acc.finalize();
            let cost_share_pct = if summary_acc.cost_usd > 0.0 {
                (s.acc.cost_usd / summary_acc.cost_usd) * 100.0
            } else {
                0.0
            };
            SourceBreakdown {
                source: s.source,
                cost_usd: s.acc.cost_usd,
                cost_share_pct,
                steps: s.acc.steps,
                conversation_count: s.root_ids.len() as u64,
                input_tokens: s.acc.input_tokens,
                cache_read_tokens: s.acc.cache_read_tokens,
                cache_write_tokens: s.acc.cache_write_tokens,
                output_tokens: s.acc.output_tokens,
                total_tokens: fin.total_tokens,
                total_prompt_tokens: fin.total_prompt_tokens,
                cache_hit_rate: fin.cache_hit_rate,
                avg_speed: fin.avg_speed,
                avg_ttft_sec: fin.avg_ttft_sec,
            }
        })
        .collect();

    let mut cascades_vec: Vec<CascadeAcc> = cascade_map.into_values().collect();
    cascades_vec.sort_by(|a, b| {
        b.last_activity_ms
            .cmp(&a.last_activity_ms)
            .then_with(|| {
                b.acc
                    .cost_usd
                    .partial_cmp(&a.acc.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let cascades: Vec<CascadeOverview> = cascades_vec
        .into_iter()
        .map(|c| {
            let fin = c.acc.finalize();
            let mut child_list: Vec<ChildConvAcc> = c.children.into_values().collect();
            child_list.sort_by(|a, b| {
                a.started_at_ms.cmp(&b.started_at_ms).then_with(|| {
                    b.acc
                        .cost_usd
                        .partial_cmp(&a.acc.cost_usd)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            });
            let children: Vec<ChildConversationOverview> = child_list
                .into_iter()
                .map(|ch| {
                    let cfin = ch.acc.finalize();
                    ChildConversationOverview {
                        conversation_id: ch.conversation_id,
                        source: ch.source,
                        models: sort_model_list(&ch.models_cost),
                        steps: ch.acc.steps,
                        cost_usd: ch.acc.cost_usd,
                        input_tokens: ch.acc.input_tokens,
                        cache_read_tokens: ch.acc.cache_read_tokens,
                        cache_write_tokens: ch.acc.cache_write_tokens,
                        output_tokens: ch.acc.output_tokens,
                        cache_hit_rate: cfin.cache_hit_rate,
                        avg_speed: cfin.avg_speed,
                        avg_ttft_sec: cfin.avg_ttft_sec,
                        started_at_ms: ch.started_at_ms,
                        last_activity_ms: ch.last_activity_ms,
                    }
                })
                .collect();

            CascadeOverview {
                root_conversation_id: c.root_conversation_id,
                source: c.source,
                workspace: c.workspace,
                workspace_label: c.workspace_label,
                branch: c.branch,
                started_at_ms: c.started_at_ms,
                last_activity_ms: c.last_activity_ms,
                total_cost_usd: c.acc.cost_usd,
                main_cost_usd: c.acc.main_cost_usd,
                subagent_cost_usd: c.acc.subagent_cost_usd,
                subagent_count: children.len() as u64,
                steps: c.acc.steps,
                input_tokens: c.acc.input_tokens,
                cache_read_tokens: c.acc.cache_read_tokens,
                cache_write_tokens: c.acc.cache_write_tokens,
                output_tokens: c.acc.output_tokens,
                total_prompt_tokens: fin.total_prompt_tokens,
                cache_hit_rate: fin.cache_hit_rate,
                avg_speed: fin.avg_speed,
                avg_ttft_sec: fin.avg_ttft_sec,
                models: sort_model_list(&c.models_cost),
                children,
            }
        })
        .collect();

    let from_ms = if is_unbounded_all && min_recorded_ts != i64::MAX {
        min_recorded_ts
    } else {
        time_range.from_ms
    };
    let to_ms = if is_unbounded_all && max_recorded_ts != i64::MIN {
        time_range.to_ms.max(max_recorded_ts)
    } else {
        time_range.to_ms
    };

    let root_cascade_count = cascades.len() as u64;
    let summary = LedgerSummary {
        total_cost_usd: summary_acc.cost_usd,
        main_cost_usd: summary_acc.main_cost_usd,
        subagent_cost_usd: summary_acc.subagent_cost_usd,
        total_steps: summary_acc.steps,
        root_cascade_count,
        subagent_conversation_count: matched_subagent_ids.len() as u64,
        input_tokens: summary_acc.input_tokens,
        cache_read_tokens: summary_acc.cache_read_tokens,
        cache_write_tokens: summary_acc.cache_write_tokens,
        output_tokens: summary_acc.output_tokens,
        total_prompt_tokens: summary_fin.total_prompt_tokens,
        total_tokens: summary_fin.total_tokens,
        cache_hit_rate: summary_fin.cache_hit_rate,
        avg_speed: summary_fin.avg_speed,
        avg_ttft_sec: summary_fin.avg_ttft_sec,
        stream_samples: summary_acc.stream_samples,
        ttft_samples: summary_acc.ttft_samples,
    };

    let meta = LedgerMeta {
        range: time_range.range.as_str().to_string(),
        source: filter.source.as_str().to_string(),
        from_ms,
        to_ms,
        granularity: granularity.as_str().to_string(),
        ledger_path: ledger_path.to_string_lossy().into_owned(),
        backend: "sqlite".to_string(),
        total_recorded_steps: steps.len() as u64,
        total_recorded_conversations: all_convs_set.len() as u64,
        available_models: available_models_set.into_iter().collect(),
        available_workspaces: available_workspaces_set.into_iter().collect(),
        available_sources,
    };

    Ok(LedgerOverview {
        meta,
        summary,
        timeseries,
        by_model,
        by_source,
        cascades,
    })
}

fn csv_cell(val: &str) -> String {
    if val.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", val.replace('"', "\"\""))
    } else {
        val.to_string()
    }
}

fn csv_row(cells: &[String]) -> String {
    let escaped: Vec<String> = cells.iter().map(|c| csv_cell(c)).collect();
    escaped.join(",")
}

fn fmt_num_csv(val: Option<f64>, digits: usize) -> String {
    match val {
        Some(v) if v.is_finite() => {
            let formatted = format!("{v:.digits$}");
            // Match JS `Number(val.toFixed(digits))` which strips trailing decimal zeros
            if formatted.contains('.') {
                formatted
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string()
            } else {
                formatted
            }
        }
        _ => String::new(),
    }
}

pub(super) fn format_ledger_csv(overview: &LedgerOverview, kind: CsvExportKind) -> String {
    let mut rows: Vec<String> = Vec::new();
    match kind {
        CsvExportKind::Models => {
            rows.push(csv_row(&[
                "model".to_string(),
                "costUsd".to_string(),
                "costSharePct".to_string(),
                "steps".to_string(),
                "inputTokens".to_string(),
                "cacheReadTokens".to_string(),
                "cacheWriteTokens".to_string(),
                "outputTokens".to_string(),
                "totalPromptTokens".to_string(),
                "cacheHitRate".to_string(),
                "avgSpeed".to_string(),
                "avgTtftSec".to_string(),
                "lastUsedMs".to_string(),
            ]));
            for m in &overview.by_model {
                rows.push(csv_row(&[
                    m.model.clone(),
                    fmt_num_csv(Some(m.cost_usd), 6),
                    fmt_num_csv(Some(m.cost_share_pct), 2),
                    m.steps.to_string(),
                    m.input_tokens.to_string(),
                    m.cache_read_tokens.to_string(),
                    m.cache_write_tokens.to_string(),
                    m.output_tokens.to_string(),
                    m.total_prompt_tokens.to_string(),
                    fmt_num_csv(Some(m.cache_hit_rate), 2),
                    fmt_num_csv(m.avg_speed, 2),
                    fmt_num_csv(m.avg_ttft_sec, 3),
                    m.last_used_ms.to_string(),
                ]));
            }
        }
        CsvExportKind::Cascades => {
            rows.push(csv_row(&[
                "rootConversationId".to_string(),
                "source".to_string(),
                "workspace".to_string(),
                "workspaceLabel".to_string(),
                "branch".to_string(),
                "startedAtMs".to_string(),
                "lastActivityMs".to_string(),
                "totalCostUsd".to_string(),
                "mainCostUsd".to_string(),
                "subagentCostUsd".to_string(),
                "subagentCount".to_string(),
                "steps".to_string(),
                "inputTokens".to_string(),
                "cacheReadTokens".to_string(),
                "cacheWriteTokens".to_string(),
                "outputTokens".to_string(),
                "totalPromptTokens".to_string(),
                "cacheHitRate".to_string(),
                "avgSpeed".to_string(),
                "avgTtftSec".to_string(),
                "models".to_string(),
            ]));
            for c in &overview.cascades {
                rows.push(csv_row(&[
                    c.root_conversation_id.clone(),
                    c.source.as_str().to_string(),
                    c.workspace.clone(),
                    c.workspace_label.clone(),
                    c.branch.clone(),
                    c.started_at_ms.to_string(),
                    c.last_activity_ms.to_string(),
                    fmt_num_csv(Some(c.total_cost_usd), 6),
                    fmt_num_csv(Some(c.main_cost_usd), 6),
                    fmt_num_csv(Some(c.subagent_cost_usd), 6),
                    c.subagent_count.to_string(),
                    c.steps.to_string(),
                    c.input_tokens.to_string(),
                    c.cache_read_tokens.to_string(),
                    c.cache_write_tokens.to_string(),
                    c.output_tokens.to_string(),
                    c.total_prompt_tokens.to_string(),
                    fmt_num_csv(Some(c.cache_hit_rate), 2),
                    fmt_num_csv(c.avg_speed, 2),
                    fmt_num_csv(c.avg_ttft_sec, 3),
                    c.models.join(";"),
                ]));
            }
        }
        CsvExportKind::Timeseries => {
            rows.push(csv_row(&[
                "bucketKey".to_string(),
                "bucketStartMs".to_string(),
                "label".to_string(),
                "costUsd".to_string(),
                "mainCostUsd".to_string(),
                "subagentCostUsd".to_string(),
                "steps".to_string(),
                "inputTokens".to_string(),
                "cacheReadTokens".to_string(),
                "cacheWriteTokens".to_string(),
                "outputTokens".to_string(),
                "totalPromptTokens".to_string(),
                "cacheHitRate".to_string(),
                "avgSpeed".to_string(),
                "avgTtftSec".to_string(),
            ]));
            for b in &overview.timeseries {
                rows.push(csv_row(&[
                    b.bucket_key.clone(),
                    b.bucket_start_ms.to_string(),
                    b.label.clone(),
                    fmt_num_csv(Some(b.cost_usd), 6),
                    fmt_num_csv(Some(b.main_cost_usd), 6),
                    fmt_num_csv(Some(b.subagent_cost_usd), 6),
                    b.steps.to_string(),
                    b.input_tokens.to_string(),
                    b.cache_read_tokens.to_string(),
                    b.cache_write_tokens.to_string(),
                    b.output_tokens.to_string(),
                    b.total_prompt_tokens.to_string(),
                    fmt_num_csv(Some(b.cache_hit_rate), 2),
                    fmt_num_csv(b.avg_speed, 2),
                    fmt_num_csv(b.avg_ttft_sec, 3),
                ]));
            }
        }
    }
    format!("{}\n", rows.join("\n"))
}

pub(super) fn format_date_short(ms: i64) -> String {
    if ms <= 0 {
        return "--".to_string();
    }
    format_local_date_str(ms)
}

pub(super) fn format_datetime_short(ms: i64) -> String {
    if ms <= 0 {
        return "--".to_string();
    }
    let dt = local_datetime(ms);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}
