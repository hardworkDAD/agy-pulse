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
const MAX_EXTRACTED_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TAR_LONG_NAME_BYTES: u64 = 64 * 1024;
const MAX_TAR_TOTAL_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
const TAR_SKIP_SCRATCH_BYTES: usize = 8192;

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
    trimmed.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Validate that a release or metadata URL uses `https://` (or `http://` on loopback only).
pub fn validate_release_url(raw: &str) -> Result<Url, OpsError> {
    let parsed =
        Url::parse(raw.trim()).map_err(|err| OpsError::InvalidUrl(format!("{raw}: {err}")))?;
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
        let trimmed = raw
            .trim()
            .trim_start_matches("git+")
            .trim_end_matches(".git");
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
                    OpsError::Http(format!(
                        "Redirect {status} missing Location header at {current}"
                    ))
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
                && (rec.latest_version.is_empty()
                    || !is_newer_version(&rec.latest_version, VERSION))
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
        let clean_name = file_part.trim_start_matches('*').trim_start_matches("./");
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

fn skip_exact_bytes<R: Read>(
    reader: &mut R,
    mut remaining: u64,
    context: &str,
) -> Result<(), OpsError> {
    let mut scratch = [0u8; TAR_SKIP_SCRATCH_BYTES];
    while remaining > 0 {
        let chunk = remaining.min(scratch.len() as u64) as usize;
        reader
            .read_exact(&mut scratch[..chunk])
            .map_err(|err| OpsError::Archive(format!("Truncated {context}: {err}")))?;
        remaining -= chunk as u64;
    }
    Ok(())
}

fn read_bounded_tar_payload<R: Read>(
    reader: &mut R,
    size: u64,
    context: &str,
) -> Result<Vec<u8>, OpsError> {
    let initial_cap = usize::try_from(size).unwrap_or(usize::MAX).min(1024 * 1024);
    let mut payload = Vec::with_capacity(initial_cap);
    reader
        .by_ref()
        .take(size)
        .read_to_end(&mut payload)
        .map_err(|err| OpsError::Archive(format!("Failed reading tar entry {context}: {err}")))?;
    if (payload.len() as u64) != size {
        return Err(OpsError::Archive(format!(
            "Truncated tar entry {context}: expected {size} bytes, got {}",
            payload.len()
        )));
    }
    Ok(payload)
}

fn extract_binary_from_tar_gz(
    archive_bytes: &[u8],
    expected_names: &[&str],
) -> Result<Vec<u8>, OpsError> {
    let mut gz = GzDecoder::new(Cursor::new(archive_bytes));
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    let mut total_uncompressed: u64 = 0;

    loop {
        match gz.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => {
                return Err(OpsError::Archive(format!(
                    "Failed reading tar header: {err}"
                )));
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

        let padding = (512 - (size % 512)) % 512;
        total_uncompressed = total_uncompressed
            .checked_add(512)
            .and_then(|t| t.checked_add(size))
            .and_then(|t| t.checked_add(padding))
            .filter(|&t| t <= MAX_TAR_TOTAL_UNCOMPRESSED_BYTES)
            .ok_or_else(|| {
                OpsError::Archive(format!(
                    "Tar archive uncompressed size exceeds maximum limit ({MAX_TAR_TOTAL_UNCOMPRESSED_BYTES} bytes)"
                ))
            })?;

        if typeflag == b'L' {
            if size > MAX_TAR_LONG_NAME_BYTES {
                return Err(OpsError::Archive(format!(
                    "Tar GNU longname size {size} exceeds maximum limit ({MAX_TAR_LONG_NAME_BYTES} bytes)"
                )));
            }
            let name_bytes = read_bounded_tar_payload(&mut gz, size, "GNU longname")?;
            skip_exact_bytes(&mut gz, padding, "tar longname padding")?;
            long_name = Some(trim_cstr(&name_bytes));
            continue;
        }

        if (typeflag == b'0' || typeflag == 0)
            && matches_target_binary_name(&entry_name, expected_names)
        {
            if size == 0 {
                return Err(OpsError::Archive(format!(
                    "Extracted binary {entry_name} is empty"
                )));
            }
            if size > MAX_EXTRACTED_BINARY_BYTES {
                return Err(OpsError::Archive(format!(
                    "Tar entry {entry_name} size {size} exceeds maximum extracted binary limit ({MAX_EXTRACTED_BINARY_BYTES} bytes)"
                )));
            }
            return read_bounded_tar_payload(&mut gz, size, &entry_name);
        }

        skip_exact_bytes(&mut gz, size, &format!("tar entry {entry_name}"))?;
        skip_exact_bytes(&mut gz, padding, "tar padding")?;
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
        let file = zip
            .by_index(idx)
            .map_err(|err| OpsError::Archive(format!("Zip entry read error: {err}")))?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        if matches_target_binary_name(&name, expected_names) {
            if file.size() > MAX_EXTRACTED_BINARY_BYTES {
                return Err(OpsError::Archive(format!(
                    "Zip entry {name} declares size {} exceeding maximum extracted binary limit ({MAX_EXTRACTED_BINARY_BYTES} bytes)",
                    file.size()
                )));
            }
            let mut buf = Vec::new();
            file.take(MAX_EXTRACTED_BINARY_BYTES + 1)
                .read_to_end(&mut buf)
                .map_err(|err| {
                    OpsError::Archive(format!("Failed reading {name} from zip: {err}"))
                })?;
            if (buf.len() as u64) > MAX_EXTRACTED_BINARY_BYTES {
                return Err(OpsError::Archive(format!(
                    "Zip entry {name} uncompressed size exceeds maximum limit ({MAX_EXTRACTED_BINARY_BYTES} bytes)"
                )));
            }
            if buf.is_empty() {
                return Err(OpsError::Archive(format!(
                    "Extracted binary {name} is empty"
                )));
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
    let primary_bytes =
        extract_release_binary(asset_name, &archive_bytes, &[primary_name, alias_name])?;

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_tar_header(name: &str, typeflag: u8, size: u64) -> [u8; 512] {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(100);
        header[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        header[100..108].copy_from_slice(b"0000755\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size_str = format!("{size:011o}\0");
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = typeflag;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        let cksum_str = format!("{cksum:06o}\0 ");
        header[148..156].copy_from_slice(cksum_str.as_bytes());
        header
    }

    fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(raw).expect("gzip write");
        enc.finish().expect("gzip finish")
    }

    #[test]
    fn tar_gz_rejects_oversized_longname_entry_before_allocating() {
        let header = make_tar_header("././@LongLink", b'L', MAX_TAR_LONG_NAME_BYTES + 1);
        let mut tar = Vec::from(header);
        tar.resize(tar.len() + 1024, 0);
        let err = extract_binary_from_tar_gz(&gzip_bytes(&tar), &["agy-pulse"])
            .expect_err("oversized GNU longname must fail");
        assert!(
            matches!(err, OpsError::Archive(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tar_gz_rejects_oversized_target_binary_before_allocating() {
        let header = make_tar_header("agy-pulse", b'0', MAX_EXTRACTED_BINARY_BYTES + 1);
        let mut tar = Vec::from(header);
        tar.resize(tar.len() + 1024, 0);
        let err = extract_binary_from_tar_gz(&gzip_bytes(&tar), &["agy-pulse"])
            .expect_err("oversized target binary must fail");
        assert!(
            matches!(err, OpsError::Archive(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tar_gz_rejects_cumulative_non_target_entries_exceeding_total_limit() {
        // Two non-target files of 300 MiB each exceed MAX_TAR_TOTAL_UNCOMPRESSED_BYTES (512 MiB)
        let h1 = make_tar_header("docs/a.bin", b'0', 300 * 1024 * 1024);
        let h2 = make_tar_header("docs/b.bin", b'0', 300 * 1024 * 1024);
        let mut tar = Vec::new();
        tar.extend_from_slice(&h1);
        tar.extend_from_slice(&h2);
        tar.resize(tar.len() + 1024, 0);
        let err = extract_binary_from_tar_gz(&gzip_bytes(&tar), &["agy-pulse"])
            .expect_err("cumulative tar size exceeding limit must fail");
        assert!(
            matches!(err, OpsError::Archive(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn tar_gz_streams_non_target_entries_and_extracts_target_with_longname() {
        let long_path = format!("{}/agy-pulse", "nested/dir".repeat(12));
        let mut long_payload = long_path.as_bytes().to_vec();
        long_payload.push(0);

        let non_target_payload = vec![b'x'; 200_000];
        let target_payload = b"#!/bin/sh\necho ok\n".to_vec();

        let mut tar = Vec::new();
        // 1. Large non-target file
        tar.extend_from_slice(&make_tar_header(
            "share/big-readme.txt",
            b'0',
            non_target_payload.len() as u64,
        ));
        tar.extend_from_slice(&non_target_payload);
        let rem1 = non_target_payload.len() % 512;
        if rem1 != 0 {
            tar.resize(tar.len() + (512 - rem1), 0);
        }
        // 2. GNU longname for target
        tar.extend_from_slice(&make_tar_header(
            "././@LongLink",
            b'L',
            long_payload.len() as u64,
        ));
        tar.extend_from_slice(&long_payload);
        let rem2 = long_payload.len() % 512;
        if rem2 != 0 {
            tar.resize(tar.len() + (512 - rem2), 0);
        }
        // 3. Target binary
        tar.extend_from_slice(&make_tar_header(
            "truncated-name",
            b'0',
            target_payload.len() as u64,
        ));
        tar.extend_from_slice(&target_payload);
        let rem3 = target_payload.len() % 512;
        if rem3 != 0 {
            tar.resize(tar.len() + (512 - rem3), 0);
        }
        tar.resize(tar.len() + 1024, 0);

        let extracted = extract_binary_from_tar_gz(&gzip_bytes(&tar), &["agy-pulse"])
            .expect("should stream-skip non-target and extract longname target");
        assert_eq!(extracted, target_payload);
    }

    #[test]
    fn zip_rejects_zip_bomb_even_when_header_lies_about_uncompressed_size() {
        // Build a zip containing (MAX_EXTRACTED_BINARY_BYTES + 1024) zero bytes,
        // then patch the uncompressed size in local file header and central directory to 16 bytes
        // so `file.size()` lies and only bounded `take()` prevents unbounded decompression.
        let payload = vec![0u8; (MAX_EXTRACTED_BINARY_BYTES as usize) + 1024];
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("agy-pulse", opts).expect("start_file");
            zip.write_all(&payload).expect("write zip bomb payload");
            zip.finish().expect("finish zip");
        }
        let mut zip_bytes = cursor.into_inner();

        // Patch local file header uncompressed size (offset 22..26 for standard local header `PK\x03\x04`)
        assert_eq!(&zip_bytes[0..4], b"PK\x03\x04");
        zip_bytes[22..26].copy_from_slice(&16u32.to_le_bytes());

        // Patch central directory header uncompressed size (`PK\x01\x02` + 24..28)
        if let Some(cd_pos) = zip_bytes.windows(4).position(|w| w == b"PK\x01\x02") {
            zip_bytes[cd_pos + 24..cd_pos + 28].copy_from_slice(&16u32.to_le_bytes());
        }

        let err = extract_binary_from_zip(&zip_bytes, &["agy-pulse"])
            .expect_err("zip bomb with spoofed uncompressed size must be rejected");
        assert!(
            matches!(err, OpsError::Archive(_)),
            "unexpected error: {err}"
        );
    }
}
