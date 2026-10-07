//! Desktop application state: the engine, the store, the queue runner and
//! the M3 automation surfaces (quota ledger, post-action cancellation).

use std::sync::Arc;

use swiftfetch_engine::Engine;
use swiftfetch_scheduler::{QuotaConfig, QuotaLedger};
use swiftfetch_store::Store;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

/// Quota bookkeeping shared between the event forwarder (producer) and the
/// queue runner (enforcer). The ledger snapshot persists to the settings
/// table at most once every few seconds.
pub struct QuotaState {
    /// Configured limits (bytes per window).
    pub config: QuotaConfig,
    /// Byte counters for the rolling windows.
    pub ledger: QuotaLedger,
    /// Last time the ledger snapshot was persisted.
    pub last_persist: std::time::Instant,
    /// Whether the last tick announced an exhausted quota (edge-triggered
    /// `quota://changed` events).
    pub exhausted_announced: bool,
}

impl QuotaState {
    /// True when persisting now is due (5 s throttle).
    #[must_use]
    pub fn persist_due(&self) -> bool {
        self.last_persist.elapsed() >= std::time::Duration::from_secs(5)
    }
}

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
    /// Quota gate state (M3).
    pub quota: std::sync::Mutex<QuotaState>,
    /// Cancels a pending sleep/hibernate/shutdown countdown (M3); each
    /// countdown runs on a child token so later actions are unaffected.
    pub post_action_cancel: CancellationToken,
    /// Queues whose post-action countdown already fired this drain.
    pub post_action_fired: std::sync::Mutex<std::collections::HashSet<String>>,
}
