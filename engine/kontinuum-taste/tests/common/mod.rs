//! Shared test fixture: a real mock HTTP server on 127.0.0.1 (ephemeral
//! port) so the bundled [`kontinuum_taste::http::TcpTransport`] is
//! exercised end-to-end — request parsing, headers, pagination URLs —
//! with zero live credentials.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// One request the server received.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.clone())
    }

    pub fn query_param(&self, key: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    }
}

/// `(status, headers, body)` the server answers with.
pub type MockResponse = (u16, Vec<(String, String)>, String);
pub type Responder = Arc<dyn Fn(&RecordedRequest) -> MockResponse + Send + Sync>;

/// Sequential accept-loop server. Tests talk to it one request at a time
/// (the transport sends `Connection: close`), which keeps assertions on
/// the request log deterministic.
pub struct MockServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
    handle: Option<JoinHandle<()>>,
}

impl MockServer {
    /// Spawns the server with a responder that may need `base_url` (the
    /// factory runs after the port is known).
    pub fn start_with_base(make: impl FnOnce(&str) -> Responder) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let responder = make(&base_url);
        Self::spawn(listener, base_url, responder)
    }

    pub fn start(responder: Responder) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        Self::spawn(listener, base_url, responder)
    }

    fn spawn(listener: TcpListener, base_url: String, responder: Responder) -> Self {
        let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown2 = shutdown.clone();
        let requests2 = requests.clone();
        // Blocking accept: under full-workspace parallel test load a polled
        // nonblocking accept (sleep-poll) let a connection land between polls
        // and be dropped un-answered, surfacing as a truncated client read.
        // Blocking accept serves every connection in arrival order; Drop wakes
        // the accept with a throwaway connect.
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if shutdown2.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let mut read_half = stream.try_clone().expect("clone stream");
                        if let Some(req) = read_request(&mut read_half) {
                            let (status, headers, body) = responder(&req);
                            requests2.lock().unwrap().push(req);
                            write_response(&stream, status, &headers, &body);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        MockServer { base_url, requests, shutdown, addr, handle: Some(handle) }
    }

    pub fn start_simple(status: u16, body: &str) -> Self {
        let body = body.to_string();
        MockServer::start(Arc::new(move |_| (status, vec![], body.clone())))
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
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

/// Reads one HTTP/1.1 request (head + Content-Length body).
fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    // Generous under parallel test load; the transport side allows 30s.
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
    let (path_with_query, _fragment) = target.split_once('#').unwrap_or((&target, ""));
    let (path, query) = path_with_query
        .split_once('?')
        .map_or((path_with_query, ""), |(p, q)| (p, q));
    Some(RecordedRequest {
        method,
        path: path.to_string(),
        query: query.to_string(),
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
