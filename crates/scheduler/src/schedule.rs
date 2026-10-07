//! Schedule model and fire-time math (system-design §4.3).
//!
//! Schedules serialize to the `queues.schedule_json` column as tagged JSON,
//! e.g. `{"kind":"start_stop","start":"22:00:00","stop":"06:00:00"}`. Daily
//! and start/stop times are wall-clock (system local timezone).

use chrono::{DateTime, Duration, Local, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

/// What a fired timer event means for a queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireKind {
    /// Open the queue (start accepting/starting downloads).
    Open,
    /// Close the queue (pause everything it started).
    Close,
}

/// A queue schedule (system-design §4.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Fires once at the given instant.
    Once {
        /// Absolute UTC instant.
        at: DateTime<Utc>,
    },
    /// Fires every day at the given wall-clock time.
    Daily {
        /// Wall-clock time of day (system local timezone).
        at: NaiveTime,
    },
    /// Fires every `every_secs`, plus up to `jitter_secs` of random delay.
    Periodic {
        /// Period in seconds (> 0).
        every_secs: u64,
        /// Extra random delay up to this many seconds.
        jitter_secs: u64,
    },
    /// Opens at `start` and closes at `stop` each day. When `start` is later
    /// than `stop` the window crosses midnight (e.g. 22:00 → 06:00).
    StartStop {
        /// Wall-clock open time.
        start: NaiveTime,
        /// Wall-clock close time.
        stop: NaiveTime,
    },
}

impl Schedule {
    /// Parses the `schedule_json` column value.
    #[must_use]
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// Validates a schedule's invariants.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Once { .. } | Self::Daily { .. } => true,
            Self::Periodic { every_secs, .. } => *every_secs > 0,
            Self::StartStop { start, stop } => start != stop,
        }
    }
}

/// An upcoming timer event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    /// When the event fires.
    pub at: DateTime<Utc>,
    /// What to do.
    pub kind: FireKind,
}

/// Small xorshift64 PRNG seeded from the clock — jitter without a rand dep.
struct Jitter(u64);

impl Jitter {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 { 0 } else { self.next() % bound }
    }
}

fn jitter_rng() -> Jitter {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |d| {
            u64::try_from(d.as_nanos()).unwrap_or(0x9E37_79B9_7F4A_7C15)
        });
    Jitter(nanos | 1)
}

/// Resolves a naive local date+time to a real instant, taking the earliest
/// offset (`None` only inside a DST gap).
fn local_at(day: chrono::NaiveDate, at: NaiveTime) -> Option<DateTime<Local>> {
    day.and_time(at).and_local_timezone(Local).earliest()
}

/// Next wall-clock occurrence of a local time strictly after `now`.
/// Walks at most two days forward, which also skips DST gaps.
fn next_daily_occurrence(at: NaiveTime, now: DateTime<Local>) -> DateTime<Utc> {
    let mut day = now.date_naive();
    for _ in 0..3 {
        if let Some(candidate) = local_at(day, at)
            && candidate > now
        {
            return candidate.with_timezone(&Utc);
        }
        day = day.succ_opt().unwrap_or(day);
    }
    now.with_timezone(&Utc)
}

/// Most recent wall-clock occurrence of a local time at or before `now`.
fn previous_daily_occurrence(at: NaiveTime, now: DateTime<Local>) -> DateTime<Utc> {
    let mut day = now.date_naive();
    for _ in 0..3 {
        if let Some(candidate) = local_at(day, at)
            && candidate <= now
        {
            return candidate.with_timezone(&Utc);
        }
        day = day.pred_opt().unwrap_or(day);
    }
    now.with_timezone(&Utc)
}

/// The next `Open`/`Close` events after `now`, soonest last. `StartStop`
/// yields both; the timer heap picks whichever comes first.
#[must_use]
pub fn next_events(schedule: &Schedule, now: DateTime<Utc>) -> Vec<Fire> {
    match schedule {
        Schedule::Once { at } => (*at > now)
            .then_some(Fire {
                at: *at,
                kind: FireKind::Open,
            })
            .into_iter()
            .collect(),
        Schedule::Daily { at } => vec![Fire {
            at: next_daily_occurrence(*at, now.with_timezone(&Local)),
            kind: FireKind::Open,
        }],
        Schedule::Periodic {
            every_secs,
            jitter_secs,
        } => {
            let every = i64::try_from(*every_secs).unwrap_or(i64::MAX);
            let jitter = i64::try_from(jitter_rng().below(*jitter_secs)).unwrap_or(0);
            vec![Fire {
                at: now + Duration::seconds(every + jitter),
                kind: FireKind::Open,
            }]
        }
        Schedule::StartStop { start, stop } => {
            let now_local = now.with_timezone(&Local);
            // Cross-midnight windows (start > stop): the day boundary moves —
            // a window that opened yesterday closes this morning, and a
            // window opening this evening closes tomorrow morning. Handle
            // both by checking whether `now_local` sits inside the window.
            let crosses_midnight = *start > *stop;
            let in_window = if crosses_midnight {
                now_local.time() >= *start || now_local.time() < *stop
            } else {
                now_local.time() >= *start && now_local.time() < *stop
            };
            let mut events = Vec::with_capacity(2);
            if !in_window {
                events.push(Fire {
                    at: next_daily_occurrence(*start, now_local),
                    kind: FireKind::Open,
                });
            }
            // The close event: today's stop if it is still ahead of now,
            // else tomorrow's.
            let today = now_local.date_naive();
            let close_at = local_at(today, *stop)
                .filter(|t| *t > now_local)
                .or_else(|| today.succ_opt().and_then(|d| local_at(d, *stop)))
                .unwrap_or_else(|| now_local + Duration::hours(24));
            events.push(Fire {
                at: close_at.with_timezone(&Utc),
                kind: FireKind::Close,
            });
            events
        }
    }
}

/// What the timer service should do about an `Open` event that was overdue
/// at boot (the persisted schedule outlives restarts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissedOpen {
    /// Fire the open now (it was overdue by less than the tolerance).
    Fire,
    /// Mark it skipped (overdue beyond the tolerance) and wait for the next.
    Skip,
    /// Nothing was overdue.
    None,
}

/// Missed-fire policy (system-design §4.3): run an overdue open if it is
/// less than `tolerance` late, else mark it skipped.
#[must_use]
pub fn missed_open(schedule: &Schedule, now: DateTime<Utc>, tolerance: Duration) -> MissedOpen {
    let now_local = now.with_timezone(&Local);
    let last_open: Option<DateTime<Utc>> = match schedule {
        // Once schedules fire their single instant or miss it entirely.
        Schedule::Once { at } => (*at <= now).then_some(*at),
        Schedule::Daily { at } => Some(previous_daily_occurrence(*at, now_local)),
        Schedule::Periodic { .. } => None, // relative: nothing to miss
        Schedule::StartStop { start, stop } => {
            let crosses_midnight = start > stop;
            let in_window = if crosses_midnight {
                now_local.time() >= *start || now_local.time() < *stop
            } else {
                now_local.time() >= *start && now_local.time() < *stop
            };
            // An open that is still "live" (inside its window) was never
            // missed; the window simply has not closed yet.
            if in_window {
                None
            } else {
                Some(previous_daily_occurrence(*start, now_local))
            }
        }
    };
    match last_open {
        Some(at) => {
            let late = now - at;
            if late < tolerance {
                MissedOpen::Fire
            } else {
                MissedOpen::Skip
            }
        }
        None => MissedOpen::None,
    }
}

/// One-shot validation helper used by the UI/commands: `Ok(())` when the
/// serialized schedule parses and passes [`Schedule::is_valid`].
///
/// # Errors
///
/// Returns a human-readable reason when the schedule is malformed.
pub fn validate_schedule_json(json: Option<&str>) -> Result<(), String> {
    let Some(json) = json else {
        return Ok(()); // manual queue
    };
    match Schedule::parse(json) {
        Some(schedule) if schedule.is_valid() => Ok(()),
        Some(_) => Err("invalid schedule: periodic needs every_secs > 0 and \
                        start/stop windows must differ"
            .into()),
        None => Err("schedule is not valid JSON".into()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;
    use chrono::TimeZone;

    /// A fixed local instant, timezone-resolved (earliest offset).
    fn local_ymd_hms(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .earliest()
            .expect("valid local time")
            .with_timezone(&Utc)
    }

    #[test]
    fn once_schedule_parses_and_fires() {
        let at = Utc::now() + Duration::hours(1);
        let json = serde_json::to_string(&Schedule::Once { at }).unwrap();
        let schedule = Schedule::parse(&json).unwrap();
        let events = next_events(&schedule, Utc::now());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, FireKind::Open);
        assert_eq!(events[0].at, at);
        // In the past: nothing left to fire.
        assert_eq!(next_events(&schedule, at + Duration::seconds(1)), []);
    }

    #[test]
    fn daily_schedule_fires_tomorrow_when_today_passed() {
        let schedule = Schedule::Daily {
            at: NaiveTime::from_hms_opt(0, 0, 1).unwrap(),
        };
        // 12:00 local is well past 00:00:01 → next fire is tomorrow.
        let now = local_ymd_hms(2026, 10, 7, 12, 0, 0);
        let events = next_events(&schedule, now);
        assert_eq!(events.len(), 1);
        assert!(events[0].at > now);
        assert!(events[0].at - now <= Duration::hours(36));
    }

    #[test]
    fn start_stop_day_window_yields_both_events() {
        let schedule = Schedule::StartStop {
            start: NaiveTime::from_hms_opt(1, 0, 0).unwrap(),
            stop: NaiveTime::from_hms_opt(23, 0, 0).unwrap(),
        };
        // 00:30 local sits before the 01:00 open → both events pending.
        let now = local_ymd_hms(2026, 10, 7, 0, 30, 0);
        let events = next_events(&schedule, now);
        assert!(events.iter().any(|f| f.kind == FireKind::Open));
        assert!(events.iter().any(|f| f.kind == FireKind::Close));
    }

    #[test]
    fn start_stop_cross_midnight_inside_window_has_no_pending_open() {
        // A 22:00→06:00 window: at 23:00 local the window is open, so only a
        // Close event should be pending.
        let schedule = Schedule::StartStop {
            start: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            stop: NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
        };
        let night = local_ymd_hms(2026, 10, 7, 23, 0, 0);
        let events = next_events(&schedule, night);
        assert!(events.iter().all(|f| f.kind == FireKind::Close));
    }

    #[test]
    fn periodic_next_is_relative() {
        let schedule = Schedule::Periodic {
            every_secs: 60,
            jitter_secs: 0,
        };
        let now = Utc::now();
        let events = next_events(&schedule, now);
        assert_eq!(events[0].at - now, Duration::seconds(60));
    }

    #[test]
    fn missed_open_respects_tolerance() {
        let tolerance = Duration::minutes(15);
        // Once, 10 minutes overdue → fire.
        let schedule = Schedule::Once {
            at: Utc::now() - Duration::minutes(10),
        };
        assert_eq!(
            missed_open(&schedule, Utc::now(), tolerance),
            MissedOpen::Fire
        );
        // Once, 2 hours overdue → skip.
        let schedule = Schedule::Once {
            at: Utc::now() - Duration::hours(2),
        };
        assert_eq!(
            missed_open(&schedule, Utc::now(), tolerance),
            MissedOpen::Skip
        );
        // Future once → nothing overdue.
        let schedule = Schedule::Once {
            at: Utc::now() + Duration::hours(1),
        };
        assert_eq!(
            missed_open(&schedule, Utc::now(), tolerance),
            MissedOpen::None
        );
        // Daily at 06:00, now 06:10 → 10 minutes late → fire.
        let schedule = Schedule::Daily {
            at: NaiveTime::from_hms_opt(6, 0, 0).unwrap(),
        };
        let now = local_ymd_hms(2026, 10, 7, 6, 10, 0);
        assert_eq!(missed_open(&schedule, now, tolerance), MissedOpen::Fire);
    }

    #[test]
    fn validate_json_rejects_garbage_and_degenerate_windows() {
        assert!(validate_schedule_json(None).is_ok());
        assert!(validate_schedule_json(Some("not json")).is_err());
        assert!(
            validate_schedule_json(Some(
                "{\"kind\":\"periodic\",\"every_secs\":0,\"jitter_secs\":0}"
            ))
            .is_err()
        );
        assert!(
            validate_schedule_json(Some(
                "{\"kind\":\"start_stop\",\"start\":\"22:00:00\",\"stop\":\"22:00:00\"}"
            ))
            .is_err()
        );
        assert!(validate_schedule_json(Some("{\"kind\":\"daily\",\"at\":\"06:30:00\"}")).is_ok());
    }
}
