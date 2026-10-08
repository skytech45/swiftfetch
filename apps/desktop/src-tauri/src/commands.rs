//! Tauri commands exposed to the UI: job control, listing, categories,
//! queues, settings and deletion semantics.

use std::sync::Arc;

use serde::Serialize;
use swiftfetch_engine::JobSpec;
use swiftfetch_store::repos;
use tauri::{AppHandle, Emitter};

use crate::state::AppState;

/// UI-facing job row (DB row + live snapshot merged).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub filename: String,
    pub url: String,
    pub state: String,
    pub done_bytes: i64,
    pub total_len: Option<i64>,
    pub speed_bps: f64,
    pub category_id: Option<String>,
    pub queue_id: Option<String>,
    pub error_code: Option<String>,
    pub error_msg: Option<String>,
    pub created_at: String,
}

/// UI-facing category.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryView {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub folder: String,
}

/// UI-facing queue.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    pub id: String,
    pub name: String,
    pub max_concurrent: i64,
    pub is_active: bool,
    pub job_ids: Vec<String>,
    /// Scheduler JSON (M3); `None` = manual queue.
    pub schedule_json: Option<String>,
    /// Post-drain action: `none|sleep|hibernate|shutdown`.
    pub post_action: String,
}

async fn db<F, T>(state: &Arc<AppState>, f: F) -> Result<T, String>
where
    F: FnOnce(&swiftfetch_store::Store) -> Result<T, swiftfetch_store::StoreError> + Send + 'static,
    T: Send + 'static,
{
    let store = Arc::clone(&state.store);
    tokio::task::spawn_blocking(move || {
        let guard = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&guard)
    })
    .await
    .map_err(|e| format!("db task: {e}"))?
    .map_err(|e| e.to_string())
}

/// Lists all jobs (DB rows merged with live speed from the engine).
#[tauri::command]
pub async fn list_jobs(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<JobView>, String> {
    let rows = db(&state, repos::list_downloads).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let speed_bps = state
                .engine
                .snapshot(&row.id)
                .map(|snap| snap.bps)
                .unwrap_or(0.0);
            JobView {
                id: row.id,
                filename: row.filename,
                url: row.url,
                state: row.state,
                done_bytes: row.done_bytes,
                total_len: row.total_len,
                speed_bps,
                category_id: row.category_id,
                queue_id: row.queue_id,
                error_code: row.error_code,
                error_msg: row.error_msg,
                created_at: row.created_at,
            }
        })
        .collect())
}

/// Adds a URL: probes, creates the row, assigns a category, optionally
/// enqueues, and starts (or parks paused for queue-later).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn add_url(
    app: AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    url: String,
    category_id: Option<String>,
    queue_id: Option<String>,
    max_conns: Option<u8>,
    start_now: bool,
) -> Result<String, String> {
    let dest_dir = dirs::download_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("SwiftFetch");
    let mut spec = JobSpec::new(url.clone(), dest_dir);
    spec.max_conns = max_conns.unwrap_or(8);
    spec.start_paused = !start_now;

    let (id, rx) = state
        .engine
        .start_job(spec)
        .await
        .map_err(|e| e.to_string())?;

    // Assign category: explicit pick wins; else extension-map; else "other".
    let filename_hint = url
        .rsplit('/')
        .next()
        .unwrap_or("download.bin")
        .split('?')
        .next()
        .unwrap_or("download.bin")
        .to_owned();
    let row_id = id.clone();
    let resolved = match category_id {
        Some(c) if !c.is_empty() => Some(c),
        _ => {
            let cats = db(&state, repos::list_categories).await?;
            repos::categorize(&cats, &filename_hint)
        }
    };
    let queue = queue_id.clone();
    db(&state, move |s| {
        if let Some(cat) = resolved.as_deref() {
            repos::set_category(s, &row_id, Some(cat))?;
        }
        if let Some(q) = queue.as_deref() {
            repos::enqueue(s, q, &row_id)?;
        }
        Ok(())
    })
    .await?;

    // Forward engine events to the UI for live jobs.
    crate::queue::forward_for(app, id.clone(), rx).await;
    Ok(id)
}

/// Pauses a job (queued/paused jobs are no-ops).
#[tauri::command]
pub async fn pause_job(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    state.engine.pause(&id).map_err(|e| e.to_string())
}

/// Resumes a paused/queued/interrupted job.
#[tauri::command]
pub async fn resume_job(state: tauri::State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    let rx = state.engine.resume(&id).map_err(|e| e.to_string())?;
    crate::queue::forward_for(state.app_handle.clone(), id, rx).await;
    Ok(())
}

/// Cancels a job; with `delete_file` also removes the partial/final file.
#[tauri::command]
pub async fn cancel_job(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
    delete_file: bool,
) -> Result<(), String> {
    match state.engine.cancel(&id, delete_file) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Not a live job (queued/paused) — just remove the row.
            db(&state, move |s| repos::delete_download(s, &id)).await
        }
    }
}

/// Deletes a job row (and optionally its file). Cancel first if live.
#[tauri::command]
pub async fn delete_job(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
    delete_file: bool,
) -> Result<(), String> {
    let _ = state.engine.cancel(&id, delete_file);
    db(&state, {
        let id = id.clone();
        move |s| repos::delete_download(s, &id)
    })
    .await?;
    if delete_file
        && let Some(row) = db(&state, {
            let id = id.clone();
            move |s| repos::get_download(s, &id)
        })
        .await?
    {
        let path = std::path::PathBuf::from(&row.final_path);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
            "{}.sfpart",
            row.final_path
        )));
    }
    Ok(())
}

/// Per-job progress detail: segment journal rows.
#[tauri::command]
pub async fn job_segments(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = db(&state, move |s| repos::list_segments(s, &id)).await?;
    Ok(rows
        .into_iter()
        .map(|(idx, start, end, done, seg_state)| {
            serde_json::json!({
                "idx": idx, "start": start, "end": end,
                "done": done, "state": seg_state,
            })
        })
        .collect())
}

/// Sets the global speed limit (KiB/s; None = unlimited).
#[tauri::command]
pub async fn set_global_speed(
    state: tauri::State<'_, Arc<AppState>>,
    kib_per_s: Option<u64>,
) -> Result<(), String> {
    state.engine.set_global_speed_limit(kib_per_s).await;
    db(&state, move |s| {
        repos::set_setting(
            s,
            "speed.global_kbps",
            &serde_json::to_string(&kib_per_s).unwrap_or_else(|_| "null".into()),
        )
    })
    .await
}

/// Sets a per-download speed limit (KiB/s; None = inherit global).
#[tauri::command]
pub async fn set_job_speed(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
    kib_per_s: Option<u64>,
) -> Result<(), String> {
    state
        .engine
        .set_job_speed_limit(&id, kib_per_s)
        .await
        .map_err(|e| e.to_string())
}

/// Lists categories.
#[tauri::command]
pub async fn list_categories(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<CategoryView>, String> {
    let rows = db(&state, repos::list_categories).await?;
    Ok(rows
        .into_iter()
        .map(|c| CategoryView {
            id: c.id,
            name: c.name,
            extensions: serde_json::from_str(&c.extensions).unwrap_or_default(),
            folder: c.folder,
        })
        .collect())
}

/// Creates a custom category.
#[tauri::command]
pub async fn create_category(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    extensions: Vec<String>,
    folder: String,
) -> Result<String, String> {
    let id = format!("custom-{}", uuid::Uuid::new_v4().simple());
    let exts = serde_json::to_string(&extensions).unwrap_or_else(|_| "[]".into());
    let row_id = id.clone();
    db(&state, move |s| {
        repos::create_category(s, &row_id, &name, &exts, &folder)
    })
    .await?;
    Ok(id)
}

/// Deletes a category.
#[tauri::command]
pub async fn delete_category(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    db(&state, move |s| repos::delete_category(s, &id)).await?;
    Ok(())
}

/// Lists queues with their ordered job ids.
#[tauri::command]
pub async fn list_queues(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<QueueView>, String> {
    let rows = db(&state, repos::list_queues).await?;
    let mut out = Vec::new();
    for q in rows {
        let ids = db(&state, {
            let qid = q.id.clone();
            move |s| Ok(repos::queue_order(s, &qid))
        })
        .await?;
        out.push(QueueView {
            id: q.id,
            name: q.name,
            max_concurrent: q.max_concurrent,
            is_active: q.is_active != 0,
            job_ids: ids,
            schedule_json: q.schedule_json,
            post_action: q.post_action,
        });
    }
    Ok(out)
}

/// Creates a queue (default concurrency 2, configurable 1–5).
#[tauri::command]
pub async fn create_queue(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    max_concurrent: Option<i64>,
) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conc = max_concurrent.unwrap_or(2).clamp(1, 5);
    let row_id = id.clone();
    db(&state, move |s| {
        repos::create_queue(s, &row_id, &name, conc)
    })
    .await?;
    Ok(id)
}

/// Deletes a queue.
#[tauri::command]
pub async fn delete_queue(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    db(&state, move |s| repos::delete_queue(s, &id)).await
}

/// Enqueues an existing job.
#[tauri::command]
pub async fn enqueue_job(
    state: tauri::State<'_, Arc<AppState>>,
    queue_id: String,
    job_id: String,
) -> Result<(), String> {
    db(&state, move |s| repos::enqueue(s, &queue_id, &job_id)).await
}

/// Removes a job from its queue.
#[tauri::command]
pub async fn dequeue_job(
    state: tauri::State<'_, Arc<AppState>>,
    job_id: String,
) -> Result<(), String> {
    db(&state, move |s| repos::dequeue(s, &job_id)).await
}

/// Moves a queued item up/down.
#[tauri::command]
pub async fn move_queue_item(
    state: tauri::State<'_, Arc<AppState>>,
    queue_id: String,
    job_id: String,
    offset: i64,
) -> Result<(), String> {
    db(&state, move |s| {
        repos::move_queue_item(s, &queue_id, &job_id, offset)
    })
    .await
}

/// Starts a queue (activates the runner).
#[tauri::command]
pub async fn start_queue(
    app: AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    db(&state, {
        let id = id.clone();
        move |s| repos::set_queue_active(s, &id, true)
    })
    .await?;
    state.queue_wake.notify_one();
    let _ = app.emit("queue://changed", &id);
    Ok(())
}

/// Stops a queue: deactivates and pauses its active downloads.
#[tauri::command]
pub async fn stop_queue(
    app: AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    db(&state, {
        let id = id.clone();
        move |s| repos::set_queue_active(s, &id, false)
    })
    .await?;
    let members = db(&state, {
        let id = id.clone();
        move |s| Ok(repos::queue_order(s, &id))
    })
    .await?;
    for job in members {
        let live = state.engine.snapshot(&job).is_some();
        if live {
            let _ = state.engine.pause(&job);
        }
    }
    let _ = app.emit("queue://changed", &id);
    Ok(())
}

/// Reads a JSON setting.
#[tauri::command]
pub async fn get_setting(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<String>, String> {
    db(&state, move |s| repos::get_setting(s, &key)).await
}

/// Writes a JSON setting.
#[tauri::command]
pub async fn set_setting(
    state: tauri::State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    db(&state, move |s| repos::set_setting(s, &key, &value)).await
}

// ── M3: schedules, quotas, post-action control ──────────────────────────

/// Sets (or clears) a queue's schedule and post-queue action. The schedule
/// arrives as scheduler-crate JSON; it is validated before persisting.
#[tauri::command]
pub async fn set_queue_schedule(
    state: tauri::State<'_, Arc<AppState>>,
    queue_id: String,
    schedule: Option<String>,
    post_action: String,
) -> Result<(), String> {
    swiftfetch_scheduler::validate_schedule_json(schedule.as_deref())?;
    db(&state, move |s| {
        repos::set_queue_schedule(s, &queue_id, schedule.as_deref(), &post_action)
    })
    .await
}

/// Cancels a pending sleep/hibernate/shutdown countdown.
#[tauri::command]
pub async fn cancel_post_action(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    state.post_action_cancel.cancel();
    Ok(())
}

/// Current quota configuration and usage for the UI status line.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStatus {
    pub hourly_limit: Option<u64>,
    pub daily_limit: Option<u64>,
    pub hourly_used: u64,
    pub daily_used: u64,
    pub exhausted: bool,
}

/// Reports the quota gate state.
#[tauri::command]
pub async fn get_quota_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<QuotaStatus, String> {
    let quota = state
        .quota
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = chrono::Utc::now();
    let (hourly_used, daily_used) = quota.ledger.usage(now);
    let exhausted = matches!(
        quota.ledger.verdict(&quota.config, now),
        swiftfetch_scheduler::QuotaVerdict::Exhausted { .. }
    );
    Ok(QuotaStatus {
        hourly_limit: quota.config.hourly_limit,
        daily_limit: quota.config.daily_limit,
        hourly_used,
        daily_used,
        exhausted,
    })
}

// ── M5: site grabber, mirrors, updater ───────────────────────────────────

/// UI-facing grabber project.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrabberProjectView {
    pub id: String,
    pub name: String,
    pub seed_url: String,
    pub queue_id: Option<String>,
    pub last_found: i64,
}

/// Creates a site-grabber project (spider config persists for re-grabs).
#[tauri::command]
pub async fn create_grabber_project(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    seed_url: String,
    config_json: String,
    queue_id: Option<String>,
) -> Result<String, String> {
    if !seed_url.starts_with("http://") && !seed_url.starts_with("https://") {
        return Err("seed URL must be http(s)".to_owned());
    }
    db(&state, move |s| {
        repos::create_grabber_project(s, &name, &seed_url, &config_json, queue_id.as_deref())
    })
    .await
}

/// Lists site-grabber projects.
#[tauri::command]
pub async fn list_grabber_projects(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<GrabberProjectView>, String> {
    let rows = db(&state, repos::list_grabber_projects).await?;
    Ok(rows
        .into_iter()
        .map(|p| GrabberProjectView {
            id: p.id,
            name: p.name,
            seed_url: p.seed_url,
            queue_id: p.queue_id,
            last_found: p.last_found,
        })
        .collect())
}

/// Runs a grabber project: crawls the seed, inserts found files as paused
/// downloads on the project's queue, and records the run.
#[tauri::command]
pub async fn run_grabber_project(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    project_id: String,
) -> Result<i64, String> {
    let project = db(&state, move |s| {
        repos::list_grabber_projects(s).map(|v| v.into_iter().find(|p| p.id == project_id))
    })
    .await?
    .ok_or_else(|| "grabber project not found".to_owned())?;

    let mut config: swiftfetch_grabber::GrabConfig =
        serde_json::from_str(&project.config_json).map_err(|e| format!("bad grab config: {e}"))?;
    config.seed_url = project.seed_url.clone();
    if config.politeness.is_zero() {
        config.politeness = swiftfetch_grabber::DEFAULT_POLITENESS;
    }
    if config.timeout.is_zero() {
        config.timeout = swiftfetch_grabber::DEFAULT_TIMEOUT;
    }
    if config.user_agent.is_empty() {
        config.user_agent = "SwiftFetch-site-grabber/0.1".to_owned();
    }
    let report = swiftfetch_grabber::crawl(config)
        .await
        .map_err(|e| e.to_string())?;

    let found: Vec<(String, String)> = report
        .files
        .iter()
        .map(|f| {
            let name = f
                .url
                .rsplit('/')
                .next()
                .unwrap_or("grabbed")
                .split('?')
                .next()
                .unwrap_or("grabbed")
                .to_owned();
            (f.url.clone(), name)
        })
        .collect();
    let count = found.len() as i64;
    let queue_id = project.queue_id.clone();
    let dest_base = repos::default_download_dir().to_string_lossy().into_owned();
    db(&state, move |s| {
        for (url, name) in &found {
            let id = uuid::Uuid::new_v4().to_string();
            let final_path = std::path::Path::new(&dest_base)
                .join(name)
                .to_string_lossy()
                .into_owned();
            repos::insert_download(s, &id, url, &final_path, None, queue_id.as_deref(), 8)?;
            if let Some(q) = queue_id.as_deref() {
                let _ = repos::enqueue(s, q, &id);
            }
        }
        repos::record_grabber_run(s, &project.id, count)?;
        Ok::<(), swiftfetch_store::StoreError>(())
    })
    .await?;
    let _ = app.emit("queue://changed", "grabber");
    Ok(count)
}

/// Deletes a grabber project.
#[tauri::command]
pub async fn delete_grabber_project(
    state: tauri::State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<(), String> {
    db(&state, move |s| {
        repos::delete_grabber_project(s, &project_id)
    })
    .await
}

/// UI-facing mirror row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorView {
    pub url: String,
    pub priority: i64,
    pub fails: i64,
    pub bytes_ok: i64,
}

/// Adds a mirror URL to a download.
#[tauri::command]
pub async fn add_mirror(
    state: tauri::State<'_, Arc<AppState>>,
    job_id: String,
    url: String,
) -> Result<(), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("mirror URL must be http(s)".to_owned());
    }
    db(&state, move |s| repos::add_mirror(s, &job_id, &url, 0)).await
}

/// Lists a download's mirrors in try-order.
#[tauri::command]
pub async fn list_mirrors(
    state: tauri::State<'_, Arc<AppState>>,
    job_id: String,
) -> Result<Vec<MirrorView>, String> {
    let rows = db(&state, move |s| repos::list_mirrors(s, &job_id)).await?;
    Ok(rows
        .into_iter()
        .map(|m| MirrorView {
            url: m.url,
            priority: m.priority,
            fails: m.fails,
            bytes_ok: m.bytes_ok,
        })
        .collect())
}

/// Removes a mirror URL from a download.
#[tauri::command]
pub async fn remove_mirror(
    state: tauri::State<'_, Arc<AppState>>,
    job_id: String,
    url: String,
) -> Result<(), String> {
    db(&state, move |s| repos::remove_mirror(s, &job_id, &url)).await
}

/// Update-channel status for the Settings UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub channel: String,
    pub check_on_startup: bool,
    pub configured: bool,
    pub version: String,
}

/// Reports the updater configuration. `configured` is false until release
/// signing (M5) provisions the artifact feed + pinned public key — the UI
/// shows the "not configured in this build" note in that case.
#[tauri::command]
pub async fn get_update_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<UpdateStatus, String> {
    let channel = db(&state, |s| repos::get_setting(s, "update.channel"))
        .await?
        .and_then(|v| serde_json::from_str::<String>(&v).ok())
        .unwrap_or_else(|| "stable".to_owned());
    let check_on_startup = db(&state, |s| repos::get_setting(s, "update.checkOnStartup"))
        .await?
        .and_then(|v| serde_json::from_str::<bool>(&v).ok())
        .unwrap_or(true);
    Ok(UpdateStatus {
        channel,
        check_on_startup,
        configured: false,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}
