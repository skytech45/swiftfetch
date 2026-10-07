//! Desktop application state: the engine, the store, and the queue runner.

use std::sync::Arc;

use swiftfetch_engine::Engine;
use swiftfetch_store::Store;
use tauri::AppHandle;

/// Shared desktop state managed by Tauri.
pub struct AppState {
    /// Download engine (M1).
    pub engine: Engine,
    /// SQLite store (repos connection; engine journal is separate).
    pub store: Arc<std::sync::Mutex<Store>>,
    /// Queue-runner tick signal.
    pub queue_wake: tokio::sync::Notify,
    /// App handle for emitting events from commands.
    pub app_handle: AppHandle,
}
