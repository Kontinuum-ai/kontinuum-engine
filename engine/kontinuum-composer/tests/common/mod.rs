//! Shared test fixture for the #36 provider tests: a real mock HTTP server
//! on 127.0.0.1 (ephemeral port) — the #21 taste-importer pattern — so the
//! bundled std-only [`tcp_transport`] is exercised end to end (envelopes,
//! auth headers, extraction) with zero live credentials.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use kontinuum_composer::{BackendError, TransportRequest};

/// One request the server received. Shared fixture: each test binary reads
/// the subset it asserts on, hence the blanket dead-code allowance.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl RecordedRequest {
    #[allow(dead_code)]
    pub fn header(&self, name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.clone())
    }
}

/// `(status, headers, body)` the server answers with.
pub type MockResponse = (u16, Vec<(String, String)>, String);
pub type Responder = Arc<dyn Fn(&RecordedRequest) -> MockResponse + Send + Sync>;

/// Sequential accept-loop server (blocking accept: under full-workspace
/// parallel test load a polled loop drops connections). Tests talk to it
/// one request at a time; `Drop` wakes the accept with a throwaway connect.
#[allow(dead_code)]
pub struct MockServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
    handle: Option<JoinHandle<()>>,
}

impl MockServer {
    pub fn start(responder: Responder) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let addr = listener.local_addr().expect("local addr");
        let shutdown2 = shutdown.clone();
        let requests2 = requests.clone();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if shutdown2.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let mut read_half = stream.try_clone().expect("clone stream");
                        if let Some(req) = read_request(&mut read_half) {
                            // Recorded at receipt: slow responders outlive
                            // the client's timeout, and attempt-counting
                            // assertions need to see those attempts.
                            requests2.lock().unwrap().push(req.clone());
                            let (status, headers, body) = responder(&req);
                            write_response(&stream, status, &headers, &body);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        MockServer { base_url, requests, shutdown, addr, handle: Some(handle) }
    }

    #[allow(dead_code)]
    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.addr);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let mut parts = status_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    let (path, _query) = target.split_once('?').unwrap_or((&target, ""));
    Some(RecordedRequest {
        method,
        path: path.to_string(),
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn write_response(mut stream: &TcpStream, status: u16, headers: &[(String, String)], body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut out = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// The bundled std-only transport: HTTP/1.1 over `TcpStream`, one
/// connection per request, honoring the [`TransportRequest`] timeout as
/// the socket timeout — a read/write expiry maps to
/// [`BackendError::Timeout`] per the transport contract (hosts inject
/// TLS-capable stacks in production).
pub fn tcp_transport(req: &TransportRequest<'_>) -> Result<String, BackendError> {
    inner_send(req).map_err(|(timed_out, msg)| {
        if timed_out {
            BackendError::Timeout(req.timeout.as_millis() as u64)
        } else {
            BackendError::Transport(msg)
        }
    })
}

type Failure = (bool, String);

fn inner_send(req: &TransportRequest<'_>) -> Result<String, Failure> {
    let rest = req
        .url
        .strip_prefix("http://")
        .ok_or_else(|| (false, format!("only plain http:// over the test transport: {}", req.url)))?;
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, "/"), |(a, _)| (a, &rest[a.len()..]));
    let addr = format!("{authority}")
        .to_socket_addrs()
        .map_err(|e| (false, e.to_string()))?
        .next()
        .ok_or_else(|| (false, format!("no address for {authority}")))?;
    let mut stream = TcpStream::connect(addr).map_err(|e| (false, e.to_string()))?;
    stream
        .set_read_timeout(Some(req.timeout))
        .map_err(|e| (false, e.to_string()))?;
    stream
        .set_write_timeout(Some(req.timeout))
        .map_err(|e| (false, e.to_string()))?;
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n",
        req.method, path
    );
    for (k, v) in req.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", req.body.len()));
    stream.write_all(head.as_bytes()).map_err(|e| (false, e.to_string()))?;
    stream.write_all(req.body.as_bytes()).map_err(|e| (false, e.to_string()))?;
    let mut raw = Vec::new();
    match stream.read_to_end(&mut raw) {
        Ok(_) => {}
        Err(e)
            if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) =>
        {
            return Err((true, e.to_string()))
        }
        Err(e) => return Err((false, e.to_string())),
    }
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| (false, "no header terminator".to_string()))?;
    let status_line = String::from_utf8_lossy(&raw[..split]);
    let status = status_line
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| (false, "bad status line".to_string()))?;
    if status != 200 {
        return Err((false, format!("provider answered HTTP {status}")));
    }
    Ok(String::from_utf8_lossy(&raw[split + 4..]).to_string())
}
