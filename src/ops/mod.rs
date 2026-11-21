mod installer;
mod uninstall;
mod updater;

use std::path::PathBuf;

use thiserror::Error;

pub use updater::{
    UpdateStatusRecord, maybe_spawn_background_update_check, read_available_update_version,
    validate_redirect_url, validate_release_url,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Errors produced by installation, uninstallation, and self-update operations.
#[derive(Debug, Error)]
pub enum OpsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("URL error: {0}")]
    InvalidUrl(String),
    #[error("Insecure protocol rejected: {0}")]
    InsecureProtocol(String),
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("Unsupported platform target for release binary download")]
    UnsupportedTarget,
    #[error("SHA-256 verification failed: {0}")]
    ChecksumMismatch(String),
    #[error("Release archive extraction failed: {0}")]
    Archive(String),
    #[error("Invalid semantic version: {0}")]
    InvalidVersion(String),
}

fn parse_bin_dir_flag(args: &[String]) -> Option<PathBuf> {
    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(val) = arg.strip_prefix("--bin-dir=") {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        } else if arg == "--bin-dir"
            && let Some(next) = args.get(i + 1)
        {
            let trimmed = next.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
        i += 1;
    }
    None
}

/// CLI entrypoint for `agypl install` / `agy-pulse --install`.
pub fn run_install_cli(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: agypl install [--bin-dir <dir>]\n\n\
             Deploy agy-pulse & agypl binaries to the user bin directory and register statusLine in host settings.json."
        );
        return 0;
    }

    let bin_dir_override = parse_bin_dir_flag(args);
    match installer::install(bin_dir_override.as_deref()) {
        Ok(report) => {
            println!("agy-pulse install complete");
            println!("settings: {}", report.settings_path.display());
            println!("statusLine: {}", report.command);
            println!("binDir: {}", report.bin_dir.display());
            for deployed in &report.deployed_bins {
                println!("cli: {}", deployed.display());
            }
            if !report.cleaned_stale_plugin_paths.is_empty() {
                println!(
                    "cleaned {} stale v1 plugin artifact(s)",
                    report.cleaned_stale_plugin_paths.len()
                );
            }
            0
        }
        Err(err) => {
            eprintln!("agy-pulse install error: {err}");
            1
        }
    }
}

/// CLI entrypoint for `agypl uninstall` / `agy-pulse --uninstall`.
pub fn run_uninstall_cli(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: agypl uninstall [--purge-ledger] [--bin-dir <dir>]\n\n\
             Safely remove agy-pulse statusLine registration, binaries, shortcuts, and caches.\n\
             Preserves agy-pulse-ledger.sqlite and user config backups by default unless --purge-ledger is passed."
        );
        return 0;
    }

    let purge_ledger = args
        .iter()
        .any(|a| a == "--purge-ledger" || a == "--purge");
    let bin_dir_override = parse_bin_dir_flag(args);

    match uninstall::uninstall(purge_ledger, bin_dir_override.as_deref()) {
        Ok(report) => {
            println!("agy-pulse uninstall complete");
            println!(
                "settings: {} ({})",
                report.settings_path.display(),
                report.primary_settings_status
            );
            for path in &report.removed_bins {
                println!("cli removed: {}", path.display());
            }
            for path in &report.removed_shortcuts {
                println!("dashboard shortcut removed: {}", path.display());
            }
            for path in &report.removed_caches {
                println!("cache removed: {}", path.display());
            }
            for path in &report.purged_ledgers {
                println!("ledger removed: {}", path.display());
            }
            for path in &report.preserved_ledgers {
                println!(
                    "ledger preserved: {} (pass --purge-ledger to remove)",
                    path.display()
                );
            }
            0
        }
        Err(err) => {
            eprintln!("agy-pulse uninstall error: {err}");
            1
        }
    }
}

/// CLI entrypoint for `agypl update` / `agy-pulse --update`.
pub fn run_update_cli(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: agypl update [--force]\n\n\
             Download the matching release archive from GitHub Releases, verify SHA256SUMS, and atomically update the binary."
        );
        return 0;
    }

    let force = args.iter().any(|a| a == "--force");
    match updater::perform_self_update(force) {
        Ok(report) => {
            if report.updated {
                println!(
                    "Updated agy-pulse from v{} to v{} ({})",
                    report.previous_version,
                    report.target_version,
                    report.asset_name
                );
            } else {
                println!(
                    "agy-pulse is already up to date (v{})",
                    report.previous_version
                );
            }
            0
        }
        Err(err) => {
            eprintln!("agy-pulse update error: {err}");
            1
        }
    }
}

/// Internal background CLI entrypoint (`--check-update-bg`) spawned detached after 24h cooldown.
pub fn run_check_update_bg_cli() -> i32 {
    let _ = updater::check_for_updates_and_persist();
    0
}
