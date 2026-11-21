mod query;
mod schema;
mod stats_report;
mod sync;

use std::env;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::{self, LanguageSetting};
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
    LedgerSyncOptions, LedgerSyncReport, RollingCostInput, RollingCostTotals, SingleSyncReport,
    SourceRoot,
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
}

/// Deep module owning the unified `agy-pulse-ledger.sqlite` (WAL) connection.
/// Never exposes `rusqlite::Connection` outside `ledger`.
pub struct LedgerStore {
    conn: Connection,
    ledger_path: PathBuf,
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

    /// Open (or create with `0600` permissions) the unified SQLite WAL ledger at `ledger_path`.
    pub fn open_at(ledger_path: &Path) -> Result<Self, LedgerError> {
        let conn = schema::open_ledger_connection(ledger_path)?;
        Ok(Self {
            conn,
            ledger_path: ledger_path.to_path_buf(),
        })
    }

    /// Path to the opened `agy-pulse-ledger.sqlite` file.
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
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
        )
    }

    /// Update rolling daily & 7-day cost cursors atomically inside SQLite and return combined totals.
    pub fn record_rolling_costs(
        &mut self,
        input: &RollingCostInput<'_>,
    ) -> Result<Option<RollingCostTotals>, LedgerError> {
        sync::record_rolling_costs(&mut self.conn, input)
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

    if !args.no_sync
        && let Err(err) = store.sync_all(&LedgerSyncOptions::default())
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
