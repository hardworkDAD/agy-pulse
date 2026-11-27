use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::panic;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::domain::SyncDeviceRecord;
use crate::ledger::{LedgerSyncOptions, LedgerStore, validate_and_normalize_account_email};
use crate::platform;

use super::config::{
    DEFAULT_PUSH_INTERVAL_SEC, DEFAULT_SYNC_PREFIX, RemoteConfig, SyncConfig, load_sync_config,
    save_sync_config,
};
use super::http_util::{is_loopback_host, redact_error_message};
use super::{SyncError, SyncOutcomeKind, build_remote, prune_device, pull, push, sync_both};

const SYNC_BG_HARD_TIMEOUT_SECS: u64 = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncMode {
    Both,
    Push,
    Pull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SyncCliArgs {
    mode: SyncMode,
    dry_run: bool,
    no_local_scan: bool,
    help: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitCliArgs {
    pub help: bool,
    pub remote_type: Option<String>,
    pub path: Option<String>,
    pub url: Option<String>,
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub path_style: Option<bool>,
    pub allow_insecure_http: bool,
    pub device_name: Option<String>,
    pub default_account: Option<String>,
    pub prefix: Option<String>,
    pub push_interval_sec: Option<u64>,
    pub skip_probe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PruneCliArgs {
    help: bool,
    device_id: String,
    force: bool,
    dry_run: bool,
}

fn print_sync_help() {
    println!(
        "Usage: agypl sync [subcommand] [options]\n\n\
         Subcommands:\n  \
           (default)                            Run bidirectional sync (pull then push)\n  \
           init [options]                       Configure sync remote and test connectivity\n  \
           status                               Show sync status, remote target, and known devices\n  \
           prune-device <device_id> [options]   Delete remote & cached data for a device\n\n\
         Sync options:\n  \
           --push                               Only push local data to remote\n  \
           --pull                               Only pull remote data into local ledger\n  \
           --dry-run                            Preview changes without writing remote or local DB\n  \
           --no-local-scan                      Skip scanning local conversations/*.db before sync\n  \
           -h, --help                           Show this help message"
    );
}

fn print_init_help() {
    println!(
        "Usage: agypl sync init [options]\n\n\
         Interactive or flag-driven setup for ~/.gemini/antigravity-cli/agy-pulse-sync.json (0600).\n\
         Runs a live connectivity probe before saving unless --skip-probe is passed.\n\n\
         Options:\n  \
           --type <local-dir|webdav|s3|cf-worker>  Remote backend type\n  \
           --path <dir>                            Directory path (local-dir)\n  \
           --url <url>                             Remote URL (webdav, cf-worker)\n  \
           --endpoint <url>                        S3 endpoint URL (s3)\n  \
           --bucket <name>                         S3 bucket name (s3)\n  \
           --region <region>                       S3 region (default: auto)\n  \
           --username <user>                       WebDAV username\n  \
           --password <pass>                       WebDAV password\n  \
           --token <token>                         Bearer token (webdav, cf-worker)\n  \
           --access-key <ak>                       S3 access key\n  \
           --secret-key <sk>                       S3 secret key\n  \
           --path-style <true|false>               S3 path-style addressing (default: true)\n  \
           --allow-insecure-http                   Allow non-loopback http:// URLs\n  \
           --device-name <name>                    Custom display name for this device\n  \
           --default-account <email>               Default fallback account email\n  \
           --prefix <prefix>                       Remote object key prefix (default: agy-pulse-sync)\n  \
           --push-interval <sec>                   Background push interval in seconds (default: 300)\n  \
           --skip-probe                            Skip connectivity probe before saving\n  \
           -h, --help                              Show this help message"
    );
}

fn print_status_help() {
    println!(
        "Usage: agypl sync status\n\n\
         Show current multi-device sync configuration, redacted remote target, timestamps, and known devices."
    );
}

fn print_prune_help() {
    println!(
        "Usage: agypl sync prune-device <device_id> [--force] [--dry-run]\n\n\
         Remove remote objects and local SQLite rows for <device_id>.\n\
         Refuses to prune the local device unless --force is specified.\n\n\
         Options:\n  \
           --force      Allow pruning the local device from remote storage\n  \
           --dry-run    Report objects that would be deleted without modifying remote or local DB\n  \
           -h, --help   Show this help message"
    );
}

fn collect_secrets(cfg: Option<&SyncConfig>) -> Vec<String> {
    let mut secrets = Vec::new();
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
        if let Some(val) = platform::env_var(env_name) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                secrets.push(trimmed.to_string());
                if env_name == "AGY_PULSE_SYNC_TOKEN" {
                    secrets.push(format!("Bearer {trimmed}"));
                }
            }
        }
    }
    secrets
}

fn redact_with_env(msg: &str, cfg: Option<&SyncConfig>) -> String {
    let secrets = collect_secrets(cfg);
    redact_error_message(msg, &secrets)
}

fn format_epoch_ms(ts_ms: Option<i64>) -> String {
    match ts_ms {
        Some(ms) => {
            if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(ms) {
                dt.to_rfc3339()
            } else {
                ms.to_string()
            }
        }
        None => "never".to_string(),
    }
}

fn parse_bool_flag_value(raw: &str, flag_name: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" => Ok(true),
        "false" | "0" | "no" | "n" => Ok(false),
        _ => Err(format!(
            "invalid boolean value '{raw}' for {flag_name} (expected true or false)"
        )),
    }
}

fn next_flag_value<'a>(
    flag: &str,
    inline: Option<&'a str>,
    args: &'a [String],
    idx: &mut usize,
) -> Result<&'a str, String> {
    if let Some(v) = inline {
        return Ok(v);
    }
    let Some(next) = args.get(*idx + 1) else {
        return Err(format!("{flag} requires a value"));
    };
    *idx += 1;
    Ok(next.as_str())
}

fn parse_sync_args(args: &[String]) -> Result<SyncCliArgs, String> {
    let mut push_flag = false;
    let mut pull_flag = false;
    let mut dry_run = false;
    let mut no_local_scan = false;
    let mut help = false;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => help = true,
            "--push" => push_flag = true,
            "--pull" => pull_flag = true,
            "--dry-run" => dry_run = true,
            "--no-local-scan" => no_local_scan = true,
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    if push_flag && pull_flag {
        return Err("cannot combine --push and --pull".to_string());
    }

    let mode = if push_flag {
        SyncMode::Push
    } else if pull_flag {
        SyncMode::Pull
    } else {
        SyncMode::Both
    };

    Ok(SyncCliArgs {
        mode,
        dry_run,
        no_local_scan,
        help,
    })
}

fn parse_init_args(args: &[String]) -> Result<InitCliArgs, String> {
    let mut out = InitCliArgs::default();
    let mut i = 0usize;

    while i < args.len() {
        let arg = args[i].as_str();
        if matches!(arg, "-h" | "--help") {
            out.help = true;
            i += 1;
            continue;
        }
        if arg == "--skip-probe" {
            out.skip_probe = true;
            i += 1;
            continue;
        }
        if arg == "--allow-insecure-http" {
            out.allow_insecure_http = true;
            i += 1;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--allow-insecure-http=") {
            out.allow_insecure_http = parse_bool_flag_value(val, "--allow-insecure-http")?;
            i += 1;
            continue;
        }

        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v)),
            None => (arg, None),
        };

        match flag {
            "--type" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                if val.is_empty() {
                    return Err("--type must not be empty".to_string());
                }
                out.remote_type = Some(val.to_string());
            }
            "--path" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.path = Some(val.to_string());
            }
            "--url" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.url = Some(val.to_string());
            }
            "--endpoint" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.endpoint = Some(val.to_string());
            }
            "--bucket" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.bucket = Some(val.to_string());
            }
            "--region" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.region = Some(val.to_string());
            }
            "--username" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.username = Some(val.to_string());
            }
            "--password" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.password = Some(val.to_string());
            }
            "--token" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.token = Some(val.to_string());
            }
            "--access-key" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.access_key = Some(val.to_string());
            }
            "--secret-key" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                out.secret_key = Some(val.to_string());
            }
            "--path-style" => {
                let val = next_flag_value(flag, inline, args, &mut i)?;
                out.path_style = Some(parse_bool_flag_value(val, "--path-style")?);
            }
            "--device-name" => {
                let val = next_flag_value(flag, inline, args, &mut i)?;
                out.device_name = Some(val.to_string());
            }
            "--default-account" => {
                let val = next_flag_value(flag, inline, args, &mut i)?;
                out.default_account = Some(val.to_string());
            }
            "--prefix" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                if val.is_empty() {
                    return Err("--prefix must not be empty".to_string());
                }
                out.prefix = Some(val.to_string());
            }
            "--push-interval" => {
                let val = next_flag_value(flag, inline, args, &mut i)?.trim();
                let parsed = val
                    .parse::<u64>()
                    .map_err(|_| format!("invalid integer '{val}' for --push-interval"))?;
                if parsed == 0 {
                    return Err("--push-interval must be >= 1".to_string());
                }
                out.push_interval_sec = Some(parsed);
            }
            _ => return Err(format!("unknown option '{arg}'")),
        }

        i += 1;
    }

    Ok(out)
}

fn parse_prune_args(args: &[String]) -> Result<PruneCliArgs, String> {
    let mut help = false;
    let mut force = false;
    let mut dry_run = false;
    let mut device_id: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => help = true,
            "--force" => force = true,
            "--dry-run" => dry_run = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            positional => {
                if device_id.is_some() {
                    return Err(format!("unexpected extra argument '{positional}'"));
                }
                let trimmed = positional.trim();
                if trimmed.is_empty() {
                    return Err("device_id must not be empty".to_string());
                }
                device_id = Some(trimmed.to_string());
            }
        }
    }

    if help {
        return Ok(PruneCliArgs {
            help: true,
            device_id: device_id.unwrap_or_default(),
            force,
            dry_run,
        });
    }

    let target = device_id.ok_or_else(|| "missing required <device_id> argument".to_string())?;
    Ok(PruneCliArgs {
        help: false,
        device_id: target,
        force,
        dry_run,
    })
}

fn read_existing_sync_config_raw(path: &Path) -> Option<SyncConfig> {
    let raw = fs::read_to_string(path).ok()?;
    let mut cfg: SyncConfig = serde_json::from_str(&raw).ok()?;
    cfg.normalize();
    Some(cfg)
}

fn normalize_remote_type_choice(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "local-dir" | "local_dir" | "localdir" | "local" => Some("local-dir"),
        "2" | "webdav" | "web-dav" => Some("webdav"),
        "3" | "s3" => Some("s3"),
        "4" | "cf-worker" | "cf_worker" | "cfworker" | "worker" => Some("cf-worker"),
        _ => None,
    }
}

fn validate_optional_account_input(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let normalized = validate_and_normalize_account_email(trimmed)
        .ok_or_else(|| format!("invalid email '{trimmed}'; expected <name>@<domain>"))?;
    Ok(Some(normalized))
}

fn non_empty_clean(opt: Option<&str>) -> Option<String> {
    opt.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn url_needs_insecure_http_prompt(raw_url: &str) -> bool {
    let trimmed = raw_url.trim();
    if !trimmed.to_ascii_lowercase().starts_with("http://") {
        return false;
    }
    url::Url::parse(trimmed)
        .ok()
        .and_then(|u| u.host_str().map(is_loopback_host))
        != Some(true)
}

fn build_sync_config_from_flags(
    existing: Option<&SyncConfig>,
    args: &InitCliArgs,
) -> Result<SyncConfig, String> {
    let raw_type = args
        .remote_type
        .as_deref()
        .ok_or_else(|| "missing --type".to_string())?;
    let rtype = normalize_remote_type_choice(raw_type)
        .ok_or_else(|| format!("unsupported remote type '{raw_type}'"))?;

    let remote = match rtype {
        "local-dir" => {
            let path = non_empty_clean(args.path.as_deref()).ok_or_else(|| {
                "--path is required when --type local-dir is selected".to_string()
            })?;
            RemoteConfig::LocalDir { path }
        }
        "webdav" => {
            let url = non_empty_clean(args.url.as_deref())
                .ok_or_else(|| "--url is required when --type webdav is selected".to_string())?;
            RemoteConfig::WebDav {
                url,
                username: non_empty_clean(args.username.as_deref()),
                password: non_empty_clean(args.password.as_deref()),
                token: non_empty_clean(args.token.as_deref()),
                allow_insecure_http: args.allow_insecure_http,
            }
        }
        "s3" => {
            let endpoint = non_empty_clean(args.endpoint.as_deref()).ok_or_else(|| {
                "--endpoint is required when --type s3 is selected".to_string()
            })?;
            let bucket = non_empty_clean(args.bucket.as_deref())
                .ok_or_else(|| "--bucket is required when --type s3 is selected".to_string())?;
            let region = non_empty_clean(args.region.as_deref()).unwrap_or_else(|| "auto".into());
            RemoteConfig::S3 {
                endpoint,
                region,
                bucket,
                access_key: non_empty_clean(args.access_key.as_deref()),
                secret_key: non_empty_clean(args.secret_key.as_deref()),
                path_style: args.path_style.unwrap_or(true),
                allow_insecure_http: args.allow_insecure_http,
            }
        }
        "cf-worker" => {
            let url = non_empty_clean(args.url.as_deref()).ok_or_else(|| {
                "--url is required when --type cf-worker is selected".to_string()
            })?;
            RemoteConfig::CfWorker {
                url,
                token: non_empty_clean(args.token.as_deref()),
                allow_insecure_http: args.allow_insecure_http,
            }
        }
        _ => return Err(format!("unsupported remote type '{raw_type}'")),
    };

    let device_name = match args.device_name.as_deref() {
        Some(raw) => platform::sanitize_device_name(raw),
        None => existing
            .and_then(|c| c.device_name.clone())
            .or_else(platform::current_hostname),
    };

    let default_account = match args.default_account.as_deref() {
        Some(raw) => validate_optional_account_input(raw)?,
        None => existing.and_then(|c| c.default_account.clone()),
    };

    let prefix = args
        .prefix
        .clone()
        .or_else(|| existing.map(|c| c.prefix.clone()))
        .unwrap_or_else(|| DEFAULT_SYNC_PREFIX.to_string());

    let push_interval_sec = args
        .push_interval_sec
        .or_else(|| existing.map(|c| c.push_interval_sec))
        .unwrap_or(DEFAULT_PUSH_INTERVAL_SEC)
        .max(1);

    let mut cfg = SyncConfig {
        enabled: true,
        device_id: existing.and_then(|c| c.device_id.clone()),
        device_name,
        default_account,
        push_interval_sec,
        prefix,
        remote,
    };
    cfg.normalize();
    Ok(cfg)
}

fn prompt_line<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
    default: Option<&str>,
) -> Result<Option<String>, String> {
    match default {
        Some(def) if !def.is_empty() => {
            write!(writer, "{label} [{def}]: ").map_err(|e| e.to_string())?;
        }
        _ => {
            write!(writer, "{label}: ").map_err(|e| e.to_string())?;
        }
    }
    writer.flush().map_err(|e| e.to_string())?;

    let mut buf = String::new();
    let bytes = reader.read_line(&mut buf).map_err(|e| e.to_string())?;
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(buf.trim_end_matches(['\r', '\n']).trim().to_string()))
}

fn prompt_required<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
    default: Option<&str>,
) -> Result<String, String> {
    for _ in 0..5 {
        match prompt_line(reader, writer, label, default)? {
            Some(val) if !val.is_empty() => return Ok(val),
            Some(_) => {
                if let Some(def) = default
                    && !def.is_empty()
                {
                    return Ok(def.to_string());
                }
                writeln!(writer, "Value is required.").map_err(|e| e.to_string())?;
            }
            None => {
                if let Some(def) = default
                    && !def.is_empty()
                {
                    return Ok(def.to_string());
                }
                return Err(format!("missing required input for '{label}'"));
            }
        }
    }
    Err(format!("missing required input for '{label}'"))
}

fn prompt_optional<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
    default: Option<&str>,
) -> Result<Option<String>, String> {
    match prompt_line(reader, writer, label, default)? {
        Some(val) if val.is_empty() => Ok(default.map(ToOwned::to_owned)),
        Some(val) => Ok(Some(val)),
        None => Ok(default.map(ToOwned::to_owned)),
    }
}

fn prompt_yes_no<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
    default: bool,
) -> Result<bool, String> {
    let hint = if default { "Y/n" } else { "y/N" };
    match prompt_line(reader, writer, &format!("{label} ({hint})"), None)? {
        Some(val) if val.is_empty() => Ok(default),
        Some(val) => match val.to_ascii_lowercase().as_str() {
            "y" | "yes" | "true" | "1" => Ok(true),
            "n" | "no" | "false" | "0" => Ok(false),
            _ => Err(format!("invalid yes/no response '{val}'")),
        },
        None => Ok(default),
    }
}

/// Build a `SyncConfig` interactively from `reader` and `writer`, applying any CLI overrides.
pub fn build_sync_config_interactive<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    existing: Option<&SyncConfig>,
    overrides: &InitCliArgs,
) -> Result<SyncConfig, String> {
    let default_type = existing.map(SyncConfig::remote_type_str).unwrap_or("local-dir");
    writeln!(writer, "Multi-Device Sync Setup").map_err(|e| e.to_string())?;
    writeln!(
        writer,
        "Select remote backend: 1) local-dir  2) webdav  3) s3  4) cf-worker"
    )
    .map_err(|e| e.to_string())?;

    let mut chosen_type: Option<&'static str> = None;
    for _ in 0..5 {
        let Some(raw) = prompt_line(reader, writer, "Remote type", Some(default_type))? else {
            return Err("aborted sync init (end of input)".to_string());
        };
        let candidate = if raw.is_empty() {
            default_type
        } else {
            raw.as_str()
        };
        if let Some(norm) = normalize_remote_type_choice(candidate) {
            chosen_type = Some(norm);
            break;
        }
        writeln!(
            writer,
            "Invalid remote type '{raw}'. Choose local-dir, webdav, s3, or cf-worker."
        )
        .map_err(|e| e.to_string())?;
    }
    let rtype = chosen_type.ok_or_else(|| "invalid remote type selection".to_string())?;

    let remote = match rtype {
        "local-dir" => {
            let existing_path = existing.and_then(|c| match &c.remote {
                RemoteConfig::LocalDir { path } if !path.trim().is_empty() => Some(path.as_str()),
                _ => None,
            });
            let path = match overrides.path.as_deref().filter(|s| !s.is_empty()) {
                Some(p) => p.to_string(),
                None => prompt_required(reader, writer, "Local sync directory path", existing_path)?,
            };
            RemoteConfig::LocalDir { path }
        }
        "webdav" => {
            let (ex_url, ex_user, ex_pass, ex_tok, ex_insecure) = existing
                .and_then(|c| match &c.remote {
                    RemoteConfig::WebDav {
                        url,
                        username,
                        password,
                        token,
                        allow_insecure_http,
                    } => Some((
                        Some(url.as_str()),
                        username.as_deref(),
                        password.as_deref(),
                        token.as_deref(),
                        *allow_insecure_http,
                    )),
                    _ => None,
                })
                .unwrap_or((None, None, None, None, false));

            let url = match overrides.url.as_deref().filter(|s| !s.is_empty()) {
                Some(u) => u.to_string(),
                None => prompt_required(reader, writer, "WebDAV URL", ex_url)?,
            };
            let username = match overrides.username.as_deref() {
                Some(u) => non_empty_clean(Some(u)),
                None => prompt_optional(reader, writer, "Username (optional)", ex_user)?,
            };
            let password = match overrides.password.as_deref() {
                Some(p) => non_empty_clean(Some(p)),
                None => {
                    let hint = ex_pass.map(|_| "[keep existing]");
                    match prompt_line(
                        reader,
                        writer,
                        "Password (optional, or AGY_PULSE_SYNC_PASSWORD)",
                        hint,
                    )? {
                        Some(v) if v.is_empty() => ex_pass.map(ToOwned::to_owned),
                        Some(v) => Some(v),
                        None => ex_pass.map(ToOwned::to_owned),
                    }
                }
            };
            let token = match overrides.token.as_deref() {
                Some(t) => non_empty_clean(Some(t)),
                None => {
                    let hint = ex_tok.map(|_| "[keep existing]");
                    match prompt_line(
                        reader,
                        writer,
                        "Bearer token (optional, or AGY_PULSE_SYNC_TOKEN)",
                        hint,
                    )? {
                        Some(v) if v.is_empty() => ex_tok.map(ToOwned::to_owned),
                        Some(v) => Some(v),
                        None => ex_tok.map(ToOwned::to_owned),
                    }
                }
            };
            let allow_insecure_http = if overrides.allow_insecure_http {
                true
            } else if url_needs_insecure_http_prompt(&url) {
                prompt_yes_no(reader, writer, "Allow insecure http://?", ex_insecure)?
            } else {
                false
            };
            RemoteConfig::WebDav {
                url,
                username,
                password,
                token,
                allow_insecure_http,
            }
        }
        "s3" => {
            let (ex_ep, ex_reg, ex_bkt, ex_ak, ex_sk, ex_ps, ex_insec) = existing
                .and_then(|c| match &c.remote {
                    RemoteConfig::S3 {
                        endpoint,
                        region,
                        bucket,
                        access_key,
                        secret_key,
                        path_style,
                        allow_insecure_http,
                    } => Some((
                        Some(endpoint.as_str()),
                        Some(region.as_str()),
                        Some(bucket.as_str()),
                        access_key.as_deref(),
                        secret_key.as_deref(),
                        *path_style,
                        *allow_insecure_http,
                    )),
                    _ => None,
                })
                .unwrap_or((None, Some("auto"), None, None, None, true, false));

            let endpoint = match overrides.endpoint.as_deref().filter(|s| !s.is_empty()) {
                Some(e) => e.to_string(),
                None => prompt_required(reader, writer, "S3 endpoint URL", ex_ep)?,
            };
            let region = match overrides.region.as_deref().filter(|s| !s.is_empty()) {
                Some(r) => r.to_string(),
                None => prompt_required(
                    reader,
                    writer,
                    "S3 region",
                    ex_reg.or(Some("auto")),
                )?,
            };
            let bucket = match overrides.bucket.as_deref().filter(|s| !s.is_empty()) {
                Some(b) => b.to_string(),
                None => prompt_required(reader, writer, "S3 bucket", ex_bkt)?,
            };
            let access_key = match overrides.access_key.as_deref() {
                Some(ak) => non_empty_clean(Some(ak)),
                None => {
                    let hint = ex_ak.map(|_| "[keep existing]");
                    match prompt_line(
                        reader,
                        writer,
                        "Access key ID (optional, or AGY_PULSE_S3_ACCESS_KEY)",
                        hint,
                    )? {
                        Some(v) if v.is_empty() => ex_ak.map(ToOwned::to_owned),
                        Some(v) => Some(v),
                        None => ex_ak.map(ToOwned::to_owned),
                    }
                }
            };
            let secret_key = match overrides.secret_key.as_deref() {
                Some(sk) => non_empty_clean(Some(sk)),
                None => {
                    let hint = ex_sk.map(|_| "[keep existing]");
                    match prompt_line(
                        reader,
                        writer,
                        "Secret access key (optional, or AGY_PULSE_S3_SECRET_KEY)",
                        hint,
                    )? {
                        Some(v) if v.is_empty() => ex_sk.map(ToOwned::to_owned),
                        Some(v) => Some(v),
                        None => ex_sk.map(ToOwned::to_owned),
                    }
                }
            };
            let path_style = match overrides.path_style {
                Some(ps) => ps,
                None => prompt_yes_no(reader, writer, "Use path-style addressing?", ex_ps)?,
            };
            let allow_insecure_http = if overrides.allow_insecure_http {
                true
            } else if url_needs_insecure_http_prompt(&endpoint) {
                prompt_yes_no(reader, writer, "Allow insecure http://?", ex_insec)?
            } else {
                false
            };
            RemoteConfig::S3 {
                endpoint,
                region,
                bucket,
                access_key,
                secret_key,
                path_style,
                allow_insecure_http,
            }
        }
        "cf-worker" => {
            let (ex_url, ex_tok, ex_insec) = existing
                .and_then(|c| match &c.remote {
                    RemoteConfig::CfWorker {
                        url,
                        token,
                        allow_insecure_http,
                    } => Some((Some(url.as_str()), token.as_deref(), *allow_insecure_http)),
                    _ => None,
                })
                .unwrap_or((None, None, false));

            let url = match overrides.url.as_deref().filter(|s| !s.is_empty()) {
                Some(u) => u.to_string(),
                None => prompt_required(reader, writer, "Cloudflare Worker URL", ex_url)?,
            };
            let token = match overrides.token.as_deref() {
                Some(t) => non_empty_clean(Some(t)),
                None => {
                    let hint = ex_tok.map(|_| "[keep existing]");
                    match prompt_line(
                        reader,
                        writer,
                        "Bearer token (optional, or AGY_PULSE_SYNC_TOKEN)",
                        hint,
                    )? {
                        Some(v) if v.is_empty() => ex_tok.map(ToOwned::to_owned),
                        Some(v) => Some(v),
                        None => ex_tok.map(ToOwned::to_owned),
                    }
                }
            };
            let allow_insecure_http = if overrides.allow_insecure_http {
                true
            } else if url_needs_insecure_http_prompt(&url) {
                prompt_yes_no(reader, writer, "Allow insecure http://?", ex_insec)?
            } else {
                false
            };
            RemoteConfig::CfWorker {
                url,
                token,
                allow_insecure_http,
            }
        }
        _ => return Err(format!("unsupported remote type '{rtype}'")),
    };

    let default_dev_name = existing
        .and_then(|c| c.device_name.clone())
        .or_else(platform::current_hostname);
    let device_name = match overrides.device_name.as_deref() {
        Some(raw) => platform::sanitize_device_name(raw),
        None => {
            let chosen = prompt_optional(
                reader,
                writer,
                "Device name",
                default_dev_name.as_deref(),
            )?;
            chosen.and_then(|s| platform::sanitize_device_name(&s))
        }
    };

    let existing_acct = existing.and_then(|c| c.default_account.as_deref());
    let default_account = match overrides.default_account.as_deref() {
        Some(raw) => validate_optional_account_input(raw)?,
        None => {
            let entered = prompt_optional(
                reader,
                writer,
                "Default account email (optional)",
                existing_acct,
            )?;
            match entered {
                Some(raw) => validate_optional_account_input(&raw)?,
                None => None,
            }
        }
    };

    let default_prefix = existing
        .map(|c| c.prefix.as_str())
        .unwrap_or(DEFAULT_SYNC_PREFIX);
    let prefix = match overrides.prefix.as_deref() {
        Some(p) => p.to_string(),
        None => prompt_optional(reader, writer, "Object key prefix", Some(default_prefix))?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default_prefix.to_string()),
    };

    let default_interval = existing
        .map(|c| c.push_interval_sec)
        .unwrap_or(DEFAULT_PUSH_INTERVAL_SEC);
    let push_interval_sec = match overrides.push_interval_sec {
        Some(sec) => sec.max(1),
        None => {
            let def_str = default_interval.to_string();
            match prompt_optional(
                reader,
                writer,
                "Background push interval (sec)",
                Some(&def_str),
            )? {
                Some(raw) => {
                    let parsed = raw
                        .trim()
                        .parse::<u64>()
                        .map_err(|_| format!("invalid integer '{raw}' for push interval"))?;
                    if parsed == 0 {
                        return Err("push interval must be >= 1".to_string());
                    }
                    parsed
                }
                None => default_interval.max(1),
            }
        }
    };

    let mut cfg = SyncConfig {
        enabled: true,
        device_id: existing.and_then(|c| c.device_id.clone()),
        device_name,
        default_account,
        push_interval_sec,
        prefix,
        remote,
    };
    cfg.normalize();
    Ok(cfg)
}

fn run_sync_exec(args: &[String]) -> i32 {
    let parsed = match parse_sync_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("agy-pulse sync error: {msg}");
            eprintln!("Run 'agy-pulse sync --help' for usage.");
            return 2;
        }
    };
    if parsed.help {
        print_sync_help();
        return 0;
    }

    let cfg = match load_sync_config() {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!(
                "Multi-device sync is not configured. Run 'agy-pulse sync init' to set up a remote."
            );
            return 1;
        }
        Err(err) => {
            eprintln!(
                "agy-pulse sync error: failed to load sync config: {}",
                redact_with_env(&err.to_string(), None)
            );
            return 1;
        }
    };

    if !cfg.enabled {
        eprintln!(
            "Multi-device sync is disabled in {}. Run 'agy-pulse sync init' or set \"enabled\": true.",
            platform::resolve_sync_config_path().display()
        );
        return 1;
    }

    let mut store = match LedgerStore::open_default() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse sync error: failed to open ledger: {err}");
            return 1;
        }
    };

    if !parsed.dry_run && !parsed.no_local_scan && parsed.mode != SyncMode::Pull {
        let (hint_default_account, hint_device_name) = platform::read_sync_identity_hints();
        let sync_opts = LedgerSyncOptions {
            default_account_id: cfg.default_account.clone().or(hint_default_account),
            device_name: cfg.device_name.clone().or(hint_device_name),
            ..Default::default()
        };
        if let Err(err) = store.sync_all(&sync_opts) {
            eprintln!("agy-pulse sync error: local ledger scan failed: {err}");
            return 1;
        }
    }

    let remote = match build_remote(&cfg) {
        Ok(r) => r,
        Err(err) => {
            eprintln!(
                "agy-pulse sync error: {}",
                redact_with_env(&err.to_string(), Some(&cfg))
            );
            return 1;
        }
    };

    let result = match parsed.mode {
        SyncMode::Both => sync_both(&store, remote.as_ref(), &cfg, parsed.dry_run),
        SyncMode::Push => push(&store, remote.as_ref(), &cfg, parsed.dry_run),
        SyncMode::Pull => pull(&store, remote.as_ref(), &cfg, parsed.dry_run),
    };

    match result {
        Ok(outcome) => {
            if outcome.kind == SyncOutcomeKind::AlreadyRunning {
                println!("Sync skipped: another sync process is already running.");
                return 0;
            }
            let mode_str = match parsed.mode {
                SyncMode::Both => "sync",
                SyncMode::Push => "push",
                SyncMode::Pull => "pull",
            };
            let dry_tag = if outcome.dry_run { " [dry-run]" } else { "" };
            println!("Sync ({mode_str}){dry_tag} -> {}", cfg.redacted_target());
            println!("  accounts processed: {}", outcome.accounts_processed);
            println!("  chunks uploaded:    {}", outcome.chunks_uploaded);
            println!("  chunks downloaded:  {}", outcome.chunks_downloaded);
            println!("  chunks skipped:     {}", outcome.chunks_skipped);
            println!("  manifests uploaded: {}", outcome.manifests_uploaded);
            println!("  devices synced:     {}", outcome.devices_synced);
            println!("  steps imported:     {}", outcome.steps_imported);
            println!("  remote cache reset: {}", outcome.remote_cache_cleared);
            println!("  duration:           {}ms", outcome.duration_ms);
            0
        }
        Err(err) => {
            eprintln!(
                "agy-pulse sync error: {}",
                redact_with_env(&err.to_string(), Some(&cfg))
            );
            1
        }
    }
}

fn run_sync_status(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_status_help();
        return 0;
    }
    if let Some(extra) = args.first() {
        eprintln!("agy-pulse sync status error: unknown argument '{extra}'");
        eprintln!("Run 'agy-pulse sync status --help' for usage.");
        return 2;
    }

    let cfg_path = platform::resolve_sync_config_path();
    let cfg_opt = match load_sync_config() {
        Ok(c) => c,
        Err(err) => {
            eprintln!(
                "agy-pulse sync status error: failed to parse {}: {}",
                cfg_path.display(),
                redact_with_env(&err.to_string(), None)
            );
            return 1;
        }
    };

    let store = match LedgerStore::open_default() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse sync status error: failed to open ledger: {err}");
            return 1;
        }
    };

    let local_device_id = match store.local_device_id() {
        Ok(id) => id,
        Err(err) => {
            eprintln!("agy-pulse sync status error: failed to read local device_id: {err}");
            return 1;
        }
    };
    let last_observed_email = store.last_observed_email().ok().flatten();
    let sync_meta = store.get_sync_meta().unwrap_or_default();
    let devices = store.list_sync_devices().unwrap_or_default();

    let effective_device_name = cfg_opt
        .as_ref()
        .and_then(|c| c.device_name.as_deref())
        .and_then(platform::sanitize_device_name)
        .or_else(platform::current_hostname)
        .unwrap_or_else(|| local_device_id.clone());

    println!("Multi-Device Sync Status:");
    match cfg_opt.as_ref() {
        Some(cfg) => {
            println!("  Config path:         {} (configured)", cfg_path.display());
            println!("  Enabled:             {}", cfg.enabled);
            println!("  Local device ID:     {local_device_id}");
            println!("  Device name:         {effective_device_name}");
            println!(
                "  Default account:     {}",
                cfg.default_account.as_deref().unwrap_or("<none>")
            );
            println!(
                "  Last observed email: {}",
                last_observed_email.as_deref().unwrap_or("<none>")
            );
            println!("  Remote target:       {}", cfg.redacted_target());
            println!("  Push interval:       {}s", cfg.push_interval_sec);
        }
        None => {
            println!(
                "  Config path:         {} (not configured; run 'agy-pulse sync init')",
                cfg_path.display()
            );
            println!("  Enabled:             false");
            println!("  Local device ID:     {local_device_id}");
            println!("  Device name:         {effective_device_name}");
            println!("  Default account:     <none>");
            println!(
                "  Last observed email: {}",
                last_observed_email.as_deref().unwrap_or("<none>")
            );
            println!("  Remote target:       <none>");
            println!("  Push interval:       {DEFAULT_PUSH_INTERVAL_SEC}s");
        }
    }

    println!(
        "  Last push:           {}",
        format_epoch_ms(sync_meta.last_sync_push_ms)
    );
    println!(
        "  Last pull:           {}",
        format_epoch_ms(sync_meta.last_sync_pull_ms)
    );
    let redacted_error = sync_meta
        .last_sync_error
        .as_deref()
        .map(|e| redact_with_env(e, cfg_opt.as_ref()));
    println!(
        "  Last sync error:     {}",
        redacted_error.as_deref().unwrap_or("<none>")
    );

    println!("Known devices:");
    if devices.is_empty() {
        println!("  (none)");
    } else {
        let mut grouped: BTreeMap<String, Vec<SyncDeviceRecord>> = BTreeMap::new();
        for dev in devices {
            grouped.entry(dev.account_id.clone()).or_default().push(dev);
        }
        for (account_id, list) in grouped {
            println!("  Account: {account_id}");
            for dev in list {
                let local_tag = if dev.is_local || dev.device_id == local_device_id {
                    " [local]"
                } else {
                    ""
                };
                let tier_tag = dev
                    .plan_tier
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(|t| format!(" [plan: {t}]"))
                    .unwrap_or_default();
                println!(
                    "    - {} ({}){}{} last_seen={}",
                    dev.device_name,
                    dev.device_id,
                    local_tag,
                    tier_tag,
                    format_epoch_ms(Some(dev.last_seen_ms))
                );
            }
        }
    }

    0
}

fn run_sync_init(args: &[String]) -> i32 {
    let parsed = match parse_init_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("agy-pulse sync init error: {msg}");
            eprintln!("Run 'agy-pulse sync init --help' for usage.");
            return 2;
        }
    };
    if parsed.help {
        print_init_help();
        return 0;
    }

    let cfg_path = platform::resolve_sync_config_path();
    let existing = read_existing_sync_config_raw(&cfg_path);

    let mut cfg = if parsed.remote_type.is_some() {
        match build_sync_config_from_flags(existing.as_ref(), &parsed) {
            Ok(c) => c,
            Err(msg) => {
                eprintln!("agy-pulse sync init error: {msg}");
                eprintln!("Run 'agy-pulse sync init --help' for usage.");
                return 2;
            }
        }
    } else {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let mut stdout = io::stdout();
        match build_sync_config_interactive(&mut reader, &mut stdout, existing.as_ref(), &parsed) {
            Ok(c) => c,
            Err(msg) => {
                eprintln!("agy-pulse sync init error: {msg}");
                return 2;
            }
        }
    };

    let mut probe_detail: Option<String> = None;
    if !parsed.skip_probe {
        let mut probe_cfg = cfg.clone();
        probe_cfg.normalize();
        probe_cfg.apply_env_overrides();

        let remote = match build_remote(&probe_cfg) {
            Ok(r) => r,
            Err(err) => {
                eprintln!(
                    "agy-pulse sync init error: {}",
                    redact_with_env(&err.to_string(), Some(&probe_cfg))
                );
                return 1;
            }
        };

        match remote.probe() {
            Ok(info) => {
                probe_detail = Some(format!("{} ({})", info.backend, info.detail));
            }
            Err(err) => {
                eprintln!(
                    "agy-pulse sync init probe failed: {}",
                    redact_with_env(&err.to_string(), Some(&probe_cfg))
                );
                return 1;
            }
        }
    }

    let store = match LedgerStore::open_default() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse sync init error: failed to open ledger: {err}");
            return 1;
        }
    };
    let local_device_id = match store.local_device_id() {
        Ok(id) => id,
        Err(err) => {
            eprintln!("agy-pulse sync init error: failed to read local device_id: {err}");
            return 1;
        }
    };

    cfg.device_id = Some(local_device_id.clone());
    cfg.enabled = true;

    let saved_path = match save_sync_config(&cfg) {
        Ok(p) => p,
        Err(err) => {
            eprintln!(
                "agy-pulse sync init error: failed to save sync config: {}",
                redact_with_env(&err.to_string(), Some(&cfg))
            );
            return 1;
        }
    };

    let effective_device_name = cfg
        .device_name
        .as_deref()
        .and_then(platform::sanitize_device_name)
        .or_else(platform::current_hostname)
        .unwrap_or_else(|| local_device_id.clone());

    println!("Sync configuration saved to {}", saved_path.display());
    println!("  device_id:   {local_device_id}");
    println!("  device_name: {effective_device_name}");
    println!("  target:      {}", cfg.redacted_target());
    if let Some(detail) = probe_detail {
        println!("  probe:       ok — {detail}");
    } else {
        println!("  probe:       skipped");
    }
    0
}

fn run_sync_prune_device(args: &[String]) -> i32 {
    let parsed = match parse_prune_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("agy-pulse sync prune-device error: {msg}");
            eprintln!("Run 'agy-pulse sync prune-device --help' for usage.");
            return 2;
        }
    };
    if parsed.help {
        print_prune_help();
        return 0;
    }

    let cfg = match load_sync_config() {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!(
                "Multi-device sync is not configured. Run 'agy-pulse sync init' before pruning devices."
            );
            return 1;
        }
        Err(err) => {
            eprintln!(
                "agy-pulse sync prune-device error: {}",
                redact_with_env(&err.to_string(), None)
            );
            return 1;
        }
    };

    let store = match LedgerStore::open_default() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse sync prune-device error: failed to open ledger: {err}");
            return 1;
        }
    };

    if let Ok(local_id) = store.local_device_id()
        && local_id == parsed.device_id
        && !parsed.force
    {
        eprintln!(
            "agy-pulse sync prune-device error: refusing to prune local device '{}' without --force",
            parsed.device_id
        );
        return 2;
    }

    let remote = match build_remote(&cfg) {
        Ok(r) => r,
        Err(err) => {
            eprintln!(
                "agy-pulse sync prune-device error: {}",
                redact_with_env(&err.to_string(), Some(&cfg))
            );
            return 1;
        }
    };

    match prune_device(
        &store,
        remote.as_ref(),
        &cfg,
        &parsed.device_id,
        parsed.force,
        parsed.dry_run,
    ) {
        Ok(outcome) => {
            let dry_tag = if outcome.dry_run { " [dry-run]" } else { "" };
            println!(
                "Pruned device {}{dry_tag} (is_local={})",
                outcome.target_device_id, outcome.is_local
            );
            println!(
                "  remote objects deleted: {}",
                outcome.remote_objects_deleted
            );
            println!("  local rows deleted:     {}", outcome.local_rows_deleted);
            0
        }
        Err(SyncError::InvalidKey(msg)) => {
            eprintln!("agy-pulse sync prune-device error: {msg}");
            2
        }
        Err(SyncError::Config(msg)) if msg.contains("without --force") => {
            eprintln!("agy-pulse sync prune-device error: {msg}");
            2
        }
        Err(err) => {
            eprintln!(
                "agy-pulse sync prune-device error: {}",
                redact_with_env(&err.to_string(), Some(&cfg))
            );
            1
        }
    }
}

/// CLI entrypoint for `agypl sync ...` / `agy-pulse sync ...`.
pub fn run_sync_cli(args: &[String]) -> i32 {
    let sub = args.get(1).map(String::as_str);
    match sub {
        Some("status") => run_sync_status(&args[2..]),
        Some("init") => run_sync_init(&args[2..]),
        Some("prune-device") => run_sync_prune_device(&args[2..]),
        Some("help" | "--help" | "-h") => {
            print_sync_help();
            0
        }
        Some(other) if !other.starts_with('-') => {
            eprintln!("agy-pulse sync error: unknown subcommand '{other}'");
            eprintln!("Run 'agy-pulse sync --help' for usage.");
            2
        }
        _ => run_sync_exec(&args[1..]),
    }
}

/// Detached background push entrypoint (`agy-pulse --sync-bg`).
/// Enforces a 45-second hard timeout watchdog and always returns exit code `0`.
pub fn run_sync_bg() -> i32 {
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
    let watchdog = thread::Builder::new()
        .name("agy-pulse-sync-bg-watchdog".to_string())
        .spawn(move || {
            if matches!(
                done_rx.recv_timeout(Duration::from_secs(SYNC_BG_HARD_TIMEOUT_SECS)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                std::process::exit(0);
            }
        });

    let _ = panic::catch_unwind(|| {
        let cfg = match load_sync_config() {
            Ok(Some(c)) if c.enabled => c,
            Ok(_) => return,
            Err(err) => {
                platform::write_error_log_safely(&format!(
                    "Background sync config load error: {}",
                    redact_with_env(&err.to_string(), None)
                ));
                return;
            }
        };

        let remote = match build_remote(&cfg) {
            Ok(r) => r,
            Err(err) => {
                platform::write_error_log_safely(&format!(
                    "Background sync remote init error: {}",
                    redact_with_env(&err.to_string(), Some(&cfg))
                ));
                return;
            }
        };

        let store = match LedgerStore::open_default() {
            Ok(s) => s,
            Err(err) => {
                platform::write_error_log_safely(&format!(
                    "Background sync ledger open error: {err}"
                ));
                return;
            }
        };

        if let Err(err) = push(&store, remote.as_ref(), &cfg, false) {
            platform::write_error_log_safely(&format!(
                "Background sync push error: {}",
                redact_with_env(&err.to_string(), Some(&cfg))
            ));
        }
    });

    let _ = done_tx.send(());
    drop(done_tx);
    if let Ok(handle) = watchdog {
        let _ = handle.join();
    }
    0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parse_sync_args_rejects_conflicting_push_and_pull() {
        let args = vec!["--push".to_string(), "--pull".to_string()];
        let err = parse_sync_args(&args).unwrap_err();
        assert!(err.contains("cannot combine"));
    }

    #[test]
    fn parse_sync_args_accepts_modes_and_flags() {
        let parsed = parse_sync_args(&[
            "--push".to_string(),
            "--dry-run".to_string(),
            "--no-local-scan".to_string(),
        ])
        .unwrap();
        assert_eq!(parsed.mode, SyncMode::Push);
        assert!(parsed.dry_run);
        assert!(parsed.no_local_scan);
    }

    #[test]
    fn parse_init_args_and_build_s3_config_from_flags() {
        let args = vec![
            "--type=s3".to_string(),
            "--endpoint".to_string(),
            "https://r2.example.com".to_string(),
            "--bucket".to_string(),
            "my-bucket".to_string(),
            "--region".to_string(),
            "auto".to_string(),
            "--path-style=false".to_string(),
            "--device-name".to_string(),
            "macbook-pro".to_string(),
            "--default-account".to_string(),
            "Alice@Example.COM".to_string(),
            "--push-interval".to_string(),
            "120".to_string(),
            "--skip-probe".to_string(),
        ];
        let parsed = parse_init_args(&args).unwrap();
        let cfg = build_sync_config_from_flags(None, &parsed).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.device_name.as_deref(), Some("macbook-pro"));
        assert_eq!(cfg.default_account.as_deref(), Some("alice@example.com"));
        assert_eq!(cfg.push_interval_sec, 120);
        assert_eq!(
            cfg.remote,
            RemoteConfig::S3 {
                endpoint: "https://r2.example.com".to_string(),
                region: "auto".to_string(),
                bucket: "my-bucket".to_string(),
                access_key: None,
                secret_key: None,
                path_style: false,
                allow_insecure_http: false,
            }
        );
    }

    #[test]
    fn interactive_wizard_supports_local_dir_and_defaults() {
        let input = "local-dir\n/tmp/my-sync\nworkstation-01\nalice@example.com\ncustom-pfx\n60\n";
        let mut reader = Cursor::new(input.as_bytes());
        let mut out = Vec::new();
        let cfg = build_sync_config_interactive(
            &mut reader,
            &mut out,
            None,
            &InitCliArgs::default(),
        )
        .unwrap();
        assert_eq!(cfg.device_name.as_deref(), Some("workstation-01"));
        assert_eq!(cfg.default_account.as_deref(), Some("alice@example.com"));
        assert_eq!(cfg.prefix, "custom-pfx");
        assert_eq!(cfg.push_interval_sec, 60);
        assert_eq!(
            cfg.remote,
            RemoteConfig::LocalDir {
                path: "/tmp/my-sync".to_string()
            }
        );
    }

    #[test]
    fn redact_with_env_scrubs_config_secrets() {
        let cfg = SyncConfig {
            remote: RemoteConfig::CfWorker {
                url: "https://w.example.com".to_string(),
                token: Some("super-secret-token-xyz".to_string()),
                allow_insecure_http: false,
            },
            ..Default::default()
        };
        let redacted = redact_with_env(
            "auth failed with Bearer super-secret-token-xyz on endpoint",
            Some(&cfg),
        );
        assert!(!redacted.contains("super-secret-token-xyz"));
        assert!(redacted.contains("[REDACTED]"));
    }
}
