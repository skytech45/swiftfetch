//! Job supervision: one task per download orchestrating segment tasks, the
//! disk writer, dynamic rebalancing (split-largest + stall reassignment),
//! pause/cancel, URL refresh, verification and the final atomic rename.
//!
//! Concurrency contract (docs/system-design.md §8): one tokio task per
//! segment, a single disk writer per job, bounded channels for backpressure,
//! and journal checkpoints at most once per second per segment.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use swiftfetch_store::StoreError;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::Instrument;

use crate::connection::{self, RequestContext};
use crate::disk::{DiskWriter, WriterMsg};
use crate::engine::{EngineInner, JobEvent, JobShared, JobState, SegRt, SegState, SegmentSnapshot};
use crate::errors::EngineError;
use crate::journal::{Journal, SegmentRow};
use crate::segment;
use crate::segmenter;

/// Write batches are chunked to this size for limiter/journal granularity.
pub(crate) const CHUNK: usize = 256 * 1024;

/// How the job execution starts.
pub(crate) enum StartMode {
    /// Fresh download: plan segments from the probe result.
    Fresh,
    /// Resume: reload the journaled plan, clamp it to disk, re-verify the
    /// entity.
    Resume,
}

/// What the entity re-verification decided.
enum EntityDecision {
    Keep,
    Restart,
    /// The server cannot resume at all — restart as a single connection.
    SingleConnection,
}

/// Why the worker loop ended.
enum Outcome {
    Done,
    Paused,
    Cancelled,
    Failed(EngineError),
    /// Entity changed on the server: reset and restart.
    Restart,
}

/// Exponential backoff for segment retries (quadratic, capped at 8 s).
pub(crate) fn backoff(base: Duration, retry: u32) -> Duration {
    let secs = base.as_secs_f64() * f64::from(retry) * f64::from(retry);
    Duration::from_secs_f64(secs.min(8.0))
}

/// Runs blocking journal mutations off the async worker threads.
pub(crate) async fn journal_do<T, F>(journal: &Journal, f: F) -> Result<T, EngineError>
where
    F: FnOnce(&Journal) -> Result<T, StoreError> + Send + 'static,
    T: Send + 'static,
{
    let j = journal.clone();
    tokio::task::spawn_blocking(move || f(&j))
        .await
        .map_err(|err| EngineError::Config(format!("journal task: {err}")))?
        .map_err(EngineError::from)
}

/// Builds the in-memory segment mirror from a journal row.
pub(crate) fn seg_from_row(row: &SegmentRow) -> Arc<SegRt> {
    let state = match row.state.as_str() {
        "done" => SegState::Done,
        "stalled" => SegState::Stalled,
        "failed" => SegState::Failed,
        "active" => SegState::Active,
        _ => SegState::Queued,
    };
    Arc::new(SegRt {
        idx: u32::try_from(row.idx).unwrap_or(0),
        start: row.start,
        end: Mutex::new(row.end),
        done: AtomicU64::new(row.done),
        state: Mutex::new(state),
        ewma: AtomicU64::new(0),
        since: AtomicU64::new(0),
        slow_ticks: AtomicU32::new(0),
        retries: AtomicU32::new(0),
        active_since: Mutex::new(None),
        task: Mutex::new(None),
    })
}

/// Point-in-time snapshot for the UI and tests.
pub(crate) fn snapshot_of(shared: &JobShared) -> crate::engine::JobSnapshot {
    let segments = shared
        .segments
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|s| SegmentSnapshot {
            idx: s.idx,
            start: s.start,
            end: *s
                .end
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            done: s.done.load(Ordering::Relaxed),
            state: s
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_str()
                .to_owned(),
        })
        .collect();
    let meta = shared
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::engine::JobSnapshot {
        id: shared.id.clone(),
        state: shared.state_now(),
        url: meta.url.clone(),
        final_path: shared.final_path.clone(),
        total: meta.total_len,
        done: shared.done.load(Ordering::Relaxed),
        bps: f64::from_bits(shared.speed_bps.load(Ordering::Relaxed)),
        resume_cap: meta.resume_cap,
        segments,
    }
}

/// Runs a job to a terminal state.
pub(crate) async fn run(inner: Arc<EngineInner>, shared: Arc<JobShared>, mode: StartMode) {
    let span = tracing::info_span!(parent: None, "download", id = %shared.id);
    run_inner(inner, shared, mode).instrument(span).await;
}

#[allow(clippy::too_many_lines)] // the job state machine reads best in one body
async fn run_inner(inner: Arc<EngineInner>, shared: Arc<JobShared>, mode: StartMode) {
    shared.set_state(JobState::Downloading, None);
    let mut restarts = 0u32;
    let mut fresh_disk = matches!(mode, StartMode::Fresh);
    let mut need_plan = matches!(mode, StartMode::Fresh);

    loop {
        // 1. Disk setup: fresh downloads start with a clean partial file.
        if fresh_disk && let Err(err) = crate::disk::remove_partial(&shared.part_path) {
            fail_job(&inner, &shared, EngineError::Io(err)).await;
            return;
        }
        let total_now = shared
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_len;
        let prealloc = if fresh_disk { total_now } else { None };
        let (writer, writer_tx) = match DiskWriter::spawn(shared.part_path.clone(), prealloc).await
        {
            Ok(pair) => pair,
            Err(err) => {
                fail_job(&inner, &shared, EngineError::Io(err)).await;
                return;
            }
        };
        *shared
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(writer);

        // 2. Plan (fresh) or clamp + verify (resume).
        if need_plan {
            if let Err(err) = plan_fresh(&inner, &shared).await {
                fail_job(&inner, &shared, err).await;
                return;
            }
        } else {
            let file_len = std::fs::metadata(&shared.part_path).map_or(0, |m| m.len());
            let id = shared.id.clone();
            if let Err(err) =
                journal_do(&inner.journal, move |j| j.clamp_to_file(&id, file_len)).await
            {
                fail_job(&inner, &shared, err).await;
                return;
            }
            match verify_entity(&inner, &shared).await {
                Ok(EntityDecision::Keep) => {
                    let id = shared.id.clone();
                    match journal_do(&inner.journal, move |j| j.load_segments(&id)).await {
                        Ok(rows) => {
                            let done = rows.iter().map(|s| s.done).sum::<u64>();
                            *shared
                                .segments
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                rows.iter().map(seg_from_row).collect();
                            shared.done.store(done, Ordering::Relaxed);
                        }
                        Err(err) => {
                            fail_job(&inner, &shared, err).await;
                            return;
                        }
                    }
                }
                Ok(EntityDecision::SingleConnection) => {
                    shared.set_state(
                        JobState::Downloading,
                        Some("resume not supported by server".to_owned()),
                    );
                    if let Err(err) = replan_single_connection(&inner, &shared).await {
                        fail_job(&inner, &shared, err).await;
                        return;
                    }
                }
                Ok(EntityDecision::Restart) => {
                    restarts += 1;
                    if restarts > 2 {
                        fail_job(
                            &inner,
                            &shared,
                            EngineError::RangeMismatch {
                                url: shared.spec.url.clone(),
                            },
                        )
                        .await;
                        return;
                    }
                    reset_disk(&shared).await;
                    fresh_disk = true;
                    need_plan = true;
                    continue;
                }
                Err(err) => {
                    fail_job(&inner, &shared, err).await;
                    return;
                }
            }
        }

        // 3. Run to a terminal outcome.
        let outcome = worker_loop(&inner, &shared, writer_tx).await;
        match outcome {
            Outcome::Done => {
                finalize_done(&inner, &shared).await;
                return;
            }
            Outcome::Paused => {
                pause_job(&inner, &shared).await;
                return;
            }
            Outcome::Cancelled => {
                cancel_job(&inner, &shared).await;
                return;
            }
            Outcome::Failed(err) => {
                fail_job(&inner, &shared, err).await;
                return;
            }
            Outcome::Restart => {
                restarts += 1;
                if restarts > 2 {
                    fail_job(
                        &inner,
                        &shared,
                        EngineError::RangeMismatch {
                            url: shared.spec.url.clone(),
                        },
                    )
                    .await;
                    return;
                }
                reset_disk(&shared).await;
                fresh_disk = true;
                need_plan = true;
            }
        }
    }
}

/// Plans segments for a fresh (or reset) download and persists them.
async fn plan_fresh(inner: &Arc<EngineInner>, shared: &Arc<JobShared>) -> Result<(), EngineError> {
    let meta = shared
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let total = meta.total_len.unwrap_or(0);
    let rows: Vec<SegmentRow> = if total == 0 {
        Vec::new()
    } else if meta.total_len.is_none() {
        // Unknown length: one full-download segment (no Range header).
        vec![SegmentRow {
            idx: 0,
            start: 0,
            end: u64::MAX,
            done: 0,
            state: "queued".into(),
        }]
    } else if meta.resume_cap != crate::connection::ResumeCap::Ranges {
        // No range support: a single connection covers the whole file.
        vec![SegmentRow {
            idx: 0,
            start: 0,
            end: total - 1,
            done: 0,
            state: "queued".into(),
        }]
    } else {
        segmenter::plan_segments(total, shared.max_conns, shared.config.min_segment)
            .into_iter()
            .map(|p| SegmentRow {
                idx: i64::from(p.idx),
                start: p.start,
                end: p.end,
                done: 0,
                state: "queued".into(),
            })
            .collect()
    };
    let id = shared.id.clone();
    journal_do(&inner.journal, move |j| j.replace_segments(&id, &rows)).await?;
    let rows = {
        let id = shared.id.clone();
        journal_do(&inner.journal, move |j| j.load_segments(&id)).await?
    };
    *shared
        .segments
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        rows.iter().map(seg_from_row).collect();
    shared.done.store(0, Ordering::Relaxed);
    Ok(())
}

/// Downgrades to a single full-range connection after a no-resume detection.
async fn replan_single_connection(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
) -> Result<(), EngineError> {
    {
        let mut meta = shared
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        meta.resume_cap = crate::connection::ResumeCap::None;
    }
    let id = shared.id.clone();
    journal_do(&inner.journal, move |j| j.set_resume_cap(&id, "none")).await?;
    plan_fresh(inner, shared).await
}

/// Re-probes the server and decides whether the journaled plan is still
/// valid. Updates the stored identity either way.
async fn verify_entity(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
) -> Result<EntityDecision, EngineError> {
    let (url, ctx) = current_request_context(shared);
    let probe = connection::probe(&inner.client, &url, &ctx).await?;
    let (old_len, old_etag, old_modified) = {
        let meta = shared
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            meta.total_len,
            meta.etag.clone(),
            meta.last_modified.clone(),
        )
    };
    let length_changed = match (old_len, probe.total_len) {
        (Some(old), Some(new)) => old != new,
        (None, Some(_)) | (Some(_), None) => true,
        (None, None) => false,
    };
    let etag_changed = match (&old_etag, &probe.etag) {
        (Some(old), Some(new)) => old != new,
        (Some(_), None) => true,
        _ => false,
    };
    let modified_changed = match (&old_modified, &probe.last_modified) {
        (Some(old), Some(new)) => old != new,
        (Some(_), None) => true,
        _ => false,
    };

    let update = crate::journal::ProbeUpdate {
        total_len: probe.total_len,
        resume_cap: probe.resume_cap.as_str().to_owned(),
        etag: probe.etag.clone(),
        last_modified: probe.last_modified.clone(),
        content_type: probe.content_type.clone(),
    };
    let id = shared.id.clone();
    let url_clone = url.clone();
    journal_do(&inner.journal, move |j| {
        j.save_probe(&id, &url_clone, &update)
    })
    .await?;
    {
        let mut meta = shared
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        meta.total_len = probe.total_len;
        meta.etag = probe.etag;
        meta.last_modified = probe.last_modified;
        meta.resume_cap = probe.resume_cap;
        meta.expected_digest = probe.advertised_digest;
    }

    if probe.resume_cap == crate::connection::ResumeCap::None {
        return Ok(EntityDecision::SingleConnection);
    }
    if length_changed || etag_changed || modified_changed {
        return Ok(EntityDecision::Restart);
    }
    Ok(EntityDecision::Keep)
}

/// Refresh flow: swap the URL, re-probe, decide. Callers hold
/// `shared.refresh_lock` so concurrent segment failures coalesce.
async fn try_refresh_probe(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
) -> Result<EntityDecision, EngineError> {
    let attempts = shared.refreshes.fetch_add(1, Ordering::Relaxed);
    let current_url = shared
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .url
        .clone();
    if attempts >= shared.config.max_refresh {
        return Err(EngineError::UrlRefreshExhausted { url: current_url });
    }
    let Some(refresher) = shared.spec.url_refresher.as_ref() else {
        return Err(EngineError::UrlRefreshExhausted { url: current_url });
    };
    let Some(new_url) = refresher.refresh() else {
        return Err(EngineError::UrlRefreshExhausted { url: current_url });
    };
    tracing::info!(attempt = attempts + 1, "refreshing download URL");
    shared
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .url = new_url;
    verify_entity(inner, shared).await
}

fn current_request_context(shared: &Arc<JobShared>) -> (String, RequestContext) {
    let meta = shared
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (
        meta.url.clone(),
        RequestContext {
            referer: shared.spec.referer.clone(),
            cookies: shared.spec.cookies.clone(),
            user_agent: shared.spec.user_agent.clone(),
        },
    )
}

/// Stops tasks, flushes and drops the writer, deletes the partial file so the
/// next loop iteration starts clean.
async fn reset_disk(shared: &Arc<JobShared>) {
    stop_all_tasks(shared).await;
    let writer = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(writer) = writer {
        let _ = writer.finalize().await;
    }
    if let Err(err) = crate::disk::remove_partial(&shared.part_path) {
        tracing::warn!(error = %err, "could not remove partial during reset");
    }
}

/// Gracefully stops every segment task (they checkpoint on their way out).
async fn stop_all_tasks(shared: &Arc<JobShared>) {
    let handles: Vec<tokio::task::JoinHandle<()>> = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        segments
            .iter()
            .filter_map(|s| {
                s.task
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            })
            .collect()
    };
    for handle in handles {
        let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
    }
}

async fn worker_loop(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
) -> Outcome {
    let (seg_tx, mut seg_rx) = mpsc::channel::<segment::SegEvent>(64);

    let initial: Vec<u32> = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        segments
            .iter()
            .filter(|s| {
                *s.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    == SegState::Queued
            })
            .map(|s| s.idx)
            .collect()
    };
    for idx in initial {
        spawn_segment(inner, shared, writer_tx.clone(), seg_tx.clone(), idx);
    }

    let mut tick = tokio::time::interval(shared.config.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // first tick fires immediately
    let mut last_progress = Instant::now()
        .checked_sub(shared.config.progress_interval)
        .unwrap_or_else(Instant::now);

    loop {
        tokio::select! {
            () = shared.cancel.cancelled() => {
                stop_all_tasks(shared).await;
                return Outcome::Cancelled;
            }
            () = shared.pause.cancelled() => {
                stop_all_tasks(shared).await;
                return Outcome::Paused;
            }
            event = seg_rx.recv() => {
                let Some(event) = event else { break };
                match handle_seg_event(inner, shared, writer_tx.clone(), seg_tx.clone(), event).await {
                    EventResult::Continue => {}
                    EventResult::Outcome(outcome) => return outcome,
                }
            }
            _ = tick.tick() => {
                tick_update(inner, shared, writer_tx.clone(), seg_tx.clone()).await;
                shared.maybe_crash();
                let now = Instant::now();
                if now.duration_since(last_progress) >= shared.config.progress_interval {
                    last_progress = now;
                    let total = shared
                        .meta
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .total_len;
                    shared.emit(JobEvent::Progress {
                        done: shared.done.load(Ordering::Relaxed),
                        total,
                        bps: f64::from_bits(shared.speed_bps.load(Ordering::Relaxed)),
                    });
                }
                if all_segments_settled(shared) {
                    return Outcome::Done;
                }
            }
        }
    }
    Outcome::Done
}

enum EventResult {
    Continue,
    Outcome(Outcome),
}

async fn handle_seg_event(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<segment::SegEvent>,
    event: segment::SegEvent,
) -> EventResult {
    match event {
        segment::SegEvent::Done(idx) => {
            tracing::debug!(idx, "segment done");
            start_queued_or_split(inner, shared, writer_tx, seg_tx).await;
            EventResult::Continue
        }
        segment::SegEvent::Failed {
            idx,
            err,
            url_at_failure,
        } => {
            let _current_url = shared
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .url
                .clone();
            let refresher_available = shared.spec.url_refresher.is_some();
            if err.is_refresh_worthy() && refresher_available {
                // Coalesce concurrent failures: another segment may have
                // already swapped the URL for this round.
                let _guard = shared.refresh_lock.lock().await;
                let current_url = shared
                    .meta
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .url
                    .clone();
                if current_url != url_at_failure {
                    requeue_segment(inner, shared, writer_tx, seg_tx, idx).await;
                    return EventResult::Continue;
                }
                match try_refresh_probe(inner, shared).await {
                    Ok(EntityDecision::Keep) => {
                        requeue_segment(inner, shared, writer_tx, seg_tx, idx).await;
                        EventResult::Continue
                    }
                    Ok(EntityDecision::Restart | EntityDecision::SingleConnection) => {
                        EventResult::Outcome(Outcome::Restart)
                    }
                    Err(e) => EventResult::Outcome(Outcome::Failed(e)),
                }
            } else {
                EventResult::Outcome(Outcome::Failed(err))
            }
        }
        segment::SegEvent::StaleRange => {
            // Journal offsets are stale; re-verify identity, then replan.
            let _ = verify_entity(inner, shared).await;
            EventResult::Outcome(Outcome::Restart)
        }
        segment::SegEvent::EntityChanged(idx) => match verify_entity(inner, shared).await {
            Ok(EntityDecision::Keep) => {
                // Entity identical despite the odd response — retry the segment.
                requeue_segment(inner, shared, writer_tx, seg_tx, idx).await;
                EventResult::Continue
            }
            Ok(EntityDecision::Restart | EntityDecision::SingleConnection) => {
                EventResult::Outcome(Outcome::Restart)
            }
            Err(err) => EventResult::Outcome(Outcome::Failed(err)),
        },
    }
}

/// One supervisor tick: EWMA accounting, stall reassignment, slow-segment
/// splitting.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "EWMA/threshold math; all values are small and bounded"
)]
async fn tick_update(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<segment::SegEvent>,
) {
    let tick_secs = shared.config.tick.as_secs_f64();
    let mut job_bps = 0.0f64;
    let mut mean = 0.0f64;
    let mut active = 0usize;
    let stall_victims: Vec<u32>;
    {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut victims = Vec::new();
        let now = Instant::now();
        for seg in segments.iter() {
            let is_active = *seg
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                == SegState::Active;
            let delta = seg.since.swap(0, Ordering::Relaxed);
            if is_active {
                let instant_bps = delta as f64 / tick_secs;
                let old = f64::from_bits(seg.ewma.load(Ordering::Relaxed));
                let ewma = 0.7 * old + 0.3 * instant_bps;
                seg.ewma.store(ewma.to_bits(), Ordering::Relaxed);
                job_bps += ewma;
                mean += ewma;
                active += 1;
                let since_start = seg
                    .active_since
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .map(|t| now.duration_since(t))
                    .unwrap_or_default();
                if ewma < shared.config.stall_floor_bps as f64
                    && since_start > shared.config.stall_after
                {
                    victims.push(seg.idx);
                }
            } else {
                seg.ewma.store(0, Ordering::Relaxed);
            }
        }
        stall_victims = victims;
    }
    if active > 0 {
        mean /= active as f64;
    }
    shared.speed_bps.store(job_bps.to_bits(), Ordering::Relaxed);

    for idx in stall_victims {
        reassign_stalled(inner, shared, &writer_tx, &seg_tx, idx).await;
    }

    if !shared.config.rebalancing_enabled {
        return;
    }
    // Slow-segment splitting: below 50% of the mean for a full slow window.
    let threshold = (shared.config.slow_window.as_secs_f64() / tick_secs)
        .ceil()
        .max(1.0) as u32;
    let split_candidate = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut candidate: Option<(u32, u64)> = None;
        for seg in segments.iter() {
            let is_active = *seg
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                == SegState::Active;
            if !is_active || mean <= 0.0 {
                continue;
            }
            let ewma = f64::from_bits(seg.ewma.load(Ordering::Relaxed));
            if ewma < 0.5 * mean {
                let ticks = seg.slow_ticks.fetch_add(1, Ordering::Relaxed) + 1;
                if ticks >= threshold {
                    let remaining = seg.remaining();
                    if remaining > 2 * shared.config.min_segment
                        && candidate.as_ref().is_none_or(|(_, best)| remaining > *best)
                    {
                        candidate = Some((seg.idx, remaining));
                    }
                }
            } else {
                seg.slow_ticks.store(0, Ordering::Relaxed);
            }
        }
        candidate
    };
    if let Some((idx, _)) = split_candidate {
        split_segment(inner, shared, &writer_tx, &seg_tx, idx).await;
    }
}

fn all_segments_settled(shared: &Arc<JobShared>) -> bool {
    let segments = shared
        .segments
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    segments.iter().all(|s| {
        matches!(
            *s.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            SegState::Done | SegState::Stalled
        )
    })
}

/// A connection freed up: start queued work, or split the largest remaining
/// segment into the free slot.
async fn start_queued_or_split(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<segment::SegEvent>,
) {
    let queued = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        segments
            .iter()
            .find(|s| {
                *s.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    == SegState::Queued
            })
            .map(|s| s.idx)
    };
    if let Some(idx) = queued {
        spawn_segment(inner, shared, writer_tx, seg_tx, idx);
        return;
    }
    if !shared.config.rebalancing_enabled {
        return;
    }
    let (active_count, candidate) = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active = u32::try_from(
            segments
                .iter()
                .filter(|s| {
                    *s.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        == SegState::Active
                })
                .count(),
        )
        .unwrap_or(u32::MAX);
        let candidate = segments
            .iter()
            .filter(|s| {
                *s.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    == SegState::Active
            })
            .max_by_key(|s| s.remaining())
            .map(|s| (s.idx, s.remaining()));
        (active, candidate)
    };
    if active_count >= u32::from(shared.max_conns) {
        return;
    }
    if let Some((idx, remaining)) = candidate
        && remaining > 2 * shared.config.min_segment
    {
        split_segment(inner, shared, &writer_tx, &seg_tx, idx).await;
    }
}

/// Splits a segment in half, handing the second half to a new connection.
async fn split_segment(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: &mpsc::Sender<WriterMsg>,
    seg_tx: &mpsc::Sender<segment::SegEvent>,
    idx: u32,
) {
    let (seg, old_end, _done, mid, new_start, new_idx) = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(seg) = segments.iter().find(|s| s.idx == idx) else {
            return;
        };
        let is_active = *seg
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            == SegState::Active;
        if !is_active {
            return;
        }
        let old_end = *seg
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let done = seg.done.load(Ordering::Relaxed);
        let remaining = old_end.saturating_sub(seg.start + done - 1);
        if remaining <= 2 * shared.config.min_segment {
            return;
        }
        let mid = seg.start + done + remaining / 2;
        let new_start = mid + 1;
        let new_idx = next_segment_idx(&segments);
        (Arc::clone(seg), old_end, done, mid, new_start, new_idx)
    };

    let id = shared.id.clone();
    let journal = inner.journal.clone();
    if let Err(err) = journal_do(&journal, move |j| {
        j.shrink_segment(&id, i64::from(idx), mid)?;
        j.append_segment(
            &id,
            &SegmentRow {
                idx: i64::from(new_idx),
                start: new_start,
                end: old_end,
                done: 0,
                state: "queued".into(),
            },
        )
    })
    .await
    {
        tracing::error!(error = %err, "split journal update failed");
        return;
    }
    *seg.end
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = mid;
    let new_seg = seg_from_row(&SegmentRow {
        idx: i64::from(new_idx),
        start: new_start,
        end: old_end,
        done: 0,
        state: "queued".into(),
    });
    shared
        .segments
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Arc::clone(&new_seg));
    tracing::info!(
        old = idx,
        new = new_idx,
        mid,
        "split segment (work stealing)"
    );
    spawn_segment_idx(inner, shared, writer_tx.clone(), seg_tx.clone(), &new_seg);
}

/// Moves a stalled segment's remaining range to a fresh queued segment and
/// starts a connection on it.
async fn reassign_stalled(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: &mpsc::Sender<WriterMsg>,
    seg_tx: &mpsc::Sender<segment::SegEvent>,
    idx: u32,
) {
    let (seg, end, _done, new_start, new_idx) = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(seg) = segments.iter().find(|s| s.idx == idx) else {
            return;
        };
        if let Some(task) = seg
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        let end = *seg
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let done = seg.done.load(Ordering::Relaxed);
        let new_start = seg.start + done;
        if end < new_start {
            return;
        }
        let new_idx = next_segment_idx(&segments);
        (Arc::clone(seg), end, done, new_start, new_idx)
    };

    let id = shared.id.clone();
    let journal = inner.journal.clone();
    if let Err(err) = journal_do(&journal, move |j| {
        j.set_segment_state(&id, i64::from(idx), "stalled")?;
        j.append_segment(
            &id,
            &SegmentRow {
                idx: i64::from(new_idx),
                start: new_start,
                end,
                done: 0,
                state: "queued".into(),
            },
        )
    })
    .await
    {
        tracing::error!(error = %err, "stall reassignment journal update failed");
        return;
    }
    *seg.state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = SegState::Stalled;
    let new_seg = seg_from_row(&SegmentRow {
        idx: i64::from(new_idx),
        start: new_start,
        end,
        done: 0,
        state: "queued".into(),
    });
    // The replacement inherits the retry count so a dead range cannot
    // retry forever through reassignment.
    new_seg
        .retries
        .store(seg.retries.load(Ordering::Relaxed), Ordering::Relaxed);
    shared
        .segments
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Arc::clone(&new_seg));
    tracing::info!(old = idx, new = new_idx, "stalled segment reassigned");
    spawn_segment_idx(inner, shared, writer_tx.clone(), seg_tx.clone(), &new_seg);
}

/// Puts a segment back into the queue and starts it again (used after URL
/// refreshes and spurious entity-change reports).
async fn requeue_segment(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<segment::SegEvent>,
    idx: u32,
) {
    let seg = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(seg) = segments.iter().find(|s| s.idx == idx) else {
            return;
        };
        *seg.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = SegState::Queued;
        seg.retries.store(0, Ordering::Relaxed);
        Arc::clone(seg)
    };
    let id = shared.id.clone();
    let journal = inner.journal.clone();
    if let Err(err) = journal_do(&journal, move |j| {
        j.set_segment_state(&id, i64::from(idx), "queued")
    })
    .await
    {
        tracing::error!(error = %err, "requeue journal update failed");
        return;
    }
    spawn_segment_idx(inner, shared, writer_tx, seg_tx, &seg);
}

fn next_segment_idx(segments: &[Arc<SegRt>]) -> u32 {
    segments.iter().map(|s| s.idx).max().unwrap_or(0) + 1
}

fn spawn_segment(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<segment::SegEvent>,
    idx: u32,
) {
    let seg = {
        let segments = shared
            .segments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match segments.iter().find(|s| s.idx == idx) {
            Some(s) => Arc::clone(s),
            None => return,
        }
    };
    spawn_segment_idx(inner, shared, writer_tx, seg_tx, &seg);
}

fn spawn_segment_idx(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<segment::SegEvent>,
    seg: &Arc<SegRt>,
) {
    if shared.pause.is_cancelled() || shared.cancel.is_cancelled() {
        return;
    }
    *seg.state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = SegState::Active;
    *seg.active_since
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    let task = tokio::spawn(segment::run_segment(
        Arc::clone(inner),
        Arc::clone(shared),
        writer_tx,
        seg_tx,
        Arc::clone(seg),
    ));
    *seg.task
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
}

async fn finalize_done(inner: &Arc<EngineInner>, shared: &Arc<JobShared>) {
    shared.set_state(JobState::Verifying, None);
    let writer_taken = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let (sha, md5) = if let Some(writer) = writer_taken {
        match writer.finalize().await {
            Ok(digests) => digests,
            Err(err) => {
                fail_job(inner, shared, EngineError::Io(err)).await;
                return;
            }
        }
    } else {
        fail_job(
            inner,
            shared,
            EngineError::Io(std::io::Error::other("writer missing at finalize")),
        )
        .await;
        return;
    };

    let meta = shared
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(expected) = meta.total_len {
        let actual = std::fs::metadata(&shared.part_path).map_or(0, |m| m.len());
        if actual != expected {
            fail_job(
                inner,
                shared,
                EngineError::SizeMismatch {
                    path: shared.part_path.clone(),
                    expected,
                    actual,
                },
            )
            .await;
            return;
        }
    }
    if let Some(expected) = meta.expected_digest {
        let computed = match expected {
            crate::checksum::Digest::Sha256(_) => sha,
            crate::checksum::Digest::Md5(_) => md5,
        };
        if computed != expected {
            fail_job(
                inner,
                shared,
                EngineError::ChecksumMismatch {
                    path: shared.part_path.clone(),
                },
            )
            .await;
            return;
        }
    }

    if let Err(err) = std::fs::rename(&shared.part_path, &shared.final_path) {
        fail_job(inner, shared, EngineError::Io(err)).await;
        return;
    }
    let size = std::fs::metadata(&shared.final_path).map(|m| m.len()).ok();
    let url = meta.url.clone();
    let final_path = shared.final_path.clone();
    if let Err(err) = journal_do(&inner.journal, move |j| {
        j.record_history(&url, &final_path, size, None)
    })
    .await
    {
        tracing::warn!(error = %err, "history row failed");
    }
    let id = shared.id.clone();
    let _ = journal_do(&inner.journal, move |j| j.set_state(&id, "done", None)).await;
    shared.set_state(JobState::Done, None);
    shared.emit(JobEvent::Completed {
        path: shared.final_path.clone(),
    });
    tracing::info!(path = %shared.final_path.display(), "download completed");
}

async fn pause_job(inner: &Arc<EngineInner>, shared: &Arc<JobShared>) {
    let writer = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(writer) = writer {
        if let Err(err) = writer.flush().await {
            tracing::warn!(error = %err, "flush on pause failed");
        }
        *shared
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(writer);
    }
    let id = shared.id.clone();
    let _ = journal_do(&inner.journal, move |j| j.set_state(&id, "paused", None)).await;
    shared.set_state(JobState::Paused, None);
}

async fn cancel_job(inner: &Arc<EngineInner>, shared: &Arc<JobShared>) {
    let writer = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(writer) = writer {
        let _ = writer.finalize().await;
    }
    if shared.delete_partial_on_cancel.load(Ordering::Relaxed)
        && let Err(err) = crate::disk::remove_partial(&shared.part_path)
    {
        tracing::warn!(error = %err, "could not delete partial on cancel");
    }
    let id = shared.id.clone();
    let _ = journal_do(&inner.journal, move |j| j.set_state(&id, "cancelled", None)).await;
    shared.set_state(JobState::Cancelled, None);
}

async fn fail_job(inner: &Arc<EngineInner>, shared: &Arc<JobShared>, err: EngineError) {
    let code = err.code();
    let message = err.to_string();
    tracing::warn!(code, message = %message, "download failed");
    let writer = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(writer) = writer {
        let _ = writer.finalize().await;
    }
    let id = shared.id.clone();
    let code_str = code.to_owned();
    let message_str = message.clone();
    let code_for_db = code_str.clone();
    let message_for_db = message_str.clone();
    let _ = journal_do(&inner.journal, move |j| {
        j.set_state(
            &id,
            "error",
            Some((code_for_db.as_str(), message_for_db.as_str())),
        )
    })
    .await;
    shared.set_state(JobState::Error, None);
    shared.emit(JobEvent::Failed {
        code: code_str,
        message: message_str,
    });
}
