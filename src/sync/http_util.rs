use std::net::IpAddr;
use std::time::Duration;

use sha2::{Digest, Sha256};
use url::Url;

use super::SyncError;
use super::config::sanitize_url_for_display;

pub const HTTP_TIMEOUT_SECS: u64 = 15;
pub const MAX_REMOTE_OBJECT_BYTES: usize = 32 * 1024 * 1024;

/// Return `true` when `host` is a loopback hostname or IP (`localhost`, `127.0.0.1`, `::1`, `[::1]`).
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    trimmed.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Validate a remote endpoint URL against transport security rules:
/// - `https://` is always allowed;
/// - `http://` is allowed only for loopback hosts or when `allow_insecure_http == true`;
/// - other schemes, missing hosts, or illegal whitespace/control characters are rejected;
/// - all error messages contain only the credential-stripped URL.
pub fn validate_remote_url(raw: &str, allow_insecure_http: bool) -> Result<Url, SyncError> {
    if raw.chars().any(|c| c.is_control()) {
        return Err(SyncError::Config(format!(
            "remote URL contains control characters: {}",
            sanitize_url_for_display(raw)
        )));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SyncError::Config(
            "remote URL must not be empty".to_string(),
        ));
    }
    if trimmed.chars().any(|c| c.is_whitespace()) {
        return Err(SyncError::Config(format!(
            "remote URL contains whitespace: {}",
            sanitize_url_for_display(trimmed)
        )));
    }

    let safe_display = sanitize_url_for_display(trimmed);
    let lower = trimmed.to_ascii_lowercase();
    let after_scheme = lower
        .strip_prefix("https://")
        .map(|rest| ("https", rest))
        .or_else(|| lower.strip_prefix("http://").map(|rest| ("http", rest)));
    let Some((scheme_lit, rest)) = after_scheme else {
        let scheme = trimmed.split_once(':').map(|(s, _)| s).unwrap_or("unknown");
        return Err(SyncError::Config(format!(
            "unsupported remote URL scheme '{scheme}': {safe_display}"
        )));
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err(SyncError::Config(format!(
            "remote URL missing host: {safe_display}"
        )));
    }
    let parsed = Url::parse(trimmed)
        .map_err(|err| SyncError::Config(format!("invalid remote URL '{safe_display}': {err}")))?;

    let host = parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| SyncError::Config(format!("remote URL missing host: {safe_display}")))?;

    match scheme_lit {
        "https" => Ok(parsed),
        "http" if allow_insecure_http || is_loopback_host(host) => Ok(parsed),
        "http" => Err(SyncError::Config(format!(
            "non-loopback http:// remote URL is forbidden unless allowInsecureHttp is enabled: {safe_display}"
        ))),
        _ => Err(SyncError::Config(format!(
            "unsupported remote URL scheme '{scheme_lit}': {safe_display}"
        ))),
    }
}

/// Strip userinfo, query, and fragment from a validated `Url` before issuing HTTP requests.
pub(crate) fn strip_url_credentials(mut url: Url) -> Url {
    if !url.username().is_empty() || url.password().is_some() {
        let _ = url.set_username("");
        let _ = url.set_password(None);
    }
    url.set_query(None);
    url.set_fragment(None);
    url
}

/// Build the shared `ureq::Agent` with 15s global timeout, zero redirects, and non-throwing HTTP statuses.
pub fn build_http_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(HTTP_TIMEOUT_SECS)))
        .max_redirects(0)
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .user_agent(concat!("agy-pulse/", env!("CARGO_PKG_VERSION")))
        .build();
    config.into()
}

/// Read an HTTP response body up to `max_bytes`, returning `SyncError::Remote` if exceeded.
pub fn read_body_capped(body: ureq::Body, max_bytes: usize) -> Result<Vec<u8>, SyncError> {
    body.into_with_config()
        .limit(max_bytes as u64)
        .read_to_vec()
        .map_err(|err| {
            SyncError::Remote(format!(
                "failed reading remote response body (limit {max_bytes} bytes): {err}"
            ))
        })
}

/// Encode raw bytes as standard RFC 4648 Base64 with `=` padding.
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i];
        let b1 = if i + 1 < input.len() { input[i + 1] } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] } else { 0 };

        let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if i + 1 < input.len() {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

/// Compute pure-Rust HMAC-SHA256 using `sha2::Sha256` (RFC 2104).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let digest = Sha256::digest(key);
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for idx in 0..BLOCK_SIZE {
        ipad[idx] ^= key_block[idx];
        opad[idx] ^= key_block[idx];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let outer_hash = outer.finalize();

    let mut out = [0u8; 32];
    out.copy_from_slice(&outer_hash);
    out
}

/// Percent-encode a single URI path or query component per RFC 3986 unreserved (`[A-Za-z0-9._~-]`)
/// with uppercase hex `%XX`.
pub fn uri_encode_segment(seg: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(seg.len());
    for &b in seg.as_bytes() {
        if matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Percent-encode each `/` separated segment of a relative object key.
pub(crate) fn uri_encode_path(key: &str) -> String {
    key.split('/')
        .map(uri_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode a URL path string into UTF-8.
pub(crate) fn percent_decode_str(s: &str) -> Result<String, SyncError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out)
        .map_err(|err| SyncError::Remote(format!("remote URL path is not valid UTF-8: {err}")))
}

/// Unescape standard XML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`) and numeric references (`&#...;` / `&#x...;`).
pub fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp_idx) = rest.find('&') {
        out.push_str(&rest[..amp_idx]);
        let after_amp = &rest[amp_idx + 1..];
        if let Some(semi_idx) = after_amp.find(';') {
            let entity = &after_amp[..semi_idx];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    u32::from_str_radix(&entity[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if let Some(dec) = entity.strip_prefix('#') => {
                    dec.parse::<u32>().ok().and_then(char::from_u32)
                }
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after_amp[semi_idx + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after_amp;
    }
    out.push_str(rest);
    out
}

/// Scrub any known secret strings and URL userinfo from a diagnostic error string.
pub(crate) fn redact_error_message(msg: &str, secrets: &[String]) -> String {
    let mut out = msg.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out
}

/// Format a `Host` header value from a `Url`, omitting default ports (`https:443`, `http:80`).
pub(crate) fn format_host_header(url: &Url) -> Result<String, SyncError> {
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| SyncError::Config("URL missing host for Host header".to_string()))?;
    match (url.scheme(), url.port()) {
        ("https", Some(443)) | ("http", Some(80)) | (_, None) => Ok(host.to_string()),
        (_, Some(port)) => Ok(format!("{host}:{port}")),
    }
}
