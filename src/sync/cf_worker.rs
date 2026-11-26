use serde::Deserialize;
use url::Url;

use super::config::sanitize_url_for_display;
use super::http_util::{
    MAX_REMOTE_OBJECT_BYTES, build_http_agent, read_body_capped, redact_error_message,
    strip_url_credentials, uri_encode_path, uri_encode_segment, validate_remote_url,
};
use super::protocol::{validate_relative_key, validate_relative_prefix};
use super::{RemoteProbeInfo, SyncError, SyncRemote};

const MAX_CF_LIST_PAGES: usize = 1000;

#[derive(Deserialize)]
struct WorkerHealthResponse {
    ok: bool,
    #[serde(default)]
    storage: Option<String>,
}

#[derive(Deserialize)]
struct WorkerListResponse {
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    cursor: Option<String>,
}

/// `SyncRemote` adapter backed by the companion `deploy/cloudflare-worker` HTTP API.
pub struct CfWorkerRemote {
    base_url: Url,
    display_url: String,
    auth_header: String,
    agent: ureq::Agent,
    secrets: Vec<String>,
}

impl std::fmt::Debug for CfWorkerRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CfWorkerRemote")
            .field("url", &self.display_url)
            .finish()
    }
}

impl CfWorkerRemote {
    pub fn new(
        url: &str,
        token: Option<&str>,
        allow_insecure_http: bool,
    ) -> Result<Self, SyncError> {
        let parsed = validate_remote_url(url, allow_insecure_http)?;
        let display_url = sanitize_url_for_display(parsed.as_str());
        let base_url = strip_url_credentials(parsed);

        let tok = token
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                SyncError::Config("cf-worker remote requires a non-empty token".to_string())
            })?;
        let auth_header = format!("Bearer {tok}");
        let secrets = vec![tok.to_string(), auth_header.clone()];

        Ok(Self {
            base_url,
            display_url,
            auth_header,
            agent: build_http_agent(),
            secrets,
        })
    }

    fn build_endpoint_url(&self, sub_path: &str, query: Option<&str>) -> String {
        let mut url = self.base_url.clone();
        let base_trimmed = self.base_url.path().trim_matches('/');
        let sub_trimmed = sub_path.trim_start_matches('/');
        let full_path = if base_trimmed.is_empty() {
            format!("/{sub_trimmed}")
        } else {
            format!("/{base_trimmed}/{sub_trimmed}")
        };
        url.set_path(&full_path);
        url.set_query(query);
        url.into()
    }

    fn send(
        &self,
        method: &str,
        target_url: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<ureq::http::Response<ureq::Body>, SyncError> {
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(target_url)
            .header("Authorization", self.auth_header.as_str());
        if let Some(ct) = content_type {
            builder = builder.header("Content-Type", ct);
        }
        let req = builder.body(body).map_err(|err| {
            SyncError::Remote(redact_error_message(
                &format!(
                    "failed to construct cf-worker {method} request for {}: {err}",
                    self.display_url
                ),
                &self.secrets,
            ))
        })?;
        self.agent.run(req).map_err(|err| {
            SyncError::Remote(redact_error_message(
                &format!("cf-worker {method} failed at {}: {err}", self.display_url),
                &self.secrets,
            ))
        })
    }

    fn status_err(&self, op: &str, status: u16) -> SyncError {
        if matches!(status, 401 | 403) {
            SyncError::Remote(format!(
                "cf-worker authentication failed (HTTP {status}) at {}",
                self.display_url
            ))
        } else {
            SyncError::Remote(format!(
                "cf-worker {op} failed with HTTP {status} at {}",
                self.display_url
            ))
        }
    }
}

impl SyncRemote for CfWorkerRemote {
    fn probe(&self) -> Result<RemoteProbeInfo, SyncError> {
        let url = self.build_endpoint_url("v1/health", None);
        let resp = self.send("GET", &url, None, &[])?;
        let status = resp.status().as_u16();
        if status != 200 {
            return Err(self.status_err("health probe", status));
        }

        let body = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
        let parsed: WorkerHealthResponse = serde_json::from_slice(&body).map_err(|err| {
            SyncError::Remote(format!("cf-worker health returned invalid JSON: {err}"))
        })?;
        if !parsed.ok {
            return Err(SyncError::Remote(format!(
                "cf-worker health reported ok=false at {}",
                self.display_url
            )));
        }
        let storage = parsed
            .storage
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");

        Ok(RemoteProbeInfo {
            backend: "cf-worker".to_string(),
            detail: format!("{} ({storage})", self.display_url),
        })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, SyncError> {
        let norm_prefix = validate_relative_prefix(prefix)?;
        let match_prefix = if norm_prefix.is_empty() {
            String::new()
        } else {
            format!("{norm_prefix}/")
        };

        let mut results = Vec::new();
        let mut cursor: Option<String> = None;

        for _page in 0..MAX_CF_LIST_PAGES {
            let mut qs = format!("prefix={}", uri_encode_segment(&norm_prefix));
            if let Some(ref cur) = cursor {
                qs.push_str("&cursor=");
                qs.push_str(&uri_encode_segment(cur));
            }
            let url = self.build_endpoint_url("v1/objects", Some(&qs));
            let resp = self.send("GET", &url, None, &[])?;
            let status = resp.status().as_u16();
            if status != 200 {
                return Err(self.status_err("list", status));
            }
            let body = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
            let parsed: WorkerListResponse = serde_json::from_slice(&body).map_err(|err| {
                SyncError::Remote(format!("cf-worker list returned invalid JSON: {err}"))
            })?;

            for key in parsed.keys {
                if validate_relative_key(&key).is_err() {
                    continue;
                }
                if norm_prefix.is_empty() || key == norm_prefix || key.starts_with(&match_prefix) {
                    results.push(key);
                }
            }

            match parsed.cursor.as_deref().map(str::trim) {
                Some(next_cur) if !next_cur.is_empty() => {
                    cursor = Some(next_cur.to_string());
                }
                _ => {
                    results.sort();
                    results.dedup();
                    return Ok(results);
                }
            }
        }

        Err(SyncError::Remote(format!(
            "cf-worker list pagination exceeded {MAX_CF_LIST_PAGES} pages"
        )))
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SyncError> {
        validate_relative_key(key)?;
        let encoded = uri_encode_path(key);
        let url = self.build_endpoint_url(&format!("v1/objects/{encoded}"), None);
        let resp = self.send("GET", &url, None, &[])?;
        let status = resp.status().as_u16();
        match status {
            200 => {
                let bytes = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
                Ok(Some(bytes))
            }
            404 => Ok(None),
            _ => Err(self.status_err("GET", status)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), SyncError> {
        validate_relative_key(key)?;
        let encoded = uri_encode_path(key);
        let url = self.build_endpoint_url(&format!("v1/objects/{encoded}"), None);
        let resp = self.send("PUT", &url, Some(content_type), bytes)?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 201 | 204) {
            Ok(())
        } else {
            Err(self.status_err("PUT", status))
        }
    }

    fn delete(&self, key: &str) -> Result<(), SyncError> {
        validate_relative_key(key)?;
        let encoded = uri_encode_path(key);
        let url = self.build_endpoint_url(&format!("v1/objects/{encoded}"), None);
        let resp = self.send("DELETE", &url, None, &[])?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 204 | 404) {
            Ok(())
        } else {
            Err(self.status_err("DELETE", status))
        }
    }
}
