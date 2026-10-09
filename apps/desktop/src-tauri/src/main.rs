//! SwiftFetch desktop shell (Tauri v2).
//!
//! Thin Tauri layer over the workspace crates: commands wire the M1 engine
//! and the SQLite store into the React UI; a queue runner starts queued
//! jobs respecting per-queue concurrency; the tray offers quick control.
//! M3 automation: queue scheduler, quota gate, CLI bridge (shared-DB IPC),
//! clipboard monitor and the AV scan hook.
//! Milestones 2–3 of the build contract.

mod auth;
mod automation;
mod browser;
mod commands;
mod preview;
mod queue;
mod state;
mod torrents;
mod update;

use std::sync::Arc;

use state::{AppState, QuotaState};
use swiftfetch_scheduler::{QuotaConfig, QuotaLedger};
use swiftfetch_store::repos;
use tauri::{
    Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("swiftfetch=debug,info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let store = swiftfetch_store::Store::open_default()?;
            // Engine gets its own journal connection; UI repos use a second
            // connection on the same WAL database (single-process, serialized
            // writes through the mutex).
            // Seed categories + default settings on first run.
            let base = swiftfetch_store::repos::default_download_dir();
            let _ = repos::seed_default_categories(&store, &base);
            if repos::get_setting(&store, "ui.theme")?.is_none() {
                repos::set_setting(&store, "ui.theme", "\"system\"")?;
            }
            if repos::get_setting(&store, "speed.global_kbps")?.is_none() {
                repos::set_setting(&store, "speed.global_kbps", "null")?;
            }
            if repos::get_setting(&store, "clipboard.monitor")?.is_none() {
                repos::set_setting(&store, "clipboard.monitor", "false")?;
            }
            if repos::get_setting(&store, "quota.config")?.is_none() {
                repos::set_setting(
                    &store,
                    "quota.config",
                    "{\"hourlyLimit\":null,\"dailyLimit\":null}",
                )?;
            }
            if repos::get_setting(&store, "update.channel")?.is_none() {
                repos::set_setting(&store, "update.channel", "\"stable\"")?;
            }
            if repos::get_setting(&store, "update.checkOnStartup")?.is_none() {
                repos::set_setting(&store, "update.checkOnStartup", "true")?;
            }
            // Default "Main" queue.
            if repos::list_queues(&store)?.is_empty() {
                repos::create_queue(&store, "main", "Main", 2)?;
            }

            // Restore the global speed limit from settings.
            let global_kbps: Option<u64> = repos::get_setting(&store, "speed.global_kbps")?
                .and_then(|v| serde_json::from_str(&v).unwrap_or(None));
            let engine_config = swiftfetch_engine::EngineConfig {
                global_speed_limit_kib: global_kbps,
                ..swiftfetch_engine::EngineConfig::default()
            };
            let repos_store = swiftfetch_store::Store::open_default()?;
            // AV hook (M3): completed downloads go through the system
            // scanner when one is available (Windows Defender).
            let engine = open_engine(engine_config, store)?;

            // Recover interrupted jobs so the UI can offer one-click resume.
            let recovered = engine.job_ids_in_state(swiftfetch_engine::JobState::Interrupted)?;
            if !recovered.is_empty() {
                tracing::info!(count = recovered.len(), "interrupted downloads recovered");
            }

            // Quota gate state (M3): config + persisted ledger snapshot.
            let quota_config: QuotaConfig = repos::get_setting(&repos_store, "quota.config")?
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            let quota_ledger: QuotaLedger = repos::get_setting(&repos_store, "quota.ledger")?
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();

            let app_state = Arc::new(AppState {
                engine,
                store: Arc::new(std::sync::Mutex::new(repos_store)),
                queue_wake: tokio::sync::Notify::new(),
                app_handle: app.handle().clone(),
                quota: std::sync::Mutex::new(QuotaState {
                    config: quota_config,
                    ledger: quota_ledger,
                    last_persist: std::time::Instant::now(),
                    exhausted_announced: false,
                }),
                post_action_cancel: tokio_util::sync::CancellationToken::new(),
                post_action_fired: std::sync::Mutex::new(std::collections::HashSet::new()),
                torrent: tokio::sync::Mutex::new(None),
                torrent_ids: std::sync::Mutex::new(std::collections::HashMap::new()),
                preview_servers: std::sync::Mutex::new(std::collections::HashMap::new()),
            });
            app.manage(app_state.clone());

            // First-run browser integration (native host + sideload entries).
            browser::integrate_if_needed(app.handle(), &app_state);

            // Queue runner needs the app handle for events + notifications.
            queue::spawn(app.handle().clone(), app_state.clone());
            // M3 automation: scheduler timer, CLI bridge, clipboard monitor.
            automation::spawn_scheduler(app.handle().clone(), app_state.clone());
            automation::spawn_cli_bridge(app.handle().clone(), app_state.clone());
            automation::spawn_clipboard_monitor(app.handle().clone(), app_state);

            // Tray: show/hide, pause all, resume all, quit.
            let show = MenuItem::with_id(app, "show", "Show SwiftFetch", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "Hide window", true, None::<&str>)?;
            let pause_all =
                MenuItem::with_id(app, "pause-all", "Pause all downloads", true, None::<&str>)?;
            let resume_all =
                MenuItem::with_id(app, "resume-all", "Resume all", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &pause_all, &resume_all, &quit])?;
            TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().expect("window icon").clone())
                .menu(&menu)
                .on_menu_event(|app, event| {
                    let Some(state) = app.try_state::<Arc<AppState>>() else {
                        return;
                    };
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(win) = app.get_webview_window("main") {
                                let _ = win.show();
                                let _ = win.set_focus();
                            }
                        }
                        "hide" => {
                            if let Some(win) = app.get_webview_window("main") {
                                let _ = win.hide();
                            }
                        }
                        "pause-all" => {
                            for job in state.engine.list_jobs() {
                                if job.state == swiftfetch_engine::JobState::Downloading {
                                    let _ = state.engine.pause(&job.id);
                                }
                            }
                        }
                        "resume-all" => {
                            for snap in state.engine.list_jobs() {
                                if matches!(
                                    snap.state,
                                    swiftfetch_engine::JobState::Paused
                                        | swiftfetch_engine::JobState::Interrupted
                                ) && let Ok(rx) = state.engine.resume(&snap.id)
                                {
                                    let id = snap.id.clone();
                                    let app2 = app.clone();
                                    tauri::async_runtime::spawn(async move {
                                        crate::queue::forward_for(app2, id, rx).await;
                                    });
                                }
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_jobs,
            commands::add_url,
            commands::pause_job,
            commands::resume_job,
            commands::cancel_job,
            commands::delete_job,
            commands::job_segments,
            commands::set_global_speed,
            commands::set_job_speed,
            commands::list_categories,
            commands::create_category,
            commands::delete_category,
            commands::list_queues,
            commands::create_queue,
            commands::delete_queue,
            commands::enqueue_job,
            commands::dequeue_job,
            commands::move_queue_item,
            commands::start_queue,
            commands::stop_queue,
            commands::get_setting,
            commands::set_setting,
            commands::set_queue_schedule,
            commands::cancel_post_action,
            commands::get_quota_status,
            commands::create_grabber_project,
            commands::list_grabber_projects,
            commands::run_grabber_project,
            commands::delete_grabber_project,
            commands::add_mirror,
            commands::list_mirrors,
            commands::remove_mirror,
            commands::get_update_status,
            auth::auth_session,
            auth::auth_signup,
            auth::auth_signin,
            auth::auth_signout,
            browser::integrate_browsers,
            torrents::torrent_add,
            torrents::torrent_list,
            torrents::torrent_pause,
            torrents::torrent_resume,
            torrents::torrent_remove,
            torrents::set_expected_hash,
            torrents::verify_job,
            torrents::verify_all,
            torrents::preview_start,
            torrents::preview_open,
            torrents::preview_stop,
            update::check_for_updates,
            update::get_remote_config,
        ])
        .run(tauri::generate_context!())
        .expect("SwiftFetch desktop runtime failed to start");
}

/// Opens the engine, installing the system AV scanner when one is available
/// (Windows Defender via `MpCmdRun.exe`).
fn open_engine(
    config: swiftfetch_engine::EngineConfig,
    store: swiftfetch_store::Store,
) -> Result<swiftfetch_engine::Engine, swiftfetch_engine::EngineError> {
    #[cfg(windows)]
    if let Some(defender) = swiftfetch_engine::WindowsDefender::detect() {
        tracing::info!("AV hook active: windows-defender");
        return swiftfetch_engine::Engine::open_with_av_scanner(
            config,
            store,
            std::sync::Arc::new(defender),
        );
    }
    #[cfg(not(windows))]
    let _ = &config;
    swiftfetch_engine::Engine::open(config, store)
}
