use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::dashboard::{
    DASHBOARD_SPAWN_LOCK_FILENAME, DASHBOARD_STATE_FILENAME, DEFAULT_DASHBOARD_PORT,
    LOOPBACK_HOST, stop_background_dashboard, uninstall_desktop_shortcuts,
};
use crate::platform;

use super::OpsError;

const LEDGER_PRIMARY_FILENAMES: &[&str] = &["agy-pulse-ledger.sqlite", "agy-pulse-ledger.json"];
const LEDGER_ALL_FILENAMES: &[&str] = &[
    "agy-pulse-ledger.sqlite",
    "agy-pulse-ledger.sqlite-wal",
    "agy-pulse-ledger.sqlite-shm",
    "agy-pulse-ledger.sqlite-journal",
    "agy-pulse-ledger.json",
];

const EXTRA_CACHE_TARGETS: &[&str] = &[
    "agy-pulse-payload.json",
    "agy-pulse-token.json",
    "agy-pulse-quota-cache.json",
    "agy-pulse-error.log",
    "agy-pulse-update-status.json",
    "agy-pulse-compaction-cache.json",
    DASHBOARD_STATE_FILENAME,
    DASHBOARD_SPAWN_LOCK_FILENAME,
    "agy-pulse-ledger-sync.lock",
    "agy-pulse-sync.lock",
    "daily-cost.json",
    "speed-cache",
    "transcript-cache",
    "hud",
    "statusline-combined.sh",
    "agy-statusline",
    "plugin_data/agy-pulse",
];

/// Summary of actions performed during `uninstall`.
#[derive(Debug, Clone)]
pub(super) struct UninstallReport {
    pub settings_path: PathBuf,
    pub primary_settings_status: &'static str,
    pub removed_bins: Vec<PathBuf>,
    pub removed_shortcuts: Vec<PathBuf>,
    pub removed_caches: Vec<PathBuf>,
    pub preserved_ledgers: Vec<PathBuf>,
    pub purged_ledgers: Vec<PathBuf>,
}

fn extract_executable_token(command: &str) -> &str {
    let trimmed = command.trim();
    if let Some(after_quote) = trimmed.strip_prefix('"') {
        after_quote.split('"').next().unwrap_or("")
    } else if let Some(after_quote) = trimmed.strip_prefix('\'') {
        after_quote.split('\'').next().unwrap_or("")
    } else {
        trimmed.split_whitespace().next().unwrap_or("")
    }
}

fn is_our_statusline_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.contains("agy-pulse-runtime")
        || trimmed.contains("statusline-combined.sh")
        || trimmed.contains("agy-pulse.js")
        || trimmed.contains("agy-pulse.cmd")
    {
        return true;
    }

    let token = extract_executable_token(trimmed);
    let basename = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    matches!(
        basename.as_str(),
        "agy-pulse" | "agy-pulse.exe" | "agypl" | "agypl.exe"
    )
}

fn clear_statusline_in_settings(settings_path: &Path) -> &'static str {
    if !settings_path.exists() {
        return "no_settings";
    }
    let Ok(raw) = fs::read_to_string(settings_path) else {
        return "read_error";
    };
    let Ok(mut root_val) = serde_json::from_str::<Value>(&raw) else {
        return "parse_error";
    };
    let Some(root_obj) = root_val.as_object_mut() else {
        return "parse_error";
    };
    let Some(cmd) = root_obj
        .get("statusLine")
        .and_then(|v| v.get("command"))
        .and_then(Value::as_str)
    else {
        return "no_status_line";
    };

    if !is_our_statusline_command(cmd) {
        return "foreign_status_line";
    }

    let bak_path = PathBuf::from(format!("{}.bak", settings_path.display()));
    let _ = fs::copy(settings_path, &bak_path);
    root_obj.remove("statusLine");

    if let Ok(mut json) = serde_json::to_string_pretty(&root_val) {
        json.push('\n');
        if platform::write_atomic_0600(settings_path, json.as_bytes()).is_ok() {
            return "cleared";
        }
    }
    "write_error"
}

fn collect_secondary_settings_paths(
    primary_settings: &Path,
    roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for root in roots {
        candidates.push(root.join("settings.json"));
    }
    if let Some(home) = platform::home_dir() {
        candidates.push(home.join(".gemini").join("config").join("settings.json"));
        candidates.push(home.join(".gemini").join("settings.json"));
    }
    if let Ok(xdg_cfg) = env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg_cfg.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed).join("gemini").join("settings.json"));
        }
    }

    let mut seen = HashSet::new();
    let primary_key =
        fs::canonicalize(primary_settings).unwrap_or_else(|_| primary_settings.to_path_buf());
    seen.insert(primary_key);

    let mut unique = Vec::new();
    for p in candidates {
        let key = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        if seen.insert(key) {
            unique.push(p);
        }
    }
    unique
}

fn remove_path_best_effort(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path).is_ok()
    } else {
        fs::remove_file(path).is_ok()
    }
}

fn remove_extra_files(root: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let runtime_dir = root.join("agy-pulse-runtime");
    if remove_path_best_effort(&runtime_dir) {
        removed.push(runtime_dir);
    }

    for rel in EXTRA_CACHE_TARGETS {
        let mut target = root.to_path_buf();
        for part in rel.split('/') {
            target.push(part);
        }
        if remove_path_best_effort(&target) {
            removed.push(target);
        }
    }
    removed
}

fn remove_temp_artifacts() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let tmp_dir = env::temp_dir();
    for name in ["agy-pulse-token.json", "agy-pulse-quota-cache.json"] {
        let p = tmp_dir.join(name);
        if remove_path_best_effort(&p) {
            removed.push(p);
        }
    }
    if let Ok(entries) = fs::read_dir(&tmp_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with("agy-pulse-db-")
                || name.starts_with("agy-pulse-bootstrap-")
                || name.starts_with("agy-pulse-update-")
            {
                let p = entry.path();
                if remove_path_best_effort(&p) {
                    removed.push(p);
                }
            }
        }
    }
    removed
}

fn remove_cli_binaries(bin_dir_override: Option<&Path>) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let candidates = [
        "agy-pulse",
        "agy-pulse.exe",
        "agy-pulse.exe.old",
        "agy-pulse.cmd",
        "agypl",
        "agypl.exe",
        "agypl.exe.old",
        "agypl.cmd",
    ];

    for dir in platform::cli_shim_candidate_dirs(bin_dir_override) {
        for name in candidates {
            let p = dir.join(name);
            if matches!(platform::remove_binary_file_safely(&p), Ok(true)) {
                removed.push(p);
            }
        }
    }
    removed
}

fn handle_ledger_files(
    root: &Path,
    purge_ledger: bool,
    seen: &mut HashSet<PathBuf>,
    preserved: &mut Vec<PathBuf>,
    purged: &mut Vec<PathBuf>,
) {
    if !purge_ledger {
        for name in LEDGER_PRIMARY_FILENAMES {
            let p = root.join(name);
            if p.exists() {
                let key = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
                if seen.insert(key) {
                    preserved.push(p);
                }
            }
        }
        return;
    }

    for name in LEDGER_ALL_FILENAMES {
        let p = root.join(name);
        if p.exists() {
            let key = fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            if fs::remove_file(&p).is_ok() && seen.insert(key) {
                purged.push(p);
            }
        }
    }
}

/// Safely uninstall `agy-pulse`, keeping `agy-pulse-ledger.sqlite` and config backups unless `purge_ledger` is true.
pub(super) fn uninstall(
    purge_ledger: bool,
    bin_dir_override: Option<&Path>,
) -> Result<UninstallReport, OpsError> {
    let _ = stop_background_dashboard(LOOPBACK_HOST, DEFAULT_DASHBOARD_PORT);

    let shortcut_report = uninstall_desktop_shortcuts();
    let roots = platform::get_antigravity_roots();
    let settings_path = platform::resolve_antigravity_path("settings.json");

    let primary_settings_status = clear_statusline_in_settings(&settings_path);
    for secondary in collect_secondary_settings_paths(&settings_path, &roots) {
        let _ = clear_statusline_in_settings(&secondary);
    }

    let mut removed_caches = Vec::new();
    let mut seen_ledgers = HashSet::new();
    let mut preserved_ledgers = Vec::new();
    let mut purged_ledgers = Vec::new();

    for root in &roots {
        removed_caches.extend(remove_extra_files(root));
        handle_ledger_files(
            root,
            purge_ledger,
            &mut seen_ledgers,
            &mut preserved_ledgers,
            &mut purged_ledgers,
        );
    }
    removed_caches.extend(remove_temp_artifacts());

    let removed_bins = remove_cli_binaries(bin_dir_override);

    Ok(UninstallReport {
        settings_path,
        primary_settings_status,
        removed_bins,
        removed_shortcuts: shortcut_report.removed,
        removed_caches,
        preserved_ledgers,
        purged_ledgers,
    })
}
