use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;
pub(super) const REQUEST_READ_TOTAL_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub(super) struct HttpRequest {
    pub method: String,
    pub pathname: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub(super) enum HttpParseError {
    EmptyOrEof,
    PayloadTooLarge,
    InvalidRequest(&'static str),
    TimedOut,
    Io,
}

impl From<io::Error> for HttpParseError {
    fn from(_err: io::Error) -> Self {
        Self::Io
    }
}

fn read_with_deadline(
    stream: &mut TcpStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<usize, HttpParseError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HttpParseError::TimedOut);
        }
        let _ = stream.set_read_timeout(Some(remaining));
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err)
                if err.kind() == io::ErrorKind::TimedOut
                    || err.kind() == io::ErrorKind::WouldBlock =>
            {
                return Err(HttpParseError::TimedOut);
            }
            Err(_) => return Err(HttpParseError::Io),
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((pos, 4));
    }
    if let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
        return Some((pos, 2));
    }
    None
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpParseError> {
    read_http_request_with_deadline(stream, REQUEST_READ_TOTAL_DEADLINE)
}

pub(super) fn read_http_request_with_deadline(
    stream: &mut TcpStream,
    total_deadline: Duration,
) -> Result<HttpRequest, HttpParseError> {
    let deadline = Instant::now() + total_deadline;
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];

    let (header_end, sep_len) = loop {
        if let Some(found) = find_header_end(&buf) {
            break found;
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return Err(HttpParseError::InvalidRequest("request headers too large"));
        }
        let n = read_with_deadline(stream, &mut tmp, deadline)?;
        if n == 0 {
            if buf.is_empty() {
                return Err(HttpParseError::EmptyOrEof);
            }
            return Err(HttpParseError::InvalidRequest("unexpected EOF in headers"));
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head_bytes = &buf[..header_end];
    let head_str = std::str::from_utf8(head_bytes)
        .map_err(|_| HttpParseError::InvalidRequest("non-UTF8 request headers"))?;
    let mut lines = head_str.lines();
    let request_line = lines
        .next()
        .ok_or(HttpParseError::InvalidRequest("missing request line"))?;

    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(HttpParseError::InvalidRequest("missing HTTP method"))?
        .to_ascii_uppercase();
    let raw_target = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(HttpParseError::InvalidRequest("missing request target"))?;
    let _version = parts
        .next()
        .filter(|s| s.starts_with("HTTP/"))
        .ok_or(HttpParseError::InvalidRequest("missing HTTP version"))?;

    let (raw_path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p, q.to_string()),
        None => (raw_target, String::new()),
    };
    let pathname = percent_decode_str(raw_path);

    let mut headers = Vec::new();
    let mut content_length: Option<usize> = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "transfer-encoding" {
                return Err(HttpParseError::InvalidRequest(
                    "unsupported Transfer-Encoding",
                ));
            }
            if key == "content-length" {
                if content_length.is_some() {
                    return Err(HttpParseError::InvalidRequest("duplicate Content-Length"));
                }
                if val.is_empty() || !val.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(HttpParseError::InvalidRequest("invalid Content-Length"));
                }
                let len = val
                    .parse::<usize>()
                    .map_err(|_| HttpParseError::InvalidRequest("invalid Content-Length"))?;
                content_length = Some(len);
            }
            headers.push((key, val));
        }
    }
    let content_length = content_length.unwrap_or(0);

    if content_length > MAX_BODY_BYTES {
        return Err(HttpParseError::PayloadTooLarge);
    }

    let mut body = Vec::with_capacity(content_length);
    if content_length > 0 {
        let already_slice = &buf[header_end + sep_len..];
        let copy_len = already_slice.len().min(content_length);
        body.extend_from_slice(&already_slice[..copy_len]);

        while body.len() < content_length {
            let to_read = (content_length - body.len()).min(tmp.len());
            let n = read_with_deadline(stream, &mut tmp[..to_read], deadline)?;
            if n == 0 {
                return Err(HttpParseError::InvalidRequest("incomplete request body"));
            }
            body.extend_from_slice(&tmp[..n]);
        }
    }

    Ok(HttpRequest {
        method,
        pathname,
        query,
        headers,
        body,
    })
}

pub(super) fn percent_decode_str(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub(super) fn parse_query_params(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((k, v)) => out.push((percent_decode_str(k), percent_decode_str(v))),
            None => out.push((percent_decode_str(pair), String::new())),
        }
    }
    out
}

pub(super) fn query_param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

pub(super) fn is_allowed_host(host_header: Option<&str>, actual_port: u16) -> bool {
    let Some(raw) = host_header else {
        return false;
    };
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized == format!("127.0.0.1:{actual_port}")
        || normalized == format!("localhost:{actual_port}")
    {
        return true;
    }
    if actual_port == 80 && (normalized == "127.0.0.1" || normalized == "localhost") {
        return true;
    }
    false
}

pub(super) fn is_allowed_origin(origin_header: Option<&str>, actual_port: u16) -> bool {
    let Some(raw) = origin_header else {
        return true;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = match rest.split_once('/') {
        Some((auth, "")) => auth,
        Some(_) => return false,
        None => rest,
    };
    if authority.contains(['@', '?', '#']) {
        return false;
    }
    if authority == format!("127.0.0.1:{actual_port}")
        || authority == format!("localhost:{actual_port}")
    {
        return true;
    }
    if actual_port == 80
        && (authority == "127.0.0.1"
            || authority == "localhost"
            || authority == "127.0.0.1:80"
            || authority == "localhost:80")
    {
        return true;
    }
    false
}

/// Enforce strict same-origin constraints for browser-initiated mutating/config requests.
pub(super) fn is_strict_same_origin(req: &HttpRequest, actual_port: u16) -> bool {
    if let Some(sec_fetch_site) = req.header("sec-fetch-site")
        && !sec_fetch_site.trim().eq_ignore_ascii_case("same-origin")
    {
        return false;
    }
    if let Some(origin) = req.header("origin")
        && !origin.trim().is_empty()
        && !is_allowed_origin(Some(origin), actual_port)
    {
        return false;
    }
    true
}

/// Enforce strict same-origin with a mandatory valid loopback `Origin` header.
pub(super) fn is_strict_same_origin_with_required_origin(
    req: &HttpRequest,
    actual_port: u16,
) -> bool {
    let Some(origin) = req
        .header("origin")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    is_allowed_origin(Some(origin), actual_port) && is_strict_same_origin(req, actual_port)
}

/// Constant-time check for the `X-Agy-Pulse-Token` CSRF request header.
pub(super) fn has_valid_csrf_token(req: &HttpRequest, expected_token: &str) -> bool {
    req.header("x-agy-pulse-token")
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected_token.as_bytes()))
}

/// Constant-time byte slice comparison (returns `false` immediately on length mismatch).
pub(super) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Check that the `Content-Type` header media type (before `;`) equals `application/json`.
pub(super) fn is_json_content_type(req: &HttpRequest) -> bool {
    let Some(ct) = req.header("content-type") else {
        return false;
    };
    let media_type = ct.split(';').next().unwrap_or("").trim();
    media_type.eq_ignore_ascii_case("application/json")
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

pub(super) fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    send_body: bool,
    extra_headers: &[(&str, &str)],
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Cache-Control: no-store\r\n\
         X-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n",
        reason = reason_phrase(status),
        len = body.len(),
    );
    for (k, v) in extra_headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");

    if send_body && !body.is_empty() && body.len() <= MAX_BODY_BYTES {
        let mut frame = Vec::with_capacity(head.len() + body.len());
        frame.extend_from_slice(head.as_bytes());
        frame.extend_from_slice(body);
        stream.write_all(&frame)?;
    } else {
        stream.write_all(head.as_bytes())?;
        if send_body && !body.is_empty() {
            stream.write_all(body)?;
        }
    }
    stream.flush()
}

/// Gracefully half-close and boundedly drain an early-rejected HTTP connection
/// (RFC 9112 §9.6) so closing the socket does not send a TCP RST that truncates
/// the client's unread error response.
pub(super) fn finish_early_error_response(stream: &mut TcpStream) {
    const MAX_DRAIN_BYTES: usize = 256 * 1024;

    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));

    let deadline = Instant::now() + Duration::from_millis(150);
    let mut discard = [0u8; 4096];
    let mut drained = 0usize;

    while drained < MAX_DRAIN_BYTES && Instant::now() < deadline {
        match stream.read(&mut discard) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained = drained.saturating_add(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn send_on_loopback(payload: Vec<u8>) -> Result<HttpRequest, HttpParseError> {
        let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
            panic!("bind loopback failed");
        };
        let Ok(addr) = listener.local_addr() else {
            panic!("local_addr failed");
        };
        let client_handle = thread::spawn(move || {
            if let Ok(mut client) = TcpStream::connect(addr) {
                let _ = client.write_all(&payload);
                let _ = client.flush();
            }
        });
        let Ok((mut server_stream, _)) = listener.accept() else {
            panic!("accept failed");
        };
        let res = read_http_request(&mut server_stream);
        let _ = client_handle.join();
        res
    }

    #[test]
    fn http_wire_preserves_body_and_handles_payload_limits() {
        let Ok(req) = send_on_loopback(
            b"POST /api/config/pulse HTTP/1.1\r\nHost: 127.0.0.1:3789\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: 14\r\n\r\n{\"force\":true}".to_vec(),
        ) else {
            panic!("expected valid body request");
        };
        assert_eq!(req.method, "POST");
        assert_eq!(req.pathname, "/api/config/pulse");
        assert_eq!(req.body, br#"{"force":true}"#);
        assert!(is_json_content_type(&req));

        let too_large = format!(
            "POST /api/config/pulse HTTP/1.1\r\nHost: 127.0.0.1:3789\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let err = send_on_loopback(too_large.into_bytes());
        assert!(matches!(err, Err(HttpParseError::PayloadTooLarge)));

        let eof_err = send_on_loopback(
            b"POST /api/config/pulse HTTP/1.1\r\nHost: 127.0.0.1:3789\r\nContent-Length: 20\r\n\r\n{\"a\":1}".to_vec(),
        );
        assert!(matches!(
            eof_err,
            Err(HttpParseError::InvalidRequest("incomplete request body"))
        ));
    }

    #[test]
    fn http_wire_same_origin_constant_time_and_status_phrases() {
        assert!(constant_time_eq(b"abcdef01", b"abcdef01"));
        assert!(!constant_time_eq(b"abcdef01", b"abcdef02"));
        assert!(!constant_time_eq(b"abc", b"abcd"));

        let req_ok = HttpRequest {
            method: "PUT".to_string(),
            pathname: "/api/config/pulse".to_string(),
            query: String::new(),
            headers: vec![
                ("sec-fetch-site".to_string(), "Same-Origin".to_string()),
                ("origin".to_string(), "http://127.0.0.1:3789".to_string()),
                (
                    "content-type".to_string(),
                    "Application/JSON ; charset=utf-8".to_string(),
                ),
            ],
            body: Vec::new(),
        };
        assert!(is_strict_same_origin(&req_ok, 3789));
        assert!(is_json_content_type(&req_ok));

        let req_cross_site = HttpRequest {
            headers: vec![("sec-fetch-site".to_string(), "cross-site".to_string())],
            ..req_ok.clone()
        };
        assert!(!is_strict_same_origin(&req_cross_site, 3789));

        let req_bad_origin = HttpRequest {
            headers: vec![("origin".to_string(), "https://evil.example".to_string())],
            ..req_ok.clone()
        };
        assert!(!is_strict_same_origin(&req_bad_origin, 3789));

        let req_bad_ct = HttpRequest {
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            ..req_ok.clone()
        };
        assert!(!is_json_content_type(&req_bad_ct));

        assert!(is_strict_same_origin_with_required_origin(&req_ok, 3789));
        let req_missing_origin = HttpRequest {
            headers: vec![("sec-fetch-site".to_string(), "same-origin".to_string())],
            ..req_ok.clone()
        };
        assert!(is_strict_same_origin(&req_missing_origin, 3789));
        assert!(!is_strict_same_origin_with_required_origin(
            &req_missing_origin,
            3789
        ));

        let req_csrf = HttpRequest {
            headers: vec![("x-agy-pulse-token".to_string(), "secret-token".to_string())],
            ..req_ok
        };
        assert!(has_valid_csrf_token(&req_csrf, "secret-token"));
        assert!(!has_valid_csrf_token(&req_csrf, "wrong-token"));

        assert_eq!(reason_phrase(409), "Conflict");
        assert_eq!(reason_phrase(413), "Payload Too Large");
        assert_eq!(reason_phrase(422), "Unprocessable Entity");
        assert_eq!(reason_phrase(503), "Service Unavailable");
    }

    #[test]
    fn http_wire_early_413_coalesced_and_graceful_close_delivers_full_body() {
        let oversized_body = vec![b'x'; 65 * 1024];
        let expected_body = br#"{"error":"request body too large"}"#;

        for iter in 0..20 {
            let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
                panic!("iter {iter}: bind loopback failed");
            };
            let Ok(addr) = listener.local_addr() else {
                panic!("iter {iter}: local_addr failed");
            };

            let server_handle = thread::spawn(move || {
                let Ok((mut server_stream, _)) = listener.accept() else {
                    panic!("iter {iter}: accept failed");
                };
                let parse_res = read_http_request(&mut server_stream);
                assert!(
                    matches!(parse_res, Err(HttpParseError::PayloadTooLarge)),
                    "iter {iter}: expected PayloadTooLarge, got {parse_res:?}"
                );
                let write_res = write_http_response(
                    &mut server_stream,
                    413,
                    "application/json; charset=utf-8",
                    expected_body,
                    true,
                    &[],
                );
                assert!(write_res.is_ok(), "iter {iter}: write_http_response failed");
                finish_early_error_response(&mut server_stream);
            });

            let Ok(mut client) = TcpStream::connect(addr) else {
                panic!("iter {iter}: connect failed");
            };
            let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = client.set_write_timeout(Some(Duration::from_secs(5)));

            let req_head = format!(
                "POST /api/sync-remote HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                addr.port(),
                oversized_body.len()
            );
            let _ = client.write_all(req_head.as_bytes());
            let _ = client.write_all(&oversized_body);
            let _ = client.flush();

            let mut raw = Vec::new();
            if let Err(err) = client.read_to_end(&mut raw) {
                panic!("iter {iter}: client read_to_end failed without clean EOF: {err}");
            }
            drop(client);

            let text = String::from_utf8_lossy(&raw);
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                panic!("iter {iter}: missing HTTP header/body separator in {text:?}");
            };
            let Some(status_line) = head.lines().next() else {
                panic!("iter {iter}: missing status line in {head:?}");
            };
            assert!(
                status_line.starts_with("HTTP/1.1 413"),
                "iter {iter}: unexpected status line {status_line:?}"
            );
            assert_eq!(
                body.as_bytes(),
                expected_body,
                "iter {iter}: truncated or mismatched 413 JSON body"
            );

            assert!(
                server_handle.join().is_ok(),
                "iter {iter}: server thread panicked"
            );
        }
    }

    #[test]
    fn http_wire_rejects_malformed_duplicate_content_length_and_transfer_encoding() {
        for bad_cl in ["abc", "-1", "+5", "", "99999999999999999999999999999"] {
            let raw = format!(
                "POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:3789\r\nContent-Length: {bad_cl}\r\n\r\n"
            );
            let err = send_on_loopback(raw.into_bytes());
            assert!(
                matches!(err, Err(HttpParseError::InvalidRequest(_))),
                "expected InvalidRequest for Content-Length '{bad_cl}', got {err:?}"
            );
        }

        for dup in [
            "Content-Length: 2\r\nContent-Length: 2",
            "Content-Length: 2\r\nContent-Length: 5",
        ] {
            let raw = format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:3789\r\n{dup}\r\n\r\nhi");
            let err = send_on_loopback(raw.into_bytes());
            assert!(
                matches!(
                    err,
                    Err(HttpParseError::InvalidRequest("duplicate Content-Length"))
                ),
                "expected duplicate Content-Length error for {dup:?}, got {err:?}"
            );
        }

        for te in ["Transfer-Encoding: chunked", "transfer-encoding: gzip"] {
            let raw =
                format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:3789\r\n{te}\r\n\r\n0\r\n\r\n");
            let err = send_on_loopback(raw.into_bytes());
            assert!(
                matches!(
                    err,
                    Err(HttpParseError::InvalidRequest(
                        "unsupported Transfer-Encoding"
                    ))
                ),
                "expected Transfer-Encoding rejection for {te:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn http_wire_enforces_slowloris_total_deadline_across_headers_and_body() {
        for send_header_first in [false, true] {
            let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)) else {
                panic!("bind loopback failed");
            };
            let Ok(addr) = listener.local_addr() else {
                panic!("local_addr failed");
            };
            let client_handle = thread::spawn(move || {
                if let Ok(mut client) = TcpStream::connect(addr) {
                    if send_header_first {
                        let _ = client.write_all(
                            b"POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:3789\r\nContent-Length: 20\r\n\r\n",
                        );
                        let _ = client.flush();
                    }
                    // Trickle 1 byte every 40ms (each individual read < 120ms, total > 120ms)
                    for _ in 0..8 {
                        if client.write_all(b"x").is_err() || client.flush().is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(40));
                    }
                }
            });
            let Ok((mut server_stream, _)) = listener.accept() else {
                panic!("accept failed");
            };
            let started = Instant::now();
            let res =
                read_http_request_with_deadline(&mut server_stream, Duration::from_millis(120));
            let elapsed = started.elapsed();
            assert!(
                matches!(res, Err(HttpParseError::TimedOut)),
                "expected TimedOut for Slowloris trickle (send_header_first={send_header_first}), got {res:?}"
            );
            assert!(
                elapsed < Duration::from_millis(400),
                "Slowloris deadline took too long: {elapsed:?}"
            );
            drop(server_stream);
            let _ = client_handle.join();
        }
    }
}
