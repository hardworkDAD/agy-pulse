#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod dashboard;
pub mod domain;
pub mod ingest;
pub mod ledger;
pub mod ops;
pub mod platform;
pub(crate) mod profile;
pub mod render;
pub mod sync;

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::panic;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ingest::HostReader;
use crate::render::resolve_language_from_locale;

const STDIN_HARD_TIMEOUT: Duration = Duration::from_millis(1500);
const STATUSLINE_POST_STDIN_BUDGET: Duration = Duration::from_millis(900);
const STATUSLINE_POST_STDIN_HARD_CAP: Duration = Duration::from_millis(2500);
const STATUSLINE_TOTAL_HARD_CAP: Duration = Duration::from_millis(3500);
// Leave time for the host to collect output and observe exit before its 5-second timeout.
const STATUSLINE_EXIT_TIMEOUT: Duration = Duration::from_secs(4);
const LEDGER_BG_SYNC_LOCK_FILENAME: &str = "agy-pulse-ledger-sync.lock";
const LEDGER_BG_SYNC_SPAWNING_GRACE_MS: u64 = 5_000;
const LEDGER_BG_SYNC_LOCK_TTL_MS: u64 = 30_000;
const LEDGER_BG_SYNC_LOCK_MAX_TTL_MS: u64 = 10 * 60 * 1000;
const VERSION: &str = env!("CARGO_PKG_VERSION");

static BG_LOCK_TOKEN_SEQ: AtomicU64 = AtomicU64::new(1);

fn build_cli_help_text() -> String {
    format!(
        "agy-pulse v{VERSION} — Antigravity CLI status line & telemetry (CLI: agypl / agy-pulse)\n\n\
Usage:\n  \
  agypl dashboard [options]               Start local web dashboard (alias: dash, ui, --dashboard)\n  \
  agypl stats [today|7d|30d|all]          Show CLI & IDE telemetry summary (alias: report, --stats)\n  \
  agypl config                            Launch interactive configuration wizard (alias: --config)\n  \
  agypl install                           Deploy CLI binaries & register host statusLine (alias: --install)\n  \
  agypl uninstall [--purge-ledger]        Safely remove statusLine & caches (keeps ledger by default)\n  \
  agypl account assign <email> [options]  Assign local unassigned records to an email\n  \
  agypl sync [init|status|prune-device]   Synchronize multi-device telemetry with remote storage\n  \
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BgLedgerSyncLock {
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    role: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    ts: i64,
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn acquire_bg_ledger_sync_lock(token: &str) -> bool {
    let lock_path = platform::resolve_antigravity_path(LEDGER_BG_SYNC_LOCK_FILENAME);
    let now_ms = current_epoch_ms();
    let payload = BgLedgerSyncLock {
        pid: std::process::id(),
        role: "spawning".to_string(),
        token: token.to_string(),
        ts: now_ms,
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return false;
    };

    for _attempt in 0..2 {
        match platform::create_exclusive_file_0600(&lock_path, &bytes) {
            Ok(true) => return true,
            Ok(false) => {
                let meta = match fs::metadata(&lock_path) {
                    Ok(m) => m,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return false,
                };
                let Ok(modified) = meta.modified() else {
                    return false;
                };
                let age_ms = SystemTime::now()
                    .duration_since(modified)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                let is_active = match fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<BgLedgerSyncLock>(&raw).ok())
                {
                    Some(parsed) if parsed.role == "worker" && parsed.pid > 0 => {
                        platform::is_process_alive(parsed.pid)
                            && age_ms < LEDGER_BG_SYNC_LOCK_MAX_TTL_MS
                    }
                    Some(parsed) if parsed.role == "spawning" => {
                        age_ms < LEDGER_BG_SYNC_SPAWNING_GRACE_MS
                            && (parsed.pid == 0 || platform::is_process_alive(parsed.pid))
                    }
                    _ => age_ms < LEDGER_BG_SYNC_LOCK_TTL_MS,
                };
                if is_active {
                    return false;
                }
                if fs::remove_file(&lock_path).is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

fn release_bg_ledger_sync_lock_if_owner(expected_token: Option<&str>) {
    let lock_path = platform::resolve_antigravity_path(LEDGER_BG_SYNC_LOCK_FILENAME);
    let Ok(raw) = fs::read_to_string(&lock_path) else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<BgLedgerSyncLock>(&raw) else {
        return;
    };
    let owned = match expected_token {
        Some(tok) => !tok.is_empty() && parsed.token == tok,
        None => parsed.pid == std::process::id(),
    };
    if owned {
        let _ = fs::remove_file(&lock_path);
    }
}

fn maybe_spawn_background_ledger_sync() {
    if env::var("AGY_PULSE_DISABLE_BG_LEDGER_SYNC").ok().as_deref() == Some("1") {
        return;
    }
    let now_ms = current_epoch_ms();
    let seq = BG_LOCK_TOKEN_SEQ.fetch_add(1, Ordering::Relaxed);
    let token = format!("{}-{now_ms}-{seq}", std::process::id());
    if !acquire_bg_ledger_sync_lock(&token) {
        return;
    }
    if let Ok(exe) = env::current_exe()
        && let Ok(child_pid) =
            platform::spawn_detached(&exe, &["--sync-ledger-bg", "--lock-token", &token])
    {
        let lock_path = platform::resolve_antigravity_path(LEDGER_BG_SYNC_LOCK_FILENAME);
        if let Ok(raw) = fs::read_to_string(&lock_path)
            && let Ok(parsed) = serde_json::from_str::<BgLedgerSyncLock>(&raw)
            && parsed.token == token
            && parsed.role == "spawning"
        {
            let updated = BgLedgerSyncLock {
                pid: child_pid,
                role: "worker".to_string(),
                token: token.clone(),
                ts: now_ms,
            };
            if let Ok(bytes) = serde_json::to_vec(&updated) {
                let _ = platform::write_cache_atomic_0600(&lock_path, &bytes);
            }
        }
        return;
    }
    release_bg_ledger_sync_lock_if_owner(Some(&token));
}

fn parse_lock_token_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--lock-token" {
            return iter
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        if let Some(rest) = arg.strip_prefix("--lock-token=") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn register_bg_worker_lock(token: Option<&str>) {
    let lock_path = platform::resolve_antigravity_path(LEDGER_BG_SYNC_LOCK_FILENAME);
    let payload = BgLedgerSyncLock {
        pid: std::process::id(),
        role: "worker".to_string(),
        token: token.unwrap_or_default().to_string(),
        ts: current_epoch_ms(),
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    match fs::read_to_string(&lock_path) {
        Ok(raw) => {
            if let Some(tok) = token
                && let Ok(parsed) = serde_json::from_str::<BgLedgerSyncLock>(&raw)
                && parsed.token == tok
            {
                let _ = platform::write_cache_atomic_0600(&lock_path, &bytes);
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let _ = platform::create_exclusive_file_0600(&lock_path, &bytes);
        }
        Err(_) => {}
    }
}

struct BgLedgerSyncLockGuard {
    token: Option<String>,
}

impl Drop for BgLedgerSyncLockGuard {
    fn drop(&mut self) {
        release_bg_ledger_sync_lock_if_owner(self.token.as_deref());
    }
}

fn run_sync_ledger_bg_cli(args: &[String]) -> i32 {
    let token = parse_lock_token_arg(args);
    register_bg_worker_lock(token.as_deref());
    let _guard = BgLedgerSyncLockGuard { token };
    let _ = panic::catch_unwind(|| {
        if let Ok(mut store) = ledger::LedgerStore::open_default() {
            let (default_account_id, device_name) = platform::read_sync_identity_hints();
            let opts = ledger::LedgerSyncOptions {
                default_account_id,
                device_name,
                ..Default::default()
            };
            let _ = store.sync_all_if_stale(60_000, &opts);
        }
    });
    0
}

fn is_truthy_env(name: &str) -> bool {
    env::var(name).ok().is_some_and(|raw| {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

fn maybe_spawn_background_remote_push(store: &ledger::LedgerStore, now_ms: i64) {
    if is_truthy_env("AGY_PULSE_NO_SYNC_AUTOSTART") {
        return;
    }
    let cfg_path = platform::resolve_sync_config_path();
    if !cfg_path.is_file() {
        return;
    }
    let Ok(Some(cfg)) = sync::load_sync_config_from(&cfg_path) else {
        return;
    };
    if !cfg.enabled {
        return;
    }
    let Ok(meta) = store.get_sync_meta() else {
        return;
    };
    let interval_ms = i64::try_from(cfg.push_interval_sec)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    if let Some(prev) = meta.last_sync_push_ms
        && now_ms >= prev
        && (now_ms - prev) < interval_ms
    {
        return;
    }
    if !store
        .has_pending_local_sync_changes(meta.last_sync_push_ms)
        .unwrap_or(false)
    {
        return;
    }
    if let Ok(exe) = env::current_exe() {
        let _ = platform::spawn_detached(&exe, &["--sync-bg"]);
    }
}

struct StatuslineWorkerOutput {
    output: Option<String>,
    summary: String,
}

fn resolve_post_stdin_hard_cap(process_elapsed: Duration) -> Duration {
    if let Ok(raw) = env::var("AGY_PULSE_POST_STDIN_HARD_CAP_MS")
        && let Ok(ms) = raw.trim().parse::<u64>()
    {
        return Duration::from_millis(ms.max(1));
    }
    let remaining_total = STATUSLINE_TOTAL_HARD_CAP
        .saturating_sub(process_elapsed)
        .max(Duration::from_millis(400));
    STATUSLINE_POST_STDIN_HARD_CAP.min(remaining_total)
}

fn render_hard_timeout_fallback(stdin_text: &str, fallback_cwd: &Path) -> String {
    let (snapshot, _workspace_dir) = HostReader::parse_stdin_fallback(stdin_text, fallback_cwd);
    let cfg = config::PulseConfig::default();
    let locale = platform::read_locale_env();
    let lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let unicode = cfg
        .display
        .unicode
        .unwrap_or_else(platform::detect_unicode_support);
    let columns = snapshot.terminal_width.or_else(|| {
        env::var("COLUMNS")
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|&w| w > 0)
    });
    let term = platform::TerminalCaps { unicode, columns };
    render::render_statusline(&snapshot, &cfg, &term, lang)
}

fn run_statusline_enriched(
    stdin_text: &str,
    fallback_cwd: &std::path::Path,
    stdin_ms: u64,
    pipeline_deadline: Instant,
) -> StatuslineWorkerOutput {
    let mut ingest = HostReader::read_live_pulse_with_deadline(
        stdin_text,
        fallback_cwd,
        Some(pipeline_deadline),
    );
    if !ingest.diagnostics.is_empty() {
        platform::write_error_log_safely(&ingest.diagnostics.join("\n"));
    }

    profile::set_stage(profile::Stage::Config);
    let t_cfg = Instant::now();
    let cfg = config::load_config(&ingest.workspace_dir);
    let cfg_ms = t_cfg.elapsed().as_millis() as u64;
    if !cfg.enabled {
        return StatuslineWorkerOutput {
            output: None,
            summary: format!("stdin={stdin_ms} disabled=1"),
        };
    }

    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let auto_start_dashboard =
        env::var("AGY_PULSE_NO_DASHBOARD_AUTOSTART").ok().as_deref() != Some("1");
    let show_dashboard_url = cfg.display.show_dashboard_url.unwrap_or(true);
    profile::set_stage(profile::Stage::DashboardResolve);
    let t_dash = Instant::now();
    let dashboard_info = if show_dashboard_url && Instant::now() < pipeline_deadline {
        dashboard::resolve_dashboard_url_for_statusline(
            &cfg,
            resolved_lang,
            auto_start_dashboard && Instant::now() < pipeline_deadline,
        )
    } else {
        None
    };
    let dash_ms = t_dash.elapsed().as_millis() as u64;
    if let Some(ref info) = dashboard_info {
        ingest.snapshot.dashboard_url = Some(info.url.clone());
    }

    profile::set_stage(profile::Stage::UpdateCheck);
    let t_upd = Instant::now();
    ingest.snapshot.update_available_version = ops::read_available_update_version();
    ops::maybe_spawn_background_update_check();
    let upd_ms = t_upd.elapsed().as_millis() as u64;

    let mut l_open_ms = 0u64;
    let mut l_bg_ms = 0u64;
    let mut l_sync_ms = 0u64;
    let mut l_roll_ms = 0u64;

    let show_daily = cfg.display.show_daily_cost.unwrap_or(false);
    let show_weekly = cfg.display.show_weekly_cost.unwrap_or(false);
    let active_account = ingest
        .snapshot
        .host
        .email
        .as_deref()
        .and_then(domain::normalize_account_id)
        .filter(|acct| acct != domain::UNASSIGNED_ACCOUNT_ID);
    let has_valid_quota = ingest
        .snapshot
        .quota
        .as_ref()
        .is_some_and(|q| domain::has_valid_provider_quota(q, ingest.snapshot.observed_at_ms));
    if (ingest.snapshot.has_conversation_identity
        || show_daily
        || show_weekly
        || active_account.is_some()
        || has_valid_quota)
        && Instant::now() < pipeline_deadline
    {
        let (default_account, device_name_hint) = platform::read_sync_identity_hints();
        let plan_tier = ingest
            .snapshot
            .host
            .plan_tier
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let sync_opts = ledger::LedgerSyncOptions {
            now_ms: Some(ingest.snapshot.observed_at_ms),
            active_account_id: active_account.clone(),
            default_account_id: default_account.clone(),
            device_name: device_name_hint.clone(),
            plan_tier: plan_tier.clone(),
            ..Default::default()
        };
        profile::set_stage(profile::Stage::LedgerOpen);
        let t_l_open = Instant::now();
        let opened = ledger::LedgerStore::open_default_for_statusline();
        l_open_ms = t_l_open.elapsed().as_millis() as u64;
        match opened {
            Ok(mut store) => {
                let mut ledger_write_busy = false;
                if let Some(ref acct) = active_account
                    && let Err(err) = store.set_last_observed_email(acct)
                    && err.is_busy()
                {
                    ledger_write_busy = true;
                }

                profile::set_stage(profile::Stage::LedgerBgSyncSpawn);
                let t_l_bg = Instant::now();
                if (show_daily || show_weekly)
                    && store
                        .is_full_scan_stale(60_000, ingest.snapshot.observed_at_ms)
                        .unwrap_or(false)
                {
                    maybe_spawn_background_ledger_sync();
                }
                l_bg_ms = t_l_bg.elapsed().as_millis() as u64;

                profile::set_stage(profile::Stage::LedgerSyncConv);
                let t_l_sync = Instant::now();
                if !ledger_write_busy
                    && Instant::now() < pipeline_deadline
                    && let Some(ref conv_id) = ingest.conversation_id
                    && let Err(err) = store.sync_conversation(
                        conv_id,
                        &ingest.child_conversation_ids,
                        &sync_opts,
                    )
                {
                    if err.is_busy() {
                        ledger_write_busy = true;
                    }
                    platform::write_error_log_safely(&format!(
                        "Ledger conversation sync error: {err}"
                    ));
                }
                l_sync_ms = t_l_sync.elapsed().as_millis() as u64;

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
                profile::set_stage(profile::Stage::LedgerRolling);
                let t_l_roll = Instant::now();
                if !ledger_write_busy
                    && (ingest.snapshot.has_conversation_identity || show_daily || show_weekly)
                    && Instant::now() < pipeline_deadline
                {
                    match store.record_rolling_costs(&rolling_input) {
                        Ok(Some(totals)) => {
                            ingest.snapshot.cost.today_usd = Some(totals.today_usd);
                            ingest.snapshot.cost.week_usd = Some(totals.week_usd);
                        }
                        Ok(None) => {}
                        Err(err) => {
                            if err.is_busy() {
                                ledger_write_busy = true;
                            }
                            platform::write_error_log_safely(&format!(
                                "Ledger rolling cost error: {err}"
                            ));
                        }
                    }
                }
                l_roll_ms = t_l_roll.elapsed().as_millis() as u64;

                if !ledger_write_busy
                    && has_valid_quota
                    && Instant::now() < pipeline_deadline
                    && let Some(ref quota) = ingest.snapshot.quota
                    && let Ok(local_device_id) = store.local_device_id()
                {
                    let quota_account_id = active_account
                        .clone()
                        .or_else(|| default_account.clone())
                        .or_else(|| store.last_observed_email().ok().flatten())
                        .unwrap_or_else(|| domain::UNASSIGNED_ACCOUNT_ID.to_string());
                    let resolved_device_name = device_name_hint
                        .clone()
                        .or_else(platform::current_hostname)
                        .unwrap_or_else(|| "local".to_string());
                    let qs = domain::QuotaSnapshot {
                        account_id: quota_account_id.clone(),
                        device_id: local_device_id.clone(),
                        device_name: resolved_device_name.clone(),
                        plan_tier: plan_tier.clone(),
                        observed_at_ms: ingest.snapshot.observed_at_ms,
                        quota: quota.clone(),
                    };
                    if let Err(err) = store.upsert_quota_snapshot(&qs)
                        && err.is_busy()
                    {
                        ledger_write_busy = true;
                    }
                    if !ledger_write_busy {
                        let _ = store.upsert_sync_device(
                            &quota_account_id,
                            &local_device_id,
                            &resolved_device_name,
                            true,
                            ingest.snapshot.observed_at_ms,
                            plan_tier.as_deref(),
                        );
                    }
                }

                if Instant::now() < pipeline_deadline {
                    maybe_spawn_background_remote_push(&store, ingest.snapshot.observed_at_ms);
                }
            }
            Err(err) => {
                platform::write_error_log_safely(&format!("Ledger open error: {err}"));
            }
        }
    }

    profile::set_stage(profile::Stage::Render);
    let t_render = Instant::now();
    let term = platform::detect_terminal_caps(
        ingest.snapshot.terminal_width,
        cfg.display.terminal_width,
        cfg.display.unicode,
    );
    let output = render::render_statusline(&ingest.snapshot, &cfg, &term, resolved_lang);
    let render_ms = t_render.elapsed().as_millis() as u64;

    profile::set_stage(profile::Stage::DashboardTouch);
    let t_touch = Instant::now();
    if Instant::now() < pipeline_deadline
        && let Some(ref info) = dashboard_info
        && info.running
    {
        let _ = dashboard::touch_running_dashboard(
            info,
            &cfg,
            resolved_lang,
            auto_start_dashboard,
            ingest.snapshot.observed_at_ms,
        );
    }
    let touch_ms = t_touch.elapsed().as_millis() as u64;

    let st = &ingest.stage_timings;
    let summary = format!(
        "stdin={stdin_ms} parse={} transcript={} speed={} compaction={} db_perf={} cascade={} ws={} cfg={cfg_ms} dash={dash_ms} upd={upd_ms} l_open={l_open_ms} l_bg={l_bg_ms} l_sync={l_sync_ms} l_roll={l_roll_ms} render={render_ms} touch={touch_ms}",
        st.parse_ms,
        st.transcript_ms,
        st.speed_ms,
        st.compaction_ms,
        st.db_perf_ms,
        st.cascade_ms,
        st.workspace_ms,
    );

    StatuslineWorkerOutput {
        output: Some(output),
        summary,
    }
}

fn run_statusline_pipeline(process_start: Instant) {
    let profiler = profile::Profiler::start_from_env();
    let _main_stage_guard = profile::enter_stage_scope(profiler.stage_handle());

    profile::set_stage(profile::Stage::Stdin);
    let t_stdin = Instant::now();
    let (stdin_text, timed_out) = read_stdin_with_timeout(STDIN_HARD_TIMEOUT);
    let stdin_ms = t_stdin.elapsed().as_millis() as u64;
    if timed_out && stdin_text.trim().is_empty() {
        platform::write_error_log_safely("Statusline stdin read timed out after 1500ms");
    }

    let hard_cap = resolve_post_stdin_hard_cap(process_start.elapsed());
    let soft_budget = STATUSLINE_POST_STDIN_BUDGET.min(hard_cap);
    let pipeline_deadline = Instant::now() + soft_budget;
    let fallback_cwd = env::current_dir().unwrap_or_default();

    let worker_stdin = stdin_text.clone();
    let worker_cwd = fallback_cwd.clone();
    let worker_stage = profiler.stage_handle();
    let (tx, rx) = mpsc::sync_channel::<StatuslineWorkerOutput>(1);
    let spawn_res = thread::Builder::new()
        .name("agy-pulse-worker".to_string())
        .spawn(move || {
            let _worker_stage_guard = profile::enter_stage_scope(worker_stage);
            let res = run_statusline_enriched(&worker_stdin, &worker_cwd, stdin_ms, pipeline_deadline);
            let _ = tx.send(res);
        });

    let result = match spawn_res {
        Ok(_) => rx
            .recv_timeout(hard_cap)
            .unwrap_or_else(|_| StatuslineWorkerOutput {
                output: Some(render_hard_timeout_fallback(&stdin_text, &fallback_cwd)),
                summary: format!("stdin={stdin_ms} hard_timeout_fallback=1"),
            }),
        Err(_) => run_statusline_enriched(&stdin_text, &fallback_cwd, stdin_ms, pipeline_deadline),
    };
    if let Some(output) = result.output
        && !output.is_empty()
    {
        profile::set_stage(profile::Stage::WriteStdout);
        let mut stdout = io::stdout().lock();
        let _ = stdout
            .write_all(output.as_bytes())
            .and_then(|()| stdout.flush());
    }
    profiler.finish(&result.summary);
}

/// Primary CLI entrypoint shared by `agy-pulse` and `agypl` binaries.
pub fn run_cli() -> i32 {
    let process_start = Instant::now();
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

    if matches!(first, Some("account")) {
        return ops::run_account_cli(&args);
    }

    if matches!(first, Some("sync")) {
        return sync::run_sync_cli(&args);
    }

    if matches!(first, Some("update")) || args.iter().any(|a| a == "--update") {
        return ops::run_update_cli(&args);
    }

    if args.iter().any(|a| a == "--check-update-bg") {
        return ops::run_check_update_bg_cli();
    }

    if args.iter().any(|a| a == "--sync-ledger-bg") {
        return run_sync_ledger_bg_cli(&args);
    }

    if args.iter().any(|a| a == "--sync-bg") {
        return sync::run_sync_bg();
    }

    if matches!(first, Some("help" | "--help" | "-h"))
        || args.iter().any(|a| a == "--help" || a == "-h")
    {
        print!("{}", build_cli_help_text());
        return 0;
    }

    let watchdog_thread = thread::Builder::new()
        .name("agy-pulse-exit-watchdog".into())
        .spawn(move || {
            thread::sleep(STATUSLINE_EXIT_TIMEOUT.saturating_sub(process_start.elapsed()));
            platform::exit_immediately(0);
        });
    if watchdog_thread.is_err() {
        return 0;
    }

    let res = panic::catch_unwind(|| run_statusline_pipeline(process_start));
    if res.is_err() {
        platform::write_error_log_safely("Recovered from unexpected panic in statusline mode");
    }
    // Keep the watchdog armed through normal process shutdown.
    std::process::exit(0)
}
