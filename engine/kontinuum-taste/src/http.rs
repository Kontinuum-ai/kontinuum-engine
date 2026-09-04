//! HTTP seam for the taste importer.
//!
//! **No third-party network stack.** The workspace carries none (checked
//! against the lockfile), so — mirroring the #22 composer backend — the
//! transport is injected: hosts wire URLSession (iOS FFI) or their own
//! TLS stack into [`HttpTransport`]. [`TcpTransport`] is the bundled
//! std-only implementation: HTTP/1.1 over `std::net::TcpStream`, enough
//! for the mock-server test suite and any plain-HTTP deployment. `https`
//! URLs on the default transport are a typed error, never a silent
//! downgrade.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::TasteError;

#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        HttpRequest { method: "GET".into(), url: url.into(), headers: Vec::new(), body: None }
    }

    pub fn post_form(url: impl Into<String>, form: &str) -> Self {
        HttpRequest {
            method: "POST".into(),
            url: url.into(),
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: Some(form.to_string()),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn bearer(url: impl Into<String>, token: &str) -> Self {
        Self::get(url).with_header("Authorization", &format!("Bearer {token}"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Host-injected transport. Object-safe so the importer can hold
/// `Arc<dyn HttpTransport>` across the FFI boundary.
pub trait HttpTransport: Send + Sync {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TasteError>;
}

/// HTTP/1.1 over `std::net::TcpStream`. One connection per request (the
/// connector's call rate makes keep-alive irrelevant; simplicity wins).
pub struct TcpTransport {
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl Default for TcpTransport {
    fn default() -> Self {
        TcpTransport {
            connect_timeout: Duration::from_secs(10),
            io_timeout: Duration::from_secs(30),
        }
    }
}

struct ParsedUrl<'a> {
    host: &'a str,
    port: u16,
    path: &'a str,
}

fn parse_url(url: &str) -> Result<ParsedUrl<'_>, TasteError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| TasteError::Transport(format!("only plain http:// is supported by TcpTransport (hosts inject TLS): {url}")))?;
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, "/"), |(a, _)| (a, &rest[a.len()..]));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| TasteError::Transport(format!("bad port in {url}")))?,
        ),
        None => (authority, 80),
    };
    Ok(ParsedUrl { host, port, path })
}

impl HttpTransport for TcpTransport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TasteError> {
        let ParsedUrl { host, port, path } = parse_url(&req.url)?;
        let addr = (host, port)
            .to_socket_addrs()
            .map_err(|e| TasteError::Transport(format!("resolve {host}:{port}: {e}")))?
            .next()
            .ok_or_else(|| TasteError::Transport(format!("no address for {host}:{port}")))?;
        let mut stream = TcpStream::connect_timeout(&addr, self.connect_timeout)
            .map_err(|e| TasteError::Transport(format!("connect {addr}: {e}")))?;
        stream.set_read_timeout(Some(self.io_timeout)).map_err(|e| TasteError::Transport(e.to_string()))?;
        stream.set_write_timeout(Some(self.io_timeout)).map_err(|e| TasteError::Transport(e.to_string()))?;

        let mut head = format!("{} {} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n", req.method, path);
        let mut has_ua = false;
        for (k, v) in &req.headers {
            if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("connection") {
                continue;
            }
            has_ua |= k.eq_ignore_ascii_case("user-agent");
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if !has_ua {
            head.push_str("User-Agent: kontinuum-taste\r\n");
        }
        if let Some(body) = &req.body {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .map_err(|e| TasteError::Transport(format!("send head: {e}")))?;
        if let Some(body) = &req.body {
            stream
                .write_all(body.as_bytes())
                .map_err(|e| TasteError::Transport(format!("send body: {e}")))?;
        }

        let mut raw = Vec::new();
        stream
            .read_to_end(&mut raw)
            .map_err(|e| TasteError::Transport(format!("read response: {e}")))?;
        parse_response(&raw)
    }
}

fn parse_response(raw: &[u8]) -> Result<HttpResponse, TasteError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| TasteError::Transport("response has no header terminator".into()))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = head.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| TasteError::Transport("empty response".into()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| TasteError::Transport(format!("bad status line: {status_line}")))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let body_bytes = &raw[split + 4..];
    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked")
    });
    let body = if chunked {
        decode_chunked(body_bytes)?
    } else {
        String::from_utf8_lossy(body_bytes).to_string()
    };
    Ok(HttpResponse { status, headers, body })
}

fn decode_chunked(mut bytes: &[u8]) -> Result<String, TasteError> {
    let mut out = String::new();
    loop {
        let line_end = bytes
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| TasteError::Transport("truncated chunk header".into()))?;
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&bytes[..line_end])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )
        .map_err(|e| TasteError::Transport(format!("bad chunk size: {e}")))?;
        bytes = &bytes[line_end + 2..];
        if size == 0 {
            break;
        }
        if bytes.len() < size + 2 {
            return Err(TasteError::Transport("truncated chunk body".into()));
        }
        out.push_str(&String::from_utf8_lossy(&bytes[..size]));
        bytes = &bytes[size + 2..];
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(head: &str, body: &[u8]) -> Vec<u8> {
        let mut v = head.as_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parses_content_length_responses() {
        let raw = response("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n", br#"{"ok":1}"#);
        let r = parse_response(&raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, r#"{"ok":1}"#);
        assert_eq!(r.header("content-type"), Some("application/json"));
    }

    #[test]
    fn decodes_chunked_bodies() {
        let body = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let raw = response("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n", body);
        let r = parse_response(&raw).unwrap();
        assert_eq!(r.body, "Wikipedia");
    }

    #[test]
    fn https_on_default_transport_is_a_typed_error() {
        let t = TcpTransport::default();
        let err = t.send(&HttpRequest::get("https://api.spotify.com/v1/me")).unwrap_err();
        assert!(matches!(err, TasteError::Transport(msg) if msg.contains("hosts inject TLS")));
    }
}
