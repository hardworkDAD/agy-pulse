use crate::{collector, config, dashboard, domain, ledger, ops, platform, profile, render, sync};

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ingest::HostReader;
use crate::render::resolve_language_from_locale;

pub(crate) const STDIN_HARD_TIMEOUT: Duration = Duration::from_millis(1500);
pub(crate) const STATUSLINE_POST_STDIN_BUDGET: Duration = Duration::from_millis(900);
const STATUSLINE_POST_STDIN_HARD_CAP: Duration = Duration::from_millis(2500);
const STATUSLINE_TOTAL_HARD_CAP: Duration = Duration::from_millis(3500);
// Leave time for the host to collect output and observe exit before its 5-second timeout.
const STATUSLINE_EXIT_TIMEOUT: Duration = Duration::from_secs(4);
const LEDGER_BG_SYNC_LOCK_FILENAME: &str = "agy-pulse-ledger-sync.lock";
const LEDGER_BG_SYNC_SPAWNING_GRACE_MS: u64 = 5_000;
const LEDGER_BG_SYNC_SPAWNING_HANDOFF_GRACE_MS: u64 = 1_500;
const LEDGER_BG_SYNC_CLOCK_SKEW_TOLERANCE_MS: i64 = 5_000;
const LEDGER_BG_SYNC_LOCK_TTL_MS: u64 = 30_000;
const LEDGER_BG_SYNC_LOCK_MAX_TTL_MS: u64 = 10 * 60 * 1000;
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

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

pub(crate) fn read_stdin_with_timeout(timeout: Duration) -> (String, bool) {
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

fn is_bg_ledger_sync_lock_active<F>(
    parsed: Option<&BgLedgerSyncLock>,
    mtime_ms: Option<i64>,
    now_ms: i64,
    is_pid_alive: F,
) -> bool
where
    F: Fn(u32) -> bool,
{
    let mtime_ahead = mtime_ms.is_some_and(|m| m > now_ms);
    let ts_ahead = parsed.is_some_and(|p| p.ts > now_ms);

    if mtime_ms.is_some_and(|m| m - now_ms > LEDGER_BG_SYNC_CLOCK_SKEW_TOLERANCE_MS)
        || parsed
            .is_some_and(|p| p.ts > 0 && p.ts - now_ms > LEDGER_BG_SYNC_CLOCK_SKEW_TOLERANCE_MS)
    {
        return false;
    }

    let mtime_age_ms = mtime_ms.map(|m| now_ms.saturating_sub(m).max(0));
    let ts_age_ms = parsed
        .filter(|p| p.ts > 0)
        .map(|p| now_ms.saturating_sub(p.ts).max(0));
    let effective_age_ms = match (mtime_age_ms, ts_age_ms) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return false,
    };

    match parsed {
        Some(lock) if lock.role == "worker" && lock.pid > 0 => {
            is_pid_alive(lock.pid) && effective_age_ms < LEDGER_BG_SYNC_LOCK_MAX_TTL_MS as i64
        }
        Some(lock) if lock.role == "spawning" => {
            if lock.pid == 0 || is_pid_alive(lock.pid) {
                effective_age_ms < LEDGER_BG_SYNC_SPAWNING_GRACE_MS as i64
            } else {
                !mtime_ahead
                    && !ts_ahead
                    && effective_age_ms < LEDGER_BG_SYNC_SPAWNING_HANDOFF_GRACE_MS as i64
            }
        }
        _ => effective_age_ms < LEDGER_BG_SYNC_LOCK_TTL_MS as i64,
    }
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
                let mtime_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64);
                let parsed = fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<BgLedgerSyncLock>(&raw).ok());
                let check_now_ms = current_epoch_ms();

                if is_bg_ledger_sync_lock_active(
                    parsed.as_ref(),
                    mtime_ms,
                    check_now_ms,
                    platform::is_process_alive,
                ) {
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
    if platform::env_var("AGY_PULSE_DISABLE_BG_LEDGER_SYNC").as_deref() == Some("1") {
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
    platform::env_var(name).is_some_and(|raw| {
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

pub(crate) struct CollectedEnrichment {
    pub enabled: bool,
    pub snapshot: domain::PulseSnapshot,
    pub config: config::PulseConfig,
    pub workspace_dir: PathBuf,
    #[allow(dead_code)]
    pub conversation_id: Option<String>,
    pub summary: String,
}

pub(crate) fn resolve_post_stdin_hard_cap(process_elapsed: Duration) -> Duration {
    if let Some(raw) = platform::env_var("AGY_PULSE_POST_STDIN_HARD_CAP_MS")
        && let Ok(ms) = raw.trim().parse::<u64>()
    {
        return Duration::from_millis(ms.max(1));
    }
    let remaining_total = STATUSLINE_TOTAL_HARD_CAP
        .saturating_sub(process_elapsed)
        .max(Duration::from_millis(400));
    STATUSLINE_POST_STDIN_HARD_CAP.min(remaining_total)
}

pub(crate) fn render_hard_timeout_fallback(stdin_text: &str, fallback_cwd: &Path) -> String {
    let (snapshot, _workspace_dir) = HostReader::parse_stdin_fallback(stdin_text, fallback_cwd);
    let cfg = config::PulseConfig::default();
    let locale = platform::read_locale_env();
    let lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let unicode = cfg
        .display
        .unicode
        .unwrap_or_else(platform::detect_unicode_support);
    let columns = snapshot.terminal_width.or_else(|| {
        platform::env_var("COLUMNS")
            .and_then(|s| s.trim().parse::<u16>().ok())
            .filter(|&w| w > 0)
    });
    let term = platform::TerminalCaps { unicode, columns };
    render::render_statusline(&snapshot, &cfg, &term, lang)
}

pub(crate) fn render_snapshot_for_terminal(
    snapshot: &domain::PulseSnapshot,
    cfg: &config::PulseConfig,
) -> (String, u64) {
    profile::set_stage(profile::Stage::Render);
    let t_render = Instant::now();
    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let term = platform::detect_terminal_caps(
        snapshot.terminal_width,
        cfg.display.terminal_width,
        cfg.display.unicode,
    );
    let output = render::render_statusline(snapshot, cfg, &term, resolved_lang);
    let render_ms = t_render.elapsed().as_millis() as u64;
    (output, render_ms)
}

pub(crate) fn collect_statusline_enrichment(
    stdin_text: &str,
    fallback_cwd: &Path,
    stdin_ms: u64,
    pipeline_deadline: Instant,
) -> CollectedEnrichment {
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
        return CollectedEnrichment {
            enabled: false,
            snapshot: ingest.snapshot,
            config: cfg,
            workspace_dir: ingest.workspace_dir,
            conversation_id: ingest.conversation_id,
            summary: format!("stdin={stdin_ms} disabled=1"),
        };
    }

    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let auto_start_dashboard =
        platform::env_var("AGY_PULSE_NO_DASHBOARD_AUTOSTART").as_deref() != Some("1");
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
                    && let Err(err) = store.sync_conversation_with_prefetched(
                        conv_id,
                        &ingest.child_conversation_ids,
                        &sync_opts,
                        &ingest.prefetched_db_records,
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
        "stdin={stdin_ms} parse={} transcript={} speed={} compaction={} db_perf={} cascade={} ws={} cfg={cfg_ms} dash={dash_ms} upd={upd_ms} l_open={l_open_ms} l_bg={l_bg_ms} l_sync={l_sync_ms} l_roll={l_roll_ms} touch={touch_ms}",
        st.parse_ms,
        st.transcript_ms,
        st.speed_ms,
        st.compaction_ms,
        st.db_perf_ms,
        st.cascade_ms,
        st.workspace_ms,
    );

    CollectedEnrichment {
        enabled: true,
        snapshot: ingest.snapshot,
        config: cfg,
        workspace_dir: ingest.workspace_dir,
        conversation_id: ingest.conversation_id,
        summary,
    }
}

fn run_statusline_enriched(
    stdin_text: &str,
    fallback_cwd: &Path,
    stdin_ms: u64,
    pipeline_deadline: Instant,
) -> StatuslineWorkerOutput {
    let collected =
        collect_statusline_enrichment(stdin_text, fallback_cwd, stdin_ms, pipeline_deadline);
    if !collected.enabled {
        return StatuslineWorkerOutput {
            output: None,
            summary: collected.summary,
        };
    }
    let (output, render_ms) = render_snapshot_for_terminal(&collected.snapshot, &collected.config);
    StatuslineWorkerOutput {
        output: Some(output),
        summary: format!("{} render={render_ms}", collected.summary),
    }
}

fn run_statusline_pipeline(process_start: Instant) {
    if platform::env_var("AGY_PULSE_SYNC_MODE").as_deref() != Some("1") {
        collector::run_thin_statusline_pipeline(process_start);
        return;
    }

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
            let res =
                run_statusline_enriched(&worker_stdin, &worker_cwd, stdin_ms, pipeline_deadline);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliRoute {
    Version,
    Help,
    Dashboard,
    Stats,
    Config,
    Install,
    Uninstall,
    Account,
    Sync,
    Update,
    CheckUpdateBg,
    SyncLedgerBg,
    SyncBg,
    CollectorBg,
    Statusline,
}

fn resolve_cli_route(args: &[String]) -> CliRoute {
    let first = args.first().map(String::as_str);
    match first {
        Some("version" | "--version" | "-V" | "-v") => return CliRoute::Version,
        Some("help" | "--help" | "-h") => return CliRoute::Help,
        Some("dashboard" | "dash" | "ui" | "--dashboard") => return CliRoute::Dashboard,
        Some("stats" | "report" | "--stats") => return CliRoute::Stats,
        Some("config" | "--config") => return CliRoute::Config,
        Some("install" | "--install") => return CliRoute::Install,
        Some("uninstall" | "--uninstall") => return CliRoute::Uninstall,
        Some("account") => return CliRoute::Account,
        Some("sync") => return CliRoute::Sync,
        Some("update" | "--update") => return CliRoute::Update,
        Some("--check-update-bg") => return CliRoute::CheckUpdateBg,
        Some("--sync-ledger-bg") => return CliRoute::SyncLedgerBg,
        Some("--sync-bg") => return CliRoute::SyncBg,
        Some("--collector-bg") => return CliRoute::CollectorBg,
        _ => {}
    }

    // Legacy global-flag fallback only when the first argument is not an explicit subcommand.
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("--version") || has("-V") {
        CliRoute::Version
    } else if has("--dashboard") {
        CliRoute::Dashboard
    } else if has("--stats") {
        CliRoute::Stats
    } else if has("--config") {
        CliRoute::Config
    } else if has("--install") {
        CliRoute::Install
    } else if has("--uninstall") {
        CliRoute::Uninstall
    } else if has("--update") {
        CliRoute::Update
    } else if has("--check-update-bg") {
        CliRoute::CheckUpdateBg
    } else if has("--sync-ledger-bg") {
        CliRoute::SyncLedgerBg
    } else if has("--sync-bg") {
        CliRoute::SyncBg
    } else if has("--collector-bg") {
        CliRoute::CollectorBg
    } else if has("--help") || has("-h") {
        CliRoute::Help
    } else {
        CliRoute::Statusline
    }
}

fn run_statusline_with_exit_watchdog(process_start: Instant) -> i32 {
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    struct FinishGuard(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for FinishGuard {
        fn drop(&mut self) {
            let (lock, cvar) = &*self.0;
            if let Ok(mut done) = lock.lock() {
                *done = true;
                cvar.notify_all();
            }
        }
    }

    let watchdog_finished = Arc::clone(&finished);
    let watchdog_thread = thread::Builder::new()
        .name("agy-pulse-exit-watchdog".into())
        .spawn(move || {
            let timeout = STATUSLINE_EXIT_TIMEOUT.saturating_sub(process_start.elapsed());
            let (lock, cvar) = &*watchdog_finished;
            let done = if let Ok(guard) = lock.lock() {
                cvar.wait_timeout_while(guard, timeout, |d| !*d)
                    .map(|(g, _)| *g)
                    .unwrap_or(false)
            } else {
                false
            };
            if !done {
                platform::exit_immediately(0);
            }
        });
    if watchdog_thread.is_err() {
        return 0;
    }

    let _finish_guard = FinishGuard(Arc::clone(&finished));
    let res = panic::catch_unwind(|| run_statusline_pipeline(process_start));
    if res.is_err() {
        platform::write_error_log_safely("Recovered from unexpected panic in statusline mode");
    }
    0
}

/// Primary CLI entrypoint shared by `agy-pulse` and `agypl` binaries.
pub(crate) fn run_cli() -> i32 {
    let process_start = Instant::now();
    let args: Vec<String> = env::args().skip(1).collect();

    match resolve_cli_route(&args) {
        CliRoute::Version => {
            println!("agy-pulse v{VERSION}");
            0
        }
        CliRoute::Help => {
            print!("{}", build_cli_help_text());
            0
        }
        CliRoute::Dashboard => dashboard::run_dashboard_cli(&args),
        CliRoute::Stats => ledger::run_stats_cli(&args),
        CliRoute::Config => config::run_config_cli(),
        CliRoute::Install => ops::run_install_cli(&args),
        CliRoute::Uninstall => ops::run_uninstall_cli(&args),
        CliRoute::Account => ops::run_account_cli(&args),
        CliRoute::Sync => sync::run_sync_cli(&args),
        CliRoute::Update => ops::run_update_cli(&args),
        CliRoute::CheckUpdateBg => ops::run_check_update_bg_cli(),
        CliRoute::SyncLedgerBg => run_sync_ledger_bg_cli(&args),
        CliRoute::SyncBg => sync::run_sync_bg(),
        CliRoute::CollectorBg => collector::run_collector_bg_cli(&args),
        CliRoute::Statusline => run_statusline_with_exit_watchdog(process_start),
    }
}

#[cfg(test)]
mod tests {
    use super::{BgLedgerSyncLock, CliRoute, is_bg_ledger_sync_lock_active, resolve_cli_route};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn bg_ledger_sync_lock_handles_clock_rollback_and_spawning_handoff() {
        let now = 1_700_000_000_000i64;
        let worker_lock = BgLedgerSyncLock {
            pid: 4242,
            role: "worker".to_string(),
            token: "tok".to_string(),
            ts: now - 1_000,
        };

        // Normal live worker within TTL stays active.
        assert!(is_bg_ledger_sync_lock_active(
            Some(&worker_lock),
            Some(now - 1_000),
            now,
            |_| true
        ));

        // Future mtime beyond 5s skew tolerance is treated as clock rollback -> reclaimable.
        assert!(!is_bg_ledger_sync_lock_active(
            Some(&worker_lock),
            Some(now + 5_001),
            now,
            |_| true
        ));

        // Future parsed.ts beyond 5s skew tolerance is also reclaimable even if mtime looks normal.
        let future_ts_worker = BgLedgerSyncLock {
            ts: now + 5_001,
            ..worker_lock.clone()
        };
        assert!(!is_bg_ledger_sync_lock_active(
            Some(&future_ts_worker),
            Some(now - 100),
            now,
            |_| true
        ));

        // Stale parsed.ts beyond MAX_TTL is reclaimed even when file mtime was touched recently.
        let stale_ts_worker = BgLedgerSyncLock {
            ts: now - 11 * 60 * 1000,
            ..worker_lock
        };
        assert!(!is_bg_ledger_sync_lock_active(
            Some(&stale_ts_worker),
            Some(now - 100),
            now,
            |_| true
        ));

        // Spawning handoff grace: dead parent PID gets a short (<1500ms) window when not future.
        let fresh_spawning = BgLedgerSyncLock {
            pid: 9999,
            role: "spawning".to_string(),
            token: "tok-s".to_string(),
            ts: now - 400,
        };
        assert!(is_bg_ledger_sync_lock_active(
            Some(&fresh_spawning),
            Some(now - 400),
            now,
            |_| false
        ));

        // Past 1500ms handoff window with dead parent PID -> immediately reclaimable.
        let expired_handoff = BgLedgerSyncLock {
            ts: now - 1_500,
            ..fresh_spawning.clone()
        };
        assert!(!is_bg_ledger_sync_lock_active(
            Some(&expired_handoff),
            Some(now - 1_500),
            now,
            |_| false
        ));

        // Future timestamp in spawning state with dead parent PID gets NO handoff grace.
        let future_spawning = BgLedgerSyncLock {
            ts: now + 100,
            ..fresh_spawning
        };
        assert!(!is_bg_ledger_sync_lock_active(
            Some(&future_spawning),
            Some(now - 100),
            now,
            |_| false
        ));
    }

    #[test]
    fn cli_routing_prefers_first_subcommand_and_keeps_global_compat_flags() {
        assert_eq!(
            resolve_cli_route(&args(&["sync", "--stats"])),
            CliRoute::Sync
        );
        assert_eq!(
            resolve_cli_route(&args(&["account", "--version"])),
            CliRoute::Account
        );
        assert_eq!(
            resolve_cli_route(&args(&["stats", "--version"])),
            CliRoute::Stats
        );
        assert_eq!(
            resolve_cli_route(&args(&["install", "--dashboard"])),
            CliRoute::Install
        );
        assert_eq!(
            resolve_cli_route(&args(&["--dashboard", "--help"])),
            CliRoute::Dashboard
        );
        assert_eq!(
            resolve_cli_route(&args(&["--stats", "--help"])),
            CliRoute::Stats
        );
        assert_eq!(resolve_cli_route(&args(&["--version"])), CliRoute::Version);
        assert_eq!(resolve_cli_route(&args(&["-v"])), CliRoute::Version);
        assert_eq!(resolve_cli_route(&args(&["version"])), CliRoute::Version);
        assert_eq!(resolve_cli_route(&args(&["--help"])), CliRoute::Help);
        assert_eq!(resolve_cli_route(&args(&[])), CliRoute::Statusline);
        assert_eq!(
            resolve_cli_route(&args(&["--port", "3789", "--dashboard"])),
            CliRoute::Dashboard
        );
    }
}
