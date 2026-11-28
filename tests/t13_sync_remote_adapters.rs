#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use rusqlite::{Connection, params};
use sha2::Digest;
use tempfile::TempDir;

use agy_pulse::domain::{ProviderQuota, ProviderQuotaSummary, QuotaSnapshot, QuotaWindow};
use agy_pulse::ledger::{LedgerSource, LedgerStore, LedgerSyncOptions, SourceRoot};
use agy_pulse::sync::http_util::{
    MAX_REMOTE_OBJECT_BYTES, base64_encode, hmac_sha256, uri_encode_segment, validate_remote_url,
    xml_unescape,
};
use agy_pulse::sync::s3::{S3Remote, sign_aws_v4_headers};
use agy_pulse::sync::{
    CfWorkerRemote, RemoteConfig, SyncConfig, SyncError, SyncRemote, WebDavRemote, build_remote,
    pull, push,
};

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct HttpResponseSpec {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponseSpec {
    fn new(status: u16, reason: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct MockHttpServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockHttpServer {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> HttpResponseSpec + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handler = Arc::new(handler);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                        handle_connection(stream, handler.as_ref());
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn base_url(&self, suffix: &str) -> String {
        let s = suffix.trim_start_matches('/');
        if s.is_empty() {
            format!("http://{}", self.addr)
        } else {
            format!("http://{}/{}", self.addr, s)
        }
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn handle_connection<F>(mut stream: TcpStream, handler: &F)
where
    F: Fn(&RecordedRequest) -> HttpResponseSpec,
{
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let parts: Vec<&str> = request_line
        .trim_end_matches(['\r', '\n'])
        .split_whitespace()
        .collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].to_string();
    let target = parts[1].to_string();

    let mut headers = BTreeMap::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key_lower = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key_lower == "content-length" {
                content_length = val.parse::<usize>().unwrap_or(0);
            }
            headers.insert(key_lower, val);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    let req = RecordedRequest {
        method,
        target,
        headers,
        body,
    };
    let resp = handler(&req);

    let mut wire = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.reason,
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        wire.push_str(&format!("{k}: {v}\r\n"));
    }
    wire.push_str("\r\n");
    let _ = stream.write_all(wire.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
}

fn split_target(target: &str) -> (&str, BTreeMap<String, String>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut params = BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(k), percent_decode(v));
    }
    (path, params)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(val) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ---------------------------------------------------------------------------
// Ledger seeding helpers (reused offline isolation pattern)
// ---------------------------------------------------------------------------

fn encode_varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while v >= 0x80 {
        out.push(((v & 0x7f) as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
    out
}

fn pb_varint(field_num: u32, val: u64) -> Vec<u8> {
    let mut out = encode_varint(u64::from(field_num) << 3);
    out.extend(encode_varint(val));
    out
}

fn pb_bytes(field_num: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = encode_varint((u64::from(field_num) << 3) | 2);
    out.extend(encode_varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn pb_str(field_num: u32, s: &str) -> Vec<u8> {
    pb_bytes(field_num, s.as_bytes())
}

fn pb_timestamp_ms(field_num: u32, ts_ms: i64) -> Vec<u8> {
    let sec = (ts_ms / 1000) as u64;
    let nanos = ((ts_ms % 1000) * 1_000_000) as u64;
    let mut inner = pb_varint(1, sec);
    if nanos > 0 {
        inner.extend(pb_varint(2, nanos));
    }
    pb_bytes(field_num, &inner)
}

fn pb_duration_sec(field_num: u32, sec: u64) -> Vec<u8> {
    let inner = if sec > 0 {
        pb_varint(1, sec)
    } else {
        Vec::new()
    };
    pb_bytes(field_num, &inner)
}

fn seed_conv(
    root: &Path,
    conv_id: &str,
    workspace: &str,
    branch: &str,
    ts_ms: i64,
    input_tokens: u64,
    output_tokens: u64,
) {
    let db_path = root.join("conversations").join(format!("{conv_id}.db"));
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);
         CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();

    let step_meta = pb_timestamp_ms(1, ts_ms);
    conn.execute(
        "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
        params![1i64, step_meta],
    )
    .unwrap();

    let mut gen_blob = pb_varint(2, 1);
    let mut f1 = pb_str(21, "Claude Sonnet 4.5");
    let mut usage = pb_varint(2, input_tokens);
    usage.extend(pb_varint(3, output_tokens));
    f1.extend(pb_bytes(4, &usage));
    f1.extend(pb_duration_sec(11, 1));
    f1.extend(pb_duration_sec(12, 5));
    gen_blob.extend(pb_bytes(1, &f1));
    conn.execute(
        "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
        params![1i64, gen_blob],
    )
    .unwrap();

    let mut ws = pb_str(1, workspace);
    ws.extend(pb_str(4, branch));
    let mut traj = pb_bytes(1, &ws);
    traj.extend(pb_timestamp_ms(2, ts_ms));
    traj.extend(pb_str(6, conv_id));
    conn.execute(
        "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
        params![traj],
    )
    .unwrap();
}

struct IsolatedMachine {
    _temp: TempDir,
    cli_root: PathBuf,
    ledger_path: PathBuf,
}

impl IsolatedMachine {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let cli_root = temp.path().join("cli");
        fs::create_dir_all(&cli_root).unwrap();
        let ledger_path = cli_root.join("agy-pulse-ledger.sqlite");
        Self {
            _temp: temp,
            cli_root,
            ledger_path,
        }
    }

    fn open_store(&self) -> LedgerStore {
        LedgerStore::open_at(&self.ledger_path).unwrap()
    }
}

fn utc_ms(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    Utc.with_ymd_and_hms(y, m, d, hh, mm, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn seed_machine_a_and_sync_roundtrip(remote_cfg: RemoteConfig) {
    let machine_a = IsolatedMachine::new();
    let machine_b = IsolatedMachine::new();
    let ts_oct = utc_ms(2026, 10, 15, 12, 0);
    let ts_nov = utc_ms(2026, 11, 20, 9, 30);

    seed_conv(
        &machine_a.cli_root,
        "conv-remote-1",
        "file:///secret/workspace/repo",
        "feat/confidential",
        ts_oct,
        2_000,
        500,
    );
    seed_conv(
        &machine_a.cli_root,
        "conv-remote-2",
        "file:///secret/workspace/repo2",
        "main",
        ts_nov,
        1_200,
        300,
    );

    let mut store_a = machine_a.open_store();
    let dev_a = store_a.local_device_id().unwrap();
    store_a
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_a.cli_root.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(ts_nov + 1_000),
            default_account_id: Some("alice+sync@example.com".to_string()),
            device_name: Some("laptop-a".to_string()),
            plan_tier: Some("Ultra".to_string()),
            ..Default::default()
        })
        .unwrap();
    store_a
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice+sync@example.com".to_string(),
            device_id: dev_a.clone(),
            device_name: "laptop-a".to_string(),
            plan_tier: Some("Ultra".to_string()),
            observed_at_ms: ts_nov + 2_000,
            quota: ProviderQuotaSummary {
                google: ProviderQuota {
                    five_hour: Some(QuotaWindow {
                        remaining_fraction: 0.91,
                        reset_time_ms: Some(ts_nov + 3_600_000),
                    }),
                    weekly: None,
                },
                ..Default::default()
            },
        })
        .unwrap();

    let cfg_a = SyncConfig {
        enabled: true,
        device_id: None,
        device_name: Some("laptop-a".to_string()),
        default_account: None,
        push_interval_sec: 60,
        prefix: "agy-pulse-sync".to_string(),
        remote: remote_cfg.clone(),
    };
    let remote_a = build_remote(&cfg_a).unwrap();
    let push_res = push(&store_a, remote_a.as_ref(), &cfg_a, false).unwrap();
    assert_eq!(push_res.chunks_uploaded, 2);
    assert_eq!(push_res.manifests_uploaded, 1);

    let store_b = machine_b.open_store();
    let dev_b = store_b.local_device_id().unwrap();
    assert_ne!(dev_a, dev_b);

    let cfg_b = SyncConfig {
        enabled: true,
        device_id: None,
        device_name: Some("desktop-b".to_string()),
        default_account: None,
        push_interval_sec: 60,
        prefix: "agy-pulse-sync".to_string(),
        remote: remote_cfg,
    };
    let remote_b = build_remote(&cfg_b).unwrap();
    let pull_res = pull(&store_b, remote_b.as_ref(), &cfg_b, false).unwrap();
    assert_eq!(pull_res.devices_synced, 1);
    assert_eq!(pull_res.chunks_downloaded, 2);
    assert_eq!(pull_res.steps_imported, 2);

    // Verify synced data on Machine B and verify privacy stripping (no workspace/branch).
    let conn_b = Connection::open(&machine_b.ledger_path).unwrap();
    let count_steps: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE account_id = 'alice+sync@example.com' AND device_id = ?1",
            params![dev_a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count_steps, 2);

    let leaked_ws: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE workspace IS NOT NULL OR branch IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked_ws, 0);
}

// ---------------------------------------------------------------------------
// 1. AWS SigV4 Golden Test Vectors & Utility Checks
// ---------------------------------------------------------------------------

#[test]
fn t13_aws_sigv4_golden_vectors_and_crypto_helpers() {
    // Official AWS S3 SigV4 Example: GET Object (/test.txt, Range: bytes=0-9)
    let signed_get = sign_aws_v4_headers(
        "GET",
        "/test.txt",
        &[],
        "examplebucket.s3.amazonaws.com",
        &[("range", "bytes=0-9")],
        b"",
        "us-east-1",
        "s3",
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "20130524T000000Z",
    )
    .unwrap();

    let expected_canonical_get = "\
GET
/test.txt

host:examplebucket.s3.amazonaws.com
range:bytes=0-9
x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
x-amz-date:20130524T000000Z

host;range;x-amz-content-sha256;x-amz-date
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(signed_get.canonical_request, expected_canonical_get);

    let expected_sts_get = "\
AWS4-HMAC-SHA256
20130524T000000Z
20130524/us-east-1/s3/aws4_request
7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972";
    assert_eq!(signed_get.string_to_sign, expected_sts_get);
    assert_eq!(
        signed_get.signature,
        "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
    assert_eq!(
        signed_get.authorization,
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );

    // Official AWS S3 SigV4 Example: GET Bucket List Objects (?max-keys=2&prefix=J)
    let signed_list = sign_aws_v4_headers(
        "GET",
        "/",
        &[("max-keys", "2"), ("prefix", "J")],
        "examplebucket.s3.amazonaws.com",
        &[],
        b"",
        "us-east-1",
        "s3",
        "AKIAIOSFODNN7EXAMPLE",
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        "20130524T000000Z",
    )
    .unwrap();
    assert_eq!(
        signed_list.signature,
        "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
    );

    // Base64, RFC 3986 encoding, HMAC-SHA256 (RFC 4231 test vector 2), and XML unescaping.
    assert_eq!(base64_encode(b"alice:secret"), "YWxpY2U6c2VjcmV0");
    assert_eq!(base64_encode(b"a"), "YQ==");
    assert_eq!(base64_encode(b"ab"), "YWI=");
    assert_eq!(
        uri_encode_segment("alice+1@ex.com/a b~_.-"),
        "alice%2B1%40ex.com%2Fa%20b~_.-"
    );
    assert_eq!(
        xml_unescape("a&amp;b&lt;c&gt;&quot;d&apos;&#65;&#x42;"),
        "a&b<c>\"d'AB"
    );
    assert_eq!(
        hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );

    // Verify S3 URL & Host header formatting for path-style vs virtual-hosted-style & default ports.
    let s3_vh = S3Remote::new(
        "https://s3.us-east-1.amazonaws.com:443",
        "us-east-1",
        "my-bucket",
        Some("ak"),
        Some("sk"),
        false,
        false,
    )
    .unwrap();
    let (url_vh, curi_vh, host_vh) = s3_vh
        .build_request_target(Some("agy-pulse-sync/a+b/manifest.json"), &[])
        .unwrap();
    assert_eq!(
        url_vh,
        "https://my-bucket.s3.us-east-1.amazonaws.com/agy-pulse-sync/a%2Bb/manifest.json"
    );
    assert_eq!(curi_vh, "/agy-pulse-sync/a%2Bb/manifest.json");
    assert_eq!(host_vh, "my-bucket.s3.us-east-1.amazonaws.com");

    let s3_ps = S3Remote::new(
        "http://127.0.0.1:9000",
        "auto",
        "pulse-bucket",
        Some("ak"),
        Some("sk"),
        true,
        false,
    )
    .unwrap();
    let (url_ps, curi_ps, host_ps) = s3_ps.build_request_target(Some("k/v.json"), &[]).unwrap();
    assert_eq!(url_ps, "http://127.0.0.1:9000/pulse-bucket/k/v.json");
    assert_eq!(curi_ps, "/pulse-bucket/k/v.json");
    assert_eq!(host_ps, "127.0.0.1:9000");

    let s3_http_80 = S3Remote::new(
        "http://minio.internal:80",
        "auto",
        "bkt",
        Some("ak"),
        Some("sk"),
        true,
        true,
    )
    .unwrap();
    let (_, _, host_80) = s3_http_80.build_request_target(None, &[]).unwrap();
    assert_eq!(host_80, "minio.internal");
}

// ---------------------------------------------------------------------------
// 2. WebDAV Mock Server End-to-End & Two-Machine Push/Pull
// ---------------------------------------------------------------------------

#[derive(Default)]
struct WebDavState {
    dirs: BTreeSet<String>,
    files: BTreeMap<String, Vec<u8>>,
    methods: Vec<String>,
}

#[test]
fn t13_webdav_mock_e2e_and_two_machine_sync() {
    let expected_auth = format!("Basic {}", base64_encode(b"alice:dav-secret-999"));
    let state = Arc::new(Mutex::new(WebDavState::default()));
    {
        let mut guard = state.lock().unwrap();
        guard.dirs.insert(String::new());
    }
    let state_srv = Arc::clone(&state);

    let server = MockHttpServer::start(move |req| {
        if req.headers.get("authorization").map(String::as_str) != Some(expected_auth.as_str()) {
            return HttpResponseSpec::new(401, "Unauthorized", b"auth required");
        }

        let (raw_path, _) = split_target(&req.target);
        let decoded = percent_decode(raw_path);
        let stripped = decoded
            .strip_prefix("/dav")
            .unwrap_or("")
            .trim_matches('/')
            .to_string();

        let mut st = state_srv.lock().unwrap();
        st.methods.push(format!("{} {}", req.method, stripped));

        match req.method.as_str() {
            "PROPFIND" => {
                let depth = req.headers.get("depth").map(String::as_str).unwrap_or("0");
                if depth == "0" {
                    let body = format!(
                        r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/{stripped}</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
</D:multistatus>"#
                    );
                    return HttpResponseSpec::new(207, "Multi-Status", body);
                }

                if !st.dirs.contains(&stripped) {
                    return HttpResponseSpec::new(404, "Not Found", b"no such collection");
                }

                // Mix <D:response>, <d:response>, and unprefixed <response>, plus absolute & relative hrefs,
                // and include the queried directory itself to verify cycle/self-skipping.
                let mut responses = String::new();
                let self_href = if stripped.is_empty() {
                    "/dav/".to_string()
                } else {
                    format!("/dav/{stripped}/")
                };
                responses.push_str(&format!(
                    "<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response>",
                    xml_escape(&self_href)
                ));

                let prefix_slash = if stripped.is_empty() {
                    String::new()
                } else {
                    format!("{stripped}/")
                };

                let mut child_idx = 0usize;
                for dir in &st.dirs {
                    if dir.is_empty() || dir == &stripped {
                        continue;
                    }
                    if let Some(rest) = dir.strip_prefix(&prefix_slash)
                        && !rest.contains('/')
                    {
                        let href = format!(
                            "http://{}/dav/{}/",
                            req.headers
                                .get("host")
                                .map(String::as_str)
                                .unwrap_or("127.0.0.1"),
                            uri_encode_path_for_test(dir)
                        );
                        if child_idx.is_multiple_of(2) {
                            responses.push_str(&format!(
                                "<d:response xmlns:d=\"DAV:\"><d:href>{}</d:href><d:propstat><d:prop><d:resourcetype><d:collection /></d:resourcetype></d:prop></d:propstat></d:response>",
                                xml_escape(&href)
                            ));
                        } else {
                            responses.push_str(&format!(
                                "<response xmlns=\"DAV:\"><href>{}</href><propstat><prop><resourcetype><collection/></resourcetype></prop></propstat></response>",
                                xml_escape(&href)
                            ));
                        }
                        child_idx += 1;
                    }
                }

                for file_key in st.files.keys() {
                    if let Some(rest) = file_key.strip_prefix(&prefix_slash)
                        && !rest.contains('/')
                    {
                        let href = format!("/dav/{}", uri_encode_path_for_test(file_key));
                        if child_idx.is_multiple_of(2) {
                            responses.push_str(&format!(
                                "<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype/></D:prop></D:propstat></D:response>",
                                xml_escape(&href)
                            ));
                        } else {
                            responses.push_str(&format!(
                                "<response><href>{}</href><propstat><prop><resourcetype></resourcetype></prop></propstat></response>",
                                xml_escape(&href)
                            ));
                        }
                        child_idx += 1;
                    }
                }

                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">{responses}</D:multistatus>"
                );
                HttpResponseSpec::new(207, "Multi-Status", xml)
            }
            "MKCOL" => {
                if st.dirs.contains(&stripped) {
                    HttpResponseSpec::new(405, "Method Not Allowed", b"already exists")
                } else {
                    st.dirs.insert(stripped);
                    HttpResponseSpec::new(201, "Created", b"")
                }
            }
            "PUT" => {
                if let Some((parent, _)) = stripped.rsplit_once('/')
                    && !st.dirs.contains(parent)
                {
                    return HttpResponseSpec::new(409, "Conflict", b"missing parent");
                }
                st.files.insert(stripped, req.body.clone());
                HttpResponseSpec::new(201, "Created", b"")
            }
            "GET" => match st.files.get(&stripped) {
                Some(bytes) => HttpResponseSpec::new(200, "OK", bytes.clone()),
                None => HttpResponseSpec::new(404, "Not Found", b"missing"),
            },
            "DELETE" => {
                if st.files.remove(&stripped).is_some() {
                    HttpResponseSpec::new(204, "No Content", b"")
                } else {
                    HttpResponseSpec::new(404, "Not Found", b"missing")
                }
            }
            _ => HttpResponseSpec::new(405, "Method Not Allowed", b""),
        }
    });

    let dav_url = server.base_url("dav");
    let dav =
        WebDavRemote::new(&dav_url, Some("alice"), Some("dav-secret-999"), None, false).unwrap();
    let probe = dav.probe().unwrap();
    assert_eq!(probe.backend, "webdav");
    assert_eq!(probe.detail, dav_url);

    // 401 error redaction check.
    let bad_dav = WebDavRemote::new(
        &dav_url,
        Some("alice"),
        Some("wrong-password-secret"),
        None,
        false,
    )
    .unwrap();
    let err_msg = format!("{}", bad_dav.probe().unwrap_err());
    assert!(err_msg.contains("401"), "unexpected error: {err_msg}");
    assert!(!err_msg.contains("wrong-password-secret"));
    assert!(!err_msg.contains(&base64_encode(b"alice:wrong-password-secret")));

    // CRUD + 404 handling.
    assert_eq!(dav.get("does/not/exist.txt").unwrap(), None);
    dav.put("tmp/dir/hello.txt", b"world", "text/plain")
        .unwrap();
    assert_eq!(
        dav.get("tmp/dir/hello.txt").unwrap().as_deref(),
        Some(b"world".as_slice())
    );
    assert_eq!(
        dav.list("tmp").unwrap(),
        vec!["tmp/dir/hello.txt".to_string()]
    );
    dav.delete("tmp/dir/hello.txt").unwrap();
    dav.delete("tmp/dir/hello.txt").unwrap();
    assert_eq!(dav.get("tmp/dir/hello.txt").unwrap(), None);

    // Verify PROPFIND 405 -> OPTIONS fallback.
    let fallback_srv = MockHttpServer::start(|req| match req.method.as_str() {
        "PROPFIND" => HttpResponseSpec::new(405, "Method Not Allowed", b""),
        "OPTIONS" => HttpResponseSpec::new(200, "OK", b""),
        _ => HttpResponseSpec::new(404, "Not Found", b""),
    });
    let fallback_dav = WebDavRemote::new(
        &fallback_srv.base_url("dav"),
        None,
        None,
        Some("tok"),
        false,
    )
    .unwrap();
    assert_eq!(fallback_dav.probe().unwrap().backend, "webdav");

    // Full two-machine push/pull over WebDAV via build_remote.
    seed_machine_a_and_sync_roundtrip(RemoteConfig::WebDav {
        url: dav_url,
        username: Some("alice".to_string()),
        password: Some("dav-secret-999".to_string()),
        token: None,
        allow_insecure_http: false,
    });

    // Password leading/trailing whitespace must be preserved consistently across SyncConfig::normalize()
    // and WebDavRemote Basic auth header encoding.
    let spaced_pw = "  dav secret with spaces  ";
    let expected_auth = format!(
        "Basic {}",
        base64_encode(format!("alice:{spaced_pw}").as_bytes())
    );
    let seen_auth: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let seen_auth_srv = Arc::clone(&seen_auth);
    let pw_srv = MockHttpServer::start(move |req| {
        *seen_auth_srv.lock().unwrap() = req.headers.get("authorization").cloned();
        if req.method == "PROPFIND" {
            return HttpResponseSpec::new(
                207,
                "Multi-Status",
                b"<D:multistatus xmlns:D=\"DAV:\"/>".to_vec(),
            );
        }
        HttpResponseSpec::new(404, "Not Found", b"")
    });

    let mut spaced_cfg = SyncConfig {
        enabled: true,
        remote: RemoteConfig::WebDav {
            url: pw_srv.base_url("dav"),
            username: Some("  alice  ".to_string()),
            password: Some(spaced_pw.to_string()),
            token: None,
            allow_insecure_http: false,
        },
        ..Default::default()
    };
    spaced_cfg.normalize();
    match &spaced_cfg.remote {
        RemoteConfig::WebDav {
            username, password, ..
        } => {
            assert_eq!(username.as_deref(), Some("alice"));
            assert_eq!(password.as_deref(), Some(spaced_pw));
        }
        other => panic!("unexpected remote: {other:?}"),
    }
    let spaced_remote = build_remote(&spaced_cfg).unwrap();
    spaced_remote.probe().unwrap();
    assert_eq!(
        seen_auth.lock().unwrap().as_deref(),
        Some(expected_auth.as_str())
    );
}

fn uri_encode_path_for_test(key: &str) -> String {
    key.split('/')
        .map(uri_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// 3. S3 Mock Server End-to-End, ContinuationToken Pagination & Two-Machine Sync
// ---------------------------------------------------------------------------

#[test]
fn t13_s3_mock_e2e_pagination_and_two_machine_sync() {
    let ak = "AKIA_TEST_KEY";
    let sk = "s3-super-secret-key-123";
    let bucket = "pulse-sync-bucket";
    let region = "auto";
    let objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let list_calls = Arc::new(AtomicUsize::new(0));
    let obj_srv = Arc::clone(&objects);
    let list_calls_srv = Arc::clone(&list_calls);

    let server = MockHttpServer::start(move |req| {
        let Some(auth) = req.headers.get("authorization") else {
            return HttpResponseSpec::new(403, "Forbidden", b"missing auth");
        };
        if !auth.starts_with(&format!("AWS4-HMAC-SHA256 Credential={ak}/")) {
            return HttpResponseSpec::new(403, "Forbidden", b"bad credential");
        }
        let Some(amz_date) = req.headers.get("x-amz-date") else {
            return HttpResponseSpec::new(403, "Forbidden", b"missing x-amz-date");
        };
        let Some(payload_sha) = req.headers.get("x-amz-content-sha256") else {
            return HttpResponseSpec::new(403, "Forbidden", b"missing payload hash");
        };
        let actual_sha = hex::encode(sha2::Sha256::digest(&req.body));
        if payload_sha != &actual_sha {
            return HttpResponseSpec::new(403, "Forbidden", b"payload sha256 mismatch");
        }

        let (raw_path, params) = split_target(&req.target);
        let decoded_path = percent_decode(raw_path);
        let bucket_prefix = format!("/{bucket}");

        if (decoded_path == bucket_prefix || decoded_path == format!("{bucket_prefix}/"))
            && req.method == "GET"
            && params.get("list-type").map(String::as_str) == Some("2")
        {
            list_calls_srv.fetch_add(1, Ordering::Relaxed);
            let prefix = params.get("prefix").map(String::as_str).unwrap_or("");
            let max_keys = params
                .get("max-keys")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(2);
            let start_idx = params
                .get("continuation-token")
                .and_then(|s| s.strip_prefix("tok-"))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);

            let guard = obj_srv.lock().unwrap();
            let matching: Vec<String> = guard
                .keys()
                .filter(|k| prefix.is_empty() || k.starts_with(prefix))
                .cloned()
                .collect();

            let end_idx = (start_idx + max_keys).min(matching.len());
            let slice = &matching[start_idx.min(matching.len())..end_idx];
            let is_truncated = end_idx < matching.len();

            let mut items_xml = String::new();
            for k in slice {
                items_xml.push_str(&format!(
                    "<Contents><Key>{}</Key></Contents>",
                    xml_escape(k)
                ));
            }
            let next_tok_xml = if is_truncated {
                format!("<NextContinuationToken>tok-{end_idx}</NextContinuationToken>")
            } else {
                String::new()
            };
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <IsTruncated>{is_truncated}</IsTruncated>{next_tok_xml}{items_xml}\
                     </ListBucketResult>"
            );
            return HttpResponseSpec::new(200, "OK", xml);
        }

        let Some(key_part) = decoded_path.strip_prefix(&format!("{bucket_prefix}/")) else {
            return HttpResponseSpec::new(404, "Not Found", b"no such bucket");
        };

        // Verify signature against sign_aws_v4_headers when host header is present.
        if let Some(host) = req.headers.get("host") {
            let query_vec: Vec<(&str, &str)> = params
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let mut extra: Vec<(&str, &str)> = Vec::new();
            if let Some(ct) = req.headers.get("content-type") {
                extra.push(("content-type", ct.as_str()));
            }
            let expected = sign_aws_v4_headers(
                &req.method,
                raw_path,
                &query_vec,
                host,
                &extra,
                &req.body,
                region,
                "s3",
                ak,
                sk,
                amz_date,
            )
            .unwrap();
            if auth != &expected.authorization {
                return HttpResponseSpec::new(403, "Forbidden", b"signature mismatch");
            }
        }

        let mut guard = obj_srv.lock().unwrap();
        match req.method.as_str() {
            "PUT" => {
                guard.insert(key_part.to_string(), req.body.clone());
                HttpResponseSpec::new(200, "OK", b"")
            }
            "GET" => match guard.get(key_part) {
                Some(bytes) => HttpResponseSpec::new(200, "OK", bytes.clone()),
                None => HttpResponseSpec::new(404, "Not Found", b""),
            },
            "DELETE" => {
                guard.remove(key_part);
                HttpResponseSpec::new(204, "No Content", b"")
            }
            _ => HttpResponseSpec::new(405, "Method Not Allowed", b""),
        }
    });

    let endpoint = server.base_url("");
    let s3 = S3Remote::new(&endpoint, region, bucket, Some(ak), Some(sk), true, false).unwrap();
    let probe = s3.probe().unwrap();
    assert_eq!(probe.backend, "s3");

    // 403 error redaction check.
    let bad_s3 = S3Remote::new(
        &endpoint,
        region,
        bucket,
        Some("WRONG_ACCESS"),
        Some("WRONG_SECRET_KEY"),
        true,
        false,
    )
    .unwrap();
    let err_msg = format!("{}", bad_s3.probe().unwrap_err());
    assert!(err_msg.contains("403"), "unexpected error: {err_msg}");
    assert!(!err_msg.contains("WRONG_ACCESS"));
    assert!(!err_msg.contains("WRONG_SECRET_KEY"));

    // Multi-page ListObjectsV2 pagination (5 keys, 2 per page -> 3 pages).
    for idx in 1..=5 {
        s3.put(
            &format!("page-test/item-{idx}.json"),
            format!("payload-{idx}").as_bytes(),
            "application/json",
        )
        .unwrap();
    }
    list_calls.store(0, Ordering::Relaxed);
    let listed = s3.list("page-test").unwrap();
    assert_eq!(listed.len(), 5);
    assert_eq!(list_calls.load(Ordering::Relaxed), 3);
    assert_eq!(s3.get("page-test/missing.json").unwrap(), None);
    s3.delete("page-test/item-5.json").unwrap();
    s3.delete("page-test/item-5.json").unwrap();
    assert_eq!(s3.list("page-test").unwrap().len(), 4);

    // Full two-machine push/pull over S3 via build_remote.
    seed_machine_a_and_sync_roundtrip(RemoteConfig::S3 {
        endpoint,
        region: region.to_string(),
        bucket: bucket.to_string(),
        access_key: Some(ak.to_string()),
        secret_key: Some(sk.to_string()),
        path_style: true,
        allow_insecure_http: false,
    });
}

#[test]
fn t13_s3_list_xml_hardening_comments_cdata_contents_scope_and_prefix_boundary() {
    let seen_prefixes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_prefixes_srv = Arc::clone(&seen_prefixes);

    let server = MockHttpServer::start(move |req| {
        let (raw_path, params) = split_target(&req.target);
        let decoded_path = percent_decode(raw_path);
        if (decoded_path == "/my-bucket" || decoded_path == "/my-bucket/")
            && req.method == "GET"
            && params.get("list-type").map(String::as_str) == Some("2")
        {
            if let Some(pfx) = params.get("prefix") {
                seen_prefixes_srv.lock().unwrap().push(pfx.clone());
            }
            let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <!-- <Contents><Key>team/alpha/commented-out.json</Key></Contents> -->
  <!-- comment with > angle bracket <Key>team/alpha/angle-comment.json</Key> -->
  <IsTruncated><!-- false -->false</IsTruncated>
  <CommonPrefixes><Prefix>team/alpha/sub/</Prefix><Key>team/alpha/outside-contents.json</Key></CommonPrefixes>
  <Metadata><Key>team/alpha/metadata-key.json</Key></Metadata>
  <Contents>
    <!-- <Key>team/alpha/decoy-inside-contents.json</Key> -->
    <Key>team/alpha/<!-- inline -->normal&amp;1.json</Key>
  </Contents>
  <Contents>
    <Key><![CDATA[team/alpha/cdata&amp;sub</Key>2.json]]></Key>
  </Contents>
</ListBucketResult>"#;
            return HttpResponseSpec::new(200, "OK", xml);
        }
        HttpResponseSpec::new(404, "Not Found", b"not found")
    });

    let s3 = S3Remote::new(
        &server.base_url(""),
        "auto",
        "my-bucket",
        Some("AKIA_TEST"),
        Some("SECRET_TEST"),
        true,
        false,
    )
    .unwrap();

    let keys = s3.list("team/alpha").unwrap();
    assert_eq!(
        seen_prefixes.lock().unwrap().as_slice(),
        &["team/alpha/".to_string()],
        "S3 ListObjectsV2 must send trailing '/' directory boundary in prefix query param"
    );
    assert_eq!(
        keys,
        vec![
            "team/alpha/cdata&amp;sub</Key>2.json".to_string(),
            "team/alpha/normal&1.json".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// 4. Cloudflare Worker Mock Server End-to-End, Cursor Pagination & Two-Machine Sync
// ---------------------------------------------------------------------------

#[test]
fn t13_cf_worker_mock_e2e_and_two_machine_sync() {
    type CfStore = Arc<Mutex<BTreeMap<String, (Vec<u8>, String)>>>;
    let token = "cf-secret-token-777";
    let expected_auth = format!("Bearer {token}");
    let store: CfStore = Arc::new(Mutex::new(BTreeMap::new()));
    let list_calls = Arc::new(AtomicUsize::new(0));
    let store_srv = Arc::clone(&store);
    let list_calls_srv = Arc::clone(&list_calls);

    let server = MockHttpServer::start(move |req| {
        if req.headers.get("authorization").map(String::as_str) != Some(expected_auth.as_str()) {
            return HttpResponseSpec::new(
                401,
                "Unauthorized",
                br#"{"error":"unauthorized"}"#.to_vec(),
            );
        }

        let (raw_path, params) = split_target(&req.target);
        if req.method == "GET" && raw_path == "/v1/health" {
            return HttpResponseSpec::new(200, "OK", br#"{"ok":true,"storage":"r2"}"#.to_vec())
                .with_header("Content-Type", "application/json");
        }

        if req.method == "GET" && raw_path == "/v1/objects" {
            list_calls_srv.fetch_add(1, Ordering::Relaxed);
            let prefix = params.get("prefix").map(String::as_str).unwrap_or("");
            let start_idx = params
                .get("cursor")
                .and_then(|s| s.strip_prefix("cur-"))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let guard = store_srv.lock().unwrap();
            let matching: Vec<String> = guard
                .keys()
                .filter(|k| prefix.is_empty() || k.starts_with(prefix))
                .cloned()
                .collect();
            let page_size = 2usize;
            let end_idx = (start_idx + page_size).min(matching.len());
            let page_keys = &matching[start_idx.min(matching.len())..end_idx];
            let next_cursor = if end_idx < matching.len() {
                serde_json::Value::String(format!("cur-{end_idx}"))
            } else {
                serde_json::Value::Null
            };
            let body = serde_json::to_vec(&serde_json::json!({
                "keys": page_keys,
                "cursor": next_cursor
            }))
            .unwrap();
            return HttpResponseSpec::new(200, "OK", body)
                .with_header("Content-Type", "application/json");
        }

        let Some(raw_key) = raw_path.strip_prefix("/v1/objects/") else {
            return HttpResponseSpec::new(404, "Not Found", b"not found");
        };
        let key = percent_decode(raw_key);

        let mut guard = store_srv.lock().unwrap();
        match req.method.as_str() {
            "GET" => match guard.get(&key) {
                Some((bytes, ct)) => {
                    HttpResponseSpec::new(200, "OK", bytes.clone()).with_header("Content-Type", ct)
                }
                None => {
                    HttpResponseSpec::new(404, "Not Found", br#"{"error":"not_found"}"#.to_vec())
                }
            },
            "PUT" => {
                let ct = req
                    .headers
                    .get("content-type")
                    .cloned()
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                guard.insert(key, (req.body.clone(), ct));
                HttpResponseSpec::new(204, "No Content", b"")
            }
            "DELETE" => {
                guard.remove(&key);
                HttpResponseSpec::new(204, "No Content", b"")
            }
            _ => HttpResponseSpec::new(405, "Method Not Allowed", b""),
        }
    });

    let url = server.base_url("");
    let cf = CfWorkerRemote::new(&url, Some(token), false).unwrap();
    let probe = cf.probe().unwrap();
    assert_eq!(probe.backend, "cf-worker");
    assert!(probe.detail.contains("(r2)"), "detail: {}", probe.detail);

    // 401 error redaction check.
    let bad_cf = CfWorkerRemote::new(&url, Some("bad-cf-secret-token"), false).unwrap();
    let err_msg = format!("{}", bad_cf.probe().unwrap_err());
    assert!(err_msg.contains("401"), "unexpected error: {err_msg}");
    assert!(!err_msg.contains("bad-cf-secret-token"));

    // Multi-page cursor pagination (5 keys, 2 per page -> 3 calls).
    for idx in 1..=5 {
        cf.put(
            &format!("cf-page/obj-{idx}.bin"),
            format!("bin-{idx}").as_bytes(),
            "application/octet-stream",
        )
        .unwrap();
    }
    list_calls.store(0, Ordering::Relaxed);
    let keys = cf.list("cf-page").unwrap();
    assert_eq!(keys.len(), 5);
    assert_eq!(list_calls.load(Ordering::Relaxed), 3);
    assert_eq!(cf.get("cf-page/nope.bin").unwrap(), None);
    cf.delete("cf-page/obj-5.bin").unwrap();
    cf.delete("cf-page/obj-5.bin").unwrap();
    assert_eq!(cf.list("cf-page").unwrap().len(), 4);

    // Full two-machine push/pull over CF Worker via build_remote.
    seed_machine_a_and_sync_roundtrip(RemoteConfig::CfWorker {
        url,
        token: Some(token.to_string()),
        allow_insecure_http: false,
    });
}

#[test]
fn t13_cf_worker_truncated_without_cursor_subpath_and_413_errors() {
    let token = "cf-proto-token-123";
    let expected_auth = format!("Bearer {token}");

    let server = MockHttpServer::start(move |req| {
        if req.headers.get("authorization").map(String::as_str) != Some(expected_auth.as_str()) {
            return HttpResponseSpec::new(
                401,
                "Unauthorized",
                br#"{"error":"unauthorized"}"#.to_vec(),
            );
        }
        let (raw_path, params) = split_target(&req.target);
        if req.method == "GET" && raw_path == "/pulse/v1/health" {
            return HttpResponseSpec::new(200, "OK", br#"{"ok":true,"storage":"kv"}"#.to_vec())
                .with_header("Content-Type", "application/json");
        }
        if req.method == "GET" && raw_path == "/pulse/v1/objects" {
            let prefix = params.get("prefix").map(String::as_str).unwrap_or("");
            if prefix == "broken-trunc" {
                let body = br#"{"keys":["broken-trunc/a.json"],"cursor":null,"truncated":true}"#;
                return HttpResponseSpec::new(200, "OK", body.to_vec())
                    .with_header("Content-Type", "application/json");
            }
            if prefix == "false-trunc" {
                let body = br#"{"keys":["false-trunc/a.json"],"cursor":"ignored-cursor","truncated":false}"#;
                return HttpResponseSpec::new(200, "OK", body.to_vec())
                    .with_header("Content-Type", "application/json");
            }
            let body = br#"{"keys":[],"cursor":null,"truncated":false}"#;
            return HttpResponseSpec::new(200, "OK", body.to_vec())
                .with_header("Content-Type", "application/json");
        }
        if req.method == "PUT" && raw_path == "/pulse/v1/objects/oversized.bin" {
            return HttpResponseSpec::new(
                413,
                "Payload Too Large",
                br#"{"error":"payload too large for KV storage (max 25 MiB)"}"#.to_vec(),
            )
            .with_header("Content-Type", "application/json");
        }
        HttpResponseSpec::new(404, "Not Found", b"not found")
    });

    let sub_url = server.base_url("pulse");
    let cf = CfWorkerRemote::new(&sub_url, Some(token), false).unwrap();

    // Subpath health check works.
    let probe = cf.probe().unwrap();
    assert!(probe.detail.contains("(kv)"), "detail: {}", probe.detail);

    // truncated=true with null/empty cursor MUST return SyncError::Protocol.
    let trunc_err = cf.list("broken-trunc").unwrap_err();
    assert!(
        matches!(trunc_err, SyncError::Protocol(_)),
        "expected SyncError::Protocol for truncated=true without cursor, got: {trunc_err:?}"
    );

    // truncated=false terminates even if a leftover cursor string is present.
    let false_trunc_keys = cf.list("false-trunc").unwrap();
    assert_eq!(false_trunc_keys, vec!["false-trunc/a.json".to_string()]);

    // HTTP 413 maps to a clear readable error mentioning 413 and size limit, without leaking token.
    let err_413 = cf
        .put("oversized.bin", b"data", "application/octet-stream")
        .unwrap_err();
    let err_413_msg = format!("{err_413}");
    assert!(
        err_413_msg.contains("413"),
        "unexpected 413 err: {err_413_msg}"
    );
    assert!(
        err_413_msg.to_ascii_lowercase().contains("too large") || err_413_msg.contains("25 MiB"),
        "413 error should clearly explain payload size limit: {err_413_msg}"
    );
    assert!(!err_413_msg.contains(token));
}

// ---------------------------------------------------------------------------
// 5. HTTP Security Boundaries, Redirect Guard & 32 MiB Body Cap
// ---------------------------------------------------------------------------

#[test]
fn t13_http_security_boundaries_redirect_and_size_cap() {
    // Non-loopback http:// rejected by default, allowed when allow_insecure_http = true.
    let err =
        validate_remote_url("http://user:topsecret@public.example.com/dav", false).unwrap_err();
    let err_str = format!("{err}");
    assert!(
        !err_str.contains("topsecret"),
        "error leaked secret: {err_str}"
    );
    assert!(
        !err_str.contains("user"),
        "error leaked username: {err_str}"
    );
    assert!(
        validate_remote_url("http://public.example.com/dav", true).is_ok(),
        "explicit allow_insecure_http=true should permit http://"
    );

    // Loopback http:// allowed without allow_insecure_http.
    for loopback in [
        "http://127.0.0.1:8080/dav",
        "http://localhost:8080/dav",
        "http://[::1]:8080/dav",
        "https://public.example.com/dav",
    ] {
        assert!(
            validate_remote_url(loopback, false).is_ok(),
            "expected allowed URL: {loopback}"
        );
    }

    // Disallowed schemes, missing host, whitespace, control characters.
    for bad in [
        "",
        "ftp://example.com/dav",
        "https://",
        "https:/missing-host",
        "https:///missing-host",
        "http://:8080/dav",
        "https://exa mple.com/dav",
        "https://example.com/\ndav",
    ] {
        assert!(
            validate_remote_url(bad, true).is_err(),
            "expected error for invalid URL: {bad:?}"
        );
    }

    // Redirects must NEVER be followed automatically.
    let trap_hits = Arc::new(AtomicUsize::new(0));
    let trap_hits_srv = Arc::clone(&trap_hits);
    let trap_srv = MockHttpServer::start(move |_| {
        trap_hits_srv.fetch_add(1, Ordering::Relaxed);
        HttpResponseSpec::new(200, "OK", b"should-never-be-fetched")
    });
    let trap_target = trap_srv.base_url("v1/objects/secret");

    let redir_srv = MockHttpServer::start(move |_| {
        HttpResponseSpec::new(302, "Found", b"").with_header("Location", trap_target.clone())
    });
    let cf_redir = CfWorkerRemote::new(&redir_srv.base_url(""), Some("tok"), false).unwrap();
    assert!(cf_redir.get("k").is_err());
    assert_eq!(trap_hits.load(Ordering::Relaxed), 0);

    // Response body > 32 MiB must fail with SyncError::Remote.
    let big_srv = MockHttpServer::start(|req| {
        if req.target == "/v1/objects/too-big" {
            let oversized = vec![b'x'; MAX_REMOTE_OBJECT_BYTES + 1];
            HttpResponseSpec::new(200, "OK", oversized)
        } else {
            HttpResponseSpec::new(404, "Not Found", b"")
        }
    });
    let cf_big = CfWorkerRemote::new(&big_srv.base_url(""), Some("tok"), false).unwrap();
    let big_err = cf_big.get("too-big").unwrap_err();
    assert!(
        matches!(big_err, SyncError::Remote(_)),
        "expected SyncError::Remote for >32MiB payload, got: {big_err:?}"
    );
}
