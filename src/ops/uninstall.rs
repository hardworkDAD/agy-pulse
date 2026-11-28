use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::dashboard::{
    DASHBOARD_SPAWN_LOCK_FILENAME, DASHBOARD_STATE_FILENAME, DEFAULT_DASHBOARD_PORT, LOOPBACK_HOST,
    stop_background_dashboard, uninstall_desktop_shortcuts,
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

fn collect_secondary_settings_paths(primary_settings: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
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

fn remove_non_symlink_entry(path: &Path, allow_dir: bool) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return false;
    }
    if meta.is_dir() {
        allow_dir && fs::remove_dir_all(path).is_ok()
    } else {
        fs::remove_file(path).is_ok()
    }
}

fn is_known_runtime_artifact_name(name: &str) -> bool {
    if name.starts_with("agy-pulse-ledger") {
        return false;
    }
    matches!(
        name,
        "collector.sock" | "collector.json" | "collector.lock" | "snap-global.json"
    ) || (name.starts_with("snap-") && name.ends_with(".json"))
        || (name.starts_with("agyp-") && name.ends_with(".sock"))
        || name.starts_with("agy-pulse-db-")
        || name.starts_with("agy-pulse-bootstrap-")
        || name.starts_with("agy-pulse-update-")
        || (name.starts_with("agy-pulse-")
            && (name.ends_with(".sock") || name.ends_with(".lock") || name.ends_with(".pid")))
}

fn clean_dedicated_runtime_dir(
    dir: &Path,
    seen_dirs: &mut HashSet<PathBuf>,
    removed: &mut Vec<PathBuf>,
) {
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return;
    }
    let key = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !seen_dirs.insert(key) {
        return;
    }

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if is_known_runtime_artifact_name(&name) {
                let entry_path = entry.path();
                if remove_non_symlink_entry(&entry_path, true) {
                    removed.push(entry_path);
                }
            }
        }
    }

    if fs::read_dir(dir).is_ok_and(|mut it| it.next().is_none()) {
        let _ = fs::remove_dir(dir);
    }
}

fn clean_shared_temp_dir(dir: &Path, seen_dirs: &mut HashSet<PathBuf>, removed: &mut Vec<PathBuf>) {
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return;
    }
    let key = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !seen_dirs.insert(key) {
        return;
    }

    for name in ["agy-pulse-token.json", "agy-pulse-quota-cache.json"] {
        let p = dir.join(name);
        if remove_non_symlink_entry(&p, false) {
            removed.push(p);
        }
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with("agy-pulse-db-")
                || name.starts_with("agy-pulse-bootstrap-")
                || name.starts_with("agy-pulse-update-")
                || (name.starts_with("agyp-") && name.ends_with(".sock"))
            {
                let p = entry.path();
                if remove_non_symlink_entry(&p, true) {
                    removed.push(p);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn current_process_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn current_process_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    platform::home_dir().and_then(|h| fs::symlink_metadata(h).ok().map(|m| m.uid()))
}

fn remove_temp_artifacts() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut seen_runtime_dirs = HashSet::new();
    let mut seen_shared_dirs = HashSet::new();

    let mut runtime_dirs = Vec::new();
    if let Some(override_dir) = platform::env_var("AGY_PULSE_RUNTIME_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            runtime_dirs.push(PathBuf::from(trimmed));
        }
    }
    if let Some(data_dir) = platform::env_var("AGY_PULSE_DATA_DIR") {
        let trimmed = data_dir.trim();
        if !trimmed.is_empty() {
            runtime_dirs.push(PathBuf::from(trimmed).join("runtime"));
        }
    }
    for root in platform::get_antigravity_roots() {
        runtime_dirs.push(root.join("runtime"));
    }

    #[cfg(unix)]
    {
        if let Some(xdg) = platform::env_var("XDG_RUNTIME_DIR") {
            let trimmed = xdg.trim();
            if !trimmed.is_empty() {
                let xdg_path = PathBuf::from(trimmed);
                if fs::symlink_metadata(&xdg_path)
                    .is_ok_and(|m| !m.file_type().is_symlink() && m.is_dir())
                {
                    runtime_dirs.push(xdg_path.join("agy-pulse"));
                    runtime_dirs.push(xdg_path.join("agy-pulse-tmp"));
                    clean_shared_temp_dir(&xdg_path, &mut seen_shared_dirs, &mut removed);
                }
            }
        }
        if let Some(home) = platform::home_dir() {
            runtime_dirs.push(home.join(".agy-pulse").join("runtime"));
        }
        if let Some(uid) = current_process_uid() {
            for base in [env::temp_dir(), PathBuf::from("/tmp")] {
                runtime_dirs.push(base.join(format!("agy-pulse-{uid}")));
            }
            #[cfg(target_os = "linux")]
            {
                let shm = PathBuf::from("/dev/shm");
                if fs::symlink_metadata(&shm)
                    .is_ok_and(|m| !m.file_type().is_symlink() && m.is_dir())
                {
                    runtime_dirs.push(shm.join(format!("agy-pulse-{uid}")));
                    runtime_dirs.push(shm.join(format!("agy-pulse-tmp-{uid}")));
                    clean_shared_temp_dir(&shm, &mut seen_shared_dirs, &mut removed);
                }
            }
        }
    }

    #[cfg(windows)]
    {
        if let Some(local_app) = platform::windows_local_app_data() {
            runtime_dirs.push(local_app.join("agy-pulse").join("runtime"));
        }
        runtime_dirs.push(env::temp_dir().join("agy-pulse").join("runtime"));
    }

    for dir in runtime_dirs {
        clean_dedicated_runtime_dir(&dir, &mut seen_runtime_dirs, &mut removed);
    }

    let mut temp_candidates = vec![env::temp_dir()];
    #[cfg(unix)]
    temp_candidates.push(PathBuf::from("/tmp"));
    for var in ["TMPDIR", "TMP", "TEMP"] {
        if let Some(val) = platform::env_var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                temp_candidates.push(PathBuf::from(trimmed));
            }
        }
    }
    for tmp_dir in temp_candidates {
        clean_shared_temp_dir(&tmp_dir, &mut seen_shared_dirs, &mut removed);
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn clean_dedicated_runtime_dir_removes_only_runtime_artifacts_and_preserves_ledger() {
        let dir = TempDir::new().expect("temp dir");
        let rt = dir.path().join("agy-pulse");
        fs::create_dir_all(&rt).expect("mkdir");

        fs::write(rt.join("collector.json"), b"{}").expect("write");
        fs::write(rt.join("collector.lock"), b"{}").expect("write");
        fs::write(rt.join("snap-global.json"), b"{}").expect("write");
        fs::write(rt.join("snap-1234.json"), b"{}").expect("write");
        fs::write(rt.join("agy-pulse-ledger.sqlite"), b"keep").expect("write");

        let mut seen = HashSet::new();
        let mut removed = Vec::new();
        clean_dedicated_runtime_dir(&rt, &mut seen, &mut removed);

        assert!(!rt.join("collector.json").exists());
        assert!(!rt.join("collector.lock").exists());
        assert!(!rt.join("snap-global.json").exists());
        assert!(!rt.join("snap-1234.json").exists());
        assert!(
            rt.join("agy-pulse-ledger.sqlite").is_file(),
            "runtime dir cleanup must never delete ledger files"
        );
        assert_eq!(removed.len(), 4);
    }
}
