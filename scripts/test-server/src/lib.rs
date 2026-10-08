#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::format_push_string
)]

//! Scripted local HTTP server for engine tests: serves fixed bodies with
//! configurable behaviors — per-connection throttling, slow-after-N-bytes,
//! expiring URLs (410), premature connection resets, wrong Content-Length,
//! and ETag/entity swaps — plus a request log for assertions.
//!
//! Test fixture: all casted values are bounded by in-memory buffers, so the
//! pedantic cast lints are allowed crate-wide.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A scripted route.
#[derive(Debug, Clone)]
pub struct Route {
    /// Response body.
    pub data: Vec<u8>,
    /// Advertise `Accept-Ranges: bytes` and honor Range requests.
    pub accept_ranges: bool,
    /// `ETag` to advertise (used for `If-Range`).
    pub etag: Option<String>,
    /// `Last-Modified` to advertise.
    pub last_modified: Option<String>,
    /// `Content-Type` to advertise.
    pub content_type: Option<String>,
    /// `Content-Disposition` header value.
    pub content_disposition: Option<String>,
    /// `Digest` header value.
    pub digest: Option<String>,
    /// Cap every connection's throughput (bytes/s).
    pub throttle_bps: Option<u64>,
    /// After `slow_after_bytes` bytes on a given connection, that connection
    /// slows to `slow_bps` (emulates a throttled route).
    pub slow_after: Option<(u64, u64)>,
    /// Only the Nth connection (1-based, per route) is affected by
    /// `slow_after`; when `None`, every connection is.
    pub slow_nth_conn: Option<u32>,
    /// After this many GET requests, respond 410 Gone (expiring URL).
    pub expire_after_gets: Option<u32>,
    /// After this many GET requests *started*, every further GET sends
    /// headers + this many bytes and then drops the connection.
    pub reset_after_gets: Option<(u32, u64)>,
    /// `Content-Length` is actual + this delta; negative deltas close the
    /// connection early (premature EOF).
    pub content_length_delta: Option<i64>,
}

impl Route {
    /// A resumable route serving `data` with an `ETag`.
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            accept_ranges: true,
            etag: Some("\"v1\"".to_owned()),
            last_modified: None,
            content_type: Some("application/octet-stream".to_owned()),
            content_disposition: None,
            digest: None,
            throttle_bps: None,
            slow_after: None,
            slow_nth_conn: None,
            expire_after_gets: None,
            reset_after_gets: None,
            content_length_delta: None,
        }
    }

    /// Non-resumable variant (no `Accept-Ranges`, Range ignored).
    #[must_use]
    pub fn without_ranges(mut self) -> Self {
        self.accept_ranges = false;
        self.etag = None;
        self
    }

    /// Sets the `ETag`.
    #[must_use]
    pub fn etag(mut self, etag: &str) -> Self {
        self.etag = Some(etag.to_owned());
        self
    }

    /// Caps per-connection throughput.
    #[must_use]
    pub fn throttle_bps(mut self, bps: u64) -> Self {
        self.throttle_bps = Some(bps);
        self
    }

    /// Connection `nth` (1-based) slows to `bps` after `after_bytes`.
    #[must_use]
    pub fn slow_nth(mut self, nth: u32, after_bytes: u64, bps: u64) -> Self {
        self.slow_after = Some((after_bytes, bps));
        self.slow_nth_conn = Some(nth);
        self
    }

    /// GETs beyond `n` return 410 Gone.
    #[must_use]
    pub fn expires_after_gets(mut self, n: u32) -> Self {
        self.expire_after_gets = Some(n);
        self
    }

    /// GETs beyond `n` send `bytes` then reset the connection.
    #[must_use]
    pub fn resets_after_gets(mut self, n: u32, bytes: u64) -> Self {
        self.reset_after_gets = Some((n, bytes));
        self
    }

    /// Adds `delta` to the advertised Content-Length.
    #[must_use]
    pub fn wrong_content_length(mut self, delta: i64) -> Self {
        self.content_length_delta = Some(delta);
        self
    }
}

/// One logged request (for test assertions).
#[derive(Debug, Clone)]
pub struct RequestLog {
    /// Requested path.
    pub path: String,
    /// `Range` header value.
    pub range: Option<String>,
    /// `If-Range` header value.
    pub if_range: Option<String>,
    /// `Cookie` header value (M4: capture-with-cookies assertions).
    pub cookie: Option<String>,
    /// Response status code.
    pub status: u16,
}

struct RouteState {
    route: Route,
    gets: AtomicU32,
    conns: AtomicU32,
}

struct Shared {
    routes: Mutex<HashMap<String, Arc<RouteState>>>,
    log: Mutex<Vec<RequestLog>>,
}

/// A running scripted server on an ephemeral localhost port.
pub struct TestServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl TestServer {
    /// Starts the server. Must be called from a tokio runtime; the accept
    /// loop runs until [`TestServer::shutdown`] is called or the handle is
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when binding fails.
    pub async fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Shared {
            routes: Mutex::new(HashMap::new()),
            log: Mutex::new(Vec::new()),
        });
        let loop_shared = Arc::clone(&shared);
        let mut rx = shutdown_rx;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = rx.changed() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let shared = Arc::clone(&loop_shared);
                        tokio::spawn(async move {
                            let _ = handle_connection(stream, shared).await;
                        });
                    }
                }
            }
        });
        Ok(Self {
            addr,
            shared,
            shutdown: shutdown_tx,
        })
    }

    /// Adds or replaces a route.
    pub fn set_route(&self, path: &str, route: Route) {
        self.shared
            .routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                path.to_owned(),
                Arc::new(RouteState {
                    route,
                    gets: AtomicU32::new(0),
                    conns: AtomicU32::new(0),
                }),
            );
    }

    /// URL for a path.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// Snapshot of the request log.
    #[must_use]
    pub fn request_log(&self) -> Vec<RequestLog> {
        self.shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Stops the accept loop.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

async fn handle_connection(stream: TcpStream, shared: Arc<Shared>) -> std::io::Result<()> {
    let mut conn = stream;
    // Keep-alive loop: engine connection pooling reuses sockets.
    loop {
        let Ok(Some(request)) = read_request(&mut conn).await else {
            // Client closed or sent garbage: end the keep-alive loop.
            return Ok(());
        };
        let route_key = request.path.clone();
        // Solved stream URLs carry query strings (`?sig=…`); routes are
        // registered by path, so match on the path before the `?`.
        let lookup_key = route_key.split('?').next().unwrap_or(&route_key).to_owned();
        let Some(state) = ({
            let routes = shared
                .routes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            routes.get(&lookup_key).cloned()
        }) else {
            respond_simple(&mut conn, 404, b"not found").await?;
            continue;
        };
        let route = state.route.clone();
        let log = RequestLog {
            path: request.path.clone(),
            range: request.header("range").map(str::to_owned),
            if_range: request.header("if-range").map(str::to_owned),
            cookie: request.header("cookie").map(str::to_owned),
            status: 0,
        };
        let status = respond(&mut conn, &request, &route, &state, shared.clone()).await?;
        shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RequestLog { status, ..log });
        if request
            .header("connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("close"))
        {
            return Ok(());
        }
    }
}

async fn read_request(conn: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request headers truncated",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    Ok(Some(Request {
        method,
        path,
        headers,
    }))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

#[allow(clippy::too_many_lines)] // one scripted behavior per branch
async fn respond(
    conn: &mut TcpStream,
    request: &Request,
    route: &Route,
    state: &Arc<RouteState>,
    shared: Arc<Shared>,
) -> std::io::Result<u16> {
    let _ = shared;
    let total = route.data.len() as u64;
    let conn_id = state.conns.fetch_add(1, Ordering::Relaxed) + 1;
    let mut sent_on_conn: u64 = 0;

    if request.method == "HEAD" {
        let len = total as i64 + route.content_length_delta.unwrap_or(0);
        let mut head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n", len.max(0));
        if route.accept_ranges {
            head.push_str("Accept-Ranges: bytes\r\n");
        }
        if let Some(etag) = &route.etag {
            head.push_str(&format!("ETag: {etag}\r\n"));
        }
        if let Some(lm) = &route.last_modified {
            head.push_str(&format!("Last-Modified: {lm}\r\n"));
        }
        if let Some(ct) = &route.content_type {
            head.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if let Some(cd) = &route.content_disposition {
            head.push_str(&format!("Content-Disposition: {cd}\r\n"));
        }
        if let Some(dg) = &route.digest {
            head.push_str(&format!("Digest: {dg}\r\n"));
        }
        head.push_str("\r\n");
        conn.write_all(head.as_bytes()).await?;
        return Ok(200);
    }

    // GET: expiry first.
    if let Some(after) = route.expire_after_gets {
        let gets = state.gets.fetch_add(1, Ordering::Relaxed) + 1;
        if gets > after {
            conn.write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\n\r\n")
                .await?;
            return Ok(410);
        }
    } else {
        state.gets.fetch_add(1, Ordering::Relaxed);
    }

    // Parse Range (single range only; tests never send multipart ranges).
    let range = request.header("range").and_then(parse_range);
    let if_range = request.header("if-range").map(str::to_owned);

    let (start, end_incl, partial) = match (route.accept_ranges, range) {
        (true, Some((s, e))) => {
            // If-Range mismatch → full 200 (entity changed).
            let entity_matches = match (&if_range, &route.etag) {
                (Some(ir), Some(etag)) => ir == etag,
                (Some(_), None) => false,
                _ => true,
            };
            if entity_matches {
                (s, e.min(total.saturating_sub(1)), true)
            } else {
                (0, total.saturating_sub(1), false)
            }
        }
        (true, None) | (false, _) => (0, total.saturating_sub(1), false),
    };

    // 416: range starts beyond the body.
    if partial && start >= total && total > 0 {
        conn.write_all(
            format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .await?;
        return Ok(416);
    }

    let body: &[u8] = if end_incl < total {
        &route.data[start as usize..=(end_incl as usize)]
    } else {
        &route.data[start.min(total) as usize..]
    };

    // Reset behavior: after N gets, send a partial body then drop. The
    // headers mirror the normal response (Content-Range included) so the
    // client parses it as a genuine truncated 206.
    if let Some((after, partial_bytes)) = route.reset_after_gets {
        let gets = state.gets.load(Ordering::Relaxed);
        if gets > after {
            let take = body.len().min(partial_bytes as usize);
            let advertised = body.len() as i64 + route.content_length_delta.unwrap_or(0);
            let mut head = String::new();
            let status_line = if partial {
                "206 Partial Content"
            } else {
                "200 OK"
            };
            head.push_str(&format!("HTTP/1.1 {status_line}\r\n"));
            if partial {
                head.push_str(&format!(
                    "Content-Range: bytes {start}-{end_incl}/{total}\r\n"
                ));
            }
            head.push_str(&format!("Content-Length: {}\r\n", advertised.max(0)));
            if let Some(etag) = &route.etag {
                head.push_str(&format!("ETag: {etag}\r\n"));
            }
            head.push_str("\r\n");
            conn.write_all(head.as_bytes()).await?;
            let effective = if advertised < body.len() as i64 {
                advertised.max(0) as usize
            } else {
                body.len()
            };
            conn.write_all(&body[..take.min(effective)]).await?;
            conn.shutdown().await.ok();
            return Ok(if partial { 206 } else { 200 });
        }
    }

    let advertised = body.len() as i64 + route.content_length_delta.unwrap_or(0);
    let mut head = String::new();
    let status = if partial {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    head.push_str(&format!("HTTP/1.1 {status}\r\n"));
    if partial {
        head.push_str(&format!(
            "Content-Range: bytes {start}-{end_incl}/{total}\r\n"
        ));
    }
    head.push_str(&format!("Content-Length: {}\r\n", advertised.max(0)));
    if route.accept_ranges {
        head.push_str("Accept-Ranges: bytes\r\n");
    }
    if let Some(etag) = &route.etag {
        head.push_str(&format!("ETag: {etag}\r\n"));
    }
    if let Some(lm) = &route.last_modified {
        head.push_str(&format!("Last-Modified: {lm}\r\n"));
    }
    if let Some(ct) = &route.content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if let Some(cd) = &route.content_disposition {
        head.push_str(&format!("Content-Disposition: {cd}\r\n"));
    }
    if let Some(dg) = &route.digest {
        head.push_str(&format!("Digest: {dg}\r\n"));
    }
    head.push_str("\r\n");
    conn.write_all(head.as_bytes()).await?;
    let status_code = if partial { 206 } else { 200 };

    // Wrong-length negative delta: truncate the body (premature EOF).
    let effective_len = if advertised < body.len() as i64 {
        advertised.max(0) as usize
    } else {
        body.len()
    };

    // Per-connection pacing: throttle from byte 0, or slow-after threshold
    // for the targeted connection.
    let (pace_from, rate) = if let Some((after_bytes, slow_bps)) = route.slow_after {
        let nth = route.slow_nth_conn.unwrap_or(0);
        if nth == 0 || nth == conn_id {
            (after_bytes, Some(slow_bps))
        } else {
            (u64::MAX, route.throttle_bps)
        }
    } else {
        (0, route.throttle_bps)
    };

    // 128 KiB pacing chunks: fewer timer wakes per connection, which
    // matters on constrained CI runners (3-vCPU VMs).
    let chunk = 128 * 1024;
    let mut offset = 0usize;
    while offset < effective_len {
        let take = chunk.min(effective_len - offset);
        if sent_on_conn >= pace_from
            && let Some(bps) = rate
        {
            pace(bps, take as u64).await;
        }
        conn.write_all(&body[offset..offset + take]).await?;
        sent_on_conn += take as u64;
        offset += take;
    }
    if route.content_length_delta.unwrap_or(0) < 0 && advertised < body.len() as i64 {
        // Premature EOF: close without the remaining bytes.
        conn.shutdown().await.ok();
    }
    Ok(status_code)
}

/// Paces a chunk to roughly `bps` bytes/s.
async fn pace(bps: u64, bytes: u64) {
    if bps == 0 {
        return;
    }
    let secs = bytes as f64 / bps as f64;
    if secs > 0.0005 {
        tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    }
}

/// Parses a single `bytes=a-b` range (open-ended `a-` becomes `a..=MAX`).
fn parse_range(value: &str) -> Option<(u64, u64)> {
    let rest = value.strip_prefix("bytes=")?;
    let first = rest.split(',').next()?;
    if let Some((s, e)) = first.split_once('-') {
        let start = s.trim().parse::<u64>().ok()?;
        let end = e.trim().parse::<u64>().unwrap_or(u64::MAX - 1);
        Some((start, end))
    } else {
        None
    }
}

async fn respond_simple(conn: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    conn.write_all(head.as_bytes()).await?;
    conn.write_all(body).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn serves_basic_get_and_range() {
        let server = TestServer::start().await.expect("start");
        let data = vec![7u8; 1000];
        server.set_route("/file", Route::new(data.clone()));
        let url = server.url("/file");
        let body = reqwest_get(&url).await;
        assert_eq!(body.len(), 1000);
        server.shutdown();
    }

    async fn reqwest_get(url: &str) -> Vec<u8> {
        // The test server doubles as a plain HTTP fixture; reqwest is only a
        // dev-dependency of the engine, so use a tiny hand-rolled client.
        let mut stream = tokio::net::TcpStream::connect(addr_of(url)).await.unwrap();
        let path = url.splitn(4, '/').nth(3).unwrap_or("");
        stream
            .write_all(
                format!("GET /{path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let header_end = find_double_crlf(&buf).unwrap();
        parse_chunked_or_plain(&buf[header_end..])
    }

    fn addr_of(url: &str) -> String {
        let after_scheme = url.strip_prefix("http://").expect("http url");
        let host = after_scheme.split('/').next().expect("host");
        host.to_owned()
    }

    fn parse_chunked_or_plain(body: &[u8]) -> Vec<u8> {
        // Our server always sends Content-Length, so the body is plain.
        body.to_vec()
    }
}
