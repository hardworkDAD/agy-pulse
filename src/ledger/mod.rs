mod query;
mod schema;
mod stats_report;
mod sync;

use std::env;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::{self, LanguageSetting};
use crate::domain::QuotaSnapshot;
use crate::platform;
use crate::render::resolve_language_from_locale;

pub use query::{
    CascadeOverview, ChildConversationOverview, CsvExportKind, LedgerExportFormat,
    LedgerGranularity, LedgerMeta, LedgerOverview, LedgerQueryFilter, LedgerSource,
    LedgerSourceFilter, LedgerSummary, LedgerTimeRange, ModelBreakdown, SourceBreakdown,
    TimeseriesBucket,
};
pub use stats_report::{
    StatsCliArgs, StatsOutputFormat, build_stats_help_text, parse_stats_args, render_stats_text,
};
pub use sync::{
    AccountAssignReport, LedgerSyncOptions, LedgerSyncReport, RollingCostInput,
    RollingCostTotals, SingleSyncReport, SourceRoot, validate_and_normalize_account_email,
};

/// Errors returned by `LedgerStore` operations.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("SQLite ledger error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid account email: {0}")]
    InvalidAccountEmail(String),
}

/// Deep module owning the unified `agy-pulse-ledger.sqlite` (WAL) connection.
/// Never exposes `rusqlite::Connection` outside `ledger`.
pub struct LedgerStore {
    conn: Connection,
    ledger_path: PathBuf,
    max_retries: u32,
}

impl LedgerStore {
    /// Resolve default `agy-pulse-ledger.sqlite` path under the primary Antigravity data root.
    pub fn default_ledger_path() -> PathBuf {
        platform::resolve_antigravity_path(schema::DEFAULT_LEDGER_FILENAME)
    }

    /// Open (or create with `0600` permissions) the unified SQLite WAL ledger at the default path.
    pub fn open_default() -> Result<Self, LedgerError> {
        let path = Self::default_ledger_path();
        Self::open_at(&path)
    }

    /// Open the default ledger with short statusline-safe lock timeouts so hot-path rendering
    /// never blocks past the host CLI's statusline deadline.
    pub fn open_default_for_statusline() -> Result<Self, LedgerError> {
        let path = Self::default_ledger_path();
        let conn = schema::open_ledger_connection_with_options(
            &path,
            schema::STATUSLINE_BUSY_TIMEOUT_MS,
            schema::STATUSLINE_BUSY_RETRIES,
        )?;
        Ok(Self {
            conn,
            ledger_path: path,
            max_retries: schema::STATUSLINE_BUSY_RETRIES,
        })
    }

    /// Open (or create with `0600` permissions) the unified SQLite WAL ledger at `ledger_path`.
    pub fn open_at(ledger_path: &Path) -> Result<Self, LedgerError> {
        let conn = schema::open_ledger_connection(ledger_path)?;
        Ok(Self {
            conn,
            ledger_path: ledger_path.to_path_buf(),
            max_retries: schema::DEFAULT_BUSY_RETRIES,
        })
    }

    /// Path to the opened `agy-pulse-ledger.sqlite` file.
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// Return this machine's immutable local `device_id` stored in `ledger_meta`.
    pub fn local_device_id(&self) -> Result<String, LedgerError> {
        schema::read_local_device_id(&self.conn).map_err(LedgerError::from)
    }

    /// Return the normalized `last_observed_email` persisted in `ledger_meta`, if any.
    pub fn last_observed_email(&self) -> Result<Option<String>, LedgerError> {
        sync::last_observed_email(&self.conn)
    }

    /// Normalize and persist `last_observed_email` in `ledger_meta`, skipping writes when unchanged.
    pub fn set_last_observed_email(&self, email: &str) -> Result<(), LedgerError> {
        sync::set_last_observed_email(&self.conn, email, self.max_retries)
    }

    /// Persist a device quota observation snapshot into `quota_snapshots`.
    pub fn upsert_quota_snapshot(&self, snapshot: &QuotaSnapshot) -> Result<(), LedgerError> {
        sync::upsert_quota_snapshot(&self.conn, snapshot, self.max_retries)
    }

    /// Return the latest `QuotaSnapshot` per `account_id` (`observed_at_ms` max, tie-broken by `device_id ASC`),
    /// sorted by `account_id ASC`. Corrupted `quota_json` rows are gracefully skipped.
    pub fn query_latest_account_quotas(&self) -> Result<Vec<QuotaSnapshot>, LedgerError> {
        sync::query_latest_account_quotas(&self.conn)
    }

    /// Reassign local `'unassigned'` conversations and steps to `email`.
    pub fn assign_unassigned_account(
        &self,
        email: &str,
        workspace_substr: Option<&str>,
        dry_run: bool,
    ) -> Result<AccountAssignReport, LedgerError> {
        sync::assign_unassigned_account(
            &self.conn,
            email,
            workspace_substr,
            dry_run,
            self.max_retries,
        )
    }

    /// Atomically purge all remote-device rows and pull chunk states inside a single Immediate transaction,
    /// preserving local device rows and push chunk states. Returns the number of deleted remote step rows.
    pub fn clear_remote_data(&self) -> Result<usize, LedgerError> {
        sync::clear_remote_data(&self.conn, self.max_retries)
    }

    /// Upsert a device metadata row in `sync_devices`.
    pub fn upsert_sync_device(
        &self,
        account_id: &str,
        device_id: &str,
        device_name: &str,
        is_local: bool,
        last_seen_ms: i64,
        plan_tier: Option<&str>,
    ) -> Result<(), LedgerError> {
        sync::upsert_sync_device(
            &self.conn,
            account_id,
            device_id,
            device_name,
            is_local,
            last_seen_ms,
            plan_tier,
            self.max_retries,
        )
    }

    /// Return `true` when `last_full_scan_ms` is absent or older than `max_age_ms`.
    pub fn is_full_scan_stale(&self, max_age_ms: i64, now_ms: i64) -> Result<bool, LedgerError> {
        sync::is_full_scan_stale(&self.conn, max_age_ms, now_ms)
    }

    /// Incrementally scan all CLI and IDE `conversations/*.db` roots and archive new/changed rows.
    pub fn sync_all(&mut self, options: &LedgerSyncOptions) -> Result<LedgerSyncReport, LedgerError> {
        sync::sync_all(&mut self.conn, &self.ledger_path, options)
    }

    /// Run `sync_all` only if no full scan has occurred within `max_age_ms` (used by statusline cold start).
    pub fn sync_all_if_stale(
        &mut self,
        max_age_ms: i64,
        options: &LedgerSyncOptions,
    ) -> Result<Option<LedgerSyncReport>, LedgerError> {
        sync::sync_all_if_stale(&mut self.conn, &self.ledger_path, max_age_ms, options)
    }

    /// Incrementally archive an active conversation and optional child subagent conversations.
    pub fn sync_conversation(
        &mut self,
        conversation_id: &str,
        child_conversation_ids: &[String],
        options: &LedgerSyncOptions,
    ) -> Result<SingleSyncReport, LedgerError> {
        sync::sync_conversation(
            &mut self.conn,
            &self.ledger_path,
            conversation_id,
            child_conversation_ids,
            options,
            self.max_retries,
        )
    }

    /// Update rolling daily & 7-day cost cursors atomically inside SQLite and return combined totals.
    pub fn record_rolling_costs(
        &mut self,
        input: &RollingCostInput<'_>,
    ) -> Result<Option<RollingCostTotals>, LedgerError> {
        sync::record_rolling_costs(&mut self.conn, input, self.max_retries)
    }

    /// Query multi-dimensional overview (summary KPIs, timeseries, models, sources, cascade trees).
    pub fn query_overview(&self, filter: &LedgerQueryFilter) -> Result<LedgerOverview, LedgerError> {
        query::query_overview(&self.conn, &self.ledger_path, filter, sync::current_epoch_ms())
    }

    /// Export filtered overview as pretty-printed JSON.
    pub fn export_json(&self, filter: &LedgerQueryFilter) -> Result<String, LedgerError> {
        let overview = self.query_overview(filter)?;
        let json = serde_json::to_string_pretty(&overview)?;
        Ok(format!("{json}\n"))
    }

    /// Export filtered overview as UTF-8 CSV (`timeseries`, `models`, or `cascades`/`sessions`).
    pub fn export_csv(
        &self,
        filter: &LedgerQueryFilter,
        kind: CsvExportKind,
    ) -> Result<String, LedgerError> {
        let overview = self.query_overview(filter)?;
        Ok(query::format_ledger_csv(&overview, kind))
    }

    /// Unified export helper for Ticket 07 `dashboard` and CLI `stats`.
    pub fn export(
        &self,
        filter: &LedgerQueryFilter,
        format: LedgerExportFormat,
    ) -> Result<String, LedgerError> {
        match format {
            LedgerExportFormat::Json => self.export_json(filter),
            LedgerExportFormat::Csv(kind) => self.export_csv(filter, kind),
        }
    }
}

/// Execute `agypl stats` / `report` / `agy-pulse --stats` CLI subcommand.
pub fn run_stats_cli(argv: &[String]) -> i32 {
    let args = parse_stats_args(argv);
    let cwd = env::current_dir().unwrap_or_default();

    let effective_lang = if args.lang == LanguageSetting::Auto {
        let cfg = config::load_config(&cwd);
        cfg.language
    } else {
        args.lang
    };
    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(effective_lang, locale.as_deref());

    if args.help {
        print!("{}", build_stats_help_text(resolved_lang));
        return 0;
    }

    let mut store = match LedgerStore::open_default() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse stats error: {err}");
            return 1;
        }
    };

    let (default_account_id, device_name) = platform::read_sync_identity_hints();
    let sync_opts = LedgerSyncOptions {
        default_account_id,
        device_name,
        ..Default::default()
    };
    if !args.no_sync
        && let Err(err) = store.sync_all(&sync_opts)
    {
        platform::write_error_log_safely(&format!("Ledger sync error in stats: {err}"));
    }

    let filter = LedgerQueryFilter {
        range: args.range,
        from: args.from,
        to: args.to,
        granularity: args.granularity,
        source: args.source,
        model: Some(args.model),
        workspace: Some(args.workspace),
        include_subagents: args.include_subagents,
        now_ms: None,
    };

    match args.format {
        StatsOutputFormat::Json => match store.export_json(&filter) {
            Ok(out) => {
                print!("{out}");
                0
            }
            Err(err) => {
                eprintln!("agy-pulse stats JSON export error: {err}");
                1
            }
        },
        StatsOutputFormat::Csv => match store.export_csv(&filter, args.csv_kind) {
            Ok(out) => {
                print!("{out}");
                0
            }
            Err(err) => {
                eprintln!("agy-pulse stats CSV export error: {err}");
                1
            }
        },
        StatsOutputFormat::Table => match store.query_overview(&filter) {
            Ok(overview) => {
                let no_color = env::var("NO_COLOR").is_ok_and(|v| !v.is_empty());
                print!("{}", render_stats_text(&overview, resolved_lang, !no_color));
                0
            }
            Err(err) => {
                eprintln!("agy-pulse stats query error: {err}");
                1
            }
        },
    }
}
