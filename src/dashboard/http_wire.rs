use std::io::{self, Read, Write};
use std::net::TcpStream;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub(super) struct HttpRequest {
    pub method: String,
    pub pathname: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
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
    InvalidRequest(&'static str),
    Io,
}

impl From<io::Error> for HttpParseError {
    fn from(_err: io::Error) -> Self {
        Self::Io
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
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];

    let (header_end, sep_len) = loop {
        if let Some(found) = find_header_end(&buf) {
            break found;
        }
        if buf.len() >= MAX_HEADER_BYTES {
            return Err(HttpParseError::InvalidRequest("request headers too large"));
        }
        let n = stream.read(&mut tmp)?;
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
    let mut content_length: usize = 0;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key == "content-length"
                && let Ok(len) = val.parse::<usize>()
            {
                content_length = len;
            }
            headers.push((key, val));
        }
    }

    if content_length > MAX_BODY_BYTES {
        return Err(HttpParseError::InvalidRequest("request body too large"));
    }
    let already_read = buf.len().saturating_sub(header_end + sep_len);
    let mut remaining = content_length.saturating_sub(already_read);
    while remaining > 0 {
        let to_read = remaining.min(tmp.len());
        let n = stream.read(&mut tmp[..to_read])?;
        if n == 0 {
            break;
        }
        remaining = remaining.saturating_sub(n);
    }

    Ok(HttpRequest {
        method,
        pathname,
        query,
        headers,
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

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
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

    stream.write_all(head.as_bytes())?;
    if send_body && !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()
}
