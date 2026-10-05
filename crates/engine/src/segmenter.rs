//! Segment planning and splitting math (pure functions, heavily tested).

/// Default minimum segment size: segments never shrink below this when being
/// split, so tiny tail segments cannot spawn useless connections.
pub const MIN_SEGMENT_BYTES: u64 = 1024 * 1024;

/// One planned byte range (inclusive bounds) of a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPlan {
    /// Zero-based segment index within the job.
    pub idx: u32,
    /// Absolute byte offset of the first byte (inclusive).
    pub start: u64,
    /// Absolute byte offset of the last byte (inclusive).
    pub end: u64,
}

impl SegmentPlan {
    /// Number of bytes in this segment.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Whether the segment covers no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end < self.start
    }
}

/// Splits `[0, total_len)` into at most `max_conns` segments, each at least
/// `min_seg` bytes where possible.
///
/// A zero-length file yields no segments (the engine downloads it with a
/// single empty GET). A single-byte file yields exactly one segment.
#[must_use]
pub fn plan_segments(total_len: u64, max_conns: u8, min_seg: u64) -> Vec<SegmentPlan> {
    if total_len == 0 {
        return Vec::new();
    }
    let max_conns = u64::from(max_conns.max(1));
    let min_seg = min_seg.max(1);
    let by_min = total_len.div_ceil(min_seg);
    let count = max_conns.min(by_min).max(1);
    let base = total_len / count;
    let remainder = total_len % count;
    let mut plans = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    let mut start = 0;
    for i in 0..count {
        let extra = u64::from(i < remainder);
        let len = base + extra;
        plans.push(SegmentPlan {
            idx: u32::try_from(i).unwrap_or(u32::MAX),
            start,
            end: start + len - 1,
        });
        start += len;
    }
    plans
}

/// Returns the byte offset at which a new segment should start when splitting
/// `seg` after `done` bytes have been confirmed, or `None` when the remaining
/// range is too small to be worth splitting (`remaining <= 2 * min_seg`).
///
/// The split keeps the first half for the existing connection and hands
/// `[mid + 1, end]` to the new one.
#[must_use]
pub fn split_point(seg: &SegmentPlan, done: u64, min_seg: u64) -> Option<u64> {
    let remaining = seg.end + 1 - (seg.start + done);
    if remaining <= 2 * min_seg.max(1) {
        return None;
    }
    let mid = seg.start + done + remaining / 2;
    Some(mid + 1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn coverage(plans: &[SegmentPlan]) -> u64 {
        plans.iter().map(SegmentPlan::len).sum()
    }

    #[test]
    fn plans_cover_file_exactly_without_overlap() {
        for total in [
            0,
            1,
            999,
            1024,
            1024 * 1024,
            5 * 1024 * 1024 + 7,
            u64::from(u32::MAX),
        ] {
            for conns in [1u8, 3, 8, 32] {
                let plans = plan_segments(total, conns, MIN_SEGMENT_BYTES);
                assert_eq!(
                    coverage(&plans),
                    total,
                    "coverage mismatch for {total}/{conns}"
                );
                for w in plans.windows(2) {
                    assert_eq!(
                        w[1].start,
                        w[0].end + 1,
                        "gap/overlap between {} and {}",
                        w[0].idx,
                        w[1].idx
                    );
                }
                assert!(plans.len() <= usize::from(conns.max(1)));
                for p in &plans {
                    assert!(p.end >= p.start || total == 0);
                }
            }
        }
    }

    #[test]
    fn respects_min_segment_size_where_possible() {
        // 10 MiB / 8 conns with 1 MiB min → 8 segments of ~1.25 MiB.
        let plans = plan_segments(10 * MIN_SEGMENT_BYTES, 8, MIN_SEGMENT_BYTES);
        assert_eq!(plans.len(), 8);
        for p in &plans {
            assert!(p.len() >= MIN_SEGMENT_BYTES, "segment {p:?} below minimum");
        }

        // 4 MiB / 8 conns → min(8, ceil(4/1)) = 4 segments.
        let plans = plan_segments(4 * MIN_SEGMENT_BYTES, 8, MIN_SEGMENT_BYTES);
        assert_eq!(plans.len(), 4);
    }

    #[test]
    fn empty_file_plans_nothing() {
        assert_eq!(plan_segments(0, 8, MIN_SEGMENT_BYTES).len(), 0);
    }

    #[test]
    fn split_point_respects_floor() {
        let seg = SegmentPlan {
            idx: 0,
            start: 0,
            end: 5 * MIN_SEGMENT_BYTES - 1,
        };
        // 5 MiB remaining > 2 MiB floor → split at 2.5 MiB + 1.
        let mid = split_point(&seg, 0, MIN_SEGMENT_BYTES).expect("should split");
        assert_eq!(mid, 2 * MIN_SEGMENT_BYTES + MIN_SEGMENT_BYTES / 2 + 1);
        // Remaining exactly at floor → no split.
        let small = SegmentPlan {
            idx: 0,
            start: 0,
            end: 2 * MIN_SEGMENT_BYTES - 1,
        };
        assert_eq!(split_point(&small, 0, MIN_SEGMENT_BYTES), None);
        // Done bytes shrink the remaining range first.
        assert_eq!(
            split_point(
                &SegmentPlan {
                    idx: 0,
                    start: 0,
                    end: 3 * MIN_SEGMENT_BYTES - 1
                },
                MIN_SEGMENT_BYTES,
                MIN_SEGMENT_BYTES
            ),
            None,
            "2 MiB remaining must not split"
        );
    }
}
