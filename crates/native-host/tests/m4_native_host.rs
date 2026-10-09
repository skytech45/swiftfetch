#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: setup may panic"
)]

//! Milestone 4 — native-host protocol tests (Build Prompt §12.1, §17):
//! framing round-trips through `serve`, origin allow-listing
//! (`E_ORIGIN_DENIED`), and dispatch (`ping`, `add-download` staging with
//! forwarded context, `watch`/`status`, malformed input).
//!
//! Framing unit cases (round-trip, oversize rejection, clean EOF) live in
//! `src/framing.rs`; these tests drive the whole `serve` loop over
//! in-memory pipes.

use std::sync::Arc;

use serde_json::{Value, json};
use swiftfetch_native_host::{ALLOWED_ORIGINS, serve_with_origins};
use swiftfetch_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Harness {
    _dir: tempfile::TempDir,
    store: Arc<std::sync::Mutex<Store>>,
    /// Browser → host.
    tx: tokio::io::DuplexStream,
    /// Host → browser.
    rx: tokio::io::DuplexStream,
    shutdown: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Harness {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&dir.path().join("host.db")).expect("store");
        let store = Arc::new(std::sync::Mutex::new(store));
        // serve() reads `input` and writes `output`; the harness holds the
        // opposite ends.
        let (client_tx, host_rx) = tokio::io::duplex(64 * 1024);
        let (host_tx, client_rx) = tokio::io::duplex(64 * 1024);
        let shutdown = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(serve_with_origins(
            Arc::clone(&store),
            host_rx,
            host_tx,
            shutdown.clone(),
            ALLOWED_ORIGINS,
        ));
        Self {
            _dir: dir,
            store,
            tx: client_tx,
            rx: client_rx,
            shutdown,
            task,
        }
    }

    async fn round_trip(&mut self, msg: &Value) -> Value {
        let body = serde_json::to_vec(msg).expect("json");
        let len = u32::try_from(body.len()).expect("small frame");
        self.tx
            .write_all(&len.to_le_bytes())
            .await
            .expect("write len");
        self.tx.write_all(&body).await.expect("write body");
        let mut len_bytes = [0u8; 4];
        self.rx.read_exact(&mut len_bytes).await.expect("read len");
        let len = u32::from_le_bytes(len_bytes);
        let mut buf = vec![0u8; len as usize];
        self.rx.read_exact(&mut buf).await.expect("read body");
        serde_json::from_slice(&buf).expect("reply json")
    }

    fn staged_count(&self) -> i64 {
        let store = Arc::clone(&self.store);
        store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM staged_downloads", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .expect("count staged")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ping_handshake_reports_host_id() {
    let mut host = Harness::start();
    let reply = host.round_trip(&json!({"type": "ping"})).await;
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["host"], "com.swiftfetch.host");
    assert!(reply["version"].is_string());
    host.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_download_stages_with_forwarded_context() {
    let mut host = Harness::start();
    let before = host.staged_count();
    let reply = host
        .round_trip(&json!({
            "type": "add-download",
            "url": "https://example.com/file.zip",
            "cookies": "session=abc",
            "referer": "https://example.com/",
            "filename": "file.zip",
        }))
        .await;
    assert_eq!(reply["ok"], true, "reply: {reply}");
    assert!(reply["stagedId"].is_string());
    assert_eq!(host.staged_count(), before + 1);

    // The forwarded request context survives into the staged row.
    let staged_id = reply["stagedId"].as_str().expect("staged id").to_owned();
    let store = Arc::clone(&host.store);
    let (cookies, referer, source): (Option<String>, Option<String>, String) = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .with_conn(|conn| {
            conn.query_row(
                "SELECT cookies, referer, source FROM staged_downloads WHERE id = ?1",
                [staged_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .expect("read staged row");
    assert_eq!(cookies.as_deref(), Some("session=abc"));
    assert_eq!(referer.as_deref(), Some("https://example.com/"));
    assert_eq!(source, "extension");

    // watch + status observe the staged capture.
    let reply = host
        .round_trip(&json!({"type": "watch", "stagedId": reply["stagedId"]}))
        .await;
    assert_eq!(reply["ok"], true);
    host.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_origin_rejected_right_origin_accepted() {
    let mut host = Harness::start();
    let reply = host
        .round_trip(&json!({"type": "ping", "origin": "chrome-extension://evil/"}))
        .await;
    assert_eq!(reply["ok"], false);
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("E_ORIGIN_DENIED"),
        "reply: {reply}"
    );

    // An allow-listed origin passes the gate.
    let reply = host
        .round_trip(&json!({"type": "ping", "origin": ALLOWED_ORIGINS[0]}))
        .await;
    assert_eq!(reply["ok"], true, "reply: {reply}");
    host.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_and_unknown_messages_fail_cleanly() {
    let mut host = Harness::start();
    // Bad URL scheme.
    let reply = host
        .round_trip(&json!({"type": "add-download", "url": "gopher://x/y"}))
        .await;
    assert_eq!(reply["ok"], false);
    // Missing url.
    let reply = host.round_trip(&json!({"type": "add-download"})).await;
    assert_eq!(reply["ok"], false);
    // Unknown type.
    let reply = host.round_trip(&json!({"type": "nope"})).await;
    assert_eq!(reply["ok"], false);
    assert!(
        reply["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown message type"),
        "reply: {reply}"
    );
    // HLS URLs are classified to the hls pipeline, not generic files.
    let reply = host
        .round_trip(&json!({"type": "add-download", "url": "https://cdn.example/v/pl.m3u8"}))
        .await;
    assert_eq!(reply["ok"], true, "reply: {reply}");
    let staged_id = reply["stagedId"].as_str().expect("id").to_owned();
    let store = Arc::clone(&host.store);
    let kind: String = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .with_conn(|conn| {
            conn.query_row(
                "SELECT kind FROM staged_downloads WHERE id = ?1",
                [staged_id],
                |r| r.get(0),
            )
        })
        .expect("read kind");
    assert_eq!(kind, "hls");
    host.task.abort();
}
