//! M3 automation services: the queue scheduler (timer service over the
//! `queues` schedule column), the CLI bridge (consumes `staged_downloads`
//! and `cli_commands` written by out-of-process clients) and the clipboard
//! URL monitor.

use std::sync::Arc;
use std::time::Duration;

use swiftfetch_scheduler::{FireKind, FireSink, Schedule, ScheduleSource};
use swiftfetch_store::Store;
use swiftfetch_store::repos;
use tauri::Emitter;
use tokio_util::sync::CancellationToken;

use crate::state::AppState;

// ── Scheduler (timer service) ────────────────────────────────────────────

struct QueueScheduleSource {
    store: Arc<std::sync::Mutex<Store>>,
}

impl ScheduleSource for QueueScheduleSource {
    fn schedules(&self) -> Vec<(String, Schedule)> {
        let guard = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        repos::list_queues(&guard)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|queue| {
                let json = queue.schedule_json?;
                Schedule::parse(&json).map(|s| (queue.id, s))
            })
            .collect()
    }
}

struct SchedulerSink {
    app: tauri::AppHandle,
    state: Arc<AppState>,
}

impl FireSink for SchedulerSink {
    fn fired(&self, queue_id: &str, kind: FireKind) {
        let action = match kind {
            FireKind::Open => "open",
            FireKind::Close => "close",
        };
        tracing::info!(queue_id, action, "scheduler fired");
        {
            let store = Arc::clone(&self.state.store);
            let qid = queue_id.to_owned();
            let active = kind == FireKind::Open;
            tokio::task::spawn_blocking(move || {
                repos::set_queue_active(
                    &store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    &qid,
                    active,
                )
            });
        }
        // Close pauses the queue's in-flight jobs; the runner will not
        // restart them while the queue is inactive, and a later Open
        // resumes them (runner picks paused jobs first).
        if kind == FireKind::Close {
            let state = Arc::clone(&self.state);
            let qid = queue_id.to_owned();
            tauri::async_runtime::spawn(async move {
                pause_queue_jobs(&state, &qid).await;
            });
        }
        let _ = self.app.emit(
            "scheduler://fired",
            serde_json::json!({ "queueId": queue_id, "action": action }),
        );
        self.state.queue_wake.notify_one();
    }
}

async fn pause_queue_jobs(state: &Arc<AppState>, queue_id: &str) {
    let members = {
        let store = Arc::clone(&state.store);
        let qid = queue_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let guard = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            repos::queue_order(&guard, &qid)
        })
        .await
        .unwrap_or_default()
    };
    for job in members {
        if let Some(row) = repos_get(state, &job).await
            && row.state == "downloading"
            && let Err(err) = state.engine.pause(&job)
        {
            tracing::warn!(job, error = %err, "scheduler close could not pause job");
        }
    }
}

async fn repos_get(
    state: &Arc<AppState>,
    id: &str,
) -> Option<swiftfetch_store::repos::DownloadRow> {
    let store = Arc::clone(&state.store);
    let id = id.to_owned();
    let res = tokio::task::spawn_blocking(move || {
        repos::get_download(
            &store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &id,
        )
    })
    .await
    .ok()?;
    res.ok().flatten()
}

/// Spawns the queue timer service.
pub fn spawn_scheduler(app: tauri::AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let source = QueueScheduleSource {
            store: Arc::clone(&state.store),
        };
        let sink = SchedulerSink { app, state };
        swiftfetch_scheduler::run_timer(
            Arc::new(source),
            Arc::new(sink),
            swiftfetch_scheduler::TimerConfig::default(),
            CancellationToken::new(),
        )
        .await;
    });
}

// ── CLI bridge (shared-DB IPC) ───────────────────────────────────────────

/// Spawns the CLI bridge: consumes staged downloads and control commands
/// that out-of-process clients wrote into the shared database.
pub fn spawn_cli_bridge(app: tauri::AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Err(err) = consume_staged(&app, &state).await {
                tracing::warn!(error = %err, "cli bridge staged pass failed");
            }
            if let Err(err) = consume_commands(&state).await {
                tracing::warn!(error = %err, "cli bridge command pass failed");
            }
        }
    });
}

async fn consume_staged(app: &tauri::AppHandle, state: &Arc<AppState>) -> Result<(), String> {
    let staged = {
        let store = Arc::clone(&state.store);
        tokio::task::spawn_blocking(move || {
            repos::take_staged_downloads(
                &store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        })
        .await
        .map_err(|e| format!("stage take: {e}"))?
        .map_err(|e| e.to_string())?
    };
    for item in staged {
        let id = match item.kind.as_str() {
            "youtube" => spawn_youtube_job(app, state, &item).await,
            "hls" | "dash" => spawn_media_job(app, state, &item, &item.kind).await,
            _ => start_file_job(app, state, &item).await,
        };
        if let Some(id) = &id {
            // Map the staged row to the created job so the native host can
            // push progress/completed/error events to the extension.
            let store = Arc::clone(&state.store);
            let staged_id = item.id.clone();
            let job_id = id.clone();
            let _ = tokio::task::spawn_blocking(move || {
                repos::set_staged_job_id(
                    &store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    &staged_id,
                    &job_id,
                )
            })
            .await;
        }
    }
    Ok(())
}

/// Shared categorize + enqueue tail for all staged jobs.
async fn finish_job_setup(
    state: &Arc<AppState>,
    id: &str,
    item: &repos::StagedDownload,
    filename: &str,
) {
    let cats = {
        let store = Arc::clone(&state.store);
        tokio::task::spawn_blocking(move || {
            repos::list_categories(
                &store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        })
        .await
        .ok()
        .and_then(|r| r.ok())
    };
    if let Some(cats) = cats
        && let Some(cat) = repos::categorize(&cats, filename)
    {
        let store = Arc::clone(&state.store);
        let id2 = id.to_owned();
        let cat2 = cat.clone();
        let _ = tokio::task::spawn_blocking(move || {
            repos::set_category(
                &store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                &id2,
                Some(&cat2),
            )
        })
        .await;
    }
    if let Some(qid) = &item.queue_id {
        let store = Arc::clone(&state.store);
        let id2 = id.to_owned();
        let qid2 = qid.clone();
        let _ = tokio::task::spawn_blocking(move || {
            repos::enqueue(
                &store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                &qid2,
                &id2,
            )
        })
        .await;
    }
}

/// Regular range-download job (M1 engine path) with the forwarded context.
async fn start_file_job(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    item: &repos::StagedDownload,
) -> Option<String> {
    let dest = item
        .dest_dir
        .clone()
        .unwrap_or_else(repos::default_download_dir);
    let mut spec = swiftfetch_engine::JobSpec::new(&item.url, dest);
    spec.filename = item.filename.clone();
    spec.start_paused = item.start_paused || item.source == "cli";
    spec.max_conns = 8;
    spec.cookies = item.cookies.clone();
    spec.referer = item.referer.clone();
    match state.engine.start_job(spec).await {
        Ok((id, rx)) => {
            tracing::info!(
                source = %item.source,
                staged_id = %item.id,
                job = %id,
                "staged download accepted"
            );
            let filename = item
                .filename
                .clone()
                .unwrap_or_else(|| item.url.rsplit('/').next().unwrap_or("").to_owned());
            finish_job_setup(state, &id, item, &filename).await;
            let app2 = app.clone();
            let state2 = Arc::clone(state);
            let id2 = id.clone();
            tauri::async_runtime::spawn(async move {
                crate::queue::forward_for(app2, id2, rx).await;
                state2.queue_wake.notify_one();
            });
            Some(id)
        }
        Err(err) => {
            emit_rejected(app, item, &err.to_string());
            None
        }
    }
}

/// Finalizes a media row (done or error) and reports to the UI. `result`
/// carries `(code, message)` on failure so engine/site error codes survive.
async fn settle_media_job(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    id: &str,
    url: &str,
    final_path: &std::path::Path,
    result: Result<(), (String, String)>,
) {
    match result {
        Ok(()) => {
            let size = std::fs::metadata(final_path)
                .map(|m| m.len() as i64)
                .unwrap_or(0);
            {
                let store = Arc::clone(&state.store);
                let id3 = id.to_owned();
                let url3 = url.to_owned();
                let path3 = final_path.to_string_lossy().into_owned();
                let _ = tokio::task::spawn_blocking(move || {
                    let guard = store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    repos::set_download_progress(&guard, &id3, size)?;
                    repos::set_download_state(&guard, &id3, "done", None)?;
                    repos::insert_history(&guard, &url3, &path3, size, None)
                })
                .await;
            }
            use tauri::Emitter;
            let _ = app.emit(
                "download://event",
                serde_json::json!({"jobId": id, "kind": "completed",
                    "event": {"path": final_path}}),
            );
            state.queue_wake.notify_one();
        }
        Err(err) => {
            let (code, msg) = err;
            {
                let store = Arc::clone(&state.store);
                let id3 = id.to_owned();
                let code3 = code.clone();
                let msg3 = msg.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    repos::set_download_state(
                        &store
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        &id3,
                        "error",
                        Some((&code3, &msg3)),
                    )
                })
                .await;
            }
            use tauri::Emitter;
            let _ = app.emit(
                "download://event",
                serde_json::json!({"jobId": id, "kind": "failed",
                    "event": {"code": code, "message": msg}}),
            );
            state.queue_wake.notify_one();
        }
    }
}

/// Creates the downloads row for a media pipeline job and marks it
/// downloading.
async fn create_media_row(
    state: &Arc<AppState>,
    item: &repos::StagedDownload,
    id: &str,
    final_path: &std::path::Path,
) {
    let row_store = Arc::clone(&state.store);
    let row_id = id.to_owned();
    let row_url = item.url.clone();
    let row_final = final_path.to_string_lossy().into_owned();
    let _ = tokio::task::spawn_blocking(move || {
        repos::insert_download(
            &row_store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &row_id,
            &row_url,
            &row_final,
            None,
            None,
            1,
        )
    })
    .await;
    {
        let store = Arc::clone(&state.store);
        let id2 = id.to_owned();
        let _ = tokio::task::spawn_blocking(move || {
            repos::set_download_state(
                &store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                &id2,
                "downloading",
                None,
            )
        })
        .await;
    }
}

/// HLS/DASH pipeline job: a downloads row driven by `swiftfetch-media`,
/// emitting `download://event` directly.
async fn spawn_media_job(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    item: &repos::StagedDownload,
    kind: &str,
) -> Option<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let dest_dir = item
        .dest_dir
        .clone()
        .unwrap_or_else(repos::default_download_dir);
    let filename = item
        .filename
        .clone()
        .unwrap_or_else(|| format!("media-{}.mp4", &id[..8]));
    let final_path = dest_dir.join(&filename);
    create_media_row(state, item, &id, &final_path).await;
    finish_job_setup(state, &id, item, &filename).await;
    let app2 = app.clone();
    let state2 = Arc::clone(state);
    let kind2 = kind.to_owned();
    let url = item.url.clone();
    let job_path = final_path.clone();
    let job_id = id.clone();
    let ctx = swiftfetch_media::MediaContext {
        cookies: item.cookies.clone(),
        referer: item.referer.clone(),
    };
    tauri::async_runtime::spawn(async move {
        let client = swiftfetch_net::HttpConfig::default().client();
        let result = async {
            let client = client.map_err(|e| ("E_MEDIA_PARSE".to_owned(), e.to_string()))?;
            swiftfetch_media::capture(
                &client,
                &ctx,
                &kind2,
                &url,
                &job_path,
                None,
                &tokio_util::sync::CancellationToken::new(),
                |_| {},
            )
            .await
            .map_err(|e| (e.code().to_owned(), e.to_string()))
        }
        .await;
        settle_media_job(&app2, &state2, &job_id, &url, &job_path, result).await;
    });
    Some(id)
}

/// YouTube one-click job (§12.4): re-enumerates fresh (stream URLs expire),
/// picks ≤ the staged height, downloads and merges to MP4.
async fn spawn_youtube_job(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    item: &repos::StagedDownload,
) -> Option<String> {
    let meta: serde_json::Value =
        serde_json::from_str(item.meta_json.as_deref().unwrap_or("{}")).unwrap_or_default();
    let prefer_height = meta
        .get("height")
        .and_then(serde_json::Value::as_u64)
        .and_then(|h| u32::try_from(h).ok())
        .unwrap_or(1080);
    let id = uuid::Uuid::new_v4().to_string();
    let dest_dir = item
        .dest_dir
        .clone()
        .unwrap_or_else(repos::default_download_dir);
    let filename = item
        .filename
        .clone()
        .unwrap_or_else(|| format!("youtube-{}.mp4", &id[..8]));
    let final_path = dest_dir.join(&filename);
    create_media_row(state, item, &id, &final_path).await;
    finish_job_setup(state, &id, item, &filename).await;
    let app2 = app.clone();
    let state2 = Arc::clone(state);
    let url = item.url.clone();
    let job_path = final_path.clone();
    let job_id = id.clone();
    let ctx = swiftfetch_media::MediaContext {
        cookies: item.cookies.clone(),
        referer: item.referer.clone(),
    };
    tauri::async_runtime::spawn(async move {
        let client = swiftfetch_net::HttpConfig::default().client();
        let result = async {
            let client = client.map_err(|e| ("E_MEDIA_PARSE".to_owned(), e.to_string()))?;
            let solver = swiftfetch_sites_youtube::RuntimeSolver::new(client.clone());
            let site = swiftfetch_sites_youtube::YoutubeSite::new(&client, &solver);
            site.one_click(
                &url,
                &job_path,
                prefer_height,
                &ctx,
                &tokio_util::sync::CancellationToken::new(),
                |_| {},
            )
            .await
            .map(|_| ())
            .map_err(|e| (e.code().to_owned(), e.to_string()))
        }
        .await;
        settle_media_job(&app2, &state2, &job_id, &url, &job_path, result).await;
    });
    Some(id)
}

fn emit_rejected(app: &tauri::AppHandle, item: &repos::StagedDownload, message: &str) {
    tracing::warn!(url = %item.url, message, "staged download rejected");
    use tauri::Emitter;
    let _ = app.emit(
        "download://event",
        serde_json::json!({
            "jobId": item.id, "kind": "staged-rejected",
            "event": { "code": "E_STAGED_REJECTED", "message": message }
        }),
    );
}

async fn consume_commands(state: &Arc<AppState>) -> Result<(), String> {
    let commands = {
        let store = Arc::clone(&state.store);
        tokio::task::spawn_blocking(move || {
            repos::take_pending_commands(
                &store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        })
        .await
        .map_err(|e| format!("command take: {e}"))?
        .map_err(|e| e.to_string())?
    };
    for command in commands {
        tracing::info!(job = %command.job_id, action = %command.action, "cli command");
        match command.action.as_str() {
            "pause" => {
                let _ = state.engine.pause(&command.job_id);
            }
            "resume" => {
                if let Ok(rx) = state.engine.resume(&command.job_id) {
                    let app = state.app_handle.clone();
                    let id = command.job_id.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::queue::forward_for(app, id, rx).await;
                    });
                }
            }
            "cancel" => {
                let _ = state.engine.cancel(&command.job_id, false);
            }
            other => tracing::warn!(action = %other, "unknown cli command"),
        }
        state.queue_wake.notify_one();
    }
    Ok(())
}

// ── Clipboard monitor ────────────────────────────────────────────────────

fn looks_like_url(text: &str) -> bool {
    let text = text.trim();
    text.len() <= 2048
        && !text.contains(char::is_whitespace)
        && (text.starts_with("http://")
            || text.starts_with("https://")
            || text.starts_with("ftp://"))
}

/// Spawns the clipboard URL monitor: polls the system clipboard once a
/// second while enabled and surfaces new URLs to the UI (which asks before
/// adding anything). Setting: `clipboard.monitor` = true/false.
pub fn spawn_clipboard_monitor(app: tauri::AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        let mut last_seen: Option<String> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let enabled = {
                let store = Arc::clone(&state.store);
                tokio::task::spawn_blocking(move || {
                    repos::get_setting(
                        &store
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        "clipboard.monitor",
                    )
                })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|v| serde_json::from_str::<bool>(&v).ok())
                .unwrap_or(false)
            };
            if !enabled {
                continue;
            }
            let Ok(text) = app.clipboard().read_text() else {
                continue;
            };
            let candidate = text.trim().to_owned();
            if !looks_like_url(&candidate) || last_seen.as_deref() == Some(candidate.as_str()) {
                continue;
            }
            last_seen = Some(candidate.clone());
            tracing::info!("clipboard URL detected");
            let _ = app.emit("clipboard://url", serde_json::json!({ "url": candidate }));
        }
    });
}
