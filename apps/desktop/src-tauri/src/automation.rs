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
        let dest = item
            .dest_dir
            .clone()
            .unwrap_or_else(repos::default_download_dir);
        let mut spec = swiftfetch_engine::JobSpec::new(&item.url, dest);
        spec.filename = item.filename.clone();
        spec.start_paused = item.start_paused || item.source == "cli";
        spec.max_conns = 8;
        match state.engine.start_job(spec).await {
            Ok((id, rx)) => {
                tracing::info!(source = %item.source, staged_id = %item.id, job = %id, "staged download accepted");
                // Category + queue exactly like the add_url command.
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
                    .map_err(|e| format!("categories: {e}"))?
                    .map_err(|e| e.to_string())?
                };
                let filename = item.url.rsplit('/').next().unwrap_or("").to_owned();
                if let Some(cat) = repos::categorize(&cats, &filename) {
                    let store = Arc::clone(&state.store);
                    let id2 = id.clone();
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
                    let id2 = id.clone();
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
                let app2 = app.clone();
                let state2 = Arc::clone(state);
                let id2 = id.clone();
                tauri::async_runtime::spawn(async move {
                    crate::queue::forward_for(app2, id2, rx).await;
                    state2.queue_wake.notify_one();
                });
            }
            Err(err) => {
                tracing::warn!(url = %item.url, error = %err, "staged download rejected");
                let _ = app.emit(
                    "download://event",
                    serde_json::json!({
                        "jobId": item.id, "kind": "staged-rejected",
                        "event": { "code": "E_STAGED_REJECTED", "message": err.to_string() }
                    }),
                );
            }
        }
    }
    Ok(())
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
