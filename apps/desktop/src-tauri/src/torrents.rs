//! Milestone 6 commands: torrents, checksum verification and
//! preview-while-downloading.

use std::sync::Arc;

use serde::Serialize;
use swiftfetch_store::repos;

use crate::state::AppState;

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

/// Opens the shared torrent session on first use (DHT on, trackers on,
/// TCP listen on an ephemeral port) and returns it.
async fn torrent_engine(
    state: &Arc<AppState>,
) -> Result<Arc<swiftfetch_torrent::TorrentEngine>, String> {
    let mut guard = state.torrent.lock().await;
    if let Some(engine) = guard.as_ref() {
        return Ok(Arc::clone(engine));
    }
    let dir = repos::default_download_dir().join("Torrents");
    std::fs::create_dir_all(&dir).map_err(|e| format!("torrent dir: {e}"))?;
    let addr: std::net::SocketAddr = "0.0.0.0:0"
        .parse()
        .map_err(|e| format!("listen addr: {e}"))?;
    let engine = Arc::new(
        swiftfetch_torrent::TorrentEngine::open(&dir, true, false, Some(addr))
            .await
            .map_err(|e| e.to_string())?,
    );
    *guard = Some(Arc::clone(&engine));
    Ok(engine)
}

/// UI-facing torrent row (DB row + live session status merged best-effort).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentView {
    /// SwiftFetch torrent-row id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// `downloading|paused|seeding|done|error`.
    pub state: String,
    /// Bytes downloaded.
    pub progress_bytes: u64,
    /// Total content bytes.
    pub total_bytes: u64,
    /// Download speed (bytes/s).
    pub down_bps: u64,
    /// Upload speed (bytes/s).
    pub up_bps: u64,
    /// Live peer connections.
    pub peers: u32,
    /// Seed-to ratio target.
    pub seed_ratio: f64,
}

/// Adds a torrent from a magnet link or a `.torrent` file path.
#[tauri::command]
pub async fn torrent_add(
    state: tauri::State<'_, Arc<AppState>>,
    source: String,
    seed_ratio: Option<f64>,
) -> Result<String, String> {
    let ratio = seed_ratio.unwrap_or(1.0).clamp(0.0, 100.0);
    let engine = torrent_engine(&state).await?;
    if source.starts_with("magnet:") {
        let parsed =
            swiftfetch_torrent::parse_magnet(&source).map_err(|e| format!("bad magnet: {e}"))?;
        let name = parsed
            .name
            .clone()
            .unwrap_or_else(|| parsed.info_hash_hex.clone());
        let backend_id = engine
            .add_magnet(&source, None, Vec::new())
            .await
            .map_err(|e| e.to_string())?;
        let db_id = db(&state, move |s| {
            repos::create_torrent(
                s,
                &parsed.info_hash_hex,
                Some(&source),
                &name,
                &engine.output_dir().to_string_lossy(),
                None,
                ratio,
            )
        })
        .await?;
        state
            .torrent_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(db_id.clone(), backend_id);
        return Ok(db_id);
    }
    let bytes = std::fs::read(&source).map_err(|_| "cannot read .torrent file".to_owned())?;
    let meta =
        swiftfetch_torrent::parse_torrent(&bytes).map_err(|e| format!("bad torrent: {e}"))?;
    let backend_id = engine
        .add_torrent_bytes(&bytes, None, Vec::new())
        .await
        .map_err(|e| e.to_string())?;
    let db_id = db(&state, move |s| {
        repos::create_torrent(
            s,
            &meta.info_hash_hex,
            None,
            &meta.name,
            &engine.output_dir().to_string_lossy(),
            None,
            ratio,
        )
    })
    .await?;
    state
        .torrent_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(db_id.clone(), backend_id);
    Ok(db_id)
}

/// Lists torrents with live progress merged in when the session knows them.
/// Finished torrents at their seed ratio are stopped and marked `done`.
#[tauri::command]
pub async fn torrent_list(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<TorrentView>, String> {
    let rows = db(&state, repos::list_torrents).await?;
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let backend = state
            .torrent_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&row.id)
            .copied();
        let live = if let Some(engine) = state.torrent.lock().await.as_ref() {
            backend.and_then(|id| engine.status(id).ok())
        } else {
            None
        };
        if let Some(status) = live.as_ref()
            && status.finished
            && swiftfetch_torrent::seeding_complete(
                status.uploaded_bytes,
                status.progress_bytes,
                row.seed_ratio,
            )
        {
            let engine = state.torrent.lock().await.as_ref().cloned();
            if let (Some(engine), Some(id)) = (engine, backend) {
                let _ = engine.remove(id, false).await;
            }
            db(&state, {
                let row_id = row.id.clone();
                move |s| repos::set_torrent_state(s, &row_id, "done", None)
            })
            .await?;
            views.push(TorrentView {
                id: row.id,
                name: row.name,
                state: "done".to_owned(),
                progress_bytes: status.progress_bytes,
                total_bytes: status.total_bytes,
                down_bps: 0,
                up_bps: 0,
                peers: 0,
                seed_ratio: row.seed_ratio,
            });
            continue;
        }
        views.push(TorrentView {
            id: row.id.clone(),
            name: row.name.clone(),
            state: live.as_ref().map_or(row.state.clone(), |s| {
                if s.paused {
                    "paused".to_owned()
                } else if s.finished {
                    "seeding".to_owned()
                } else {
                    "downloading".to_owned()
                }
            }),
            progress_bytes: live.as_ref().map_or(0, |s| s.progress_bytes),
            total_bytes: live.as_ref().map_or(0, |s| s.total_bytes),
            down_bps: live.as_ref().map_or(0, |s| s.down_bps),
            up_bps: live.as_ref().map_or(0, |s| s.up_bps),
            peers: live.as_ref().map_or(0, |s| s.peers),
            seed_ratio: row.seed_ratio,
        });
    }
    Ok(views)
}

/// Pauses a torrent.
#[tauri::command]
pub async fn torrent_pause(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let backend = state
        .torrent_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&id)
        .copied()
        .ok_or_else(|| "unknown torrent".to_owned())?;
    if let Some(engine) = state.torrent.lock().await.as_ref() {
        engine.pause(backend).await.map_err(|e| e.to_string())?;
    }
    db(&state, move |s| {
        repos::set_torrent_state(s, &id, "paused", None)
    })
    .await
}

/// Resumes a torrent.
#[tauri::command]
pub async fn torrent_resume(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(), String> {
    let backend = state
        .torrent_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&id)
        .copied()
        .ok_or_else(|| "unknown torrent".to_owned())?;
    if let Some(engine) = state.torrent.lock().await.as_ref() {
        engine.resume(backend).await.map_err(|e| e.to_string())?;
    }
    db(&state, move |s| {
        repos::set_torrent_state(s, &id, "downloading", None)
    })
    .await
}

/// Removes a torrent (`delete_files` also wipes downloaded data).
#[tauri::command]
pub async fn torrent_remove(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
    delete_files: bool,
) -> Result<(), String> {
    let engine = state.torrent.lock().await.as_ref().cloned();
    let backend = state
        .torrent_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id);
    if let (Some(engine), Some(backend)) = (engine, backend) {
        let _ = engine.remove(backend, delete_files).await;
    }
    db(&state, move |s| repos::delete_torrent(s, &id)).await
}

/// Sets the expected SHA-256/MD5 hex for a download.
#[tauri::command]
pub async fn set_expected_hash(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
    hex: String,
) -> Result<(), String> {
    let clean = hex.trim().to_ascii_lowercase();
    if (clean.len() != 64 && clean.len() != 32) || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("expected a 64-char (SHA-256) or 32-char (MD5) hex digest".to_owned());
    }
    db(&state, move |s| repos::set_expected_hash(s, &id, &clean)).await
}

/// Verifies one completed download now: match → `verified`; mismatch →
/// file quarantined to `.badhash`, row marked failed.
#[tauri::command]
pub async fn verify_job(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<bool, String> {
    let row = db(&state, {
        let id = id.clone();
        move |s| repos::get_download(s, &id).map(|r| r.map(|row| (row.final_path, row.id)))
    })
    .await?
    .ok_or_else(|| "download not found".to_owned())?;
    let expected = db(&state, {
        let job_id = row.1.clone();
        move |s| {
            repos::list_checksum_jobs(s).map(|jobs| {
                jobs.into_iter()
                    .find(|j| j.id == job_id)
                    .and_then(|j| j.expected_sha256)
            })
        }
    })
    .await?
    .or_else(|| swiftfetch_engine::checksum::find_sidecar_hex(std::path::Path::new(&row.0)));
    let Some(hex) = expected else {
        return Err("no expected hash set and no .sha256/.md5 sidecar found".to_owned());
    };
    let path = std::path::PathBuf::from(&row.0);
    let ok =
        swiftfetch_engine::checksum::verify_expected(&path, &hex).map_err(|e| e.to_string())?;
    if ok {
        db(&state, move |s| {
            repos::set_checksum_state(s, &row.1, "verified")
        })
        .await?;
    } else {
        let _ = swiftfetch_engine::checksum::quarantine_badhash(&path);
        db(&state, move |s| {
            repos::set_checksum_state(s, &row.1, "failed")?;
            repos::set_download_state(
                s,
                &row.1,
                "error",
                Some((
                    "E_CHECKSUM_MISMATCH",
                    "checksum failed — file kept as .badhash",
                )),
            )
        })
        .await?;
    }
    Ok(ok)
}

/// Batch-verifies every completed download with an expectation.
/// Returns `(verified, failed)`.
#[tauri::command]
pub async fn verify_all(state: tauri::State<'_, Arc<AppState>>) -> Result<(u32, u32), String> {
    let jobs = db(&state, repos::list_checksum_jobs).await?;
    let mut verified = 0u32;
    let mut failed = 0u32;
    for job in jobs {
        let expected = job.expected_sha256.or_else(|| {
            swiftfetch_engine::checksum::find_sidecar_hex(std::path::Path::new(&job.final_path))
        });
        let Some(hex) = expected else { continue };
        let path = std::path::PathBuf::from(&job.final_path);
        if swiftfetch_engine::checksum::verify_expected(&path, &hex).unwrap_or(false) {
            verified += 1;
            let id = job.id.clone();
            db(&state, move |s| {
                repos::set_checksum_state(s, &id, "verified")
            })
            .await?;
        } else {
            failed += 1;
            let _ = swiftfetch_engine::checksum::quarantine_badhash(&path);
            let id = job.id.clone();
            db(&state, move |s| {
                repos::set_checksum_state(s, &id, "failed")?;
                repos::set_download_state(
                    s,
                    &id,
                    "error",
                    Some((
                        "E_CHECKSUM_MISMATCH",
                        "checksum failed — file kept as .badhash",
                    )),
                )
            })
            .await?;
        }
    }
    Ok((verified, failed))
}

/// Starts a localhost preview server for a download's current bytes.
/// Returns `(token, url)` — open the URL in the OS default player.
#[tauri::command]
pub async fn preview_start(
    state: tauri::State<'_, Arc<AppState>>,
    id: String,
) -> Result<(String, String), String> {
    let path = db(&state, move |s| {
        repos::get_download(s, &id).map(|r| r.map(|row| row.final_path))
    })
    .await?
    .ok_or_else(|| "download not found".to_owned())?;
    // Prefer the in-progress part file so playback starts immediately.
    let part = format!("{path}.sfpart");
    let serve = if std::path::Path::new(&part).exists() {
        std::path::PathBuf::from(part)
    } else {
        std::path::PathBuf::from(path)
    };
    let (token, url, task) = crate::preview::start_server(serve).await?;
    state
        .preview_servers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(token.clone(), task);
    Ok((token, url))
}

/// Opens a URL in the OS default application (preview playback).
#[tauri::command]
pub async fn preview_open(url: String) -> Result<(), String> {
    opener::open(&url).map_err(|e| format!("cannot open preview: {e}"))?;
    Ok(())
}

/// Stops a preview server started by [`preview_start`].
#[tauri::command]
pub async fn preview_stop(
    state: tauri::State<'_, Arc<AppState>>,
    token: String,
) -> Result<(), String> {
    if let Some(handle) = state
        .preview_servers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&token)
    {
        handle.abort();
    }
    Ok(())
}
