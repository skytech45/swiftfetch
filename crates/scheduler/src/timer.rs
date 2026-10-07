//! The timer service (system-design §4.3): a single tokio task owning a
//! min-heap of `(next_fire, queue_id)` events, refreshed from a schedule
//! snapshot; persisted timers survive restart because the snapshot is read
//! from the `queues` table on boot (missed-fire policy applies there).

use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::schedule::{FireKind, MissedOpen, Schedule, missed_open, next_events};

/// Wall-clock source, injected so tests can drive time deterministically.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> DateTime<Utc>;
}

/// The real system clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Test clock: a mutable millisecond offset from the Unix epoch.
#[derive(Debug)]
pub struct MockClock(AtomicI64);

impl MockClock {
    /// Starts the clock at `at`.
    #[must_use]
    pub fn at(at: DateTime<Utc>) -> Self {
        Self(AtomicI64::new(at.timestamp_millis()))
    }

    /// Moves the clock forward.
    pub fn advance(&self, millis: i64) {
        let _ = self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.0.load(Ordering::SeqCst)).unwrap_or_else(Utc::now)
    }
}

/// Heap entry ordering: soonest fire first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    at: DateTime<Utc>,
    queue_id: String,
    kind: FireKind,
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is a max-heap; invert so the soonest entry pops first.
        other
            .at
            .cmp(&self.at)
            .then_with(|| other.queue_id.cmp(&self.queue_id))
    }
}

/// Read-only snapshot of the current queue schedules.
pub trait ScheduleSource: Send + Sync {
    /// All schedules right now, as `(queue_id, schedule)` pairs.
    fn schedules(&self) -> Vec<(String, Schedule)>;
}

/// Receives fired timer events. Implementations should not block long; the
/// timer loop calls this synchronously (spawn heavy work internally).
pub trait FireSink: Send + Sync {
    /// A timer fired for `queue_id`.
    fn fired(&self, queue_id: &str, kind: FireKind);
}

/// Tuning knobs for the service (shorten in tests with paused time).
#[derive(Debug, Clone, Copy)]
pub struct TimerConfig {
    /// Maximum sleep between wake-ups; schedule edits are picked up at this
    /// granularity.
    pub refresh: Duration,
    /// Missed-open tolerance applied on boot (system-design: 15 minutes).
    pub missed_tolerance_secs: i64,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            refresh: Duration::from_secs(60),
            missed_tolerance_secs: 15 * 60,
        }
    }
}

/// Runs the timer loop on the system clock until `shutdown` is cancelled.
pub async fn run_timer(
    source: Arc<dyn ScheduleSource>,
    sink: Arc<dyn FireSink>,
    config: TimerConfig,
    shutdown: CancellationToken,
) {
    run_timer_with_clock(source, sink, config, shutdown, Arc::new(SystemClock)).await;
}

/// Runs the timer loop with an injected clock.
pub async fn run_timer_with_clock(
    source: Arc<dyn ScheduleSource>,
    sink: Arc<dyn FireSink>,
    config: TimerConfig,
    shutdown: CancellationToken,
    clock: Arc<dyn Clock>,
) {
    let mut heap: BinaryHeap<Entry> = BinaryHeap::new();

    // Boot: apply the missed-fire policy, then prime the heap.
    let mut seen: Vec<(String, Schedule)> = Vec::new();
    for (queue_id, schedule) in source.schedules() {
        if !schedule.is_valid() {
            continue;
        }
        match missed_open(
            &schedule,
            clock.now(),
            chrono::Duration::seconds(config.missed_tolerance_secs),
        ) {
            MissedOpen::Fire => sink.fired(&queue_id, FireKind::Open),
            MissedOpen::Skip => {
                tracing::info!(queue_id, "missed schedule open beyond tolerance — skipped");
            }
            MissedOpen::None => {}
        }
        for fire in next_events(&schedule, clock.now()) {
            heap.push(Entry {
                at: fire.at,
                queue_id: queue_id.clone(),
                kind: fire.kind,
            });
        }
        seen.push((queue_id, schedule));
    }

    loop {
        let next = heap.peek().map(|entry| entry.at);
        let sleep_for = match next {
            Some(at) => (at - clock.now())
                .to_std()
                .unwrap_or(Duration::ZERO)
                .min(config.refresh),
            None => config.refresh,
        };
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(sleep_for) => {}
        }

        // Re-snapshot on every wake: cheap for a handful of queues, and it
        // covers schedule edits without an explicit refresh signal.
        let current = source.schedules();
        if current != seen {
            seen.clone_from(&current);
            heap.clear();
            for (queue_id, schedule) in &current {
                if !schedule.is_valid() {
                    continue;
                }
                for fire in next_events(schedule, clock.now()) {
                    heap.push(Entry {
                        at: fire.at,
                        queue_id: queue_id.clone(),
                        kind: fire.kind,
                    });
                }
            }
        }

        // Fire everything due.
        let now = clock.now();
        let due: Vec<Entry> = heap
            .clone()
            .into_iter()
            .filter(|entry| entry.at <= now)
            .collect();
        for entry in due {
            sink.fired(&entry.queue_id, entry.kind);
            // Schedule the next event for this queue (Once schedules simply
            // produce nothing after their single instant).
            if let Some((_, schedule)) = seen.iter().find(|(id, _)| id == &entry.queue_id) {
                for fire in next_events(schedule, clock.now()) {
                    heap.push(Entry {
                        at: fire.at,
                        queue_id: entry.queue_id.clone(),
                        kind: fire.kind,
                    });
                }
            }
        }
        heap.retain(|entry| entry.at > now);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;
    use chrono::Local;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct FixedSource {
        schedules: Arc<std::sync::RwLock<Vec<(String, Schedule)>>>,
    }
    impl ScheduleSource for FixedSource {
        fn schedules(&self) -> Vec<(String, Schedule)> {
            self.schedules
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        fires: Mutex<Vec<(String, FireKind)>>,
    }
    impl FireSink for RecordingSink {
        fn fired(&self, queue_id: &str, kind: FireKind) {
            self.fires
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((queue_id.to_owned(), kind));
        }
    }

    fn test_config() -> TimerConfig {
        TimerConfig {
            refresh: Duration::from_secs(1),
            ..TimerConfig::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_schedule_fires_on_every_period() {
        let source = FixedSource {
            schedules: Arc::new(std::sync::RwLock::new(vec![(
                "q1".into(),
                Schedule::Periodic {
                    every_secs: 5,
                    jitter_secs: 0,
                },
            )])),
        };
        let sink = Arc::new(RecordingSink::default());
        let shutdown = CancellationToken::new();
        let clock = Arc::new(MockClock::at(DateTime::UNIX_EPOCH));
        let task = tokio::spawn(run_timer_with_clock(
            Arc::new(source),
            Arc::clone(&sink) as Arc<dyn FireSink>,
            test_config(),
            shutdown.clone(),
            Arc::clone(&clock) as Arc<dyn Clock>,
        ));
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(5)).await;
            clock.advance(5_000);
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        let fires = sink
            .fires
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(fires.len() >= 2, "expected >= 2 fires, got {fires:?}");
        assert!(fires.iter().all(|(_, kind)| *kind == FireKind::Open));
        shutdown.cancel();
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn missed_open_beyond_tolerance_is_skipped_on_boot() {
        // A Once schedule 2 hours in the past must NOT fire (tolerance 15 m).
        let source = FixedSource {
            schedules: Arc::new(std::sync::RwLock::new(vec![(
                "late".into(),
                Schedule::Once {
                    at: DateTime::UNIX_EPOCH - chrono::Duration::hours(2),
                },
            )])),
        };
        let sink = Arc::new(RecordingSink::default());
        let shutdown = CancellationToken::new();
        let clock = Arc::new(MockClock::at(DateTime::UNIX_EPOCH));
        let task = tokio::spawn(run_timer_with_clock(
            Arc::new(source),
            Arc::clone(&sink) as Arc<dyn FireSink>,
            TimerConfig::default(),
            shutdown.clone(),
            Arc::clone(&clock) as Arc<dyn Clock>,
        ));
        tokio::time::advance(Duration::from_secs(2)).await;
        let fires = sink
            .fires
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            fires.is_empty(),
            "missed once-fire must be skipped, got {fires:?}"
        );
        shutdown.cancel();
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn schedule_edits_are_picked_up_on_refresh() {
        let schedules = Arc::new(std::sync::RwLock::new(Vec::<(String, Schedule)>::new()));
        let source = FixedSource {
            schedules: Arc::clone(&schedules),
        };
        let sink = Arc::new(RecordingSink::default());
        let shutdown = CancellationToken::new();
        let clock = Arc::new(MockClock::at(DateTime::UNIX_EPOCH));
        let task = tokio::spawn(run_timer_with_clock(
            Arc::new(source),
            Arc::clone(&sink) as Arc<dyn FireSink>,
            test_config(),
            shutdown.clone(),
            Arc::clone(&clock) as Arc<dyn Clock>,
        ));
        // Edit the schedule while the service runs: fire every 2 s.
        schedules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                "edited".into(),
                Schedule::Periodic {
                    every_secs: 2,
                    jitter_secs: 0,
                },
            ));
        tokio::time::advance(Duration::from_secs(1)).await;
        clock.advance(1_000); // refresh wake picks up the edit
        tokio::time::advance(Duration::from_secs(2)).await;
        clock.advance(2_000); // the new schedule's first period elapses
        tokio::time::advance(Duration::from_millis(10)).await;
        let fires = sink
            .fires
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            fires.iter().any(|(id, _)| id == "edited"),
            "edited schedule must fire, got {fires:?}"
        );
        shutdown.cancel();
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn start_stop_windows_open_and_close() {
        // Window around the mock epoch in *local* time: open 2 s after the
        // epoch's local midnight-offset time, close 4 s after — timezone
        // independent. (A real local offset is minute-aligned, so +4 s can
        // never wrap past midnight.)
        let epoch_local_time = DateTime::UNIX_EPOCH.with_timezone(&Local).time();
        let start = epoch_local_time + chrono::Duration::seconds(2);
        let stop = epoch_local_time + chrono::Duration::seconds(4);
        let source = FixedSource {
            schedules: Arc::new(std::sync::RwLock::new(vec![(
                "windowed".into(),
                Schedule::StartStop { start, stop },
            )])),
        };
        let sink = Arc::new(RecordingSink::default());
        let shutdown = CancellationToken::new();
        let clock = Arc::new(MockClock::at(DateTime::UNIX_EPOCH));
        let task = tokio::spawn(run_timer_with_clock(
            Arc::new(source),
            Arc::clone(&sink) as Arc<dyn FireSink>,
            test_config(),
            shutdown.clone(),
            Arc::clone(&clock) as Arc<dyn Clock>,
        ));
        // Step forward in 1 s increments (tokio paused time and the mock
        // clock move together, so the loop keeps waking and firing).
        for _ in 0..8 {
            tokio::time::advance(Duration::from_secs(1)).await;
            clock.advance(1_000);
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        let fires = sink
            .fires
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            fires.contains(&("windowed".into(), FireKind::Open)),
            "{fires:?}"
        );
        assert!(
            fires.contains(&("windowed".into(), FireKind::Close)),
            "{fires:?}"
        );
        shutdown.cancel();
        task.abort();
    }

    #[test]
    fn heap_orders_by_soonest_fire() {
        let mut heap = BinaryHeap::new();
        heap.push(Entry {
            at: DateTime::UNIX_EPOCH + chrono::Duration::hours(1),
            queue_id: "later".into(),
            kind: FireKind::Open,
        });
        heap.push(Entry {
            at: DateTime::UNIX_EPOCH,
            queue_id: "sooner".into(),
            kind: FireKind::Close,
        });
        assert_eq!(heap.pop().expect("entry").queue_id, "sooner");
    }
}
