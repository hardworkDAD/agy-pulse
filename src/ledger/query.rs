use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use rusqlite::Connection;
use serde::Serialize;

use crate::domain::{
    ProviderQuotaSummary, UNASSIGNED_ACCOUNT_ID, compact_model_name, normalize_account_id,
};
use crate::sync::{RemoteConfig, SyncConfig, load_sync_config};

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
    pub account: Option<String>,
    pub device: Option<String>,
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
            account: None,
            device: None,
            include_subagents: true,
            now_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountOption {
    pub account_id: String,
    pub device_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceOption {
    pub device_id: String,
    pub device_name: String,
    pub account_ids: Vec<String>,
    pub is_local: bool,
    pub last_seen_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBreakdown {
    pub account_id: String,
    pub cost_usd: f64,
    pub share: f64,
    pub steps: u64,
    pub session_count: usize,
    pub device_count: usize,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_hit_rate: f64,
    pub avg_speed: Option<f64>,
    pub avg_ttft_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBreakdown {
    pub device_id: String,
    pub device_name: String,
    pub is_local: bool,
    pub account_ids: Vec<String>,
    pub cost_usd: f64,
    pub share: f64,
    pub steps: u64,
    pub session_count: usize,
    pub cli_cost_usd: f64,
    pub ide_cost_usd: f64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub last_seen_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaOverview {
    pub account_id: String,
    pub plan_tier: Option<String>,
    pub observed_at_ms: i64,
    pub reporter_device_id: String,
    pub reporter_device_name: String,
    pub quota: ProviderQuotaSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusOverview {
    pub enabled: bool,
    pub remote_type: Option<String>,
    pub last_push_ms: Option<i64>,
    pub last_pull_ms: Option<i64>,
    pub last_error: Option<String>,
    pub in_progress: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerMeta {
    pub range: String,
    pub source: String,
    pub account: String,
    pub device: String,
    #[serde(rename = "localDeviceId")]
    pub local_device_id: String,
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
    #[serde(rename = "availableAccounts")]
    pub available_accounts: Vec<AccountOption>,
    #[serde(rename = "availableDevices")]
    pub available_devices: Vec<DeviceOption>,
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
    #[serde(rename = "deviceId")]
    pub device_id: String,
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
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceName")]
    pub device_name: String,
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
    #[serde(rename = "byAccount")]
    pub by_account: Vec<AccountBreakdown>,
    #[serde(rename = "byDevice")]
    pub by_device: Vec<DeviceBreakdown>,
    #[serde(rename = "accountQuotas")]
    pub account_quotas: Vec<AccountQuotaOverview>,
    #[serde(rename = "syncStatus")]
    pub sync_status: SyncStatusOverview,
    #[serde(rename = "sessions")]
    pub cascades: Vec<CascadeOverview>,
}

// --- Local timezone helpers (cross-platform via chrono) ---

pub(super) fn local_datetime_in_tz<Tz: TimeZone>(tz: &Tz, ms: i64) -> DateTime<Tz> {
    if let Some(dt) = tz.timestamp_millis_opt(ms).earliest() {
        return dt;
    }
    DateTime::from_timestamp_millis(ms)
        .unwrap_or(DateTime::UNIX_EPOCH)
        .with_timezone(tz)
}

pub(super) fn local_datetime(ms: i64) -> DateTime<Local> {
    local_datetime_in_tz(&Local, ms)
}

fn resolve_local_naive_ms<Tz: TimeZone>(
    tz: &Tz,
    naive: chrono::NaiveDateTime,
    prefer_latest: bool,
) -> Option<i64> {
    let res = tz.from_local_datetime(&naive);
    let chosen = if prefer_latest {
        res.latest()
    } else {
        res.earliest()
    };
    chosen.map(|dt| dt.timestamp_millis())
}

fn local_naive_to_ms_in_tz<Tz: TimeZone>(
    tz: &Tz,
    naive: chrono::NaiveDateTime,
    prefer_latest: bool,
) -> Option<i64> {
    if let Some(ms) = resolve_local_naive_ms(tz, naive, prefer_latest) {
        return Some(ms);
    }
    // DST spring-forward gap: step forward (for start-of-window) or backward (for end-of-window)
    // minute-by-minute up to 3 hours to land on the nearest valid local instant of that day.
    let target_date = naive.date();
    for step_min in 1i64..=180 {
        let delta = chrono::Duration::minutes(if prefer_latest { -step_min } else { step_min });
        let candidate = naive + delta;
        if candidate.date() != target_date {
            break;
        }
        if let Some(ms) = resolve_local_naive_ms(tz, candidate, prefer_latest) {
            return Some(ms);
        }
    }
    None
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
    local_naive_to_ms_in_tz(&Local, naive, false)
}

pub(super) fn local_date_key(ms: i64) -> String {
    let dt = local_datetime(ms);
    format!("{:04}{:02}{:02}", dt.year(), dt.month(), dt.day())
}

pub(super) fn local_today_bounds_in_tz<Tz: TimeZone>(tz: &Tz, now_ms: i64) -> (i64, i64) {
    let dt = local_datetime_in_tz(tz, now_ms);
    let fallback_start = now_ms - i64::from(dt.num_seconds_from_midnight()) * 1000;
    let date = dt.date_naive();
    let start = date
        .and_hms_milli_opt(0, 0, 0, 0)
        .and_then(|naive| local_naive_to_ms_in_tz(tz, naive, false))
        .unwrap_or(fallback_start);
    let end = date
        .and_hms_milli_opt(23, 59, 59, 999)
        .and_then(|naive| local_naive_to_ms_in_tz(tz, naive, true))
        .unwrap_or_else(|| start.saturating_add(DAY_MS - 1))
        .max(start);
    (start, end)
}

pub(super) fn local_today_bounds(now_ms: i64) -> (i64, i64) {
    local_today_bounds_in_tz(&Local, now_ms)
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
        let date = NaiveDate::from_ymd_opt(y, m, d)?;
        return if is_end {
            let naive = date.and_hms_milli_opt(23, 59, 59, 999)?;
            local_naive_to_ms_in_tz(&Local, naive, true)
        } else {
            let naive = date.and_hms_milli_opt(0, 0, 0, 0)?;
            local_naive_to_ms_in_tz(&Local, naive, false)
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
pub(super) struct ConvIdentityMeta {
    pub account_id: String,
    pub device_id: String,
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
    pub account_id: String,
    pub device_id: String,
}

fn normalize_row_account_id(raw: Option<String>) -> String {
    raw.as_deref()
        .and_then(normalize_account_id)
        .unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string())
}

fn normalize_row_device_id(raw: Option<String>, local_device_id: &str) -> String {
    match raw {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => local_device_id.to_string(),
    }
}

const NONZERO_STEP_SQL: &str = "(input_tokens > 0 OR cache_read_tokens > 0 OR cache_write_tokens > 0 OR output_tokens > 0 OR (cost_usd > 0.0 AND cost_usd < 1e308))";

const TELEMETRY_STEPS_SELECT_COLS: &str = "conversation_id, ts_ms, model, raw_model,
                    input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                    cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
                    parent_conversation_id, root_conversation_id, workspace, branch, source,
                    account_id, device_id";

struct ParsedStepRow {
    step: StoredStepRow,
    #[cfg(test)]
    parent_conversation_id: Option<String>,
    #[cfg(test)]
    root_conversation_id: Option<String>,
}

fn parse_step_row(
    row: &rusqlite::Row<'_>,
    local_device_id: &str,
) -> Result<Option<ParsedStepRow>, rusqlite::Error> {
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
        return Ok(None);
    }
    let stream_sec: Option<f64> = row.get(9)?;
    let ttft_sec: Option<f64> = row.get(10)?;
    let valid_stream = row.get::<_, i64>(11)? != 0;
    let valid_ttft = row.get::<_, i64>(12)? != 0;
    #[cfg(test)]
    let parent_conversation_id = row.get::<_, Option<String>>(13)?.filter(|s| !s.is_empty());
    #[cfg(test)]
    let root_conversation_id = row.get::<_, Option<String>>(14)?.filter(|s| !s.is_empty());
    let workspace = row.get::<_, Option<String>>(15)?.filter(|s| !s.is_empty());
    let branch = row.get::<_, Option<String>>(16)?.filter(|s| !s.is_empty());
    let src_raw: String = row.get(17)?;
    let source = LedgerSource::from_raw(&src_raw);
    let account_id = normalize_row_account_id(row.get::<_, Option<String>>(18)?);
    let device_id = normalize_row_device_id(row.get::<_, Option<String>>(19)?, local_device_id);

    Ok(Some(ParsedStepRow {
        step: StoredStepRow {
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
            account_id,
            device_id,
        },
        #[cfg(test)]
        parent_conversation_id,
        #[cfg(test)]
        root_conversation_id,
    }))
}

struct StepsSqlQuery {
    sql: String,
    params: Vec<rusqlite::types::Value>,
}

fn build_steps_sql_query(
    source_filter: LedgerSourceFilter,
    is_unbounded_all: bool,
    from_ms: i64,
    to_ms: i64,
    local_device_id: &str,
    account_filter: Option<&str>,
    device_filter: Option<&str>,
) -> StepsSqlQuery {
    use rusqlite::types::Value;

    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    let mut push_param = |val: Value| -> String {
        params.push(val);
        format!("?{}", params.len())
    };

    if let Some(target_acct) = account_filter {
        if target_acct == UNASSIGNED_ACCOUNT_ID {
            let p = push_param(Value::Text(UNASSIGNED_ACCOUNT_ID.to_string()));
            clauses.push(format!("account_id IN ({p}, '')"));
        } else {
            let p = push_param(Value::Text(target_acct.to_string()));
            clauses.push(format!("account_id = {p}"));
        }
    }

    if let Some(target_dev) = device_filter {
        if target_dev == local_device_id {
            let p = push_param(Value::Text(local_device_id.to_string()));
            clauses.push(format!("device_id IN ({p}, '')"));
        } else {
            let p = push_param(Value::Text(target_dev.to_string()));
            clauses.push(format!("device_id = {p}"));
        }
    }

    if !is_unbounded_all {
        let p_from = push_param(Value::Integer(from_ms));
        let p_to = push_param(Value::Integer(to_ms));
        clauses.push(format!("ts_ms >= {p_from} AND ts_ms <= {p_to}"));
    }

    match source_filter {
        LedgerSourceFilter::Cli => {
            let p = push_param(Value::Text("cli".to_string()));
            clauses.push(format!("source IN ({p}, '')"));
        }
        LedgerSourceFilter::Ide => {
            let p = push_param(Value::Text("ide".to_string()));
            clauses.push(format!("source = {p}"));
        }
        LedgerSourceFilter::All => {}
    }

    clauses.push(NONZERO_STEP_SQL.to_string());
    let sql = format!(
        "SELECT {TELEMETRY_STEPS_SELECT_COLS} FROM telemetry_steps WHERE {}",
        clauses.join(" AND ")
    );
    StepsSqlQuery { sql, params }
}

struct ConvMetadataMaps {
    conv_meta: HashMap<String, ConvLineageMeta>,
    conv_identity: HashMap<String, ConvIdentityMeta>,
}

fn load_conv_metadata_from_ingested_dbs(
    conn: &Connection,
    local_device_id: &str,
) -> Result<ConvMetadataMaps, LedgerError> {
    let mut conv_meta: HashMap<String, ConvLineageMeta> = HashMap::new();
    let mut conv_identity: HashMap<String, ConvIdentityMeta> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT conversation_id, workspace, branch, parent_conversation_id, root_conversation_id, source, account_id, device_id
         FROM ingested_dbs",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let cid: String = row.get(0)?;
        if cid.is_empty() {
            continue;
        }
        let src_raw: String = row.get(5)?;
        let account_id = normalize_row_account_id(row.get::<_, Option<String>>(6)?);
        let device_id = normalize_row_device_id(row.get::<_, Option<String>>(7)?, local_device_id);
        conv_identity.insert(
            cid.clone(),
            ConvIdentityMeta {
                account_id,
                device_id,
            },
        );
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
    Ok(ConvMetadataMaps {
        conv_meta,
        conv_identity,
    })
}

struct GlobalStepMetadata {
    conv_meta: HashMap<String, ConvLineageMeta>,
    conv_identity: HashMap<String, ConvIdentityMeta>,
    available_models_set: BTreeSet<String>,
    available_workspaces_set: BTreeSet<String>,
    available_sources_set: HashSet<LedgerSource>,
    all_convs_count: u64,
    total_recorded_steps: u64,
    min_recorded_ts: i64,
    max_recorded_ts: i64,
}

fn load_global_ledger_metadata(
    conn: &Connection,
    local_device_id: &str,
    device_info_map: &mut HashMap<String, DeviceInfoEntry>,
    account_devices_all: &mut HashMap<String, BTreeSet<String>>,
    root_cache: &mut HashMap<String, String>,
) -> Result<GlobalStepMetadata, LedgerError> {
    let ConvMetadataMaps {
        mut conv_meta,
        mut conv_identity,
    } = load_conv_metadata_from_ingested_dbs(conn, local_device_id)?;

    let mut all_convs_set: HashSet<String> = HashSet::new();
    let mut convs_needing_fallback_ws: Vec<String> = Vec::new();
    {
        let sql = format!(
            "SELECT conversation_id,
                    MAX(NULLIF(workspace, '')),
                    MAX(NULLIF(branch, '')),
                    MAX(NULLIF(parent_conversation_id, '')),
                    MAX(NULLIF(root_conversation_id, '')),
                    MAX(source),
                    MAX(NULLIF(NULLIF(account_id, ''), 'unassigned')),
                    MAX(NULLIF(device_id, '')),
                    MAX(CASE WHEN workspace IS NULL OR workspace = '' THEN 1 ELSE 0 END)
             FROM telemetry_steps
             WHERE {NONZERO_STEP_SQL}
             GROUP BY conversation_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let cid: String = row.get(0)?;
            if !cid.is_empty() {
                all_convs_set.insert(cid.clone());
            }
            let workspace = row.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty());
            let branch = row.get::<_, Option<String>>(2)?.filter(|s| !s.is_empty());
            let parent_conversation_id = row.get::<_, Option<String>>(3)?.filter(|s| !s.is_empty());
            let root_conversation_id = row.get::<_, Option<String>>(4)?.filter(|s| !s.is_empty());
            let src_raw: String = row.get(5)?;
            let source = LedgerSource::from_raw(&src_raw);
            let account_id = normalize_row_account_id(row.get::<_, Option<String>>(6)?);
            let device_id =
                normalize_row_device_id(row.get::<_, Option<String>>(7)?, local_device_id);
            let has_null_ws = row.get::<_, i64>(8)? != 0;
            if has_null_ws {
                convs_needing_fallback_ws.push(cid.clone());
            }

            conv_identity
                .entry(cid.clone())
                .and_modify(|existing| {
                    if existing.account_id == UNASSIGNED_ACCOUNT_ID
                        && account_id != UNASSIGNED_ACCOUNT_ID
                    {
                        existing.account_id = account_id.clone();
                    }
                    if existing.device_id.is_empty() && !device_id.is_empty() {
                        existing.device_id = device_id.clone();
                    }
                })
                .or_insert_with(|| ConvIdentityMeta {
                    account_id,
                    device_id,
                });

            conv_meta
                .entry(cid)
                .and_modify(|existing| {
                    if existing.workspace.is_none() && workspace.is_some() {
                        existing.workspace = workspace.clone();
                    }
                    if existing.branch.is_none() && branch.is_some() {
                        existing.branch = branch.clone();
                    }
                    if existing.parent_conversation_id.is_none() && parent_conversation_id.is_some()
                    {
                        existing.parent_conversation_id = parent_conversation_id.clone();
                    }
                    if existing.root_conversation_id.is_none() && root_conversation_id.is_some() {
                        existing.root_conversation_id = root_conversation_id.clone();
                    }
                })
                .or_insert_with(|| ConvLineageMeta {
                    workspace,
                    branch,
                    parent_conversation_id,
                    root_conversation_id,
                    source,
                });
        }
    }

    let (total_recorded_steps, min_recorded_ts, max_recorded_ts): (u64, i64, i64) = {
        let sql = format!(
            "SELECT COUNT(*), MIN(ts_ms), MAX(ts_ms) FROM telemetry_steps WHERE {NONZERO_STEP_SQL}"
        );
        let (cnt, min_ts, max_ts): (i64, Option<i64>, Option<i64>) =
            conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        (
            cnt.max(0) as u64,
            min_ts.unwrap_or(i64::MAX),
            max_ts.unwrap_or(i64::MIN),
        )
    };

    let mut available_models_set: BTreeSet<String> = BTreeSet::new();
    {
        let sql = format!(
            "SELECT DISTINCT model FROM telemetry_steps WHERE {NONZERO_STEP_SQL} AND model != ''"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let model: String = row.get(0)?;
            if !model.is_empty() {
                available_models_set.insert(model);
            }
        }
    }

    let mut available_workspaces_set: BTreeSet<String> = BTreeSet::new();
    {
        let sql = format!(
            "SELECT DISTINCT workspace FROM telemetry_steps WHERE {NONZERO_STEP_SQL} AND workspace IS NOT NULL AND workspace != ''"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let ws: String = row.get(0)?;
            if !ws.is_empty() {
                available_workspaces_set.insert(ws);
            }
        }
    }
    for cid in convs_needing_fallback_ws {
        let root_id = resolve_root_conversation_id(&cid, &conv_meta, root_cache);
        let ws = conv_meta
            .get(&cid)
            .and_then(|m| m.workspace.as_deref())
            .or_else(|| conv_meta.get(&root_id).and_then(|m| m.workspace.as_deref()));
        if let Some(w) = ws {
            available_workspaces_set.insert(w.to_string());
        }
    }

    let mut available_sources_set: HashSet<LedgerSource> = HashSet::new();
    {
        let sql = format!("SELECT DISTINCT source FROM telemetry_steps WHERE {NONZERO_STEP_SQL}");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let src_raw: String = row.get(0)?;
            available_sources_set.insert(LedgerSource::from_raw(&src_raw));
        }
    }

    {
        let sql = format!(
            "SELECT account_id, device_id, MAX(ts_ms)
             FROM telemetry_steps
             WHERE {NONZERO_STEP_SQL}
             GROUP BY account_id, device_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let acct = normalize_row_account_id(row.get::<_, Option<String>>(0)?);
            let dev = normalize_row_device_id(row.get::<_, Option<String>>(1)?, local_device_id);
            let max_ts: i64 = row.get(2)?;
            if !dev.is_empty() {
                let entry = device_info_map.entry(dev.clone()).or_default();
                entry.is_local |= dev == local_device_id;
                if max_ts > entry.last_seen_ms {
                    entry.last_seen_ms = max_ts;
                }
                entry.account_ids.insert(acct.clone());
                account_devices_all.entry(acct).or_default().insert(dev);
            }
        }
    }

    Ok(GlobalStepMetadata {
        conv_meta,
        conv_identity,
        available_models_set,
        available_workspaces_set,
        available_sources_set,
        all_convs_count: all_convs_set.len() as u64,
        total_recorded_steps,
        min_recorded_ts,
        max_recorded_ts,
    })
}

fn load_filtered_steps(
    conn: &Connection,
    local_device_id: &str,
    query: &StepsSqlQuery,
) -> Result<Vec<StoredStepRow>, LedgerError> {
    let mut stmt = conn.prepare(&query.sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(query.params.iter()))?;
    let mut steps = Vec::new();
    while let Some(row) = rows.next()? {
        if let Some(parsed) = parse_step_row(row, local_device_id)? {
            steps.push(parsed.step);
        }
    }
    Ok(steps)
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(super) fn load_ledger_rows(
    conn: &Connection,
) -> Result<
    (
        HashMap<String, ConvLineageMeta>,
        HashMap<String, ConvIdentityMeta>,
        Vec<StoredStepRow>,
    ),
    LedgerError,
> {
    let local_device_id = super::schema::read_local_device_id(conn).inspect_err(|err| {
        crate::platform::write_error_log_safely(&format!(
            "Failed to read local_device_id in load_ledger_rows: {err}"
        ));
    })?;
    let ConvMetadataMaps {
        mut conv_meta,
        mut conv_identity,
    } = load_conv_metadata_from_ingested_dbs(conn, &local_device_id)?;

    let mut steps = Vec::new();
    {
        let sql = format!("SELECT {TELEMETRY_STEPS_SELECT_COLS} FROM telemetry_steps");
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let Some(ParsedStepRow {
                step,
                parent_conversation_id,
                root_conversation_id,
            }) = parse_step_row(row, &local_device_id)?
            else {
                continue;
            };

            conv_identity
                .entry(step.conversation_id.clone())
                .and_modify(|existing| {
                    if existing.account_id == UNASSIGNED_ACCOUNT_ID
                        && step.account_id != UNASSIGNED_ACCOUNT_ID
                    {
                        existing.account_id = step.account_id.clone();
                    }
                    if existing.device_id.is_empty() && !step.device_id.is_empty() {
                        existing.device_id = step.device_id.clone();
                    }
                })
                .or_insert_with(|| ConvIdentityMeta {
                    account_id: step.account_id.clone(),
                    device_id: step.device_id.clone(),
                });

            conv_meta
                .entry(step.conversation_id.clone())
                .and_modify(|existing| {
                    if existing.workspace.is_none() && step.workspace.is_some() {
                        existing.workspace = step.workspace.clone();
                    }
                    if existing.branch.is_none() && step.branch.is_some() {
                        existing.branch = step.branch.clone();
                    }
                    if existing.parent_conversation_id.is_none() && parent_conversation_id.is_some()
                    {
                        existing.parent_conversation_id = parent_conversation_id.clone();
                    }
                    if existing.root_conversation_id.is_none() && root_conversation_id.is_some() {
                        existing.root_conversation_id = root_conversation_id.clone();
                    }
                })
                .or_insert_with(|| ConvLineageMeta {
                    workspace: step.workspace.clone(),
                    branch: step.branch.clone(),
                    parent_conversation_id,
                    root_conversation_id,
                    source: step.source,
                });

            steps.push(step);
        }
    }

    Ok((conv_meta, conv_identity, steps))
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

struct AccountAcc {
    account_id: String,
    acc: PerfAcc,
    root_ids: HashSet<String>,
    device_ids: HashSet<String>,
}

struct DeviceAcc {
    device_id: String,
    acc: PerfAcc,
    root_ids: HashSet<String>,
    account_ids: BTreeSet<String>,
    cli_cost_usd: f64,
    ide_cost_usd: f64,
    last_step_ts_ms: i64,
}

#[derive(Default)]
struct DeviceInfoEntry {
    device_name: String,
    is_local: bool,
    last_seen_ms: i64,
    account_ids: BTreeSet<String>,
    plan_tiers: HashMap<String, String>,
}

struct ChildConvAcc {
    conversation_id: String,
    source: LedgerSource,
    device_id: String,
    started_at_ms: i64,
    last_activity_ms: i64,
    acc: PerfAcc,
    models_cost: HashMap<String, f64>,
}

struct CascadeAcc {
    root_conversation_id: String,
    source: LedgerSource,
    account_id: String,
    device_id: String,
    workspace: String,
    workspace_label: String,
    branch: String,
    started_at_ms: i64,
    last_activity_ms: i64,
    acc: PerfAcc,
    models_cost: HashMap<String, f64>,
    children: HashMap<String, ChildConvAcc>,
}

fn cmp_account_ids(a: &str, b: &str) -> std::cmp::Ordering {
    match (a == UNASSIGNED_ACCOUNT_ID, b == UNASSIGNED_ACCOUNT_ID) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => a.cmp(b),
    }
}

fn sort_account_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut list: Vec<String> = ids.into_iter().collect();
    list.sort_by(|a, b| cmp_account_ids(a, b));
    list.dedup();
    list
}

fn strip_url_userinfo_in_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("://") {
        let after_scheme = &rest[idx + 3..];
        let auth_end = after_scheme
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..auth_end];
        if let Some(at_idx) = authority.rfind('@') {
            out.push_str(&rest[..idx + 3]);
            out.push_str(&authority[at_idx + 1..]);
            rest = &after_scheme[auth_end..];
        } else {
            out.push_str(&rest[..idx + 3]);
            rest = after_scheme;
        }
    }
    out.push_str(rest);
    out
}

/// Scrub credentials from `SyncConfig`, sync environment variables, and URL userinfo from error text.
pub fn redact_sync_error_message(msg: &str, cfg: Option<&SyncConfig>) -> String {
    let mut secrets: Vec<String> = Vec::new();
    if let Some(c) = cfg {
        match &c.remote {
            RemoteConfig::LocalDir { .. } => {}
            RemoteConfig::WebDav {
                password, token, ..
            } => {
                if let Some(p) = password.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    secrets.push(p.to_string());
                }
                if let Some(t) = token.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    secrets.push(t.to_string());
                    secrets.push(format!("Bearer {t}"));
                }
            }
            RemoteConfig::S3 {
                access_key,
                secret_key,
                ..
            } => {
                if let Some(ak) = access_key.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    secrets.push(ak.to_string());
                }
                if let Some(sk) = secret_key.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    secrets.push(sk.to_string());
                }
            }
            RemoteConfig::CfWorker { token, .. } => {
                if let Some(t) = token.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    secrets.push(t.to_string());
                    secrets.push(format!("Bearer {t}"));
                }
            }
        }
    }

    for env_name in [
        "AGY_PULSE_SYNC_TOKEN",
        "AGY_PULSE_SYNC_PASSWORD",
        "AGY_PULSE_S3_ACCESS_KEY",
        "AGY_PULSE_S3_SECRET_KEY",
    ] {
        if let Some(val) = crate::platform::env_var(env_name) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                secrets.push(trimmed.to_string());
                if env_name == "AGY_PULSE_SYNC_TOKEN" {
                    secrets.push(format!("Bearer {trimmed}"));
                }
            }
        }
    }

    let mut out = strip_url_userinfo_in_text(msg);
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(&secret, "[REDACTED]");
        }
    }
    out.chars()
        .filter(|c| !c.is_control())
        .take(512)
        .collect()
}

/// Build a `SyncStatusOverview` from `ledger_meta` and the active `SyncConfig` (redacting errors).
pub(crate) fn build_sync_status_overview(
    conn: &Connection,
    cfg: Option<&SyncConfig>,
    in_progress: bool,
) -> SyncStatusOverview {
    let sync_meta = match super::sync::get_sync_meta(conn) {
        Ok(meta) => meta,
        Err(err) => {
            crate::platform::write_error_log_safely(&format!(
                "Failed to read sync_meta in build_sync_status_overview: {err}"
            ));
            crate::domain::LedgerSyncMeta::default()
        }
    };
    let last_error = sync_meta
        .last_sync_error
        .as_deref()
        .map(|err| redact_sync_error_message(err, cfg))
        .filter(|s| !s.is_empty());

    SyncStatusOverview {
        enabled: cfg.is_some_and(|c| c.enabled),
        remote_type: cfg.map(|c| c.remote_type_str().to_string()),
        last_push_ms: sync_meta.last_sync_push_ms,
        last_pull_ms: sync_meta.last_sync_pull_ms,
        last_error,
        in_progress,
    }
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
    let local_device_id = super::schema::read_local_device_id(conn).inspect_err(|err| {
        crate::platform::write_error_log_safely(&format!(
            "Failed to read local_device_id in query_overview: {err}"
        ));
    })?;
    let sync_devices = super::sync::list_sync_devices(conn)?;
    let latest_quotas = super::sync::query_latest_account_quotas(conn)?;
    let time_range = resolve_time_range(filter, now_ms);
    let mut root_cache: HashMap<String, String> = HashMap::new();

    let mut device_info_map: HashMap<String, DeviceInfoEntry> = HashMap::new();
    let mut account_devices_all: HashMap<String, BTreeSet<String>> = HashMap::new();
    seed_device_info_maps(
        &local_device_id,
        &sync_devices,
        &latest_quotas,
        &mut device_info_map,
        &mut account_devices_all,
    );

    let global_meta = load_global_ledger_metadata(
        conn,
        &local_device_id,
        &mut device_info_map,
        &mut account_devices_all,
        &mut root_cache,
    )?;

    let is_unbounded_all = time_range.range == LedgerTimeRange::All
        && filter.from.is_none()
        && filter.to.is_none();
    let (account_filter, meta_account) = normalize_account_filter(filter.account.as_deref());
    let (device_filter, meta_device) =
        normalize_device_filter(filter.device.as_deref(), &local_device_id);

    let steps_query = build_steps_sql_query(
        filter.source,
        is_unbounded_all,
        time_range.from_ms,
        time_range.to_ms,
        &local_device_id,
        account_filter.as_deref(),
        device_filter.as_deref(),
    );
    let steps = load_filtered_steps(conn, &local_device_id, &steps_query)?;

    aggregate_overview(OverviewBuildInput {
        conn,
        ledger_path,
        filter,
        local_device_id,
        latest_quotas,
        time_range,
        is_unbounded_all,
        account_filter,
        meta_account,
        device_filter,
        meta_device,
        device_info_map,
        account_devices_all,
        global_meta,
        steps,
        root_cache,
    })
}

fn seed_device_info_maps(
    local_device_id: &str,
    sync_devices: &[crate::domain::SyncDeviceRecord],
    latest_quotas: &[crate::domain::QuotaSnapshot],
    device_info_map: &mut HashMap<String, DeviceInfoEntry>,
    account_devices_all: &mut HashMap<String, BTreeSet<String>>,
) {
    for dev in sync_devices {
        let dev_id = dev.device_id.trim();
        if dev_id.is_empty() {
            continue;
        }
        let acct = normalize_account_id(&dev.account_id)
            .unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
        let entry = device_info_map.entry(dev_id.to_string()).or_default();
        if !dev.device_name.trim().is_empty()
            && (entry.device_name.is_empty() || dev.last_seen_ms >= entry.last_seen_ms)
        {
            entry.device_name = dev.device_name.trim().to_string();
        }
        entry.is_local |= dev.is_local || dev_id == local_device_id;
        if dev.last_seen_ms > entry.last_seen_ms {
            entry.last_seen_ms = dev.last_seen_ms;
        }
        entry.account_ids.insert(acct.clone());
        if let Some(ref tier) = dev.plan_tier {
            let trimmed = tier.trim();
            if !trimmed.is_empty() {
                entry.plan_tiers.insert(acct.clone(), trimmed.to_string());
            }
        }
        account_devices_all
            .entry(acct)
            .or_default()
            .insert(dev_id.to_string());
    }

    for snap in latest_quotas {
        let dev_id = snap.device_id.trim();
        if dev_id.is_empty() {
            continue;
        }
        let acct = normalize_account_id(&snap.account_id)
            .unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
        let entry = device_info_map.entry(dev_id.to_string()).or_default();
        if entry.device_name.is_empty() && !snap.device_name.trim().is_empty() {
            entry.device_name = snap.device_name.trim().to_string();
        }
        entry.is_local |= dev_id == local_device_id;
        if snap.observed_at_ms > entry.last_seen_ms {
            entry.last_seen_ms = snap.observed_at_ms;
        }
        entry.account_ids.insert(acct.clone());
        if let Some(ref tier) = snap.plan_tier {
            let trimmed = tier.trim();
            if !trimmed.is_empty() {
                entry
                    .plan_tiers
                    .entry(acct.clone())
                    .or_insert_with(|| trimmed.to_string());
            }
        }
        account_devices_all
            .entry(acct)
            .or_default()
            .insert(dev_id.to_string());
    }
}

fn normalize_account_filter(raw: Option<&str>) -> (Option<String>, String) {
    let account_filter = raw
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"))
        .map(|s| {
            if s.eq_ignore_ascii_case(UNASSIGNED_ACCOUNT_ID) {
                UNASSIGNED_ACCOUNT_ID.to_string()
            } else {
                normalize_account_id(s).unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string())
            }
        });
    let meta_account = account_filter
        .clone()
        .unwrap_or_else(|| "all".to_string());
    (account_filter, meta_account)
}

fn normalize_device_filter(raw: Option<&str>, local_device_id: &str) -> (Option<String>, String) {
    let device_filter = raw
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"))
        .map(|s| {
            if s.eq_ignore_ascii_case("local") {
                local_device_id.to_string()
            } else {
                s.to_string()
            }
        });
    let meta_device = device_filter
        .clone()
        .unwrap_or_else(|| "all".to_string());
    (device_filter, meta_device)
}

struct OverviewBuildInput<'a> {
    conn: &'a Connection,
    ledger_path: &'a Path,
    filter: &'a LedgerQueryFilter,
    local_device_id: String,
    latest_quotas: Vec<crate::domain::QuotaSnapshot>,
    time_range: ResolvedRange,
    is_unbounded_all: bool,
    account_filter: Option<String>,
    meta_account: String,
    device_filter: Option<String>,
    meta_device: String,
    device_info_map: HashMap<String, DeviceInfoEntry>,
    account_devices_all: HashMap<String, BTreeSet<String>>,
    global_meta: GlobalStepMetadata,
    steps: Vec<StoredStepRow>,
    root_cache: HashMap<String, String>,
}

fn aggregate_overview(input: OverviewBuildInput<'_>) -> Result<LedgerOverview, LedgerError> {
    let OverviewBuildInput {
        conn,
        ledger_path,
        filter,
        local_device_id,
        latest_quotas,
        time_range,
        is_unbounded_all,
        account_filter,
        meta_account,
        device_filter,
        meta_device,
        device_info_map,
        account_devices_all,
        global_meta,
        steps,
        mut root_cache,
    } = input;

    let GlobalStepMetadata {
        conv_meta,
        conv_identity,
        available_models_set,
        available_workspaces_set,
        available_sources_set,
        all_convs_count,
        total_recorded_steps,
        min_recorded_ts,
        max_recorded_ts,
    } = global_meta;

    let mut available_accounts: Vec<AccountOption> = account_devices_all
        .iter()
        .map(|(account_id, devs)| AccountOption {
            account_id: account_id.clone(),
            device_count: devs.len(),
        })
        .collect();
    available_accounts.sort_by(|a, b| cmp_account_ids(&a.account_id, &b.account_id));

    let mut available_devices: Vec<DeviceOption> = device_info_map
        .iter()
        .map(|(device_id, info)| DeviceOption {
            device_id: device_id.clone(),
            device_name: if info.device_name.is_empty() {
                device_id.clone()
            } else {
                info.device_name.clone()
            },
            account_ids: sort_account_ids(info.account_ids.iter().cloned()),
            is_local: info.is_local,
            last_seen_ms: info.last_seen_ms,
        })
        .collect();
    available_devices.sort_by(|a, b| {
        b.is_local
            .cmp(&a.is_local)
            .then_with(|| b.last_seen_ms.cmp(&a.last_seen_ms))
            .then_with(|| a.device_name.cmp(&b.device_name))
            .then_with(|| a.device_id.cmp(&b.device_id))
    });

    let mut available_sources = Vec::new();
    if available_sources_set.contains(&LedgerSource::Cli) {
        available_sources.push("cli".to_string());
    }
    if available_sources_set.contains(&LedgerSource::Ide) {
        available_sources.push("ide".to_string());
    }

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
    let mut account_map: HashMap<String, AccountAcc> = HashMap::new();
    let mut device_map: HashMap<String, DeviceAcc> = HashMap::new();
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

        if let Some(ref target_acct) = account_filter
            && step.account_id != *target_acct
        {
            continue;
        }
        if let Some(ref target_dev) = device_filter
            && step.device_id != *target_dev
        {
            continue;
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
        let conv_id_meta = conv_identity.get(&step.conversation_id);
        let root_id_meta = conv_identity.get(&root_id);
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

        let acct_entry = account_map
            .entry(step.account_id.clone())
            .or_insert_with(|| AccountAcc {
                account_id: step.account_id.clone(),
                acc: PerfAcc::default(),
                root_ids: HashSet::new(),
                device_ids: HashSet::new(),
            });
        acct_entry.acc.accumulate(step, is_subagent);
        acct_entry.root_ids.insert(root_id.clone());
        if !step.device_id.is_empty() {
            acct_entry.device_ids.insert(step.device_id.clone());
        }

        let dev_entry = device_map
            .entry(step.device_id.clone())
            .or_insert_with(|| DeviceAcc {
                device_id: step.device_id.clone(),
                acc: PerfAcc::default(),
                root_ids: HashSet::new(),
                account_ids: BTreeSet::new(),
                cli_cost_usd: 0.0,
                ide_cost_usd: 0.0,
                last_step_ts_ms: 0,
            });
        dev_entry.acc.accumulate(step, is_subagent);
        dev_entry.root_ids.insert(root_id.clone());
        dev_entry.account_ids.insert(step.account_id.clone());
        match step.source {
            LedgerSource::Cli => dev_entry.cli_cost_usd += step.cost_usd,
            LedgerSource::Ide => dev_entry.ide_cost_usd += step.cost_usd,
        }
        if step.ts_ms > dev_entry.last_step_ts_ms {
            dev_entry.last_step_ts_ms = step.ts_ms;
        }

        let root_cascade = cascade_map.entry(root_id.clone()).or_insert_with(|| {
            let root_ws = root_m.and_then(|m| m.workspace.as_deref()).or(effective_ws);
            let root_br = root_m.and_then(|m| m.branch.as_deref()).or(effective_branch);
            let root_src = root_m.map(|m| m.source).unwrap_or(step.source);
            let root_acct = root_id_meta
                .map(|m| m.account_id.as_str())
                .filter(|a| *a != UNASSIGNED_ACCOUNT_ID)
                .unwrap_or(&step.account_id)
                .to_string();
            let root_dev = root_id_meta
                .map(|m| m.device_id.as_str())
                .filter(|d| !d.is_empty())
                .unwrap_or(&step.device_id)
                .to_string();
            CascadeAcc {
                root_conversation_id: root_id.clone(),
                source: root_src,
                account_id: root_acct,
                device_id: root_dev,
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
                    let child_dev = conv_id_meta
                        .map(|m| m.device_id.as_str())
                        .filter(|d| !d.is_empty())
                        .unwrap_or(&step.device_id)
                        .to_string();
                    ChildConvAcc {
                        conversation_id: step.conversation_id.clone(),
                        source: child_src,
                        device_id: child_dev,
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

    let mut accounts_vec: Vec<AccountAcc> = account_map.into_values().collect();
    accounts_vec.sort_by(|a, b| {
        b.acc
            .cost_usd
            .partial_cmp(&a.acc.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.acc.steps.cmp(&a.acc.steps))
            .then_with(|| cmp_account_ids(&a.account_id, &b.account_id))
    });
    let by_account: Vec<AccountBreakdown> = accounts_vec
        .into_iter()
        .map(|a| {
            let fin = a.acc.finalize();
            let share = if summary_acc.cost_usd > 0.0 {
                (a.acc.cost_usd / summary_acc.cost_usd) * 100.0
            } else {
                0.0
            };
            AccountBreakdown {
                account_id: a.account_id,
                cost_usd: a.acc.cost_usd,
                share,
                steps: a.acc.steps,
                session_count: a.root_ids.len(),
                device_count: a.device_ids.len(),
                input_tokens: a.acc.input_tokens,
                cache_read_tokens: a.acc.cache_read_tokens,
                cache_write_tokens: a.acc.cache_write_tokens,
                output_tokens: a.acc.output_tokens,
                total_tokens: fin.total_tokens,
                cache_hit_rate: fin.cache_hit_rate,
                avg_speed: fin.avg_speed,
                avg_ttft_sec: fin.avg_ttft_sec,
            }
        })
        .collect();

    let mut devices_vec: Vec<DeviceAcc> = device_map.into_values().collect();
    devices_vec.sort_by(|a, b| {
        b.acc
            .cost_usd
            .partial_cmp(&a.acc.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.acc.steps.cmp(&a.acc.steps))
            .then_with(|| a.device_id.cmp(&b.device_id))
    });
    let by_device: Vec<DeviceBreakdown> = devices_vec
        .into_iter()
        .map(|d| {
            let fin = d.acc.finalize();
            let share = if summary_acc.cost_usd > 0.0 {
                (d.acc.cost_usd / summary_acc.cost_usd) * 100.0
            } else {
                0.0
            };
            let info = device_info_map.get(&d.device_id);
            let device_name = info
                .map(|i| i.device_name.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&d.device_id)
                .to_string();
            let is_local = info.is_some_and(|i| i.is_local) || d.device_id == local_device_id;
            let last_seen_ms = info
                .map(|i| i.last_seen_ms)
                .unwrap_or(0)
                .max(d.last_step_ts_ms);
            DeviceBreakdown {
                device_id: d.device_id,
                device_name,
                is_local,
                account_ids: sort_account_ids(d.account_ids),
                cost_usd: d.acc.cost_usd,
                share,
                steps: d.acc.steps,
                session_count: d.root_ids.len(),
                cli_cost_usd: d.cli_cost_usd,
                ide_cost_usd: d.ide_cost_usd,
                input_tokens: d.acc.input_tokens,
                cache_read_tokens: d.acc.cache_read_tokens,
                cache_write_tokens: d.acc.cache_write_tokens,
                output_tokens: d.acc.output_tokens,
                total_tokens: fin.total_tokens,
                last_seen_ms,
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
                        device_id: ch.device_id,
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

            let device_name = device_info_map
                .get(&c.device_id)
                .map(|i| i.device_name.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&c.device_id)
                .to_string();

            CascadeOverview {
                root_conversation_id: c.root_conversation_id,
                source: c.source,
                account_id: c.account_id,
                device_id: c.device_id,
                device_name,
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

    let account_quotas: Vec<AccountQuotaOverview> = latest_quotas
        .into_iter()
        .filter(|q| {
            account_filter
                .as_deref()
                .is_none_or(|target| q.account_id == target)
        })
        .map(|q| {
            let info = device_info_map.get(&q.device_id);
            let reporter_device_name = info
                .map(|i| i.device_name.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    let trimmed = q.device_name.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                })
                .unwrap_or(&q.device_id)
                .to_string();
            let plan_tier = q.plan_tier.clone().or_else(|| {
                info.and_then(|i| i.plan_tiers.get(&q.account_id).cloned())
            });
            AccountQuotaOverview {
                account_id: q.account_id,
                plan_tier,
                observed_at_ms: q.observed_at_ms,
                reporter_device_id: q.device_id,
                reporter_device_name,
                quota: q.quota,
            }
        })
        .collect();

    let sync_cfg_res = load_sync_config();
    let sync_cfg = sync_cfg_res.as_ref().ok().and_then(|opt| opt.as_ref());
    let mut sync_status = build_sync_status_overview(conn, sync_cfg, false);
    if let Err(err) = &sync_cfg_res
        && sync_status.last_error.is_none()
    {
        sync_status.last_error = Some(redact_sync_error_message(&err.to_string(), None));
    }

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
        account: meta_account,
        device: meta_device,
        local_device_id,
        from_ms,
        to_ms,
        granularity: granularity.as_str().to_string(),
        ledger_path: ledger_path.to_string_lossy().into_owned(),
        backend: "sqlite".to_string(),
        total_recorded_steps,
        total_recorded_conversations: all_convs_count,
        available_models: available_models_set.into_iter().collect(),
        available_workspaces: available_workspaces_set.into_iter().collect(),
        available_sources,
        available_accounts,
        available_devices,
    };

    Ok(LedgerOverview {
        meta,
        summary,
        timeseries,
        by_model,
        by_source,
        by_account,
        by_device,
        account_quotas,
        sync_status,
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
                "account_id".to_string(),
                "device_name".to_string(),
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
                    c.account_id.clone(),
                    c.device_name.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{schema, sync};

    #[test]
    fn redact_sync_error_message_honors_thread_local_env_overrides() {
        let overrides: HashMap<String, Option<String>> = [
            ("AGY_PULSE_SYNC_TOKEN", Some("tok-thread-secret-999")),
            ("AGY_PULSE_SYNC_PASSWORD", Some("pwd-thread-secret-888")),
            ("AGY_PULSE_S3_ACCESS_KEY", Some("ak-thread-secret-777")),
            ("AGY_PULSE_S3_SECRET_KEY", Some("sk-thread-secret-666")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
        .collect();
        crate::platform::with_env_overrides(&overrides, || {
            let raw = "auth failed Bearer tok-thread-secret-999 pwd=pwd-thread-secret-888 ak=ak-thread-secret-777 sk=sk-thread-secret-666";
            let redacted = redact_sync_error_message(raw, None);
            assert!(
                !redacted.contains("tok-thread-secret-999"),
                "token leaked: {redacted}"
            );
            assert!(
                !redacted.contains("pwd-thread-secret-888"),
                "password leaked: {redacted}"
            );
            assert!(
                !redacted.contains("ak-thread-secret-777"),
                "s3 access key leaked: {redacted}"
            );
            assert!(
                !redacted.contains("sk-thread-secret-666"),
                "s3 secret key leaked: {redacted}"
            );
            assert!(redacted.contains("[REDACTED]"));
        });
    }

    #[derive(Clone, Copy)]
    struct MockMidnightDstTz {
        skip_midnight_hour: bool,
        skip_eod_hour: bool,
    }

    impl TimeZone for MockMidnightDstTz {
        type Offset = chrono::FixedOffset;

        fn from_offset(_offset: &Self::Offset) -> Self {
            Self {
                skip_midnight_hour: false,
                skip_eod_hour: false,
            }
        }

        fn offset_from_local_date(&self, _local: &NaiveDate) -> chrono::MappedLocalTime<Self::Offset> {
            let Some(off) = chrono::FixedOffset::east_opt(0) else {
                return chrono::MappedLocalTime::None;
            };
            chrono::MappedLocalTime::Single(off)
        }

        fn offset_from_local_datetime(
            &self,
            local: &chrono::NaiveDateTime,
        ) -> chrono::MappedLocalTime<Self::Offset> {
            let h = local.hour();
            if self.skip_midnight_hour && h == 0 {
                return chrono::MappedLocalTime::None;
            }
            if self.skip_eod_hour && h == 23 {
                return chrono::MappedLocalTime::None;
            }
            let offset_secs = if self.skip_midnight_hour && h >= 1 {
                3600
            } else {
                0
            };
            let Some(off) = chrono::FixedOffset::east_opt(offset_secs) else {
                return chrono::MappedLocalTime::None;
            };
            chrono::MappedLocalTime::Single(off)
        }

        fn offset_from_utc_date(&self, _utc: &NaiveDate) -> Self::Offset {
            chrono::FixedOffset::east_opt(0).unwrap_or_else(|| unreachable!())
        }

        fn offset_from_utc_datetime(&self, _utc: &chrono::NaiveDateTime) -> Self::Offset {
            let off_secs = if self.skip_midnight_hour { 3600 } else { 0 };
            chrono::FixedOffset::east_opt(off_secs).unwrap_or_else(|| unreachable!())
        }
    }

    #[test]
    fn local_today_bounds_handles_midnight_and_eod_dst_spring_forward_gaps() {
        let Some(date) = NaiveDate::from_ymd_opt(2026, 11, 1) else {
            panic!("valid date");
        };
        let Some(noon) = date.and_hms_opt(12, 0, 0) else {
            panic!("valid noon");
        };
        let tz_midnight_gap = MockMidnightDstTz {
            skip_midnight_hour: true,
            skip_eod_hour: false,
        };
        let now_ms = noon.and_utc().timestamp_millis() - 3_600_000;
        let (start, end) = local_today_bounds_in_tz(&tz_midnight_gap, now_ms);
        assert!(start < end, "expected non-empty window: {start}..{end}");
        assert!(
            start <= now_ms && now_ms <= end,
            "expected now_ms inside window: start={start}, now={now_ms}, end={end}"
        );
        // Nearest valid local start on midnight DST jump is 01:00:00+01:00 == 00:00:00Z
        let expected_start = date
            .and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc().timestamp_millis())
            .unwrap_or(0);
        assert_eq!(start, expected_start);

        let tz_eod_gap = MockMidnightDstTz {
            skip_midnight_hour: false,
            skip_eod_hour: true,
        };
        let now_eod = noon.and_utc().timestamp_millis();
        let (start2, end2) = local_today_bounds_in_tz(&tz_eod_gap, now_eod);
        assert!(start2 < end2, "expected non-empty eod gap window: {start2}..{end2}");
        assert!(start2 <= now_eod && now_eod <= end2);
        // Nearest valid local end when 23:00..24:00 is skipped is 22:59:59.999Z
        let expected_end = date
            .and_hms_milli_opt(22, 59, 59, 999)
            .map(|dt| dt.and_utc().timestamp_millis())
            .unwrap_or(0);
        assert_eq!(end2, expected_end);
    }

    fn explain_plan_details(conn: &Connection, query: &StepsSqlQuery) -> Vec<String> {
        let explain_sql = format!("EXPLAIN QUERY PLAN {}", query.sql);
        let Ok(mut stmt) = conn.prepare(&explain_sql) else {
            panic!("prepare EXPLAIN QUERY PLAN failed");
        };
        let Ok(mut rows) = stmt.query(rusqlite::params_from_iter(query.params.iter())) else {
            panic!("query EXPLAIN QUERY PLAN failed");
        };
        let mut details = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            if let Ok(detail) = row.get::<_, String>(3) {
                details.push(detail);
            }
        }
        details
    }

    fn query_overview_full_scan_reference(
        conn: &Connection,
        ledger_path: &Path,
        filter: &LedgerQueryFilter,
        default_now_ms: i64,
    ) -> Result<LedgerOverview, LedgerError> {
        let now_ms = filter.now_ms.unwrap_or(default_now_ms);
        let local_device_id = schema::read_local_device_id(conn)?;
        let sync_devices = sync::list_sync_devices(conn)?;
        let latest_quotas = sync::query_latest_account_quotas(conn)?;
        let (conv_meta, conv_identity, steps) = load_ledger_rows(conn)?;
        let time_range = resolve_time_range(filter, now_ms);
        let mut root_cache: HashMap<String, String> = HashMap::new();

        let mut device_info_map: HashMap<String, DeviceInfoEntry> = HashMap::new();
        let mut account_devices_all: HashMap<String, BTreeSet<String>> = HashMap::new();
        seed_device_info_maps(
            &local_device_id,
            &sync_devices,
            &latest_quotas,
            &mut device_info_map,
            &mut account_devices_all,
        );

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
            let root_id =
                resolve_root_conversation_id(&step.conversation_id, &conv_meta, &mut root_cache);
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
            if !step.device_id.is_empty() {
                let entry = device_info_map.entry(step.device_id.clone()).or_default();
                entry.is_local |= step.device_id == local_device_id;
                if step.ts_ms > entry.last_seen_ms {
                    entry.last_seen_ms = step.ts_ms;
                }
                entry.account_ids.insert(step.account_id.clone());
                account_devices_all
                    .entry(step.account_id.clone())
                    .or_default()
                    .insert(step.device_id.clone());
            }
        }

        let is_unbounded_all = time_range.range == LedgerTimeRange::All
            && filter.from.is_none()
            && filter.to.is_none();
        let (account_filter, meta_account) = normalize_account_filter(filter.account.as_deref());
        let (device_filter, meta_device) =
            normalize_device_filter(filter.device.as_deref(), &local_device_id);
        let global_meta = GlobalStepMetadata {
            conv_meta,
            conv_identity,
            available_models_set,
            available_workspaces_set,
            available_sources_set,
            all_convs_count: all_convs_set.len() as u64,
            total_recorded_steps: steps.len() as u64,
            min_recorded_ts,
            max_recorded_ts,
        };

        aggregate_overview(OverviewBuildInput {
            conn,
            ledger_path,
            filter,
            local_device_id,
            latest_quotas,
            time_range,
            is_unbounded_all,
            account_filter,
            meta_account,
            device_filter,
            meta_device,
            device_info_map,
            account_devices_all,
            global_meta,
            steps,
            root_cache,
        })
    }

    #[test]
    fn sql_filter_where_clause_hits_sqlite_indexes_not_table_scan() {
        let Ok(tmp) = tempfile::TempDir::new() else {
            panic!("tempdir");
        };
        let path = tmp.path().join("ledger.sqlite");
        let Ok(conn) = schema::open_ledger_connection(&path) else {
            panic!("open ledger");
        };
        let local_id = schema::read_local_device_id(&conn).unwrap_or_default();
        let now_ms = 1_794_312_000_000i64;
        let week_from = now_ms - 7 * DAY_MS;

        let cases = [
            (
                "time_window_only",
                LedgerSourceFilter::All,
                false,
                None,
                None,
            ),
            (
                "local_device_and_time",
                LedgerSourceFilter::All,
                false,
                None,
                Some(local_id.as_str()),
            ),
            (
                "remote_device_and_time",
                LedgerSourceFilter::All,
                false,
                None,
                Some("dev-remote-1"),
            ),
            (
                "account_and_device_and_time",
                LedgerSourceFilter::All,
                false,
                Some("alice@example.com"),
                Some("dev-remote-1"),
            ),
            (
                "unassigned_account_and_time",
                LedgerSourceFilter::All,
                false,
                Some(UNASSIGNED_ACCOUNT_ID),
                None,
            ),
            (
                "source_ide_unbounded",
                LedgerSourceFilter::Ide,
                true,
                None,
                None,
            ),
            (
                "source_cli_unbounded",
                LedgerSourceFilter::Cli,
                true,
                None,
                None,
            ),
        ];

        for (label, source, is_unbounded, acct, dev) in cases {
            let q = build_steps_sql_query(
                source,
                is_unbounded,
                week_from,
                now_ms,
                &local_id,
                acct,
                dev,
            );
            let plan = explain_plan_details(&conn, &q);
            let joined = plan.join(" | ");
            assert!(
                joined.contains("USING INDEX") && !joined.contains("SCAN telemetry_steps"),
                "expected index search for {label}, got query plan: {joined} (sql: {})",
                q.sql
            );
        }
    }

    fn assert_json_equivalent(
        left: &serde_json::Value,
        right: &serde_json::Value,
        path: &str,
        ctx: &LedgerQueryFilter,
    ) {
        use serde_json::Value;
        match (left, right) {
            (Value::Number(a), Value::Number(b)) => {
                if let (Some(ia), Some(ib)) = (a.as_i64(), b.as_i64()) {
                    assert_eq!(ia, ib, "int mismatch at {path} for {ctx:?}");
                } else if let (Some(ua), Some(ub)) = (a.as_u64(), b.as_u64()) {
                    assert_eq!(ua, ub, "uint mismatch at {path} for {ctx:?}");
                } else {
                    let fa = a.as_f64().unwrap_or(f64::NAN);
                    let fb = b.as_f64().unwrap_or(f64::NAN);
                    assert!(
                        (fa - fb).abs() <= 1e-12,
                        "float mismatch at {path} ({fa} vs {fb}) for {ctx:?}"
                    );
                }
            }
            (Value::Array(la), Value::Array(ra)) => {
                assert_eq!(
                    la.len(),
                    ra.len(),
                    "array len mismatch at {path} for {ctx:?}"
                );
                for (i, (lv, rv)) in la.iter().zip(ra.iter()).enumerate() {
                    assert_json_equivalent(lv, rv, &format!("{path}[{i}]"), ctx);
                }
            }
            (Value::Object(lo), Value::Object(ro)) => {
                assert_eq!(
                    lo.len(),
                    ro.len(),
                    "object key count mismatch at {path} for {ctx:?}"
                );
                for (k, lv) in lo {
                    let Some(rv) = ro.get(k) else {
                        panic!("missing key {k} at {path} for {ctx:?}");
                    };
                    assert_json_equivalent(lv, rv, &format!("{path}.{k}"), ctx);
                }
            }
            _ => assert_eq!(left, right, "value mismatch at {path} for {ctx:?}"),
        }
    }

    #[test]
    fn indexed_query_overview_matches_full_table_reference_across_all_dimensions() {
        let Ok(tmp) = tempfile::TempDir::new() else {
            panic!("tempdir");
        };
        let path = tmp.path().join("ledger.sqlite");
        let Ok(conn) = schema::open_ledger_connection(&path) else {
            panic!("open ledger");
        };
        let Ok(local_id) = schema::read_local_device_id(&conn) else {
            panic!("local_device_id");
        };

        let now_ms = local_ymd_hms_to_ms(2026, 11, 15, 14, 30, 0, 0).unwrap_or(1_794_753_000_000);
        let t_1h = now_ms - 3_600_000;
        let t_30m = now_ms - 1_800_000;
        let t_2d = now_ms - 2 * DAY_MS;
        let t_15d = now_ms - 15 * DAY_MS;
        let t_45d = now_ms - 45 * DAY_MS;

        let insert_conv = |cid: &str,
                           ws: Option<&str>,
                           br: Option<&str>,
                           parent: Option<&str>,
                           root: Option<&str>,
                           src: &str,
                           acct: &str,
                           dev: &str| {
            let res = conn.execute(
                "INSERT INTO ingested_dbs (
                   conversation_id, db_path, sig, last_synced_ms, workspace, branch,
                   created_at_ms, cascade_id, parent_conversation_id, root_conversation_id,
                   db_mtime_ms, source, account_id, device_id
                 ) VALUES (?1, '', 'sig', ?2, ?3, ?4, ?2, ?1, ?5, ?6, ?2, ?7, ?8, ?9)",
                rusqlite::params![cid, t_1h, ws, br, parent, root, src, acct, dev],
            );
            assert!(res.is_ok());
        };

        #[allow(clippy::too_many_arguments)]
        let insert_step = |uid: &str,
                           cid: &str,
                           ts: i64,
                           model: &str,
                           raw_model: &str,
                           in_tok: i64,
                           out_tok: i64,
                           cost: f64,
                           parent: Option<&str>,
                           root: Option<&str>,
                           ws: Option<&str>,
                           src: &str,
                           acct: &str,
                           dev: &str| {
            let res = conn.execute(
                "INSERT INTO telemetry_steps (
                   step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
                   model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                   cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
                   parent_conversation_id, root_conversation_id, workspace, branch, source,
                   account_id, device_id
                 ) VALUES (?1, ?2, 1, 0, 1, ?3, ?4, ?5, ?6, 1000, 0, ?7, ?8, 4.0, 0.5, 1, 1, ?9, ?10, ?11, 'main', ?12, ?13, ?14)",
                rusqlite::params![
                    uid, cid, ts, model, raw_model, in_tok, out_tok, cost, parent, root, ws, src,
                    acct, dev
                ],
            );
            assert!(res.is_ok());
        };

        // Multi-level cascade spanning 15d -> 2d -> 30m ago, with empty-string device_id fallback to local_id
        insert_conv(
            "root-tree",
            Some("file:///work/alpha"),
            Some("main"),
            None,
            None,
            "cli",
            "alice@example.com",
            &local_id,
        );
        insert_conv(
            "child-tree",
            None,
            None,
            Some("root-tree"),
            Some("root-tree"),
            "cli",
            "alice@example.com",
            "",
        );
        insert_conv(
            "grandchild-tree",
            None,
            None,
            Some("child-tree"),
            None,
            "cli",
            "",
            "",
        );
        insert_conv(
            "remote-ide-conv",
            Some("file:///work/beta"),
            Some("feat"),
            None,
            None,
            "ide",
            "bob@example.com",
            "dev-remote-1",
        );
        insert_conv(
            "old-local-unassigned",
            Some("file:///work/legacy"),
            Some("old"),
            None,
            None,
            "",
            "unassigned",
            &local_id,
        );

        insert_step(
            "s1",
            "root-tree",
            t_15d,
            "Gemini 4 Carbon",
            "Gemini 4 Carbon (High)",
            50_000,
            2_000,
            0.25,
            None,
            None,
            Some("file:///work/alpha"),
            "cli",
            "alice@example.com",
            &local_id,
        );
        insert_step(
            "s2",
            "child-tree",
            t_2d,
            "Claude 4.5 Haiku",
            "Claude 4.5 Haiku",
            20_000,
            1_000,
            0.08,
            Some("root-tree"),
            Some("root-tree"),
            None,
            "cli",
            "alice@example.com",
            "",
        );
        insert_step(
            "s3",
            "grandchild-tree",
            t_30m,
            "Claude 4.5 Haiku",
            "Claude 4.5 Haiku",
            10_000,
            500,
            0.04,
            Some("child-tree"),
            None,
            None,
            "cli",
            "",
            "",
        );
        insert_step(
            "s4",
            "remote-ide-conv",
            t_1h,
            "Claude 4.6 Sonnet",
            "Claude 4.6 Sonnet (Thinking)",
            40_000,
            3_000,
            0.35,
            None,
            None,
            Some("file:///work/beta"),
            "ide",
            "bob@example.com",
            "dev-remote-1",
        );
        insert_step(
            "s5",
            "old-local-unassigned",
            t_45d,
            "Gemini 4 Carbon",
            "Gemini 4 Carbon (High)",
            15_000,
            800,
            0.12,
            None,
            None,
            Some("file:///work/legacy"),
            "",
            "",
            "",
        );
        // Zero-cost/zero-token step that must be ignored identically
        let res_zero = conn.execute(
            "INSERT INTO telemetry_steps (
               step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
               model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
               cost_usd, valid_stream, valid_ttft, source, account_id, device_id
             ) VALUES ('s-zero', 'zero-conv', 1, 0, 1, ?1, 'EmptyModel', 0, 0, 0, 0, 0.0, 0, 0, 'cli', 'unassigned', '')",
            rusqlite::params![t_1h],
        );
        assert!(res_zero.is_ok());
        // Synthetic step whose conversation is not in ingested_dbs
        insert_step(
            "s6",
            "orphan-step-conv",
            t_2d,
            "Claude 4.6 Sonnet",
            "Claude 4.6 Sonnet",
            12_000,
            600,
            0.09,
            None,
            None,
            Some("file:///work/orphan"),
            "ide",
            "unassigned",
            "dev-remote-1",
        );

        let sync_dev_res = sync::upsert_sync_device(
            &conn,
            "bob@example.com",
            "dev-remote-1",
            "bob-linux",
            false,
            t_1h,
            Some("Pro"),
            schema::BusyRetryPolicy::standard(),
        );
        assert!(sync_dev_res.is_ok());

        let ranges = [
            LedgerTimeRange::Today,
            LedgerTimeRange::Last24h,
            LedgerTimeRange::Last7d,
            LedgerTimeRange::Last30d,
            LedgerTimeRange::ThisMonth,
            LedgerTimeRange::All,
        ];
        let sources = [
            LedgerSourceFilter::All,
            LedgerSourceFilter::Cli,
            LedgerSourceFilter::Ide,
        ];
        let accounts = [None, Some("unassigned"), Some("alice@example.com"), Some("bob@example.com")];
        let devices = [None, Some("local"), Some(local_id.as_str()), Some("dev-remote-1")];
        let models = [None, Some("Haiku 4.5")];
        let workspaces = [None, Some("alpha")];

        for range in ranges {
            for source in sources {
                for acct in accounts {
                    for dev in devices {
                        for model in models {
                            for ws in workspaces {
                                for include_subagents in [true, false] {
                                    let filter = LedgerQueryFilter {
                                        range,
                                        from: None,
                                        to: None,
                                        granularity: LedgerGranularity::Auto,
                                        source,
                                        model: model.map(str::to_string),
                                        workspace: ws.map(str::to_string),
                                        account: acct.map(str::to_string),
                                        device: dev.map(str::to_string),
                                        include_subagents,
                                        now_ms: Some(now_ms),
                                    };
                                    let Ok(indexed) = query_overview(&conn, &path, &filter, now_ms)
                                    else {
                                        panic!("indexed query_overview failed");
                                    };
                                    let Ok(reference) = query_overview_full_scan_reference(
                                        &conn, &path, &filter, now_ms,
                                    ) else {
                                        panic!("reference query_overview failed");
                                    };
                                    let Ok(idx_val) = serde_json::to_value(&indexed) else {
                                        panic!("serialize indexed");
                                    };
                                    let Ok(ref_val) = serde_json::to_value(&reference) else {
                                        panic!("serialize reference");
                                    };
                                    assert_json_equivalent(&idx_val, &ref_val, "$", &filter);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
