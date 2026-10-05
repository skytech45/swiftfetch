//! The per-segment download task: ranged GETs with retry/backoff, limiter
//! tokens, writer batches with acks, and journal checkpoints at most once
//! per second per segment.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};

use crate::connection::{self, ByteStream, RequestContext};
use crate::disk::WriterMsg;
use crate::engine::{EngineInner, JobShared, SegRt};
use crate::errors::EngineError;
use crate::supervisor::{backoff, journal_do};

/// Events segment tasks send to the supervisor.
pub(crate) enum SegEvent {
    /// Segment fully written and journaled.
    Done(u32),
    /// Segment exhausted its retry budget.
    Failed {
        idx: u32,
        err: EngineError,
        url_at_failure: String,
    },
    /// 416 — journal offsets are stale.
    StaleRange,
    /// Server ignored the range / sent a mismatching `Content-Range` — the
    /// entity may have changed.
    EntityChanged(u32),
}

/// Runs one segment to completion (or hands failure back to the supervisor).
#[allow(clippy::too_many_lines)] // segment state machine: retry/stream/write loop
pub(crate) async fn run_segment(
    inner: Arc<EngineInner>,
    shared: Arc<JobShared>,
    writer_tx: mpsc::Sender<WriterMsg>,
    seg_tx: mpsc::Sender<SegEvent>,
    seg: Arc<SegRt>,
) {
    let idx = seg.idx;
    let job_bucket = shared
        .job_bucket
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let global_bucket = inner.limiter.global().clone();
    let ctx = RequestContext {
        referer: shared.spec.referer.clone(),
        cookies: shared.spec.cookies.clone(),
        user_agent: shared.spec.user_agent.clone(),
    };
    let mut local_done = seg.done.load(Ordering::Relaxed);
    // Bytes of this segment already journaled when the task started.
    let mut ckpt_base = local_done;
    let budget = shared.config.retry_budget;
    let mut retry: u32 = seg.retries.load(Ordering::Relaxed);

    'attempt: loop {
        if shared.pause.is_cancelled() || shared.cancel.is_cancelled() {
            checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
            return;
        }
        let end = *seg
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = seg.start;
        let unknown_len = end == u64::MAX;
        if !unknown_len && start + local_done > end {
            finish_segment(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
            let _ = seg_tx.send(SegEvent::Done(idx)).await;
            return;
        }
        let (url, if_range) = {
            let meta = shared
                .meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                meta.url.clone(),
                if retry > 0 { meta.etag.clone() } else { None },
            )
        };

        let opened: Result<ByteStream, EngineError> = if unknown_len {
            connection::open_full(&inner.client, &url, &ctx).await
        } else {
            match connection::open_range(
                &inner.client,
                &url,
                start + local_done,
                end,
                if_range.as_deref(),
                &ctx,
            )
            .await
            {
                Ok(connection::RangeResponse::Partial {
                    stream,
                    verified_start,
                }) => {
                    if verified_start != start + local_done {
                        checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                        let _ = seg_tx.send(SegEvent::EntityChanged(idx)).await;
                        return;
                    }
                    Ok(stream)
                }
                Ok(connection::RangeResponse::Full { stream }) => {
                    let sole = {
                        let segments = shared
                            .segments
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        segments.len() == 1 && start == 0
                    };
                    if if_range.is_some() {
                        checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                        let _ = seg_tx.send(SegEvent::EntityChanged(idx)).await;
                        return;
                    }
                    if !(sole && local_done == 0) {
                        // Server ignores Range: retry once with If-Range so a
                        // conditional server can classify; persistent 200s on
                        // a ranged request surface as an entity change.
                        retry = seg.retries.fetch_add(1, Ordering::Relaxed) + 1;
                        if retry > budget {
                            checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                .await;
                            let _ = seg_tx.send(SegEvent::EntityChanged(idx)).await;
                            return;
                        }
                        tokio::time::sleep(backoff(shared.config.retry_backoff, retry)).await;
                        continue 'attempt;
                    }
                    Ok(stream)
                }
                Ok(connection::RangeResponse::NotSatisfiable) => {
                    checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                    let _ = seg_tx.send(SegEvent::StaleRange).await;
                    return;
                }
                Err(err) => Err(err),
            }
        };

        let mut stream = match opened {
            Ok(stream) => stream,
            Err(err) => {
                // Refresh-worthy failures (403/404/410, dead probes) never
                // retry: the supervisor refreshes the URL instead. Retrying
                // an expired link only burns the budget and stalls detection.
                let refresh_worthy = err.is_refresh_worthy();
                retry = seg.retries.fetch_add(1, Ordering::Relaxed) + 1;
                if retry > budget || refresh_worthy {
                    checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                    let _ = seg_tx
                        .send(SegEvent::Failed {
                            idx,
                            err,
                            url_at_failure: url.clone(),
                        })
                        .await;
                    return;
                }
                tokio::time::sleep(backoff(shared.config.retry_backoff, retry)).await;
                continue 'attempt;
            }
        };

        // Stream the response body into the writer.
        let mut last_checkpoint = tokio::time::Instant::now();
        loop {
            let end_now = *seg
                .end
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !unknown_len && start + local_done == end_now + 1 {
                finish_segment(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                let _ = seg_tx.send(SegEvent::Done(idx)).await;
                return;
            }
            tokio::select! {
                    () = shared.pause.cancelled() => {
                        checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                        return;
                    }
                    () = shared.cancel.cancelled() => {
                        checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                        return;
                    }
                    item = stream.next() => match item {
                    None => {
                        // EOF from the server.
                        if unknown_len {
                            // Unknown-length download completed.
                            let written = local_done;
                            {
                                let mut meta = shared
                                    .meta
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                meta.total_len = Some(written);
                            }
                            let id = shared.id.clone();
                            let _ = journal_do(&inner.journal, move |j| j.set_total_len(&id, written))
                                .await;
                            finish_segment(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                .await;
                            let _ = seg_tx.send(SegEvent::Done(idx)).await;
                            return;
                        }
                        if start + local_done == end_now + 1 {
                            finish_segment(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                .await;
                            let _ = seg_tx.send(SegEvent::Done(idx)).await;
                            return;
                        }
                        // Premature EOF: retry the remaining range.
                        retry = seg.retries.fetch_add(1, Ordering::Relaxed) + 1;
                        if retry > budget {
                            checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                            let _ = seg_tx
                                .send(SegEvent::Failed {
                                    idx,
                                    err: EngineError::Io(std::io::Error::new(
                                        std::io::ErrorKind::UnexpectedEof,
                                        "connection closed before segment completed",
                                    )),
                                    url_at_failure: url.clone(),
                                })
                                .await;
                            return;
                        }
                        tokio::time::sleep(backoff(shared.config.retry_backoff, retry)).await;
                        continue 'attempt;
                    }
                    Some(Err(err)) => {
                        retry = seg.retries.fetch_add(1, Ordering::Relaxed) + 1;
                        if retry > budget {
                            checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base).await;
                            let _ = seg_tx
                                .send(SegEvent::Failed {
                                    idx,
                                    err: EngineError::from(err),
                                    url_at_failure: url.clone(),
                                })
                                .await;
                            return;
                        }
                        tokio::time::sleep(backoff(shared.config.retry_backoff, retry)).await;
                        continue 'attempt;
                    }
                    Some(Ok(bytes)) => {
                        let mut data: &[u8] = bytes.as_ref();
                        while !data.is_empty() {
                            if shared.pause.is_cancelled() || shared.cancel.is_cancelled() {
                                checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                    .await;
                                return;
                            }
                            let end_seg = *seg
                                .end
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let limit: usize = if unknown_len {
                                data.len().min(crate::supervisor::CHUNK)
                            } else {
                                let room = (end_seg + 1).saturating_sub(start + local_done);
                                let want = (data.len() as u64)
                                    .min(room)
                                    .min(crate::supervisor::CHUNK as u64);
                                usize::try_from(want).unwrap_or(0)
                            };
                            if limit == 0 {
                                // Segment shrank under us (split) — the new
                                // owner rewrites the tail; stop here.
                                break;
                            }
                            if let Some(bucket) = &job_bucket {
                                bucket.acquire(limit as u64).await;
                            }
                            global_bucket.acquire(limit as u64).await;
                            let (ack_tx, ack_rx) = oneshot::channel();
                            if writer_tx
                                .send(WriterMsg::Write {
                                    offset: start + local_done,
                                    data: data[..limit].to_vec(),
                                    ack: ack_tx,
                                })
                                .await
                                .is_err()
                            {
                                let _ = seg_tx
                                    .send(SegEvent::Failed {
                                        idx,
                                        err: EngineError::Io(std::io::Error::other(
                                            "disk writer closed",
                                        )),
                                        url_at_failure: url.clone(),
                                    })
                                    .await;
                                return;
                            }
                            match ack_rx.await {
                                Ok(Ok(())) => {
                                    eprintln!("[seg] acked {limit}");
                                }
                                Ok(Err(err)) => {
                                    let _ = seg_tx
                                        .send(SegEvent::Failed {
                                            idx,
                                            err: EngineError::Io(err),
                                            url_at_failure: url.clone(),
                                        })
                                        .await;
                                    return;
                                }
                                Err(_) => {
                                    let _ = seg_tx
                                        .send(SegEvent::Failed {
                                            idx,
                                            err: EngineError::Io(std::io::Error::other(
                                                "writer dropped ack",
                                            )),
                                            url_at_failure: url.clone(),
                                        })
                                        .await;
                                    return;
                                }
                            }
                            local_done += limit as u64;
                            seg.done.store(local_done, Ordering::Relaxed);
                            seg.since.fetch_add(limit as u64, Ordering::Relaxed);
                            shared.done.fetch_add(limit as u64, Ordering::Relaxed);
                            data = &data[limit..];
                            shared.maybe_crash();
                            if last_checkpoint.elapsed() >= Duration::from_secs(1)
                                && local_done > ckpt_base
                            {
                                checkpoint(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                    .await;
                                last_checkpoint = tokio::time::Instant::now();
                            }
                        }
                        if unknown_len {
                            // Keep streaming until EOF.
                            continue;
                        }
                        let end_seg = *seg
                            .end
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if start + local_done == end_seg + 1 {
                            finish_segment(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                .await;
                            let _ = seg_tx.send(SegEvent::Done(idx)).await;
                            return;
                        }
                        // Segment shrank (split) and remainder moved elsewhere.
                        if start + local_done > end_seg + 1 {
                            finish_segment(&inner, &shared, &seg, &mut local_done, &mut ckpt_base)
                                .await;
                            let _ = seg_tx.send(SegEvent::Done(idx)).await;
                            return;
                        }
                        // More body expected: keep reading from this stream.
                    }
                }
            }
        }
    }
}

/// Flushes this segment's pending progress into the journal.
async fn checkpoint(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    seg: &Arc<SegRt>,
    local_done: &mut u64,
    ckpt_base: &mut u64,
) {
    if *local_done <= *ckpt_base {
        return;
    }
    let delta = *local_done - *ckpt_base;
    let id = shared.id.clone();
    let idx = i64::from(seg.idx);
    let done = *local_done;
    let journal = inner.journal.clone();
    match journal_do(&journal, move |j| {
        j.checkpoint_segment(&id, idx, done, delta)
    })
    .await
    {
        Ok(()) => *ckpt_base = *local_done,
        Err(err) => tracing::warn!(error = %err, "segment checkpoint failed"),
    }
}

/// Marks the segment done in memory and the journal (final checkpoint).
async fn finish_segment(
    inner: &Arc<EngineInner>,
    shared: &Arc<JobShared>,
    seg: &Arc<SegRt>,
    local_done: &mut u64,
    ckpt_base: &mut u64,
) {
    checkpoint(inner, shared, seg, local_done, ckpt_base).await;
    *seg.state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = crate::engine::SegState::Done;
    let id = shared.id.clone();
    let idx = i64::from(seg.idx);
    let _ = journal_do(&inner.journal, move |j| {
        j.set_segment_state(&id, idx, "done")
    })
    .await;
}
