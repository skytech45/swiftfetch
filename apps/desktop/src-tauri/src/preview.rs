//! Preview-while-downloading (Milestone 6, Build Prompt §14.6): a
//! localhost-only HTTP server exposing the bytes downloaded so far, with
//! `Range` support, so the OS default player can stream a video/audio file
//! while the download continues. The server re-stats the file per request
//! (the `.sfpart` grows), binds 127.0.0.1 on an ephemeral port, and requires
//! the unguessable token issued at start.

use std::path::PathBuf;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Starts serving `path` on localhost. Returns `(token, url, server_task)`;
/// abort the task to stop the server (it otherwise lives with the process).
///
/// # Errors
///
/// Returns a message when the listener cannot bind.
pub async fn start_server(
    path: PathBuf,
) -> Result<(String, String, tokio::task::JoinHandle<()>), String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("preview bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("preview addr: {e}"))?
        .port();
    let token: String = uuid::Uuid::new_v4().simple().to_string();
    let url = format!("http://127.0.0.1:{port}/preview?token={token}");
    let wanted = token.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, peer)) = listener.accept().await else {
                return;
            };
            if !peer.ip().is_loopback() {
                continue;
            }
            let path = path.clone();
            let wanted = wanted.clone();
            tokio::spawn(async move {
                serve_one(&mut sock, &path, &wanted).await;
            });
        }
    });
    Ok((token, url, task))
}

async fn serve_one(sock: &mut tokio::net::TcpStream, path: &PathBuf, wanted: &str) {
    let mut buf = vec![0u8; 4096];
    let Ok(n) = sock.read(&mut buf).await else {
        return;
    };
    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
    let mut lines = req.lines();
    let Some(request_line) = lines.next() else {
        return;
    };
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        respond(
            sock,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            None,
        )
        .await;
        return;
    }
    let target = parts.next().unwrap_or("/");
    let authorized = target.strip_prefix("/preview").is_some_and(|rest| {
        rest.split(['?', '&'])
            .filter_map(|pair| pair.split_once('='))
            .any(|(key, value)| key == "token" && value == wanted)
    });
    if !target.starts_with("/preview") || !authorized {
        respond(sock, "403 Forbidden", "text/plain", b"forbidden", None).await;
        return;
    }
    let total = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // Parse `Range: bytes=start-end`.
    let mut start: u64 = 0;
    let mut end: Option<u64> = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some(range) = line.strip_prefix("Range:") {
            let range = range.trim().trim_start_matches("bytes=");
            let (s, e) = range.split_once('-').unwrap_or((range, ""));
            start = s.trim().parse().unwrap_or(0);
            end = if e.trim().is_empty() {
                None
            } else {
                e.trim().parse().ok()
            };
        }
    }
    if start >= total && total > 0 {
        respond(
            sock,
            "416 Range Not Satisfiable",
            "text/plain",
            b"out of range",
            None,
        )
        .await;
        return;
    }
    let end = end.map_or(total.saturating_sub(1), |e| e.min(total.saturating_sub(1)));
    if end < start {
        respond(
            sock,
            "416 Range Not Satisfiable",
            "text/plain",
            b"out of range",
            None,
        )
        .await;
        return;
    }
    let body = match read_range(path, start, end).await {
        Ok(body) => body,
        Err(()) => {
            respond(
                sock,
                "500 Internal Server Error",
                "text/plain",
                b"read failed",
                None,
            )
            .await;
            return;
        }
    };
    let content_range = format!("bytes {start}-{end}/{total}");
    respond(
        sock,
        "206 Partial Content",
        "application/octet-stream",
        &body,
        Some(&content_range),
    )
    .await;
}

async fn read_range(path: &PathBuf, start: u64, end: u64) -> Result<Vec<u8>, ()> {
    let len = end.saturating_sub(start).saturating_add(1);
    if len > 64 * 1024 * 1024 {
        return Err(());
    }
    let mut file = tokio::fs::File::open(path).await.map_err(|_| ())?;
    use tokio::io::AsyncSeekExt as _;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| ())?;
    let capacity = usize::try_from(len).map_err(|_| ())?;
    let mut buf = vec![0u8; capacity];
    file.read_exact(&mut buf).await.map_err(|_| ())?;
    Ok(buf)
}

async fn respond(
    sock: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    content_range: Option<&str>,
) {
    let range_header = content_range.map_or(String::new(), |r| format!("Content-Range: {r}\r\n"));
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nAccept-Ranges: bytes\r\n{range_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(body).await;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn get(url: &str, range: Option<&str>) -> (String, Vec<u8>) {
        let url = url.replace("http://", "");
        let (host, path) = url.split_once('/').unwrap_or((url.as_str(), ""));
        let mut sock = tokio::net::TcpStream::connect(host).await.expect("connect");
        let range_header = range.map_or(String::new(), |r| format!("Range: {r}\r\n"));
        let req =
            format!("GET /{path} HTTP/1.1\r\nHost: x\r\n{range_header}Connection: close\r\n\r\n");
        sock.write_all(req.as_bytes()).await.expect("write");
        let mut buf = Vec::new();
        sock.read_to_end(&mut buf).await.expect("read");
        let split = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header/body split");
        let head = String::from_utf8_lossy(&buf[..split]).into_owned();
        (head, buf[split + 4..].to_vec())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn serves_ranges_and_rejects_strangers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("video.mp4");
        let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).expect("fixture");

        let (token, url, task) = start_server(path).await.expect("server starts");

        // Full body (no Range → whole file as 206).
        let (head, body) = get(&url, None).await;
        assert!(head.contains("206"), "expected 206, got {head}");
        assert_eq!(body, bytes);

        // Sub-range.
        let (head, body) = get(&url, Some("bytes=10-19")).await;
        assert!(head.contains("206"), "expected 206, got {head}");
        assert!(
            head.contains("Content-Range: bytes 10-19/1000"),
            "got {head}"
        );
        assert_eq!(body, bytes[10..20]);

        // Wrong token → 403.
        let (head, _) = get(&format!("{url}x-bogus"), None).await;
        assert!(head.contains("403"), "expected 403, got {head}");

        // Non-loopback is enforced at accept time (covered by bind);
        // out-of-range → 416.
        let (head, _) = get(&url, Some("bytes=9999-10000")).await;
        assert!(head.contains("416"), "expected 416, got {head}");

        task.abort();
        let _ = token;
    }
}
