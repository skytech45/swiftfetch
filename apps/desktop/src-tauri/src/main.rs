//! SwiftFetch desktop shell (Tauri v2).
//!
//! Thin Tauri layer over the workspace crates: commands wire the M1 engine
//! and the SQLite store into the React UI; a queue runner starts queued
//! jobs respecting per-queue concurrency; the tray offers quick control.
//! Milestone 2 of the build contract.

mod commands;
mod queue;
mod state;

use std::sync::Arc;

use state::AppState;
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
            let engine = swiftfetch_engine::Engine::open(engine_config, store)?;

            // Recover interrupted jobs so the UI can offer one-click resume.
            let recovered = engine.job_ids_in_state(swiftfetch_engine::JobState::Interrupted)?;
            if !recovered.is_empty() {
                tracing::info!(count = recovered.len(), "interrupted downloads recovered");
            }

            let app_state = Arc::new(AppState {
                engine,
                store: Arc::new(std::sync::Mutex::new(repos_store)),
                queue_wake: tokio::sync::Notify::new(),
                app_handle: app.handle().clone(),
            });
            app.manage(app_state.clone());

            // Queue runner needs the app handle for events + notifications.
            queue::spawn(app.handle().clone(), app_state);

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
        ])
        .run(tauri::generate_context!())
        .expect("SwiftFetch desktop runtime failed to start");
}
