use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::ingest::HostReader;
use crate::platform;
use crate::profile::{self, Profiler, Stage};

use super::protocol::{
    COLLECTOR_APP_ID, COLLECTOR_LOCK_TTL_MS, CollectorRequest, CollectorStateFile,
    CollectorWireRequest, CollectorWireResponse, IpcStream, acquire_collector_spawn_lock,
    connect_to_state, exe_mtime_matches, read_collector_state, read_frame,
    release_collector_spawn_lock_if_owner, send_shutdown_to_state, write_frame,
};
use super::snapshot::{
    CollectedStatuslineSnapshot, GlobalStatuslineSnapshot, compute_conversation_key,
    merge_live_stdin_into_global, merge_live_stdin_into_snapshot, read_conversation_snapshot,
    read_global_snapshot,
};

const WARM_WAIT_CAP: Duration = Duration::from_millis(800);
const SPAWN_WAIT_CAP: Duration = Duration::from_millis(600);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(150);

const FORWARDED_ENV_KEYS: &[&str] = &[
    "AGY_PULSE_DATA_DIR",
    "AGY_PULSE_IDE_DATA_DIR",
    "AGY_PULSE_RUNTIME_DIR",
    "AGY_PULSE_GLOBAL_CONFIG_DIR",
    "AGY_PULSE_SYNC_CONFIG",
    "AGY_PULSE_LANGUAGE",
    "AGY_PULSE_DASHBOARD_PORT",
    "AGY_PULSE_DASHBOARD_IDLE_SEC",
    "AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS",
    "AGY_PULSE_NO_DASHBOARD_AUTOSTART",
    "AGY_PULSE_NO_OPEN",
    "AGY_PULSE_NO_SYNC_AUTOSTART",
    "AGY_PULSE_DISABLE_BG_LEDGER_SYNC",
    "AGY_PULSE_DISABLE_UPDATE_CHECK",
    "AGY_PULSE_FORCE_ASCII",
    "AGY_PULSE_FORCE_UNICODE",
    "AGY_PULSE_COLLECTOR_IDLE_SEC",
    "AGY_PULSE_COLLECTOR_WAIT_MS",
    "AGY_PULSE_LEDGER_LOCK_BUDGET_MS",
    "AGY_PULSE_STATUSLINE_SOFT_BUDGET_MS",
    "AGY_PULSE_BIN_DIR",
    "AGY_PULSE_REPO",
    "AGY_PULSE_UPDATE_URL",
    "AGY_PULSE_RELEASE_BASE_URL",
    "AGY_PULSE_SYNC_TOKEN",
    "AGY_PULSE_SYNC_PASSWORD",
    "AGY_PULSE_S3_ACCESS_KEY",
    "AGY_PULSE_S3_SECRET_KEY",
    "HOME",
    "USERPROFILE",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
    "APPDATA",
    "LOCALAPPDATA",
    "LocalAppData",
    "AppData",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LANG",
    "USER",
    "USERNAME",
    "LOGNAME",
    "HOSTNAME",
    "COMPUTERNAME",
    "COLUMNS",
    "NO_COLOR",
];

fn collect_forwarded_env_overrides() -> HashMap<String, Option<String>> {
    let mut map = HashMap::with_capacity(FORWARDED_ENV_KEYS.len());
    for &key in FORWARDED_ENV_KEYS {
        map.insert(key.to_string(), platform::env_var(key));
    }
    map
}

fn resolve_wait_budget(hard_cap: Duration, has_warm_snapshot: bool) -> Duration {
    if let Some(raw) = platform::env_var("AGY_PULSE_COLLECTOR_WAIT_MS")
        && let Ok(ms) = raw.trim().parse::<u64>()
    {
        return Duration::from_millis(ms.max(1)).min(hard_cap);
    }
    if has_warm_snapshot {
        WARM_WAIT_CAP.min(hard_cap)
    } else {
        hard_cap
    }
}

fn state_matches_current_binary(state: &CollectorStateFile, exe_mtime_ms: i64) -> bool {
    if state.app != COLLECTOR_APP_ID || !platform::is_process_alive(state.pid) {
        return false;
    }
    if state.version != crate::VERSION {
        return false;
    }
    exe_mtime_matches(state.exe_mtime_ms, exe_mtime_ms)
}

fn try_connect_ready(
    exe_mtime_ms: i64,
    deadline: Instant,
) -> Option<(IpcStream, String)> {
    let state = read_collector_state()?;
    if !platform::is_process_alive(state.pid) {
        return None;
    }
    if !state_matches_current_binary(&state, exe_mtime_ms) {
        let _ = send_shutdown_to_state(&state, Duration::from_millis(150));
        return None;
    }
    let rem = deadline.checked_duration_since(Instant::now())?;
    let stream = connect_to_state(&state, rem.min(CONNECT_TIMEOUT)).ok()?;
    Some((stream, state.token))
}

fn connect_or_spawn_collector(wait_budget: Duration) -> Option<(IpcStream, String)> {
    let deadline = Instant::now() + wait_budget;
    let exe_mtime_ms = platform::current_exe_mtime_ms();

    if let Some(conn) = try_connect_ready(exe_mtime_ms, deadline) {
        return Some(conn);
    }

    let remaining = deadline.checked_duration_since(Instant::now())?;
    if acquire_collector_spawn_lock(COLLECTOR_LOCK_TTL_MS) {
        let spawn_ok = env::current_exe()
            .ok()
            .and_then(|exe| platform::spawn_detached(&exe, &["--collector-bg"]).ok())
            .is_some();
        if !spawn_ok {
            release_collector_spawn_lock_if_owner();
            return None;
        }
    }

    let poll_cap = SPAWN_WAIT_CAP
        .min(remaining.saturating_mul(3) / 4)
        .max(Duration::from_millis(50))
        .min(remaining);
    let poll_deadline = Instant::now() + poll_cap;
    let mut backoff_ms = 2u64;

    loop {
        if let Some(conn) = try_connect_ready(exe_mtime_ms, deadline) {
            return Some(conn);
        }
        let now = Instant::now();
        if now >= poll_deadline || now >= deadline {
            return None;
        }
        let sleep_for = Duration::from_millis(backoff_ms)
            .min(poll_deadline.saturating_duration_since(now))
            .min(deadline.saturating_duration_since(now));
        if !sleep_for.is_zero() {
            thread::sleep(sleep_for);
        }
        backoff_ms = (backoff_ms * 2).min(10);
    }
}

fn is_retryable_disconnect(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
    )
}

fn write_stdout_safely(output: &str) {
    if output.is_empty() {
        return;
    }
    profile::set_stage(Stage::WriteStdout);
    let mut stdout = io::stdout().lock();
    let _ = stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush());
}

fn render_fallback(
    warm: Option<&CollectedStatuslineSnapshot>,
    global: Option<&GlobalStatuslineSnapshot>,
    stdin_text: &str,
    fallback_cwd: &Path,
    stdin_ms: u64,
) -> String {
    if let Some(cached) = warm {
        if !cached.enabled {
            return format!("stdin={stdin_ms} collector_fallback=1 disabled=1");
        }
        let merged = merge_live_stdin_into_snapshot(cached, stdin_text, fallback_cwd);
        let (output, render_ms) = crate::render_snapshot_for_terminal(&merged, &cached.config);
        write_stdout_safely(&output);
        return format!("stdin={stdin_ms} collector_fallback=1 render={render_ms}");
    }

    if let Some(glob) = global {
        if !glob.enabled {
            return format!("stdin={stdin_ms} collector_fallback=1 disabled=1");
        }
        let merged = merge_live_stdin_into_global(glob, stdin_text, fallback_cwd);
        let (output, render_ms) = crate::render_snapshot_for_terminal(&merged, &glob.config);
        write_stdout_safely(&output);
        return format!("stdin={stdin_ms} collector_fallback=1 global=1 render={render_ms}");
    }

    let output = crate::render_hard_timeout_fallback(stdin_text, fallback_cwd);
    write_stdout_safely(&output);
    format!("stdin={stdin_ms} hard_timeout_fallback=1")
}

/// Thin frontend statusline pipeline: zero in-process SQLite/DB copies/disk ledger writes.
/// Reads `tmpfs` precomputed snapshots and delegates background enrichment to the single-instance collector.
pub(crate) fn run_thin_statusline_pipeline(process_start: Instant) {
    let profiler = Profiler::start_from_env();
    let _main_stage_guard = profile::enter_stage_scope(profiler.stage_handle());

    profile::set_stage(Stage::Stdin);
    let t_stdin = Instant::now();
    let (stdin_text, timed_out) = crate::read_stdin_with_timeout(crate::STDIN_HARD_TIMEOUT);
    let stdin_ms = t_stdin.elapsed().as_millis() as u64;
    if timed_out && stdin_text.trim().is_empty() {
        platform::write_error_log_safely("Statusline stdin read timed out after 1500ms");
    }

    let hard_cap = crate::resolve_post_stdin_hard_cap(process_start.elapsed());
    let fallback_cwd = env::current_dir().unwrap_or_default();

    if hard_cap <= Duration::from_millis(5) {
        let output = crate::render_hard_timeout_fallback(&stdin_text, &fallback_cwd);
        write_stdout_safely(&output);
        profiler.finish(&format!("stdin={stdin_ms} hard_timeout_fallback=1"));
        return;
    }

    let live_fields = HostReader::parse_stdin_live_fields(&stdin_text, &fallback_cwd);
    let conversation_key = compute_conversation_key(&live_fields);
    let warm_snap = read_conversation_snapshot(&conversation_key);
    let global_snap = if warm_snap.is_none() {
        read_global_snapshot()
    } else {
        None
    };
    let has_warm_snapshot = warm_snap.is_some();

    let wait_budget = resolve_wait_budget(hard_cap, has_warm_snapshot);
    let soft_budget_ms = (crate::STATUSLINE_POST_STDIN_BUDGET.min(hard_cap).as_millis() as u64).max(100);
    let env_overrides = collect_forwarded_env_overrides();

    let rpc_deadline = Instant::now() + wait_budget;
    let mut ready_summary: Option<String> = None;

    for attempt in 0..2 {
        let Some(connect_budget) = rpc_deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if connect_budget.is_zero() {
            break;
        }
        let Some((mut stream, token)) = connect_or_spawn_collector(connect_budget) else {
            break;
        };
        let Some(remaining) = rpc_deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if remaining.is_zero() {
            break;
        }

        let _ = stream.set_write_timeout(Some(remaining.min(Duration::from_millis(200))));
        let _ = stream.set_read_timeout(Some(remaining));
        let req = CollectorWireRequest::Collect(CollectorRequest {
            token,
            stdin_text: stdin_text.clone(),
            fallback_cwd: fallback_cwd.clone(),
            stdin_ms,
            conversation_key: conversation_key.clone(),
            has_warm_snapshot,
            wait_ms: remaining.as_millis() as u64,
            soft_budget_ms,
            env_overrides: env_overrides.clone(),
        });

        let retryable = match write_frame(&mut stream, &req) {
            Ok(()) => match read_frame::<_, CollectorWireResponse>(&mut stream) {
                Ok(CollectorWireResponse::Ready(payload)) => {
                    ready_summary = Some(if !payload.enabled {
                        payload.summary
                    } else {
                        let (output, render_ms) = crate::render_snapshot_for_terminal(
                            &payload.snapshot,
                            &payload.config,
                        );
                        write_stdout_safely(&output);
                        format!("{} render={render_ms}", payload.summary)
                    });
                    break;
                }
                Ok(CollectorWireResponse::Error(msg)) if msg == "shutting down" => true,
                Err(err) if is_retryable_disconnect(&err) => true,
                Ok(_) | Err(_) => false,
            },
            Err(err) => is_retryable_disconnect(&err),
        };

        if !retryable || attempt > 0 {
            break;
        }
        if let Some(left) = rpc_deadline.checked_duration_since(Instant::now())
            && left > Duration::from_millis(25)
        {
            thread::sleep(Duration::from_millis(10));
        } else {
            break;
        }
    }

    let summary = ready_summary.unwrap_or_else(|| {
        render_fallback(
            warm_snap.as_ref(),
            global_snap.as_ref(),
            &stdin_text,
            &fallback_cwd,
            stdin_ms,
        )
    });

    profiler.finish(&summary);
}

#[cfg(test)]
mod tests {
    use super::state_matches_current_binary;
    use crate::collector::protocol::{COLLECTOR_APP_ID, CollectorStateFile};

    fn sample_state(exe_mtime_ms: i64) -> CollectorStateFile {
        CollectorStateFile {
            app: COLLECTOR_APP_ID.to_string(),
            pid: std::process::id(),
            version: crate::VERSION.to_string(),
            exe_mtime_ms,
            mode: "collector".to_string(),
            socket_path: None,
            tcp_port: None,
            token: "tok".to_string(),
            started_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn client_state_matches_current_binary_handles_zero_and_mismatch_mtimes() {
        assert!(state_matches_current_binary(&sample_state(0), 1_700_000_000_000));
        assert!(state_matches_current_binary(&sample_state(1_700_000_000_000), 0));
        assert!(state_matches_current_binary(
            &sample_state(1_700_000_000_123),
            1_700_000_000_123
        ));
        assert!(!state_matches_current_binary(
            &sample_state(1_700_000_000_123),
            1_700_000_000_999
        ));
    }
}
