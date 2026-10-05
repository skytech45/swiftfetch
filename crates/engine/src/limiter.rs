//! Token-bucket speed limiting: one global bucket plus one bucket per active
//! job. Buckets are fair (FIFO waiters), rate changes apply live, and a rate
//! of 0 means unlimited. Uses `tokio::time::Instant` so tests can run on a
//! paused clock.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, oneshot};
use tokio::time::Instant;

/// Byte tokens per second; `0` means unlimited.
pub type BytesPerSecond = u64;

struct Waiter {
    amount: u64,
    tx: oneshot::Sender<()>,
}

struct BucketState {
    rate_bps: BytesPerSecond,
    burst: u64,
    tokens: f64,
    last_refill: Instant,
    queue: VecDeque<Waiter>,
}

/// A single token bucket (global or per-job scope). Cheap to clone.
#[derive(Clone)]
pub struct Bucket {
    state: Arc<Mutex<BucketState>>,
}

impl Bucket {
    /// Creates a bucket with the given rate (bytes/s). Burst capacity is 2 s
    /// worth of bandwidth with a 512 KiB floor (above the largest chunk any
    /// segment acquires, so a request is always eventually grantable); the
    /// bucket starts empty so pacing applies from the first chunk.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // burst fits f64 for bandwidth math
    pub fn new(rate_bps: BytesPerSecond) -> Self {
        let burst = Self::burst_for(rate_bps);
        Self {
            state: Arc::new(Mutex::new(BucketState {
                rate_bps,
                burst,
                tokens: 0.0,
                last_refill: Instant::now(),
                queue: VecDeque::new(),
            })),
        }
    }

    fn burst_for(rate_bps: BytesPerSecond) -> u64 {
        // 2 s of bandwidth, never below the largest possible chunk acquire
        // (disk writer chunks are capped at 256 KiB) — a request larger than
        // the burst could otherwise never be granted.
        (rate_bps.saturating_mul(2)).max(512 * 1024)
    }

    #[allow(clippy::cast_precision_loss)] // bandwidth values fit f64
    fn burst_of(state: &BucketState) -> f64 {
        state.burst as f64
    }

    /// Refills tokens based on elapsed time, then grants as many FIFO waiters
    /// as the tokens allow.
    #[allow(clippy::cast_precision_loss)] // bandwidth values fit f64
    fn refill_and_pump(state: &mut BucketState) {
        let now = Instant::now();
        if state.rate_bps > 0 {
            let elapsed = now
                .saturating_duration_since(state.last_refill)
                .as_secs_f64();
            if elapsed > 0.0 {
                state.tokens =
                    (state.tokens + elapsed * state.rate_bps as f64).min(Self::burst_of(state));
            }
        }
        state.last_refill = now;
        loop {
            let grantable = match state.queue.front() {
                None => false,
                Some(waiter) => state.rate_bps == 0 || state.tokens >= waiter.amount as f64,
            };
            if !grantable {
                break;
            }
            if let Some(waiter) = state.queue.pop_front() {
                if state.rate_bps > 0 {
                    state.tokens -= waiter.amount as f64;
                }
                let _ = waiter.tx.send(());
            }
        }
    }

    /// When the front waiter could next be granted, or `None` when the queue
    /// is empty or the rate is unlimited.
    #[allow(clippy::cast_precision_loss)] // bandwidth values fit f64
    fn next_wake(state: &BucketState) -> Option<Instant> {
        let front = state.queue.front()?;
        if state.rate_bps == 0 {
            return Some(Instant::now());
        }
        let missing = front.amount as f64 - state.tokens;
        if missing <= 0.0 {
            return Some(Instant::now());
        }
        let secs = (missing / state.rate_bps as f64).max(0.001);
        Some(Instant::now() + Duration::from_secs_f64(secs))
    }

    fn spawn_pump(&self, wake_at: Instant) {
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            tokio::time::sleep_until(wake_at).await;
            loop {
                let next = {
                    let mut guard = state.lock().await;
                    Self::refill_and_pump(&mut guard);
                    Self::next_wake(&guard)
                };
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => break,
                }
            }
        });
    }

    /// Enqueues a waiter and makes sure a pump is scheduled for it.
    async fn enqueue(&self, amount: u64) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        let wake_at = {
            let mut guard = self.state.lock().await;
            guard.queue.push_back(Waiter { amount, tx });
            Self::refill_and_pump(&mut guard);
            Self::next_wake(&guard)
        };
        if let Some(at) = wake_at {
            self.spawn_pump(at);
        }
        rx
    }

    /// Acquires `amount` byte tokens, waiting fairly until they are available.
    pub async fn acquire(&self, amount: u64) {
        let rx = self.enqueue(amount).await;
        let _ = rx.await;
    }

    /// Changes the rate live; applies from the next refill (≤ 1 tick).
    pub async fn set_rate(&self, rate_bps: BytesPerSecond) {
        let mut guard = self.state.lock().await;
        guard.rate_bps = rate_bps;
        guard.burst = Self::burst_for(rate_bps);
        guard.tokens = guard.tokens.min(Self::burst_of(&guard));
    }
}

/// Owns the global bucket (rate 0 = unlimited); per-job buckets are created
/// by the supervisor.
pub struct SpeedLimiter {
    global: Bucket,
}

impl SpeedLimiter {
    /// Creates the limiter; `None` means no global cap.
    #[must_use]
    pub fn new(global_rate_bps: Option<BytesPerSecond>) -> Self {
        Self {
            global: Bucket::new(global_rate_bps.unwrap_or(0)),
        }
    }

    /// The global bucket.
    #[must_use]
    pub fn global(&self) -> &Bucket {
        &self.global
    }

    /// Creates a per-job bucket (or `None` when uncapped).
    #[must_use]
    pub fn job_bucket(&self, rate_bps: Option<BytesPerSecond>) -> Option<Bucket> {
        rate_bps.map(Bucket::new)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn unlimited_rate_passes_through() {
        let bucket = Bucket::new(0);
        let start = Instant::now();
        bucket.acquire(10 * 1024 * 1024).await;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn paced_rate_takes_expected_time() {
        let bucket = Bucket::new(100_000); // 100 KiB/s
        let start = Instant::now();
        bucket.acquire(300_000).await; // 3 s worth
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_secs(2) && elapsed <= Duration::from_secs(4),
            "300 KiB at 100 KiB/s should take ~3 s, took {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fifo_waiters_are_granted_in_order() {
        let bucket = Bucket::new(100_000);
        let mut rxs = Vec::new();
        for _ in 0..5 {
            rxs.push(bucket.enqueue(100_000).await);
        }
        // Advance time in steps; grants must arrive in queue order.
        let mut granted = Vec::new();
        for _ in 0..5 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            for (i, rx) in rxs.iter_mut().enumerate() {
                if granted.contains(&i) {
                    continue;
                }
                if rx.try_recv().is_ok() {
                    granted.push(i);
                }
            }
        }
        assert_eq!(granted, vec![0, 1, 2, 3, 4], "grants must be FIFO");
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_wakes_itself_without_new_arrivals() {
        // A lone waiter for a large amount must still be granted once enough
        // time passes — this exercises the self-rescheduling pump.
        let bucket = Bucket::new(100_000);
        let start = Instant::now();
        bucket.acquire(500_000).await; // 5 s worth
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(4), "took only {elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn rate_change_applies_live() {
        let bucket = Bucket::new(100_000);
        bucket.set_rate(0).await; // unlimited now
        let start = Instant::now();
        bucket.acquire(1024 * 1024).await;
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
