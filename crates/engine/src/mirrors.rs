//! Mirror-URL selection and failover (Milestone 5, Build Prompt §13.2).
//!
//! Mirrors are alternate URLs for the same file. The engine tries them in
//! order — lowest `priority` first, then fewest `fails`, then most `bytes_ok`
//! — and plugs into the existing URL-refresh path via [`MirrorRefresher`],
//! so a primary that fails mid-download transparently continues on a mirror
//! with per-mirror stats recorded in `mirrors` (see `swiftfetch-store`).

use std::sync::Mutex;

use crate::engine::UrlRefresher;

/// One mirror candidate.
#[derive(Debug, Clone)]
pub struct MirrorCandidate {
    /// Alternate URL for the same file.
    pub url: String,
    /// Lower = tried first.
    pub priority: i64,
    /// Consecutive failure count.
    pub fails: i64,
    /// Bytes successfully fetched from this mirror.
    pub bytes_ok: i64,
}

impl MirrorCandidate {
    /// Creates a candidate with default stats.
    #[must_use]
    pub fn new(url: impl Into<String>, priority: i64) -> Self {
        Self {
            url: url.into(),
            priority,
            fails: 0,
            bytes_ok: 0,
        }
    }
}

/// Orders candidates for trying: priority, then fewest fails, then most
/// bytes delivered (stable for equal keys).
#[must_use]
pub fn order_mirrors(candidates: &[MirrorCandidate]) -> Vec<MirrorCandidate> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(a.fails.cmp(&b.fails))
            .then(b.bytes_ok.cmp(&a.bytes_ok))
    });
    sorted
}

/// A [`UrlRefresher`] that yields mirror URLs one at a time, in
/// [`order_mirrors`] order. Each call returns the next untried mirror, or
/// `None` when exhausted — exactly one attempt per mirror, no retry storms.
#[derive(Debug)]
pub struct MirrorRefresher {
    queue: Mutex<Vec<MirrorCandidate>>,
}

impl MirrorRefresher {
    /// Builds a refresher from candidates (ordered internally).
    #[must_use]
    pub fn new(candidates: &[MirrorCandidate]) -> Self {
        Self {
            queue: Mutex::new(order_mirrors(candidates)),
        }
    }

    /// Remaining mirrors.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl UrlRefresher for MirrorRefresher {
    fn refresh(&self) -> Option<String> {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue.is_empty() {
            return None;
        }
        Some(queue.remove(0).url)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn ordering_prefers_priority_then_health() {
        let cands = vec![
            MirrorCandidate {
                url: "b".into(),
                priority: 1,
                fails: 5,
                bytes_ok: 999,
            },
            MirrorCandidate {
                url: "a".into(),
                priority: 0,
                fails: 2,
                bytes_ok: 10,
            },
            MirrorCandidate {
                url: "c".into(),
                priority: 0,
                fails: 0,
                bytes_ok: 1,
            },
        ];
        let ordered = order_mirrors(&cands);
        let urls: Vec<_> = ordered.iter().map(|c| c.url.as_str()).collect();
        assert_eq!(urls, vec!["c", "a", "b"]);
    }

    #[test]
    fn refresher_yields_each_mirror_once() {
        let r =
            MirrorRefresher::new(&[MirrorCandidate::new("m1", 1), MirrorCandidate::new("m2", 0)]);
        assert_eq!(r.refresh().as_deref(), Some("m2"));
        assert_eq!(r.refresh().as_deref(), Some("m1"));
        assert_eq!(r.refresh(), None);
        assert_eq!(r.remaining(), 0);
    }
}
