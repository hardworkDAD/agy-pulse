use jiff::Timestamp;
use sha2::{Digest, Sha256};
use url::Url;

use super::config::sanitize_url_for_display;
use super::http_util::{
    MAX_REMOTE_OBJECT_BYTES, build_http_agent, format_host_header, hmac_sha256, read_body_capped,
    redact_error_message, strip_url_credentials, uri_encode_path, uri_encode_segment,
    validate_remote_url, xml_unescape,
};
use super::protocol::{validate_relative_key, validate_relative_prefix, validate_single_segment};
use super::{RemoteProbeInfo, SyncError, SyncRemote};

const MAX_S3_LIST_PAGES: usize = 1000;

/// Output of pure-Rust AWS Signature Version 4 request signing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsV4SignedHeaders {
    pub host: String,
    pub x_amz_date: String,
    pub x_amz_content_sha256: String,
    pub authorization: String,
    pub canonical_request: String,
    pub string_to_sign: String,
    pub signature: String,
}

fn normalize_header_value(val: &str) -> String {
    val.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sign an HTTP request with AWS Signature Version 4 (`AWS4-HMAC-SHA256`).
#[allow(clippy::too_many_arguments)]
pub fn sign_aws_v4_headers(
    method: &str,
    canonical_uri: &str,
    query_params: &[(&str, &str)],
    host: &str,
    extra_headers: &[(&str, &str)],
    payload: &[u8],
    region: &str,
    service: &str,
    access_key: &str,
    secret_key: &str,
    amz_date: &str,
) -> Result<AwsV4SignedHeaders, SyncError> {
    if amz_date.len() < 8 {
        return Err(SyncError::Remote(
            "invalid amz_date for AWS SigV4 signing".to_string(),
        ));
    }
    let date_stamp = &amz_date[..8];
    let payload_hash = hex::encode(Sha256::digest(payload));

    let mut encoded_query: Vec<(String, String)> = query_params
        .iter()
        .map(|(k, v)| (uri_encode_segment(k), uri_encode_segment(v)))
        .collect();
    encoded_query.sort();
    let canonical_query = encoded_query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let mut headers: Vec<(String, String)> = vec![
        ("host".to_string(), normalize_header_value(host)),
        ("x-amz-content-sha256".to_string(), payload_hash.clone()),
        ("x-amz-date".to_string(), normalize_header_value(amz_date)),
    ];
    for (k, v) in extra_headers {
        let lower = k.trim().to_ascii_lowercase();
        if !lower.is_empty()
            && lower != "host"
            && lower != "x-amz-content-sha256"
            && lower != "x-amz-date"
        {
            headers.push((lower, normalize_header_value(v)));
        }
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let mut canonical_headers = String::new();
    let mut signed_names = Vec::with_capacity(headers.len());
    for (k, v) in &headers {
        canonical_headers.push_str(k);
        canonical_headers.push(':');
        canonical_headers.push_str(v);
        canonical_headers.push('\n');
        signed_names.push(k.as_str());
    }
    let signed_headers = signed_names.join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        canonical_uri,
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );

    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let canonical_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_hash}");

    let k_secret = format!("AWS4{secret_key}");
    let k_date = hmac_sha256(k_secret.as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    Ok(AwsV4SignedHeaders {
        host: host.to_string(),
        x_amz_date: amz_date.to_string(),
        x_amz_content_sha256: payload_hash,
        authorization,
        canonical_request,
        string_to_sign,
        signature,
    })
}

/// `SyncRemote` adapter for S3-compatible object storage (Cloudflare R2, AWS S3, Backblaze B2, MinIO, Aliyun OSS).
pub struct S3Remote {
    endpoint_url: Url,
    display_endpoint: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
    path_style: bool,
    agent: ureq::Agent,
    secrets: Vec<String>,
}

impl std::fmt::Debug for S3Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Remote")
            .field("endpoint", &self.display_endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("path_style", &self.path_style)
            .finish()
    }
}

impl S3Remote {
    pub fn new(
        endpoint: &str,
        region: &str,
        bucket: &str,
        access_key: Option<&str>,
        secret_key: Option<&str>,
        path_style: bool,
        allow_insecure_http: bool,
    ) -> Result<Self, SyncError> {
        let parsed = validate_remote_url(endpoint, allow_insecure_http)?;
        let display_endpoint = sanitize_url_for_display(parsed.as_str());
        let endpoint_url = strip_url_credentials(parsed);

        let trimmed_bucket = bucket.trim();
        validate_single_segment(trimmed_bucket, "s3 bucket")
            .map_err(|err| SyncError::Config(format!("invalid s3 bucket: {err}")))?;

        let ak = access_key
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SyncError::Config("s3 accessKey must not be empty".to_string()))?;
        let sk = secret_key
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| SyncError::Config("s3 secretKey must not be empty".to_string()))?;

        let trimmed_region = region.trim();
        let effective_region = if trimmed_region.is_empty() {
            "auto".to_string()
        } else {
            trimmed_region.to_string()
        };

        let secrets = vec![ak.to_string(), sk.to_string()];

        Ok(Self {
            endpoint_url,
            display_endpoint,
            region: effective_region,
            bucket: trimmed_bucket.to_string(),
            access_key: ak.to_string(),
            secret_key: sk.to_string(),
            path_style,
            agent: build_http_agent(),
            secrets,
        })
    }

    /// Construct `(full_request_url, canonical_uri, host_header)` for an S3 object or bucket request.
    pub fn build_request_target(
        &self,
        key: Option<&str>,
        query_params: &[(&str, &str)],
    ) -> Result<(String, String, String), SyncError> {
        let mut url = self.endpoint_url.clone();
        let base_trimmed = self.endpoint_url.path().trim_matches('/');

        if !self.path_style {
            let orig_host = self
                .endpoint_url
                .host_str()
                .ok_or_else(|| SyncError::Config("s3 endpoint missing host".to_string()))?;
            let vh_host = format!("{}.{}", self.bucket, orig_host);
            url.set_host(Some(&vh_host)).map_err(|err| {
                SyncError::Config(format!(
                    "invalid virtual-hosted s3 bucket host '{vh_host}': {err}"
                ))
            })?;
        }

        let mut canonical_uri = String::from("/");
        if !base_trimmed.is_empty() {
            canonical_uri.push_str(base_trimmed);
        }
        if self.path_style {
            if !canonical_uri.ends_with('/') {
                canonical_uri.push('/');
            }
            canonical_uri.push_str(&uri_encode_segment(&self.bucket));
        }
        if let Some(k) = key {
            let encoded_key = uri_encode_path(k);
            if !canonical_uri.ends_with('/') {
                canonical_uri.push('/');
            }
            canonical_uri.push_str(&encoded_key);
        }
        url.set_path(&canonical_uri);

        let mut encoded_query: Vec<(String, String)> = query_params
            .iter()
            .map(|(k, v)| (uri_encode_segment(k), uri_encode_segment(v)))
            .collect();
        encoded_query.sort();
        if !encoded_query.is_empty() {
            let qs = encoded_query
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            url.set_query(Some(&qs));
        } else {
            url.set_query(None);
        }

        let host_header = format_host_header(&url)?;
        Ok((url.into(), canonical_uri, host_header))
    }

    fn execute_signed(
        &self,
        method: &str,
        key: Option<&str>,
        query_params: &[(&str, &str)],
        extra_headers: &[(&str, &str)],
        payload: &[u8],
    ) -> Result<ureq::http::Response<ureq::Body>, SyncError> {
        let (full_url, canonical_uri, host_header) =
            self.build_request_target(key, query_params)?;
        let amz_date = Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string();
        let signed = sign_aws_v4_headers(
            method,
            &canonical_uri,
            query_params,
            &host_header,
            extra_headers,
            payload,
            &self.region,
            "s3",
            &self.access_key,
            &self.secret_key,
            &amz_date,
        )?;

        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(&full_url)
            .header("Host", signed.host.as_str())
            .header("x-amz-date", signed.x_amz_date.as_str())
            .header("x-amz-content-sha256", signed.x_amz_content_sha256.as_str())
            .header("Authorization", signed.authorization.as_str());

        for (name, val) in extra_headers {
            builder = builder.header(*name, *val);
        }

        let req = builder.body(payload).map_err(|err| {
            SyncError::Remote(redact_error_message(
                &format!(
                    "failed to construct S3 {method} request for {}/{}: {err}",
                    self.display_endpoint, self.bucket
                ),
                &self.secrets,
            ))
        })?;

        self.agent.run(req).map_err(|err| {
            SyncError::Remote(redact_error_message(
                &format!(
                    "S3 {method} failed for {}/{}: {err}",
                    self.display_endpoint, self.bucket
                ),
                &self.secrets,
            ))
        })
    }

    fn status_err(&self, op: &str, status: u16) -> SyncError {
        if matches!(status, 401 | 403) {
            SyncError::Remote(format!(
                "S3 authentication/permission failed (HTTP {status}) for {}/{}",
                self.display_endpoint, self.bucket
            ))
        } else {
            SyncError::Remote(format!(
                "S3 {op} failed with HTTP {status} for {}/{}",
                self.display_endpoint, self.bucket
            ))
        }
    }
}

#[derive(Debug, Default)]
struct ListObjectsPage {
    keys: Vec<String>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
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

enum XmlSkip {
    Advance(usize),
    Unterminated,
    NotSpecial,
}

fn skip_xml_special(xml: &str, lt: usize) -> XmlSkip {
    let rest = &xml[lt..];
    if let Some(after) = rest.strip_prefix("<!--") {
        return match after.find("-->") {
            Some(end) => XmlSkip::Advance(lt + 4 + end + 3),
            None => XmlSkip::Unterminated,
        };
    }
    if let Some(after) = rest.strip_prefix("<![CDATA[") {
        return match after.find("]]>") {
            Some(end) => XmlSkip::Advance(lt + 9 + end + 3),
            None => XmlSkip::Unterminated,
        };
    }
    if let Some(after) = rest.strip_prefix("<?") {
        return match after.find("?>") {
            Some(end) => XmlSkip::Advance(lt + 2 + end + 2),
            None => XmlSkip::Unterminated,
        };
    }
    XmlSkip::NotSpecial
}

fn extract_all_tag_inners<'a>(xml: &'a str, target_local: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(rel_lt) = xml[cursor..].find('<') {
        let lt = cursor + rel_lt;
        match skip_xml_special(xml, lt) {
            XmlSkip::Advance(next) => {
                cursor = next;
                continue;
            }
            XmlSkip::Unterminated => break,
            XmlSkip::NotSpecial => {}
        }

        let Some(rel_gt) = xml[lt..].find('>') else {
            break;
        };
        let gt = lt + rel_gt;
        let inside = xml[lt + 1..gt].trim();
        cursor = gt + 1;

        if inside.starts_with('/')
            || inside.starts_with('!')
            || inside.ends_with('/')
            || !xml_local_name(inside).eq_ignore_ascii_case(target_local)
        {
            continue;
        }

        let inner_start = cursor;
        let mut scan = cursor;
        let mut depth = 1usize;
        while let Some(rel_next_lt) = xml[scan..].find('<') {
            let next_lt = scan + rel_next_lt;
            match skip_xml_special(xml, next_lt) {
                XmlSkip::Advance(next) => {
                    scan = next;
                    continue;
                }
                XmlSkip::Unterminated => {
                    scan = xml.len();
                    break;
                }
                XmlSkip::NotSpecial => {}
            }

            let Some(rel_next_gt) = xml[next_lt..].find('>') else {
                scan = xml.len();
                break;
            };
            let next_gt = next_lt + rel_next_gt;
            let tag_inside = xml[next_lt + 1..next_gt].trim();
            scan = next_gt + 1;

            if let Some(close_name) = tag_inside.strip_prefix('/') {
                if xml_local_name(close_name).eq_ignore_ascii_case(target_local) {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&xml[inner_start..next_lt]);
                        cursor = next_gt + 1;
                        break;
                    }
                }
            } else if !tag_inside.starts_with('!')
                && !tag_inside.ends_with('/')
                && xml_local_name(tag_inside).eq_ignore_ascii_case(target_local)
            {
                depth += 1;
            }
        }
        if depth > 0 {
            cursor = scan;
        }
    }
    out
}

fn decode_xml_text_content(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut pos = 0;
    while let Some(rel_lt) = inner[pos..].find('<') {
        let lt = pos + rel_lt;
        out.push_str(&xml_unescape(&inner[pos..lt]));
        let rest = &inner[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let Some(end) = after.find("-->") else {
                return out;
            };
            pos = lt + 4 + end + 3;
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let Some(end) = after.find("]]>") else {
                return out;
            };
            out.push_str(&after[..end]);
            pos = lt + 9 + end + 3;
        } else if let Some(after) = rest.strip_prefix("<?") {
            let Some(end) = after.find("?>") else {
                return out;
            };
            pos = lt + 2 + end + 2;
        } else {
            let Some(rel_gt) = rest.find('>') else {
                return out;
            };
            pos = lt + rel_gt + 1;
        }
    }
    out.push_str(&xml_unescape(&inner[pos..]));
    out
}

fn extract_all_tag_texts(xml: &str, target_local: &str) -> Vec<String> {
    extract_all_tag_inners(xml, target_local)
        .into_iter()
        .map(decode_xml_text_content)
        .collect()
}

fn parse_list_objects_v2_xml(xml: &str) -> ListObjectsPage {
    let keys = extract_all_tag_inners(xml, "Contents")
        .into_iter()
        .flat_map(|contents| extract_all_tag_texts(contents, "Key"))
        .collect();
    let is_truncated = extract_all_tag_texts(xml, "IsTruncated")
        .into_iter()
        .any(|v| v.trim().eq_ignore_ascii_case("true"));
    let next_continuation_token = extract_all_tag_texts(xml, "NextContinuationToken")
        .into_iter()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty());

    ListObjectsPage {
        keys,
        is_truncated,
        next_continuation_token,
    }
}

impl SyncRemote for S3Remote {
    fn probe(&self) -> Result<RemoteProbeInfo, SyncError> {
        let resp = self.execute_signed(
            "GET",
            None,
            &[("list-type", "2"), ("max-keys", "1")],
            &[],
            &[],
        )?;
        let status = resp.status().as_u16();
        if status == 200 {
            let _ = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
            Ok(RemoteProbeInfo {
                backend: "s3".to_string(),
                detail: format!("{}/{}", self.display_endpoint, self.bucket),
            })
        } else {
            Err(self.status_err("probe", status))
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, SyncError> {
        let norm_prefix = validate_relative_prefix(prefix)?;
        let match_prefix = if norm_prefix.is_empty() {
            String::new()
        } else {
            format!("{norm_prefix}/")
        };

        let mut results = Vec::new();
        let mut continuation_token: Option<String> = None;

        for _page in 0..MAX_S3_LIST_PAGES {
            let mut query: Vec<(&str, &str)> = vec![("list-type", "2")];
            if !match_prefix.is_empty() {
                query.push(("prefix", &match_prefix));
            }
            if let Some(ref tok) = continuation_token {
                query.push(("continuation-token", tok.as_str()));
            }

            let resp = self.execute_signed("GET", None, &query, &[], &[])?;
            let status = resp.status().as_u16();
            if status != 200 {
                return Err(self.status_err("ListObjectsV2", status));
            }
            let body = read_body_capped(resp.into_body(), MAX_REMOTE_OBJECT_BYTES)?;
            let xml = String::from_utf8(body).map_err(|err| {
                SyncError::Remote(format!("S3 ListObjectsV2 XML is not valid UTF-8: {err}"))
            })?;
            let page = parse_list_objects_v2_xml(&xml);

            for key in page.keys {
                if validate_relative_key(&key).is_err() {
                    continue;
                }
                if norm_prefix.is_empty() || key == norm_prefix || key.starts_with(&match_prefix) {
                    results.push(key);
                }
            }

            if !page.is_truncated {
                results.sort();
                results.dedup();
                return Ok(results);
            }

            let Some(next_tok) = page.next_continuation_token else {
                return Err(SyncError::Remote(
                    "S3 ListObjectsV2 returned IsTruncated=true without NextContinuationToken"
                        .to_string(),
                ));
            };
            continuation_token = Some(next_tok);
        }

        Err(SyncError::Remote(format!(
            "S3 ListObjectsV2 pagination exceeded {MAX_S3_LIST_PAGES} pages"
        )))
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SyncError> {
        validate_relative_key(key)?;
        let resp = self.execute_signed("GET", Some(key), &[], &[], &[])?;
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
        let resp = self.execute_signed(
            "PUT",
            Some(key),
            &[],
            &[("content-type", content_type)],
            bytes,
        )?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 201 | 204) {
            Ok(())
        } else {
            Err(self.status_err("PUT", status))
        }
    }

    fn delete(&self, key: &str) -> Result<(), SyncError> {
        validate_relative_key(key)?;
        let resp = self.execute_signed("DELETE", Some(key), &[], &[], &[])?;
        let status = resp.status().as_u16();
        if matches!(status, 200 | 204 | 404) {
            Ok(())
        } else {
            Err(self.status_err("DELETE", status))
        }
    }
}
