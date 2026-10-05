//! SwiftFetch desktop shell (Tauri v2).
//!
//! Thin Tauri layer over the workspace crates. Milestone 0 proves the
//! TS → Rust command round-trip (`ping`) and bootstraps the SQLite store at
//! the OS app-data directory; the download engine arrives in Milestone 1.

use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use swiftfetch_store::Store;
use tauri::{Manager, State};

/// Store status reported to the UI status card.
#[derive(Serialize)]
struct DbStatus {
    /// Absolute database file path (empty when the store is unavailable).
    path: String,
    /// Journal mode reported by SQLite (`wal` expected), if open.
    journal_mode: Option<String>,
    /// Number of tables in the schema, if open.
    tables: Option<usize>,
    /// Failure reason when the store could not be opened.
    error: Option<String>,
}

/// Store lifecycle state held in managed app state.
enum StoreState {
    /// Store opened and migrated.
    Ready(Store),
    /// Store failed to open; the reason is surfaced to the UI.
    Failed(String),
}

/// Shared app state managed by Tauri.
struct AppState(Mutex<StoreState>);

/// Smoke command proving the TS → Rust round-trip.
#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

/// Reports the SQLite store status for the UI status card.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands receive State by value
fn db_status(state: State<'_, AppState>) -> DbStatus {
    let guard = state.0.lock().unwrap_or_else(PoisonError::into_inner);
    match &*guard {
        StoreState::Ready(store) => DbStatus {
            path: store.path().display().to_string(),
            journal_mode: store.journal_mode().ok(),
            tables: store.table_names().ok().map(|names| names.len()),
            error: None,
        },
        StoreState::Failed(reason) => DbStatus {
            path: String::new(),
            journal_mode: None,
            tables: None,
            error: Some(reason.clone()),
        },
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("swiftfetch=debug,info")),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            match Store::open_default() {
                Ok(store) => {
                    tracing::info!(path = %store.path().display(), "store opened");
                    app.manage(AppState(Mutex::new(StoreState::Ready(store))));
                }
                Err(err) => {
                    tracing::error!(error = %err, "store failed to open");
                    app.manage(AppState(Mutex::new(StoreState::Failed(err.to_string()))));
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![ping, db_status])
        .run(tauri::generate_context!())
        .expect("SwiftFetch desktop runtime failed to start");
}
