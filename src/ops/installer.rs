use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::platform;

use super::{OpsError, VERSION, updater};

const STALE_PLUGIN_FILES: &[&str] = &["hooks.json", "mcp_config.json"];
const STALE_PLUGIN_DIRS: &[&str] = &[
    "agents",
    "commands",
    "rules",
    "extensions",
    "skills/hud-config",
];

/// Summary of actions completed by `install`.
#[derive(Debug, Clone)]
pub(super) struct InstallReport {
    pub settings_path: PathBuf,
    pub command: String,
    pub bin_dir: PathBuf,
    pub deployed_bins: Vec<PathBuf>,
    pub cleaned_stale_plugin_paths: Vec<PathBuf>,
}

fn format_statusline_command(binary_path: &Path) -> String {
    let raw = binary_path.to_string_lossy();
    if raw.chars().any(char::is_whitespace) {
        format!("\"{raw}\"")
    } else {
        raw.into_owned()
    }
}

fn collect_stale_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = platform::home_dir() {
        dirs.push(
            home.join(".gemini")
                .join("config")
                .join("plugins")
                .join("agy-pulse"),
        );
    }
    for root in platform::get_antigravity_roots() {
        dirs.push(root.join("plugins").join("agy-pulse"));
    }
    if let Ok(xdg_cfg) = env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg_cfg.trim();
        if !trimmed.is_empty() {
            dirs.push(
                PathBuf::from(trimmed)
                    .join("gemini")
                    .join("plugins")
                    .join("agy-pulse"),
            );
        }
    }
    if let Ok(appdata) = env::var("APPDATA") {
        let trimmed = appdata.trim();
        if !trimmed.is_empty() {
            dirs.push(
                PathBuf::from(trimmed)
                    .join("gemini")
                    .join("plugins")
                    .join("agy-pulse"),
            );
        }
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let key = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.insert(key) {
            out.push(dir);
        }
    }
    out
}

fn clean_stale_plugin_artifacts() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for plugin_dir in collect_stale_plugin_dirs() {
        if !plugin_dir.exists() {
            continue;
        }
        for file_name in STALE_PLUGIN_FILES {
            let p = plugin_dir.join(file_name);
            if p.exists() && fs::remove_file(&p).is_ok() {
                removed.push(p);
            }
        }
        for rel_dir in STALE_PLUGIN_DIRS {
            let mut p = plugin_dir.clone();
            for part in rel_dir.split('/') {
                p.push(part);
            }
            if p.exists() && fs::remove_dir_all(&p).is_ok() {
                removed.push(p);
            }
        }
    }
    removed
}

fn deploy_user_binaries(bin_dir: &Path) -> Result<(PathBuf, Vec<PathBuf>), OpsError> {
    fs::create_dir_all(bin_dir)?;
    let current_exe = env::current_exe()?;
    let primary_name = platform::primary_binary_name();
    let alias_name = platform::alias_binary_name();

    let mut deployed = Vec::new();
    let mut primary_dest = bin_dir.join(primary_name);

    for target_name in [primary_name, alias_name] {
        let dest = bin_dir.join(target_name);
        if target_name == primary_name {
            primary_dest = dest.clone();
        }
        let sibling_source = current_exe.with_file_name(target_name);
        let source = if sibling_source.is_file() {
            sibling_source
        } else {
            current_exe.clone()
        };

        let same_file = fs::canonicalize(&source)
            .ok()
            .zip(fs::canonicalize(&dest).ok())
            .is_some_and(|(a, b)| a == b);

        if !same_file {
            platform::replace_executable_atomically(&source, &dest)?;
        }
        platform::set_executable_0755(&dest)?;
        deployed.push(dest);
    }

    Ok((primary_dest, deployed))
}

fn strip_jsonc_comments(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.strip_prefix('\u{FEFF}').unwrap_or(input);
    let bytes = trimmed.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if b == b'"' {
            in_string = true;
            out.push(b);
            i += 1;
            continue;
        }

        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                let mut closed = false;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return None;
                }
                continue;
            }
        }

        out.push(b);
        i += 1;
    }

    Some(out)
}

fn strip_trailing_commas(bytes: &[u8]) -> Option<String> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut prev_non_ws: Option<u8> = None;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }

        if b == b'"' {
            in_string = true;
            out.push(b);
            prev_non_ws = Some(b'"');
            continue;
        }

        if b == b',' {
            let next_non_ws = bytes[i + 1..]
                .iter()
                .copied()
                .find(|c| !c.is_ascii_whitespace());
            let is_valid_trailing = matches!(next_non_ws, Some(b'}') | Some(b']'))
                && !matches!(prev_non_ws, None | Some(b'{') | Some(b'[') | Some(b','));
            if is_valid_trailing {
                continue;
            }
        }

        out.push(b);
        if !b.is_ascii_whitespace() {
            prev_non_ws = Some(b);
        }
    }

    String::from_utf8(out).ok()
}

fn parse_jsonc_object(raw: &str) -> Option<Map<String, Value>> {
    let without_comments = strip_jsonc_comments(raw)?;
    let normalized = strip_trailing_commas(&without_comments)?;
    match serde_json::from_str::<Value>(&normalized) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn configure_host_settings(settings_path: &Path, target_command: &str) -> Result<(), OpsError> {
    let (mut settings_map, backup_before_write) = if settings_path.exists() {
        let raw = fs::read_to_string(settings_path)?;
        if raw.trim().is_empty() {
            (Map::new(), false)
        } else {
            match serde_json::from_str::<Value>(&raw) {
                Ok(Value::Object(map)) => (map, false),
                Ok(_) => (Map::new(), true),
                Err(_) => match parse_jsonc_object(&raw) {
                    Some(map) => (map, true),
                    None => (Map::new(), true),
                },
            }
        }
    } else {
        (Map::new(), false)
    };

    let mut status_line_map = match settings_map.get("statusLine") {
        Some(Value::Object(existing)) => existing.clone(),
        _ => Map::new(),
    };

    let already_matches = status_line_map.get("type").and_then(Value::as_str) == Some("command")
        && status_line_map.get("command").and_then(Value::as_str) == Some(target_command);

    if !already_matches {
        if backup_before_write && settings_path.exists() {
            let bak_path = PathBuf::from(format!("{}.bak", settings_path.display()));
            let _ = fs::copy(settings_path, &bak_path);
        }
        status_line_map.insert("type".to_string(), Value::String("command".to_string()));
        status_line_map.insert(
            "command".to_string(),
            Value::String(target_command.to_string()),
        );
        settings_map.insert("statusLine".to_string(), Value::Object(status_line_map));

        let mut json = serde_json::to_string_pretty(&Value::Object(settings_map))?;
        json.push('\n');
        platform::write_atomic_0600(settings_path, json.as_bytes())?;
    }

    Ok(())
}

/// Install `agy-pulse` and `agypl` into the isolated user bin dir and register `statusLine` safely.
pub(super) fn install(bin_dir_override: Option<&Path>) -> Result<InstallReport, OpsError> {
    let cleaned_stale_plugin_paths = clean_stale_plugin_artifacts();
    let bin_dir = platform::resolve_cli_bin_dir(bin_dir_override);
    let (primary_bin, deployed_bins) = deploy_user_binaries(&bin_dir)?;

    let settings_path = platform::resolve_antigravity_path("settings.json");
    let command = format_statusline_command(&primary_bin);
    configure_host_settings(&settings_path, &command)?;

    updater::clear_stale_update_status(VERSION);

    Ok(InstallReport {
        settings_path,
        command,
        bin_dir,
        deployed_bins,
        cleaned_stale_plugin_paths,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn jsonc_parser_rejects_unclosed_block_comment_and_double_commas() {
        assert!(parse_jsonc_object(r#"{ "a": 1 /* unclosed }"#).is_none());
        assert!(parse_jsonc_object(r#"{ "a": 1,, }"#).is_none());
        assert!(parse_jsonc_object(r#"{ , }"#).is_none());
        assert!(parse_jsonc_object(r#"[1, 2,]"#).is_none());
    }

    #[test]
    fn configure_host_settings_preserves_jsonc_when_updating_and_skips_rewrite_if_matching() {
        let dir = TempDir::new().expect("temp dir");
        let settings_path = dir.path().join("settings.json");
        let raw_matching = "// comment\n{\n  \"theme\": \"nord\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"/usr/local/bin/agy-pulse\",\n  },\n}\n";
        fs::write(&settings_path, raw_matching).expect("write");

        configure_host_settings(&settings_path, "/usr/local/bin/agy-pulse").expect("matching");
        assert_eq!(
            fs::read_to_string(&settings_path).expect("read"),
            raw_matching
        );
        assert!(!dir.path().join("settings.json.bak").exists());
    }
}
