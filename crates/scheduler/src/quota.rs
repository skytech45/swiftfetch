//! Quota accounting: rolling hourly + calendar-day byte windows
//! (PRD "hourly quotas"). Pure logic — the app persists the ledger snapshot
//! through the settings table and feeds it byte deltas from progress events.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Quota limits in bytes; `None` (or `Some(0)`) disables the window.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct QuotaConfig {
    /// Max bytes per rolling 1-hour window.
    pub hourly_limit: Option<u64>,
    /// Max bytes per calendar day (UTC).
    pub daily_limit: Option<u64>,
}

impl QuotaConfig {
    /// `true` when at least one window has a nonzero limit.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.hourly_limit.is_some_and(|v| v > 0) || self.daily_limit.is_some_and(|v| v > 0)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Window {
    /// Window start (hour window start / day start).
    start: DateTime<Utc>,
    /// Bytes counted inside the current window.
    bytes: u64,
}

/// Persisted byte counters for the two quota windows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct QuotaLedger {
    hour: Option<Window>,
    day: Option<Window>,
}

/// Whether downloads may proceed right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaVerdict {
    /// Under every configured limit.
    Allowed,
    /// A limit is exhausted; downloads pause until `resets_at`.
    Exhausted {
        /// When the exhausted window rolls over.
        resets_at: DateTime<Utc>,
    },
}

impl QuotaLedger {
    /// Records `bytes` transferred at time `now`, rolling windows forward
    /// when they expire. Bytes that belong to an already-expired window
    /// start a fresh window instead of overflowing the old one.
    pub fn add(&mut self, bytes: u64, now: DateTime<Utc>) {
        let hour = self.hour.get_or_insert(Window {
            start: now,
            bytes: 0,
        });
        if now - hour.start >= Duration::hours(1) {
            *hour = Window {
                start: now,
                bytes: 0,
            };
        }
        hour.bytes = hour.bytes.saturating_add(bytes);

        let day = self.day.get_or_insert(Window {
            start: now,
            bytes: 0,
        });
        if now.date_naive() != day.start.date_naive() {
            *day = Window {
                start: now,
                bytes: 0,
            };
        }
        day.bytes = day.bytes.saturating_add(bytes);
    }

    /// Evaluates the current limits at `now` (expired windows read as 0).
    #[must_use]
    pub fn verdict(&self, config: &QuotaConfig, now: DateTime<Utc>) -> QuotaVerdict {
        let exhausted = |limit: Option<u64>, window: Option<Window>, len: Duration| {
            let Some(limit) = limit.filter(|l| *l > 0) else {
                return false;
            };
            let bytes = window
                .filter(|w| now - w.start < len)
                .map_or(0, |w| w.bytes);
            bytes >= limit
        };
        if exhausted(config.hourly_limit, self.hour, Duration::hours(1)) {
            let resets_at = self.hour.map_or(now, |w| w.start + Duration::hours(1));
            return QuotaVerdict::Exhausted { resets_at };
        }
        if exhausted(config.daily_limit, self.day, Duration::hours(24)) {
            // Daily reset is the start of the next UTC day.
            let resets_at = now
                .date_naive()
                .succ_opt()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map_or(now, |t| t.and_utc());
            return QuotaVerdict::Exhausted { resets_at };
        }
        QuotaVerdict::Allowed
    }

    /// Bytes counted in the current (or most recent) windows, for UI display.
    #[must_use]
    pub fn usage(&self, now: DateTime<Utc>) -> (u64, u64) {
        let hour = self
            .hour
            .filter(|w| now - w.start < Duration::hours(1))
            .map_or(0, |w| w.bytes);
        let day = self
            .day
            .filter(|w| now.date_naive() == w.start.date_naive())
            .map_or(0, |w| w.bytes);
        (hour, day)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + Duration::minutes(minutes)
    }

    #[test]
    fn hourly_window_gates_and_resets() {
        let config = QuotaConfig {
            hourly_limit: Some(100),
            daily_limit: None,
        };
        let mut ledger = QuotaLedger::default();
        ledger.add(60, t(0));
        ledger.add(40, t(30));
        assert_eq!(
            ledger.verdict(&config, t(31)),
            QuotaVerdict::Exhausted { resets_at: t(60) }
        );
        // Window rolled over: allowed again.
        assert_eq!(ledger.verdict(&config, t(61)), QuotaVerdict::Allowed);
        // New window starts from zero, not from the old 100.
        ledger.add(50, t(61));
        assert_eq!(ledger.verdict(&config, t(62)), QuotaVerdict::Allowed);
    }

    #[test]
    fn daily_window_gates_until_next_utc_day() {
        let config = QuotaConfig {
            hourly_limit: None,
            daily_limit: Some(1000),
        };
        let mut ledger = QuotaLedger::default();
        ledger.add(1000, t(0));
        match ledger.verdict(&config, t(10)) {
            QuotaVerdict::Exhausted { resets_at } => {
                assert_eq!(resets_at, t(24 * 60));
            }
            QuotaVerdict::Allowed => panic!("expected exhausted"),
        }
    }

    #[test]
    fn disabled_limits_never_gate() {
        let config = QuotaConfig::default();
        let mut ledger = QuotaLedger::default();
        ledger.add(u64::MAX, t(0));
        assert_eq!(ledger.verdict(&config, t(1)), QuotaVerdict::Allowed);
        // Zero limits are the same as off.
        let config = QuotaConfig {
            hourly_limit: Some(0),
            daily_limit: Some(0),
        };
        assert_eq!(ledger.verdict(&config, t(2)), QuotaVerdict::Allowed);
    }

    #[test]
    fn usage_reports_current_windows() {
        let mut ledger = QuotaLedger::default();
        ledger.add(70, t(0));
        assert_eq!(ledger.usage(t(10)), (70, 70));
        let (hour, day) = ledger.usage(t(90)); // hour expired, day still on
        assert_eq!(hour, 0);
        assert_eq!(day, 70);
    }
}
