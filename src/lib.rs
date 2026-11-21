#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod dashboard;
pub mod domain;
pub mod ingest;
pub mod ledger;
pub mod ops;
pub mod platform;
pub mod render;

use std::env;
use std::io::{self, Read};
use std::panic;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::ingest::HostReader;
use crate::render::resolve_language_from_locale;

const STDIN_HARD_TIMEOUT: Duration = Duration::from_millis(1500);
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn build_cli_help_text() -> String {
    format!(
        "agy-pulse v{VERSION} — Antigravity CLI status line & telemetry (CLI: agypl / agy-pulse)\n\n\
Usage:\n  \
  agypl dashboard [options]               Start local web dashboard (alias: dash, ui, --dashboard)\n  \
  agypl stats [today|7d|30d|all]          Show CLI & IDE telemetry summary (alias: report, --stats)\n  \
  agypl config                            Launch interactive configuration wizard (alias: --config)\n  \
  agypl install                           Deploy CLI binaries & register host statusLine (alias: --install)\n  \
  agypl uninstall [--purge-ledger]        Safely remove statusLine & caches (keeps ledger by default)\n  \
  agypl update                            Check and install the latest release (alias: --update)\n  \
  agypl help                              Show this help message (alias: --help, -h)\n  \
  agypl --version                         Show version information (alias: -V, -v, version)\n  \
  agy-pulse                               Render status line from stdin JSON (or use any subcommand above)\n"
    )
}

fn read_stdin_with_timeout(timeout: Duration) -> (String, bool) {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut timed_out = false;

    loop {
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
        }
    }

    (String::from_utf8_lossy(&bytes).into_owned(), timed_out)
}

fn extract_seven_day_reset_at_ms(snapshot: &domain::PulseSnapshot) -> Option<i64> {
    let q = snapshot.quota.as_ref()?;
    for provider in [&q.google, &q.claude, &q.argon] {
        if let Some(ref w) = provider.weekly
            && let Some(rt) = w.reset_time_ms
        {
            return Some(rt);
        }
    }
    for extra in &q.extras {
        if let Some(ref w) = extra.quota.weekly
            && let Some(rt) = w.reset_time_ms
        {
            return Some(rt);
        }
    }
    None
}

fn run_statusline_pipeline() {
    let (stdin_text, timed_out) = read_stdin_with_timeout(STDIN_HARD_TIMEOUT);
    if timed_out && stdin_text.trim().is_empty() {
        platform::write_error_log_safely("Statusline stdin read timed out after 1500ms");
    }

    let fallback_cwd = env::current_dir().unwrap_or_default();
    let mut ingest = HostReader::read_live_pulse(&stdin_text, &fallback_cwd);
    if !ingest.diagnostics.is_empty() {
        platform::write_error_log_safely(&ingest.diagnostics.join("\n"));
    }

    let cfg = config::load_config(&ingest.workspace_dir);
    if !cfg.enabled {
        return;
    }

    ingest.snapshot.update_available_version = ops::read_available_update_version();
    ops::maybe_spawn_background_update_check();

    let show_daily = cfg.display.show_daily_cost.unwrap_or(false);
    let show_weekly = cfg.display.show_weekly_cost.unwrap_or(false);
    if ingest.snapshot.has_conversation_identity || show_daily || show_weekly {
        let sync_opts = ledger::LedgerSyncOptions {
            now_ms: Some(ingest.snapshot.observed_at_ms),
            ..Default::default()
        };
        match ledger::LedgerStore::open_default() {
            Ok(mut store) => {
                if let Err(err) = store.sync_all_if_stale(60_000, &sync_opts) {
                    platform::write_error_log_safely(&format!("Ledger full scan error: {err}"));
                }
                if let Some(ref conv_id) = ingest.conversation_id
                    && let Err(err) = store.sync_conversation(
                        conv_id,
                        &ingest.child_conversation_ids,
                        &sync_opts,
                    )
                {
                    platform::write_error_log_safely(&format!(
                        "Ledger conversation sync error: {err}"
                    ));
                }
                let allow_routed = cfg.display.show_routed_cost.unwrap_or(false);
                let live_cost_usd =
                    domain::resolve_cascade_cost_usd(&ingest.snapshot, allow_routed);
                let cursor_id = ingest.rolling_cost_key.as_deref().unwrap_or("");
                let cascade_root_id = ingest
                    .conversation_id
                    .as_deref()
                    .or(ingest.rolling_cost_key.as_deref());
                let rolling_input = ledger::RollingCostInput {
                    cursor_id,
                    cascade_root_id,
                    live_cost_usd,
                    seven_day_reset_at_ms: extract_seven_day_reset_at_ms(&ingest.snapshot),
                    now_ms: ingest.snapshot.observed_at_ms,
                };
                match store.record_rolling_costs(&rolling_input) {
                    Ok(Some(totals)) => {
                        ingest.snapshot.cost.today_usd = Some(totals.today_usd);
                        ingest.snapshot.cost.week_usd = Some(totals.week_usd);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        platform::write_error_log_safely(&format!(
                            "Ledger rolling cost error: {err}"
                        ));
                    }
                }
            }
            Err(err) => {
                platform::write_error_log_safely(&format!("Ledger open error: {err}"));
            }
        }
    }

    let term = platform::detect_terminal_caps(
        ingest.snapshot.terminal_width,
        cfg.display.terminal_width,
        cfg.display.unicode,
    );
    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let output = render::render_statusline(&ingest.snapshot, &cfg, &term, resolved_lang);
    if !output.is_empty() {
        print!("{output}");
    }
}

/// Primary CLI entrypoint shared by `agy-pulse` and `agypl` binaries.
pub fn run_cli() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    let first = args.first().map(String::as_str);

    if matches!(first, Some("--version" | "-V" | "-v" | "version"))
        || args.iter().any(|a| a == "--version" || a == "-V")
    {
        println!("agy-pulse v{VERSION}");
        return 0;
    }

    if matches!(first, Some("dashboard" | "dash" | "ui"))
        || args.iter().any(|a| a == "--dashboard")
    {
        return dashboard::run_dashboard_cli(&args);
    }

    if matches!(first, Some("stats" | "report")) || args.iter().any(|a| a == "--stats") {
        return ledger::run_stats_cli(&args);
    }

    if matches!(first, Some("config")) || args.iter().any(|a| a == "--config") {
        return config::run_config_cli();
    }

    if matches!(first, Some("install")) || args.iter().any(|a| a == "--install") {
        return ops::run_install_cli(&args);
    }

    if matches!(first, Some("uninstall")) || args.iter().any(|a| a == "--uninstall") {
        return ops::run_uninstall_cli(&args);
    }

    if matches!(first, Some("update")) || args.iter().any(|a| a == "--update") {
        return ops::run_update_cli(&args);
    }

    if args.iter().any(|a| a == "--check-update-bg") {
        return ops::run_check_update_bg_cli();
    }

    if matches!(first, Some("help" | "--help" | "-h"))
        || args.iter().any(|a| a == "--help" || a == "-h")
    {
        print!("{}", build_cli_help_text());
        return 0;
    }

    let res = panic::catch_unwind(run_statusline_pipeline);
    if res.is_err() {
        platform::write_error_log_safely("Recovered from unexpected panic in statusline mode");
    }
    0
}
