use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use url::Url;
use zip::ZipArchive;

use crate::platform;

use super::{OpsError, VERSION};

const DEFAULT_REPO_SLUG: &str = "hardworkDAD/agy-pulse";
const UPDATE_STATUS_FILENAME: &str = "agy-pulse-update-status.json";
const UPDATE_CHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REDIRECTS: usize = 5;
const MAX_DOWNLOAD_BYTES: u64 = 128 * 1024 * 1024;

/// Local persisted state for 24-hour background update checks.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusRecord {
    #[serde(default)]
    pub last_check: i64,
    #[serde(default)]
    pub update_available: bool,
    #[serde(default)]
    pub latest_version: String,
    #[serde(default)]
    pub local_version: String,
}

/// Summary of a manual `update` run.
#[derive(Debug, Clone)]
pub(super) struct SelfUpdateReport {
    pub updated: bool,
    pub previous_version: String,
    pub target_version: String,
    pub asset_name: &'static str,
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn normalize_version_str(raw: &str) -> String {
    raw.trim().trim_start_matches(['v', 'V']).to_string()
}

fn parse_semver(raw: &str) -> Result<Version, OpsError> {
    let cleaned = normalize_version_str(raw);
    Version::parse(&cleaned).map_err(|err| OpsError::InvalidVersion(format!("{raw}: {err}")))
}

fn is_newer_version(remote: &str, local: &str) -> bool {
    match (parse_semver(remote), parse_semver(local)) {
        (Ok(r), Ok(l)) => r > l,
        _ => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    trimmed
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Validate that a release or metadata URL uses `https://` (or `http://` on loopback only).
pub fn validate_release_url(raw: &str) -> Result<Url, OpsError> {
    let parsed = Url::parse(raw.trim())
        .map_err(|err| OpsError::InvalidUrl(format!("{raw}: {err}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| OpsError::InvalidUrl(format!("URL missing host: {raw}")))?;

    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if is_loopback_host(host) => Ok(parsed),
        "http" => Err(OpsError::InsecureProtocol(format!(
            "Non-loopback HTTP URL forbidden (must use https://): {raw}"
        ))),
        other => Err(OpsError::InsecureProtocol(format!(
            "Unsupported URL scheme '{other}': {raw}"
        ))),
    }
}

/// Validate a redirect target URL and forbid any `https:// -> http://` downgrade.
pub fn validate_redirect_url(from: &Url, location: &str) -> Result<Url, OpsError> {
    let next = from
        .join(location.trim())
        .map_err(|err| OpsError::InvalidUrl(format!("{location}: {err}")))?;

    if from.scheme() == "https" && next.scheme() != "https" {
        return Err(OpsError::InsecureProtocol(format!(
            "Insecure redirect from https to {} forbidden: {} -> {}",
            next.scheme(),
            from,
            next
        )));
    }
    validate_release_url(next.as_str())
}

fn resolve_repo_slug() -> String {
    if let Some(raw) = platform::env_var("AGY_PULSE_REPO") {
        let trimmed = raw.trim().trim_start_matches("git+").trim_end_matches(".git");
        if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
            let slug = rest.trim_matches('/');
            if !slug.is_empty() {
                return slug.to_string();
            }
        }
        if trimmed.contains('/') && !trimmed.contains("://") {
            return trimmed.to_string();
        }
    }
    DEFAULT_REPO_SLUG.to_string()
}

fn env_non_empty(key: &str) -> Option<String> {
    platform::env_var(key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn resolve_update_metadata_url() -> String {
    if let Some(explicit) = env_non_empty("AGY_PULSE_UPDATE_URL") {
        return explicit;
    }
    let repo = resolve_repo_slug();
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

fn resolve_release_base_url(target_version: Option<&str>) -> String {
    if let Some(explicit) = env_non_empty("AGY_PULSE_RELEASE_BASE_URL") {
        return explicit.trim_end_matches('/').to_string();
    }
    let repo = resolve_repo_slug();
    if let Some(ver) = target_version {
        let cleaned = normalize_version_str(ver);
        format!("https://github.com/{repo}/releases/download/v{cleaned}")
    } else {
        format!("https://github.com/{repo}/releases/latest/download")
    }
}

fn fetch_bytes_with_redirect_guard(raw_url: &str) -> Result<Vec<u8>, OpsError> {
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
        .user_agent(format!("agy-pulse/{VERSION}"))
        .build();
    let agent: ureq::Agent = config.into();

    let mut current = validate_release_url(raw_url)?;
    for _hop in 0..=MAX_REDIRECTS {
        let response = agent
            .get(current.as_str())
            .call()
            .map_err(|err| OpsError::Http(format!("{current}: {err}")))?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    OpsError::Http(format!("Redirect {status} missing Location header at {current}"))
                })?;
            current = validate_redirect_url(&current, location)?;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(OpsError::Http(format!("HTTP {status} from {current}")));
        }
        let bytes = response
            .into_body()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(|err| OpsError::Http(format!("Failed reading body from {current}: {err}")))?;
        return Ok(bytes);
    }

    Err(OpsError::Http(format!(
        "Too many redirects fetching {raw_url}"
    )))
}

fn parse_remote_version(body: &[u8]) -> Result<String, OpsError> {
    let val: Value = serde_json::from_slice(body)?;
    let raw = val
        .get("version")
        .and_then(Value::as_str)
        .or_else(|| val.get("tag_name").and_then(Value::as_str))
        .ok_or_else(|| {
            OpsError::InvalidVersion("Response JSON missing 'version' or 'tag_name'".to_string())
        })?;
    let parsed = parse_semver(raw)?;
    Ok(parsed.to_string())
}

fn read_status_record() -> Option<UpdateStatusRecord> {
    let path = platform::resolve_antigravity_path(UPDATE_STATUS_FILENAME);
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_status_record(record: &UpdateStatusRecord) {
    let path = platform::resolve_antigravity_path(UPDATE_STATUS_FILENAME);
    if let Ok(bytes) = serde_json::to_vec_pretty(record) {
        let _ = platform::write_cache_atomic_0600(&path, &bytes);
    }
}

/// Fast local read for the statusline hot path; returns normalized version (without leading `v`) when newer.
pub fn read_available_update_version() -> Option<String> {
    let rec = read_status_record()?;
    if !rec.update_available || rec.latest_version.trim().is_empty() {
        return None;
    }
    if !is_newer_version(&rec.latest_version, VERSION) {
        return None;
    }
    Some(normalize_version_str(&rec.latest_version))
}

/// Trigger a detached `--check-update-bg` child if 24h have elapsed and checks are not disabled.
pub fn maybe_spawn_background_update_check() {
    if platform::env_var("AGY_PULSE_DISABLE_UPDATE_CHECK").as_deref() == Some("1") {
        return;
    }
    let now = now_epoch_ms();
    let existing = read_status_record().unwrap_or_default();
    if existing.last_check > 0 && now.saturating_sub(existing.last_check) < UPDATE_CHECK_INTERVAL_MS
    {
        return;
    }

    // Persist cooldown timestamp immediately so concurrent statusline calls do not stampede.
    let mut updated = existing;
    updated.last_check = now;
    if updated.local_version.is_empty() {
        updated.local_version = VERSION.to_string();
    }
    write_status_record(&updated);

    if let Ok(exe) = env::current_exe() {
        let _ = platform::spawn_detached(&exe, &["--check-update-bg"]);
    }
}

pub(super) fn clear_stale_update_status(installed_version: &str) {
    let Some(mut rec) = read_status_record() else {
        return;
    };
    if rec.latest_version.is_empty() || !is_newer_version(&rec.latest_version, installed_version) {
        rec.local_version = normalize_version_str(installed_version);
        rec.update_available = false;
        write_status_record(&rec);
    }
}

pub(super) fn check_for_updates_and_persist() -> Result<UpdateStatusRecord, OpsError> {
    let now = now_epoch_ms();
    let url = resolve_update_metadata_url();
    match fetch_bytes_with_redirect_guard(&url).and_then(|b| parse_remote_version(&b)) {
        Ok(remote_ver) => {
            let update_available = is_newer_version(&remote_ver, VERSION);
            let rec = UpdateStatusRecord {
                last_check: now,
                update_available,
                latest_version: remote_ver,
                local_version: VERSION.to_string(),
            };
            write_status_record(&rec);
            Ok(rec)
        }
        Err(err) => {
            let mut rec = read_status_record().unwrap_or_default();
            rec.last_check = now;
            rec.local_version = VERSION.to_string();
            if rec.update_available
                && (rec.latest_version.is_empty() || !is_newer_version(&rec.latest_version, VERSION))
            {
                rec.update_available = false;
            }
            write_status_record(&rec);
            Err(err)
        }
    }
}

fn parse_sha256sums(manifest: &str, asset_name: &str) -> Option<String> {
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(hash_part) = parts.next() else {
            continue;
        };
        let Some(file_part) = parts.next() else {
            continue;
        };
        if hash_part.len() != 64 || !hash_part.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let clean_name = file_part
            .trim_start_matches('*')
            .trim_start_matches("./");
        let basename = clean_name.rsplit(['/', '\\']).next().unwrap_or(clean_name);
        if clean_name == asset_name || basename == asset_name {
            return Some(hash_part.to_ascii_lowercase());
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn parse_octal_field(field: &[u8]) -> Option<u64> {
    let trimmed = std::str::from_utf8(field)
        .ok()?
        .trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
    if trimmed.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(trimmed, 8).ok()
}

fn trim_cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn matches_target_binary_name(entry_path: &str, expected_names: &[&str]) -> bool {
    let normalized = entry_path.trim_start_matches("./").replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or(&normalized);
    expected_names.contains(&base)
}

fn extract_binary_from_tar_gz(
    archive_bytes: &[u8],
    expected_names: &[&str],
) -> Result<Vec<u8>, OpsError> {
    let mut gz = GzDecoder::new(Cursor::new(archive_bytes));
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;

    loop {
        match gz.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => {
                return Err(OpsError::Archive(format!("Failed reading tar header: {err}")));
            }
        }
        if header.iter().all(|&b| b == 0) {
            break;
        }

        let size = parse_octal_field(&header[124..136])
            .ok_or_else(|| OpsError::Archive("Invalid tar entry size field".to_string()))?;
        let typeflag = header[156];
        let name_part = trim_cstr(&header[0..100]);
        let prefix_part = trim_cstr(&header[345..500]);
        let entry_name = if let Some(overridden) = long_name.take() {
            overridden
        } else if prefix_part.is_empty() {
            name_part
        } else {
            format!("{prefix_part}/{name_part}")
        };

        let size_usize = usize::try_from(size)
            .map_err(|_| OpsError::Archive("Tar entry size exceeds platform usize".to_string()))?;
        let mut payload = vec![0u8; size_usize];
        gz.read_exact(&mut payload)
            .map_err(|err| OpsError::Archive(format!("Truncated tar entry {entry_name}: {err}")))?;

        let rem = size_usize % 512;
        if rem != 0 {
            let mut pad = vec![0u8; 512 - rem];
            gz.read_exact(&mut pad)
                .map_err(|err| OpsError::Archive(format!("Truncated tar padding: {err}")))?;
        }

        if typeflag == b'L' {
            long_name = Some(trim_cstr(&payload));
            continue;
        }

        if (typeflag == b'0' || typeflag == 0) && matches_target_binary_name(&entry_name, expected_names) {
            if payload.is_empty() {
                return Err(OpsError::Archive(format!(
                    "Extracted binary {entry_name} is empty"
                )));
            }
            return Ok(payload);
        }
    }

    Err(OpsError::Archive(format!(
        "Target binary ({}) not found inside .tar.gz archive",
        expected_names.join(", ")
    )))
}

fn extract_binary_from_zip(
    archive_bytes: &[u8],
    expected_names: &[&str],
) -> Result<Vec<u8>, OpsError> {
    let mut zip = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|err| OpsError::Archive(format!("Invalid .zip archive: {err}")))?;
    for idx in 0..zip.len() {
        let mut file = zip
            .by_index(idx)
            .map_err(|err| OpsError::Archive(format!("Zip entry read error: {err}")))?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        if matches_target_binary_name(&name, expected_names) {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)
                .map_err(|err| OpsError::Archive(format!("Failed reading {name} from zip: {err}")))?;
            if buf.is_empty() {
                return Err(OpsError::Archive(format!("Extracted binary {name} is empty")));
            }
            return Ok(buf);
        }
    }
    Err(OpsError::Archive(format!(
        "Target binary ({}) not found inside .zip archive",
        expected_names.join(", ")
    )))
}

fn extract_release_binary(
    asset_name: &str,
    archive_bytes: &[u8],
    expected_names: &[&str],
) -> Result<Vec<u8>, OpsError> {
    if asset_name.ends_with(".zip") {
        extract_binary_from_zip(archive_bytes, expected_names)
    } else if asset_name.ends_with(".tar.gz") {
        extract_binary_from_tar_gz(archive_bytes, expected_names)
    } else {
        Err(OpsError::Archive(format!(
            "Unsupported archive format: {asset_name}"
        )))
    }
}

fn write_temp_and_replace(bin_bytes: &[u8], target_path: &Path) -> Result<(), OpsError> {
    let mut tmp = NamedTempFile::new()?;
    use std::io::Write;
    tmp.write_all(bin_bytes)?;
    tmp.flush()?;
    platform::replace_executable_atomically(tmp.path(), target_path)?;
    Ok(())
}

/// Check latest release, download archive + `SHA256SUMS`, verify SHA-256, and atomically self-replace.
pub(super) fn perform_self_update(force: bool) -> Result<SelfUpdateReport, OpsError> {
    let asset_name = platform::current_release_asset_name().ok_or(OpsError::UnsupportedTarget)?;

    let explicit_update_url = env_non_empty("AGY_PULSE_UPDATE_URL");
    let explicit_release_base = env_non_empty("AGY_PULSE_RELEASE_BASE_URL");

    let target_version = if explicit_update_url.is_some() || explicit_release_base.is_none() {
        let metadata_url = resolve_update_metadata_url();
        let body = fetch_bytes_with_redirect_guard(&metadata_url)?;
        let remote_ver = parse_remote_version(&body)?;
        if !force && !is_newer_version(&remote_ver, VERSION) {
            return Ok(SelfUpdateReport {
                updated: false,
                previous_version: VERSION.to_string(),
                target_version: remote_ver,
                asset_name,
            });
        }
        Some(remote_ver)
    } else {
        None
    };

    let base_url = resolve_release_base_url(target_version.as_deref());
    let sums_url = format!("{base_url}/SHA256SUMS");
    let archive_url = format!("{base_url}/{asset_name}");

    let sums_bytes = fetch_bytes_with_redirect_guard(&sums_url)?;
    let sums_text = String::from_utf8_lossy(&sums_bytes);
    let expected_sha256 = parse_sha256sums(&sums_text, asset_name).ok_or_else(|| {
        OpsError::ChecksumMismatch(format!(
            "SHA256SUMS does not contain a valid SHA-256 entry for {asset_name}"
        ))
    })?;

    let archive_bytes = fetch_bytes_with_redirect_guard(&archive_url)?;
    let actual_sha256 = sha256_hex(&archive_bytes);
    if actual_sha256 != expected_sha256 {
        return Err(OpsError::ChecksumMismatch(format!(
            "SHA-256 mismatch for {asset_name}: expected {expected_sha256}, got {actual_sha256}"
        )));
    }

    let primary_name = platform::primary_binary_name();
    let alias_name = platform::alias_binary_name();
    let primary_bytes = extract_release_binary(asset_name, &archive_bytes, &[primary_name, alias_name])?;

    let current_exe = env::current_exe()?;
    write_temp_and_replace(&primary_bytes, &current_exe)?;

    // Also update companion binary (`agypl` / `agy-pulse`) if co-located in the same directory.
    for sibling_name in [primary_name, alias_name] {
        let sibling = current_exe.with_file_name(sibling_name);
        if sibling != current_exe && sibling.exists() {
            let sibling_bytes = extract_release_binary(asset_name, &archive_bytes, &[sibling_name])
                .unwrap_or_else(|_| primary_bytes.clone());
            write_temp_and_replace(&sibling_bytes, &sibling)?;
        }
    }

    let resolved_target = target_version.unwrap_or_else(|| VERSION.to_string());
    write_status_record(&UpdateStatusRecord {
        last_check: now_epoch_ms(),
        update_available: false,
        latest_version: resolved_target.clone(),
        local_version: resolved_target.clone(),
    });

    Ok(SelfUpdateReport {
        updated: true,
        previous_version: VERSION.to_string(),
        target_version: resolved_target,
        asset_name,
    })
}
