//! Browser `native-messaging` host for `SwiftFetch` (Milestone 4).
//!
//! Protocol (system-design §7): both directions frame UTF-8 JSON as
//! `u32 LE length N` + `N` bytes, max 8 MiB. The browser spawns this
//! binary; stdin/stdout are the framing channel (stderr is free for logs).
//!
//! Messages in:
//! * `ping` → identity handshake.
//! * `add-download` `{url, cookies?, referer?, filename?, queue?}` → stages
//!   a row the running app consumes within ~1 s (the same shared-DB path
//!   the CLI uses) and replies with the staged id.
//! * `watch` `{stagedId}` → start pushing `event` frames for that capture.
//!
//! Messages out:
//! * reply frames (`{"ok":true|false, ...}`) correlated by `reqId`.
//! * `event` frames (`progress` / `completed` / `error`) pushed while a
//!   watched capture runs, with the mapped `jobId` once the app claims it.

pub mod bridge;
pub mod framing;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use swiftfetch_media::MediaContext;
use swiftfetch_sites_youtube::{RuntimeSolver, YoutubeSite};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

/// The native-messaging host id (manifests reference this).
pub const HOST_ID: &str = "com.swiftfetch.host";

/// Largest allowed frame (8 MiB, per the protocol contract).
pub const MAX_FRAME: u32 = 8 * 1024 * 1024;

/// Extension origins allowed to talk to the host. The browser enforces the
/// manifest allowlist at spawn time (primary gate); messages arriving over
/// an already-spawned pipe carry no `origin` and are trusted. Defense in
/// depth: any message that *does* carry an `origin` must match this list
/// or the host replies `E_ORIGIN_DENIED` (see `--print-manifest`, which
/// emits the same values for installers).
pub const ALLOWED_ORIGINS: &[&str] = &[
    "chrome-extension://ofinmfgldbecdccimfgknfhjekcioclb/",
    "chrome-extension://swiftfetch-edge@skytech45/",
    "swiftfetch@skytech45",
];

/// What a running host session tracks per watched capture.
struct Watch {
    staged_id: String,
    last_state: String,
    last_done: i64,
}

/// Serves one browser connection until EOF or `shutdown`. Origins gate on
/// [`ALLOWED_ORIGINS`]; see [`serve_with_origins`] for custom lists (tests).
///
/// # Errors
///
/// I/O and framing errors bubble up after a best-effort error frame.
pub async fn serve(
    store: Arc<std::sync::Mutex<swiftfetch_store::Store>>,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    serve_with_origins(store, input, output, shutdown, ALLOWED_ORIGINS).await
}

/// Serves one browser connection, gating `origin`-carrying messages on
/// `allowed_origins`.
///
/// # Errors
///
/// I/O and framing errors bubble up after a best-effort error frame.
pub async fn serve_with_origins(
    store: Arc<std::sync::Mutex<swiftfetch_store::Store>>,
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    shutdown: CancellationToken,
    allowed_origins: &[&str],
) -> std::io::Result<()> {
    let mut watches: Vec<Watch> = Vec::new();
    let mut req_seq: u64 = 0;
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            _ = poll.tick() => {
                for watch in &mut watches {
                    if watch.last_state == "done" || watch.last_state == "error"
                        || watch.last_state == "cancelled" {
                        continue;
                    }
                    if let Some((state, done, code)) =
                        bridge::job_status(&store, &watch.staged_id).await
                        && (state != watch.last_state || done != watch.last_done)
                    {
                        watch.last_state.clone_from(&state);
                        watch.last_done = done;
                        let payload = json!({
                            "type": "event",
                            "stagedId": watch.staged_id,
                            "event": match state.as_str() {
                                "done" => "completed",
                                "error" => "error",
                                _ => "progress",
                            },
                            "state": state,
                            "doneBytes": done,
                            "errorCode": code,
                        });
                        if framing::write_frame(&mut output, &payload).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            frame = framing::read_frame(&mut input) => {
                match frame {
                    Ok(None) => return Ok(()), // browser closed the port
                    Ok(Some(text)) => {
                        req_seq += 1;
                        let reply =
                            handle_message(&store, &text, &mut watches, allowed_origins).await;
                        let reply = reply.unwrap_or_else(|err| {
                            json!({"ok": false, "error": err})
                        });
                        if framing::write_frame(&mut output, &reply).await.is_err() {
                            return Ok(());
                        }
                        let _ = req_seq;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "native host frame error");
                        return Err(err);
                    }
                }
            }
        }
    }
}

/// Handles one request frame. Errors are string reasons for the reply.
#[allow(clippy::too_many_lines)] // one dispatch table reads best whole
async fn handle_message(
    store: &Arc<std::sync::Mutex<swiftfetch_store::Store>>,
    text: &str,
    watches: &mut Vec<Watch>,
    allowed_origins: &[&str],
) -> Result<Value, String> {
    let msg: Value = serde_json::from_str(text).map_err(|err| format!("bad JSON: {err}"))?;
    if let Some(origin) = msg.get("origin").and_then(Value::as_str)
        && !allowed_origins.contains(&origin)
    {
        return Err(format!("E_ORIGIN_DENIED: {origin}"));
    }
    match msg.get("type").and_then(Value::as_str) {
        Some("ping") => Ok(json!({
            "ok": true,
            "host": HOST_ID,
            "version": env!("CARGO_PKG_VERSION"),
        })),
        Some("add-download") => {
            let url = msg
                .get("url")
                .and_then(Value::as_str)
                .ok_or("missing url")?;
            if !(url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("ftp://"))
            {
                return Err("url must be http(s)/ftp".into());
            }
            let cookies = msg.get("cookies").and_then(Value::as_str);
            let referer = msg.get("referer").and_then(Value::as_str);
            let filename = msg.get("filename").and_then(Value::as_str);
            let queue = msg.get("queue").and_then(Value::as_str);
            let staged_id = bridge::stage_extension_download(
                store,
                url,
                cookies,
                referer,
                filename,
                queue,
                detect_kind(url),
                None,
            )
            .await
            .map_err(|err| err.to_string())?;
            watches.push(Watch {
                staged_id: staged_id.clone(),
                last_state: "staged".into(),
                last_done: 0,
            });
            Ok(json!({"ok": true, "stagedId": staged_id}))
        }
        Some("youtube-qualities") => {
            let url = msg
                .get("url")
                .and_then(Value::as_str)
                .ok_or("missing url")?;
            let ctx = MediaContext {
                cookies: msg
                    .get("cookies")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                referer: msg
                    .get("referer")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            let client = media_client()?;
            let solver = RuntimeSolver::new(client.clone());
            let site = YoutubeSite::new(&client, &solver);
            let options = site
                .quality_list(url, &ctx)
                .await
                .map_err(|err| err.to_string())?;
            Ok(json!({"ok": true, "qualities": options}))
        }
        Some("youtube-one-click") => {
            let url = msg
                .get("url")
                .and_then(Value::as_str)
                .ok_or("missing url")?;
            let height = msg
                .get("height")
                .and_then(Value::as_u64)
                .and_then(|h| u32::try_from(h).ok())
                .unwrap_or(1080);
            let cookies = msg.get("cookies").and_then(Value::as_str);
            let referer = msg.get("referer").and_then(Value::as_str);
            let meta = json!({"height": height, "watchUrl": url}).to_string();
            let staged_id = bridge::stage_extension_download(
                store,
                url,
                cookies,
                referer,
                None,
                None,
                "youtube",
                Some(&meta),
            )
            .await
            .map_err(|err| err.to_string())?;
            watches.push(Watch {
                staged_id: staged_id.clone(),
                last_state: "staged".into(),
                last_done: 0,
            });
            Ok(json!({"ok": true, "stagedId": staged_id}))
        }
        Some("watch") => {
            let staged_id = msg
                .get("stagedId")
                .and_then(Value::as_str)
                .ok_or("missing stagedId")?
                .to_owned();
            if !watches.iter().any(|w| w.staged_id == staged_id) {
                watches.push(Watch {
                    staged_id,
                    last_state: "staged".into(),
                    last_done: 0,
                });
            }
            Ok(json!({"ok": true}))
        }
        Some("status") => {
            let staged_id = msg
                .get("stagedId")
                .and_then(Value::as_str)
                .ok_or("missing stagedId")?;
            match bridge::job_status(store, staged_id).await {
                Some((state, done, code)) => Ok(json!({
                    "ok": true, "stagedId": staged_id, "state": state,
                    "doneBytes": done, "errorCode": code,
                })),
                None => Ok(json!({"ok": true, "stagedId": staged_id, "state": "staged"})),
            }
        }
        other => Err(format!("unknown message type {other:?}")),
    }
}

/// Pipeline kind from the URL shape: `.m3u8` → HLS, `.mpd` → DASH.
fn detect_kind(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("m3u8") => "hls",
        Some("mpd") => "dash",
        _ => "file",
    }
}

/// The outbound client used for site-module requests (watch pages, player
/// scripts).
fn media_client() -> Result<reqwest::Client, String> {
    swiftfetch_net::HttpConfig::default()
        .client()
        .map_err(|err| format!("http client: {err}"))
}
