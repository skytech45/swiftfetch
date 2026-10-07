//! `SwiftFetch` scheduler and queues (Milestone 3): named queues with
//! concurrency limits, start/stop windows, hourly/daily quotas and
//! post-actions (sleep / hibernate / shutdown with a cancellable countdown).
//!
//! Timer state survives restarts via the `queues` table in
//! `swiftfetch-store`: the timer service reads the schedule snapshot each
//! boot and applies the missed-fire policy (fire if less than 15 minutes
//! overdue, else mark skipped — system-design §4.3).

pub mod power;
pub mod power_os;
pub mod quota;
pub mod schedule;
pub mod timer;

pub use power::{CountdownOutcome, NoopPower, PostAction, PowerActions, countdown_then_action};
pub use power_os::{SystemPower, system_power};
pub use quota::{QuotaConfig, QuotaLedger, QuotaVerdict};
pub use schedule::{Fire, FireKind, Schedule, missed_open, next_events, validate_schedule_json};
pub use timer::{
    Clock, FireSink, MockClock, ScheduleSource, SystemClock, TimerConfig, run_timer,
    run_timer_with_clock,
};
