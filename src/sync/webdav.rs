use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use url::Url;

use super::config::sanitize_url_for_display;
use super::http_util::{
    MAX_REMOTE_OBJECT_BYTES, base64_encode, build_http_agent, percent_decode_str,
    read_body_capped, redact_error_message, strip_url_credentials, uri_encode_path,
    validate_remote_url, xml_unescape,
};
use super::protocol::{validate_relative_key, validate_relative_prefix};
use super::{RemoteProbeInfo, SyncError, SyncRemote};

const MAX_WEBDAV_DIRS_PER_LIST: usize = 10_000;
const PROPFIND_RESOURCETYPE_BODY: &[u8] =
    b"<?xml version=\"1.0\" encoding=\"utf-8\"?><D:propfind xmlns:D=\"DAV:\"><D:prop><D:resourcetype/></D:prop></D:propfind>";

/// `SyncRemote` adapter backed by a standard WebDAV server (Nextcloud, Nutstore, Alist, Caddy/Nginx).
pub struct WebDavRemote {
    base_url: Url,
    base_path: String,
    display_url: String,
    auth_header: Option<String>,
    agent: ureq::Agent,
    created_dirs: Mutex<HashSet<String>>,
    secrets: Vec<String>,
}

impl std::fmt::Debug for WebDavRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavRemote")
            .field("url", &self.display_url)
            .field("has_auth", &self.auth_header.is_some())
            .finish()
    }
}

impl WebDavRemote {
    pub fn new(
        url: &str,
        username: Option<&str>,
        password: Option<&str>,
        token: Option<&str>,
        allow_insecure_http: bool,
    ) -> Result<Self, SyncError> {
        let parsed = validate_remote_url(url, allow_insecure_http)?;
        let display_url = sanitize_url_for_display(parsed.as_str());
        let base_url = strip_url_credentials(parsed);
        let decoded_base = percent_decode_str(base_url.path())?;
        let base_path = decoded_base.trim_end_matches('/').to_string();

        let mut secrets = Vec::new();
        let clean_token = token.map(str::trim).filter(|s| !s.is_empty());
        let clean_user = username.map(str::trim).filter(|s| !s.is_empty());
        let clean_pass = password.filter(|s| !s.is_empty());

        let auth_header = if let Some(tok) = clean_token {
            let header = format!("Bearer {tok}");
            secrets.push(tok.to_string());
            secrets.push(header.clone());
            Some(header)
        } else if clean_user.is_some() || clean_pass.is_some() {
            let raw_creds = format!(
                "{}:{}",
                clean_user.unwrap_or(""),
                clean_pass.unwrap_or("")
            );
            let encoded = base64_encode(raw_creds.as_bytes());
            let header = format!("Basic {encoded}");
            if let Some(p) = clean_pass {
                secrets.push(p.to_string());
            }
            secrets.push(encoded);
            secrets.push(header.clone());
            Some(header)
        } else {
            None
        };

        Ok(Self {
            base_url,
            base_path,
            display_url,
            auth_header,
            agent: build_http_agent(),
            created_dirs: Mutex::new(HashSet::new()),
            secrets,
        })
    }

    fn build_target_url(&self, rel_path: &str, trailing_slash: bool) -> Result<String, SyncError> {
        let mut url = self.base_url.clone();
        let base_trimmed = self.base_url.path().trim_matches('/');
        let rel_trimmed = rel_path.trim_matches('/');
        let encoded_rel = if rel_trimmed.is_empty() {
            String::new()
        } else {
            uri_encode_path(rel_trimmed)
        };

        let mut path = String::from("/");
        if !base_trimmed.is_empty() {
            path.push_str(base_trimmed);
        }
        if !encoded_rel.is_empty() {
            if !path.ends_with('/') {
                path.push('/');
            }
            path.push_str(&encoded_rel);
        }
        if trailing_slash && !path.ends_with('/') {
            path.push('/');
        }
        url.set_path(&path);
        Ok(url.into())
    }

    fn send_custom(
        &self,
        method: &str,
        target_url: &str,
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<ureq::http::Response<ureq::Body>, SyncError> {
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(target_url);
        if let Some(ref auth) = self.auth_header {
            builder = builder.header("Authorization", auth.as_str());
        }
        for (name, val) in extra_headers {
            builder = builder.header(*name, *val);
        }
        let req = builder.body(body).map_err(|err| {
            SyncError::Remote(redact_error_message(
                &format!("failed to construct WebDAV {method} request for {}: {err}", self.display_url),
                &self.secrets,
            ))
        })?;
        self.agent.run(req).map_err(|err| {
            SyncError::Remote(redact_error_message(
                &format!("WebDAV {method} failed at {}: {err}", self.display_url),
                &self.secrets,
            ))
        })
    }

    fn auth_or_status_err(&self, op: &str, status: u16) -> SyncError {
        if matches!(status, 401 | 403) {
            SyncError::Remote(format!(
                "WebDAV authentication failed (HTTP {status}) at {}",
                self.display_url
            ))
        } else {
            SyncError::Remote(format!(
                "WebDAV {op} failed with HTTP {status} at {}",
                self.display_url
            ))
        }
    }

    fn ensure_parent_dirs(&self, key: &str) -> Result<(), SyncError> {
        let Some((parent, _)) = key.rsplit_once('/') else {
            return Ok(());
        };
        let mut cumulative = String::new();
        for seg in parent.split('/') {
            if !cumulative.is_empty() {
                cumulative.push('/');
            }
            cumulative.push_str(seg);

            let already_exists = self
                .created_dirs
                .lock()
                .map(|guard| guard.contains(&cumulative))
                .unwrap_or(false);
            if already_exists {
                continue;
            }

            let dir_url = self.build_target_url(&cumulative, true)?;
            let resp = self.send_custom("MKCOL", &dir_url, &[], &[])?;
            let status = resp.status().as_u16();
            if !matches!(status, 200 | 201 | 204 | 301 | 405) {
                return Err(self.auth_or_status_err("MKCOL", status));
            }
            if let Ok(mut guard) = self.created_dirs.lock() {
                guard.insert(cumulative.clone());
            }
        }
        Ok(())
    }

    fn propfind_depth_1(
        &self,
        dir_rel: &str,
    ) -> Result<Option<Vec<DavEntry>>, SyncError> {
        let target_url = self.build_target_url(dir_rel, true)?;
        let resp = self.send_custom(
            "PROPFIND",
            &target_url,
            &[
                ("Depth", "1"),
                ("Content-Type", "application/xml; charset=utf-8"),
            ],
            PROPFIND_RESOURCETYPE_BODY,
        )?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if !matches!(status, 200 | 207) {
            return Err(self.auth_or_status_err("PROPFIND", status));
        }

        let body = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
        let xml = String::from_utf8(body).map_err(|err| {
            SyncError::Remote(format!("WebDAV PROPFIND XML is not valid UTF-8: {err}"))
        })?;
        Ok(Some(parse_multistatus_xml(&xml)))
    }

    fn href_to_relative_key(&self, href: &str) -> Result<Option<String>, SyncError> {
        let trimmed = href.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let raw_path = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            let Ok(parsed) = Url::parse(trimmed) else {
                return Ok(None);
            };
            parsed.path().to_string()
        } else {
            trimmed
                .split(['?', '#'])
                .next()
                .unwrap_or("")
                .to_string()
        };

        let decoded = percent_decode_str(&raw_path)?;
        let normalized_slashes = collapse_slashes(&decoded);
        let without_trailing = normalized_slashes.trim_end_matches('/');

        let rel = if self.base_path.is_empty() {
            without_trailing.trim_start_matches('/').to_string()
        } else if without_trailing == self.base_path {
            String::new()
        } else if let Some(rest) = without_trailing.strip_prefix(&format!("{}/", self.base_path)) {
            rest.to_string()
        } else {
            return Ok(None);
        };

        if rel.is_empty() {
            return Ok(Some(String::new()));
        }
        Ok(Some(rel))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DavEntry {
    href: String,
    is_collection: bool,
}

fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
                prev_slash = true;
            }
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    out
}

fn xml_local_name(raw_tag: &str) -> &str {
    let trimmed = raw_tag.trim();
    let name_part = trimmed
        .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("");
    name_part
        .rsplit_once(':')
        .map_or(name_part, |(_, local)| local)
}

fn find_matching_tag_range(
    xml: &str,
    start_from: usize,
    target_local: &str,
) -> Option<(usize, usize)> {
    let mut cursor = start_from;
    let mut depth = 0usize;
    let mut open_end = None;

    while let Some(rel_lt) = xml[cursor..].find('<') {
        let lt = cursor + rel_lt;
        if xml[lt..].starts_with("<!--") {
            let rel_end = xml[lt + 4..].find("-->")?;
            cursor = lt + 4 + rel_end + 3;
            continue;
        }
        let rel_gt = xml[lt..].find('>')?;
        let gt = lt + rel_gt;
        let inside = xml[lt + 1..gt].trim();
        cursor = gt + 1;

        if inside.starts_with('?') || inside.starts_with('!') {
            continue;
        }
        if let Some(closing) = inside.strip_prefix('/') {
            if xml_local_name(closing).eq_ignore_ascii_case(target_local) && depth > 0 {
                depth -= 1;
                if depth == 0 {
                    return open_end.map(|s| (s, lt));
                }
            }
        } else {
            let self_closing = inside.ends_with('/');
            if xml_local_name(inside).eq_ignore_ascii_case(target_local) {
                if self_closing {
                    if depth == 0 {
                        return Some((gt + 1, gt + 1));
                    }
                } else {
                    if depth == 0 {
                        open_end = Some(gt + 1);
                    }
                    depth += 1;
                }
            }
        }
    }
    None
}

fn extract_next_tag_inner(xml: &str, start_from: usize, target_local: &str) -> Option<(String, usize)> {
    let mut cursor = start_from;
    while let Some(rel_lt) = xml[cursor..].find('<') {
        let lt = cursor + rel_lt;
        if xml[lt..].starts_with("<!--") {
            let rel_end = xml[lt + 4..].find("-->")?;
            cursor = lt + 4 + rel_end + 3;
            continue;
        }
        let rel_gt = xml[lt..].find('>')?;
        let gt = lt + rel_gt;
        let inside = xml[lt + 1..gt].trim();
        if !inside.starts_with('/')
            && !inside.starts_with('?')
            && !inside.starts_with('!')
            && !inside.ends_with('/')
            && xml_local_name(inside).eq_ignore_ascii_case(target_local)
            && let Some((inner_start, inner_end)) = find_matching_tag_range(xml, lt, target_local)
        {
            return Some((xml[inner_start..inner_end].to_string(), inner_end));
        }
        cursor = gt + 1;
    }
    None
}

fn block_contains_collection_tag(resourcetype_inner: &str) -> bool {
    let mut cursor = 0;
    while let Some(rel_lt) = resourcetype_inner[cursor..].find('<') {
        let lt = cursor + rel_lt;
        let Some(rel_gt) = resourcetype_inner[lt..].find('>') else {
            break;
        };
        let gt = lt + rel_gt;
        let inside = resourcetype_inner[lt + 1..gt].trim();
        if !inside.starts_with('/')
            && !inside.starts_with('?')
            && !inside.starts_with('!')
            && xml_local_name(inside).eq_ignore_ascii_case("collection")
        {
            return true;
        }
        cursor = gt + 1;
    }
    false
}

fn parse_multistatus_xml(xml: &str) -> Vec<DavEntry> {
    let mut entries = Vec::new();
    let mut cursor = 0;
    while let Some((resp_inner, resp_end)) = extract_next_tag_inner(xml, cursor, "response") {
        cursor = resp_end;
        let Some((raw_href, _)) = extract_next_tag_inner(&resp_inner, 0, "href") else {
            continue;
        };
        let href = xml_unescape(&raw_href).trim().to_string();
        if href.is_empty() {
            continue;
        }

        let mut is_collection = false;
        let mut rt_cursor = 0;
        while let Some((rt_inner, rt_end)) =
            extract_next_tag_inner(&resp_inner, rt_cursor, "resourcetype")
        {
            rt_cursor = rt_end;
            if block_contains_collection_tag(&rt_inner) {
                is_collection = true;
                break;
            }
        }

        entries.push(DavEntry {
            href,
            is_collection,
        });
    }
    entries
}

impl SyncRemote for WebDavRemote {
    fn probe(&self) -> Result<RemoteProbeInfo, SyncError> {
        let root_url = self.build_target_url("", true)?;
        let resp = self.send_custom(
            "PROPFIND",
            &root_url,
            &[
                ("Depth", "0"),
                ("Content-Type", "application/xml; charset=utf-8"),
            ],
            PROPFIND_RESOURCETYPE_BODY,
        )?;
        let status = resp.status().as_u16();
        let final_status = if status == 405 {
            let opt_resp = self.send_custom("OPTIONS", &root_url, &[], &[])?;
            opt_resp.status().as_u16()
        } else {
            status
        };

        if matches!(final_status, 200 | 204 | 207) {
            Ok(RemoteProbeInfo {
                backend: "webdav".to_string(),
                detail: self.display_url.clone(),
            })
        } else {
            Err(self.auth_or_status_err("probe", final_status))
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, SyncError> {
        let norm_prefix = validate_relative_prefix(prefix)?;
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        queue.push_back(norm_prefix.clone());
        visited.insert(norm_prefix.clone());

        let mut results = Vec::new();
        let mut is_root_dir = true;
        let match_prefix = if norm_prefix.is_empty() {
            String::new()
        } else {
            format!("{norm_prefix}/")
        };

        while let Some(dir_rel) = queue.pop_front() {
            let entries = match self.propfind_depth_1(&dir_rel)? {
                Some(items) => items,
                None => {
                    if is_root_dir {
                        return Ok(Vec::new());
                    }
                    is_root_dir = false;
                    continue;
                }
            };
            is_root_dir = false;

            for entry in entries {
                let Some(rel_key) = self.href_to_relative_key(&entry.href)? else {
                    continue;
                };
                if rel_key.is_empty() || rel_key == dir_rel {
                    continue;
                }
                if validate_relative_key(&rel_key).is_err() {
                    continue;
                }
                if entry.is_collection {
                    if visited.insert(rel_key.clone()) {
                        if visited.len() > MAX_WEBDAV_DIRS_PER_LIST {
                            return Err(SyncError::Remote(format!(
                                "WebDAV list exceeded {MAX_WEBDAV_DIRS_PER_LIST} directory limit"
                            )));
                        }
                        queue.push_back(rel_key);
                    }
                } else if norm_prefix.is_empty()
                    || rel_key == norm_prefix
                    || rel_key.starts_with(&match_prefix)
                {
                    results.push(rel_key);
                }
            }
        }

        results.sort();
        results.dedup();
        Ok(results)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SyncError> {
        validate_relative_key(key)?;
        let url = self.build_target_url(key, false)?;
        let resp = self.send_custom("GET", &url, &[], &[])?;
        let status = resp.status().as_u16();
        match status {
            200 => {
                let bytes = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
                Ok(Some(bytes))
            }
            404 => Ok(None),
            _ => Err(self.auth_or_status_err("GET", status)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), SyncError> {
        validate_relative_key(key)?;
        self.ensure_parent_dirs(key)?;
        let url = self.build_target_url(key, false)?;
        let resp = self.send_custom("PUT", &url, &[("Content-Type", content_type)], bytes)?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 201 | 204) {
            Ok(())
        } else {
            Err(self.auth_or_status_err("PUT", status))
        }
    }

    fn delete(&self, key: &str) -> Result<(), SyncError> {
        validate_relative_key(key)?;
        let url = self.build_target_url(key, false)?;
        let resp = self.send_custom("DELETE", &url, &[], &[])?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 204 | 404) {
            Ok(())
        } else {
            Err(self.auth_or_status_err("DELETE", status))
        }
    }
}
