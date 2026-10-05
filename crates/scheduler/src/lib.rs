//! `SwiftFetch` scheduler and queues: named queues with concurrency limits,
//! start/stop windows, hourly quotas, periodic sync and post-actions
//! (sleep / hibernate / shutdown with a cancellable countdown).
//!
//! Implementation lands in Milestone 3 (Build Prompt §11; design contract in
//! docs/system-design.md §4.3). Timer state survives restarts via the
//! `queues` tables in swiftfetch-store.
