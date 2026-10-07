//! Minimal queue runner: watches active queues and starts queued jobs
//! respecting per-queue concurrency. The M3 scheduler grows out of this.

use std::sync::Arc;
use std::time::Duration;

use swiftfetch_store::repos;

use tauri::Manager;

use crate::state::AppState;

/// Spawns the queue-runner task: a 500 ms tick (signaled early by the wake
/// notify) that starts the next queued job for any active queue under its
/// concurrency limit.
pub fn spawn(app: tauri::AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = state.queue_wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
            if let Err(err) = tick(&app, &state).await {
                tracing::warn!(error = %err, "queue runner tick failed");
            }
        }
    });
}

/// One runner pass: for each active queue, start queued jobs up to the
/// concurrency limit, in queue order. The M3 quota gate runs first: when a
/// limit is exhausted, in-flight downloads pause and nothing new starts
/// until the window resets.
async fn tick(app: &tauri::AppHandle, state: &Arc<AppState>) -> Result<(), String> {
    use tauri::Emitter;

    // ── Quota gate (M3) ──
    let quota_exhausted = {
        let mut quota = state
            .quota
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let verdict = quota.ledger.verdict(&quota.config, chrono::Utc::now());
        let exhausted = matches!(
            verdict,
            swiftfetch_scheduler::QuotaVerdict::Exhausted { .. }
        );
        if exhausted != quota.exhausted_announced {
            quota.exhausted_announced = exhausted;
            let _ = app.emit(
                "quota://changed",
                serde_json::json!({ "exhausted": exhausted }),
            );
        }
        exhausted
    };
    if quota_exhausted {
        for snap in state.engine.list_jobs() {
            if snap.state == swiftfetch_engine::JobState::Downloading {
                let _ = state.engine.pause(&snap.id);
            }
        }
        return Ok(());
    }

    let queues = {
        let store = Arc::clone(&state.store);
        tokio::task::spawn_blocking(move || {
            let guard = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            repos::list_queues(&guard)
        })
        .await
        .map_err(|e| format!("queue list: {e}"))?
        .map_err(|e| e.to_string())?
    };
    for queue in queues {
        if queue.is_active == 0 {
            continue;
        }
        let members = {
            let store = Arc::clone(&state.store);
            let qid = queue.id.clone();
            tokio::task::spawn_blocking(move || {
                let guard = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                repos::queue_order(&guard, &qid)
            })
            .await
            .map_err(|e| format!("queue order: {e}"))?
        };
        // Count members currently in flight; also detect a fully drained
        // queue for the post-action hook.
        let mut in_flight = 0u32;
        let mut next_queued: Option<String> = None;
        let mut all_settled = !members.is_empty();
        for job in &members {
            let Ok(Some(row)) = ({
                let store = Arc::clone(&state.store);
                let job = job.clone();
                tokio::task::spawn_blocking(move || {
                    let guard = store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    repos::get_download(&guard, &job)
                })
                .await
                .map_err(|e| format!("job get: {e}"))?
            }) else {
                all_settled = false;
                continue;
            };
            match row.state.as_str() {
                "downloading" | "probing" | "verifying" => {
                    in_flight += 1;
                    all_settled = false;
                }
                "queued" | "paused" | "interrupted" => {
                    all_settled = false;
                    if next_queued.is_none() {
                        next_queued = Some(job.clone());
                    }
                }
                _ => {}
            }
        }
        let limit = u32::try_from(queue.max_concurrent.clamp(1, 5)).unwrap_or(2);
        while in_flight < limit {
            let Some(job) = next_queued.take() else {
                break;
            };
            tracing::info!(job = %job, queue = %queue.name, "queue starting job");
            match state.engine.resume(&job) {
                Ok(rx) => {
                    in_flight += 1;
                    let _state2 = Arc::clone(state);
                    let job_id = job.clone();
                    let app2 = app.clone();
                    tokio::spawn(async move {
                        forward_for(app2, job_id, rx).await;
                    });
                }
                Err(err) => {
                    tracing::warn!(job = %job, error = %err, "queue could not start job");
                    break;
                }
            }
            // Re-pull the next queued candidate for the following slot.
            if in_flight < limit {
                next_queued = next_queued_for(state, &queue.id).await;
            }
        }

        // ── Post-action (M3): when an active queue drains, run its
        // configured power action behind a cancellable 60 s countdown.
        let post = swiftfetch_scheduler::PostAction::parse(&queue.post_action);
        let already_fired = state
            .post_action_fired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&queue.id);
        if post.is_real() && all_settled && !already_fired {
            state
                .post_action_fired
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(queue.id.clone());
            tracing::info!(queue = %queue.name, action = ?post, "queue drained — countdown started");
            let _ = app.emit(
                "scheduler://post-action",
                serde_json::json!({
                    "queueId": queue.id, "action": queue.post_action, "countdownSecs": 60
                }),
            );
            let cancel = state.post_action_cancel.child_token();
            let qname = queue.name.clone();
            let qname2 = qname.clone();
            tauri::async_runtime::spawn(async move {
                let outcome = swiftfetch_scheduler::countdown_then_action(
                    post,
                    std::time::Duration::from_secs(60),
                    cancel,
                    swiftfetch_scheduler::system_power(),
                    move |result| match result {
                        Ok(()) => tracing::info!(queue = %qname, "post-action executed"),
                        Err(err) => {
                            tracing::warn!(queue = %qname, error = %err, "post-action failed")
                        }
                    },
                )
                .await;
                if outcome == swiftfetch_scheduler::CountdownOutcome::Cancelled {
                    tracing::info!(queue = %qname2, "post-action countdown cancelled");
                }
            });
        }
    }
    Ok(())
}

/// Finds the next startable (queued/paused/interrupted) job in a queue.
async fn next_queued_for(state: &Arc<AppState>, queue_id: &str) -> Option<String> {
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
        .ok()?
    };
    for job in members {
        let row = {
            let store = Arc::clone(&state.store);
            let job = job.clone();
            tokio::task::spawn_blocking(move || {
                let guard = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                repos::get_download(&guard, &job)
            })
            .await
        };
        let Ok(Ok(Some(row))) = row else {
            continue;
        };
        if matches!(row.state.as_str(), "queued" | "paused" | "interrupted") {
            return Some(job);
        }
    }
    None
}

/// Forwards a job's engine events to the UI until it settles. Progress
/// events feed the quota ledger (M3) with the byte delta.
pub async fn forward_for(
    app: tauri::AppHandle,
    job_id: String,
    mut rx: tokio::sync::broadcast::Receiver<swiftfetch_engine::JobEvent>,
) {
    use tauri::Emitter;
    let Some(state) = app.try_state::<Arc<AppState>>() else {
        return;
    };
    let mut last_done: u64 = 0;
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let swiftfetch_engine::JobEvent::Progress { done, .. } = &event
                    && *done > last_done
                {
                    let delta = *done - last_done;
                    last_done = *done;
                    let snapshot = {
                        let mut quota = state
                            .quota
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        quota.ledger.add(delta, chrono::Utc::now());
                        quota.persist_due().then(|| {
                            quota.last_persist = std::time::Instant::now();
                            quota.ledger
                        })
                    };
                    if let Some(ledger) = snapshot {
                        let store = Arc::clone(&state.store);
                        let json = serde_json::to_string(&ledger).unwrap_or_default();
                        let _ = tokio::task::spawn_blocking(move || {
                            repos::set_setting(
                                &store
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                                "quota.ledger",
                                &json,
                            )
                        })
                        .await;
                    }
                }
                let kind = match &event {
                    swiftfetch_engine::JobEvent::Progress { .. } => "progress",
                    swiftfetch_engine::JobEvent::State { .. } => "state",
                    swiftfetch_engine::JobEvent::Completed { .. } => "completed",
                    swiftfetch_engine::JobEvent::Failed { .. } => "failed",
                };
                let _ = app.emit(
                    "download://event",
                    serde_json::json!({ "jobId": job_id, "kind": kind, "event": event }),
                );
                if let swiftfetch_engine::JobEvent::Completed { path } = &event {
                    use tauri_plugin_notification::NotificationExt;
                    let _ = app
                        .notification()
                        .builder()
                        .title("SwiftFetch")
                        .body(format!(
                            "{} downloaded",
                            path.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "File".into())
                        ))
                        .show();
                }
                if matches!(
                    event,
                    swiftfetch_engine::JobEvent::Completed { .. }
                        | swiftfetch_engine::JobEvent::Failed { .. }
                ) {
                    // Nudge the runner: a finished job frees a queue slot.
                    state.queue_wake.notify_one();
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
}
