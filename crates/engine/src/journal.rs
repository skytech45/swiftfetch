//! SQLite journal access for the engine: the only state trusted after a
//! restart. Every mutation runs in a transaction; segment progress is
//! checkpointed at most once per second per segment, bounding kill -9 loss to
//! ~1 s of bookkeeping.

use std::path::PathBuf;
use std::sync::Mutex;

use swiftfetch_store::{Store, StoreError};

/// A `downloads` row as the engine sees it.
#[derive(Debug, Clone)]
pub struct JobRow {
    /// Job id (uuid v4).
    pub id: String,
    /// Current URL (may be refreshed).
    pub url: String,
    /// Final destination path.
    pub final_path: PathBuf,
    /// Partial-file path (`.sfpart`).
    pub part_path: PathBuf,
    /// Total length when known.
    pub total_len: Option<u64>,
    /// Denormalized sum of segment progress.
    pub done_bytes: u64,
    /// `none` | `ranges` | `ifrange`.
    pub resume_cap: String,
    /// Server `ETag`.
    pub etag: Option<String>,
    /// Server Last-Modified.
    pub last_modified: Option<String>,
    /// Content-Type.
    pub content_type: Option<String>,
    /// Job state string.
    pub state: String,
    /// Max connections per file.
    pub max_conns: u8,
    /// Referer captured with the job.
    pub referer: Option<String>,
    /// User-Agent captured with the job.
    pub user_agent: Option<String>,
    /// Cookie header captured with the job.
    pub cookies: Option<String>,
}

/// A `segments` row.
#[derive(Debug, Clone)]
pub struct SegmentRow {
    /// Segment index within the job.
    pub idx: i64,
    /// First byte offset (inclusive).
    pub start: u64,
    /// Last byte offset (inclusive).
    pub end: u64,
    /// Confirmed written bytes.
    pub done: u64,
    /// `queued` | `active` | `stalled` | `done` | `failed`.
    pub state: String,
}

/// Read-write subset of the job row mutated during a download.
#[derive(Debug, Clone, Default)]
pub struct ProbeUpdate {
    /// Total length when known.
    pub total_len: Option<u64>,
    /// Resume capability string.
    pub resume_cap: String,
    /// Server `ETag`.
    pub etag: Option<String>,
    /// Server Last-Modified.
    pub last_modified: Option<String>,
    /// Content-Type.
    pub content_type: Option<String>,
}

/// Journal handle; cheap to clone, safe to share (internal mutex).
#[derive(Clone)]
pub struct Journal {
    store: std::sync::Arc<Mutex<Store>>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// SQLite stores signed 64-bit integers; byte counts are always in range.
fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Same as [`to_i64`] for optional lengths.
fn opt_to_i64(v: Option<u64>) -> Option<i64> {
    v.map(to_i64)
}

impl Journal {
    /// Wraps a store.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store: std::sync::Arc::new(Mutex::new(store)),
        }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Store> {
        // A poisoned mutex means a previous bug; the engine cannot run
        // correctly without the journal, so propagate a store error.
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Inserts a new job row (state included).
    ///
    /// # Errors
    ///
    /// Returns [`swiftfetch_store::StoreError`] on SQL failure.
    pub fn create_job(&self, row: &JobRow, state: &str) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO downloads (id, url, final_path, part_path, total_len, done_bytes, \
                 resume_cap, etag, last_modified, content_type, state, max_conns, referer, \
                 user_agent, cookies_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
                rusqlite::params![
                    row.id,
                    row.url,
                    row.final_path.display().to_string(),
                    row.part_path.display().to_string(),
                    opt_to_i64(row.total_len),
                    row.resume_cap,
                    row.etag,
                    row.last_modified,
                    row.content_type,
                    state,
                    i64::from(row.max_conns),
                    row.referer,
                    row.user_agent,
                    row.cookies,
                    now(),
                ],
            )?;
            Ok(())
        })
    }

    /// Persists a probe result and the (possibly refreshed) URL.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn save_probe(&self, id: &str, url: &str, update: &ProbeUpdate) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET url = ?2, total_len = ?3, resume_cap = ?4, etag = ?5, \
                 last_modified = ?6, content_type = ?7, updated_at = ?8 WHERE id = ?1",
                rusqlite::params![
                    id,
                    url,
                    opt_to_i64(update.total_len),
                    update.resume_cap,
                    update.etag,
                    update.last_modified,
                    update.content_type,
                    now(),
                ],
            )?;
            Ok(())
        })
    }

    /// Updates the total length once it becomes known (chunked downloads).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn set_total_len(&self, id: &str, total_len: u64) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET total_len = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, to_i64(total_len), now()],
            )?;
            Ok(())
        })
    }

    /// Updates the resume-capability classification (e.g. after a
    /// no-resume downgrade on resume).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn set_resume_cap(&self, id: &str, resume_cap: &str) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET resume_cap = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, resume_cap, now()],
            )?;
            Ok(())
        })
    }

    /// Transitions the job state (with optional error details).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn set_state(
        &self,
        id: &str,
        state: &str,
        error: Option<(&str, &str)>,
    ) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE downloads SET state = ?2, error_code = ?3, error_msg = ?4, \
                 updated_at = ?5 WHERE id = ?1",
                rusqlite::params![
                    id,
                    state,
                    error.map(|(code, _)| code),
                    error.map(|(_, msg)| msg),
                    now(),
                ],
            )?;
            Ok(())
        })
    }

    /// Replaces the segment plan in one transaction and recomputes
    /// `done_bytes` from the new rows.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn replace_segments(&self, job_id: &str, rows: &[SegmentRow]) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", ())?;
            let result = (|| {
                conn.execute("DELETE FROM segments WHERE job_id = ?1", [job_id])?;
                for r in rows {
                    conn.execute(
                        "INSERT INTO segments (job_id, idx, start_o, end_o, done, state)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            job_id,
                            r.idx,
                            to_i64(r.start),
                            to_i64(r.end),
                            to_i64(r.done),
                            r.state
                        ],
                    )?;
                }
                let total: i64 = rows
                    .iter()
                    .map(|r| i64::try_from(r.done).unwrap_or(i64::MAX))
                    .sum();
                conn.execute(
                    "UPDATE downloads SET done_bytes = ?2, updated_at = ?3 WHERE id = ?1",
                    rusqlite::params![job_id, total, now()],
                )?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    conn.execute("COMMIT", ())?;
                }
                Err(err) => {
                    let _ = conn.execute("ROLLBACK", ());
                    return Err(err);
                }
            }
            Ok(())
        })
    }

    /// Checkpoints one segment's progress and bumps the denormalized sum.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn checkpoint_segment(
        &self,
        job_id: &str,
        idx: i64,
        done: u64,
        delta: u64,
    ) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", ())?;
            let result = (|| {
                conn.execute(
                    "UPDATE segments SET done = ?3 WHERE job_id = ?1 AND idx = ?2",
                    rusqlite::params![job_id, idx, to_i64(done)],
                )?;
                conn.execute(
                    "UPDATE downloads SET done_bytes = done_bytes + ?2, updated_at = ?3 \
                     WHERE id = ?1",
                    rusqlite::params![job_id, to_i64(delta), now()],
                )?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    conn.execute("COMMIT", ())?;
                }
                Err(err) => {
                    let _ = conn.execute("ROLLBACK", ());
                    return Err(err);
                }
            }
            Ok(())
        })
    }

    /// Shrinks a segment (split: the old connection keeps the first half).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn shrink_segment(&self, job_id: &str, idx: i64, new_end: u64) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE segments SET end_o = ?3 WHERE job_id = ?1 AND idx = ?2",
                rusqlite::params![job_id, idx, to_i64(new_end)],
            )?;
            Ok(())
        })
    }

    /// Appends a segment (from a split or a stalled-range requeue).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn append_segment(&self, job_id: &str, row: &SegmentRow) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO segments (job_id, idx, start_o, end_o, done, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    job_id,
                    row.idx,
                    to_i64(row.start),
                    to_i64(row.end),
                    to_i64(row.done),
                    row.state
                ],
            )?;
            Ok(())
        })
    }

    /// Marks a segment state (e.g. `stalled`, `failed`, `done`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn set_segment_state(&self, job_id: &str, idx: i64, state: &str) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "UPDATE segments SET state = ?3 WHERE job_id = ?1 AND idx = ?2",
                rusqlite::params![job_id, idx, state],
            )?;
            Ok(())
        })
    }

    /// Loads one job row.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn load_job(&self, id: &str) -> Result<Option<JobRow>, StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, url, final_path, part_path, total_len, done_bytes, resume_cap, etag, \
                 last_modified, content_type, state, max_conns, referer, user_agent, cookies_json \
                 FROM downloads WHERE id = ?1",
            )?;
            let row = stmt
                .query_row([id], |r| {
                    Ok(JobRow {
                        id: r.get(0)?,
                        url: r.get(1)?,
                        final_path: PathBuf::from(r.get::<_, String>(2)?),
                        part_path: PathBuf::from(r.get::<_, String>(3)?),
                        total_len: r
                            .get::<_, Option<i64>>(4)?
                            .and_then(|v| u64::try_from(v).ok()),
                        done_bytes: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                        resume_cap: r.get(6)?,
                        etag: r.get(7)?,
                        last_modified: r.get(8)?,
                        content_type: r.get(9)?,
                        state: r.get(10)?,
                        max_conns: u8::try_from(r.get::<_, i64>(11)?.max(1)).unwrap_or(32),
                        referer: r.get(12)?,
                        user_agent: r.get(13)?,
                        cookies: r.get(14)?,
                    })
                })
                .map(Some)
                .or_else(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            Ok(row)
        })
    }

    /// Whether an active (non-terminal) download already claims
    /// `final_path`. Used to dedup destinations for jobs that have not
    /// created their partial file yet (paused/queued).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn final_path_claimed(&self, final_path: &str) -> Result<bool, StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            let claimed: i64 = conn.query_row(
                "SELECT COUNT(*) FROM downloads WHERE final_path = ?1                  AND state NOT IN ('done', 'error', 'cancelled')",
                [final_path],
                |row| row.get(0),
            )?;
            Ok(claimed > 0)
        })
    }

    /// Final paths claimed by active (non-terminal) downloads.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn claimed_final_paths(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT final_path FROM downloads                  WHERE state NOT IN ('done', 'error', 'cancelled')",
            )?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<std::collections::HashSet<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Loads all segment rows for a job, ordered by index.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn load_segments(&self, job_id: &str) -> Result<Vec<SegmentRow>, StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT idx, start_o, end_o, done, state FROM segments WHERE job_id = ?1 \
                 ORDER BY idx",
            )?;
            let rows = stmt
                .query_map([job_id], |r| {
                    Ok(SegmentRow {
                        idx: r.get(0)?,
                        start: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                        end: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                        done: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
                        state: r.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Loads jobs in the given states.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn jobs_in_states(&self, states: &[&str]) -> Result<Vec<JobRow>, StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            let placeholders = states.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT id, url, final_path, part_path, total_len, done_bytes, resume_cap, etag, \
                 last_modified, content_type, state, max_conns, referer, user_agent, \
                 cookies_json FROM downloads WHERE state IN ({placeholders}) ORDER BY created_at"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(states.iter()), |r| {
                    Ok(JobRow {
                        id: r.get(0)?,
                        url: r.get(1)?,
                        final_path: PathBuf::from(r.get::<_, String>(2)?),
                        part_path: PathBuf::from(r.get::<_, String>(3)?),
                        total_len: r
                            .get::<_, Option<i64>>(4)?
                            .and_then(|v| u64::try_from(v).ok()),
                        done_bytes: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                        resume_cap: r.get(6)?,
                        etag: r.get(7)?,
                        last_modified: r.get(8)?,
                        content_type: r.get(9)?,
                        state: r.get(10)?,
                        max_conns: u8::try_from(r.get::<_, i64>(11)?.max(1)).unwrap_or(32),
                        referer: r.get(12)?,
                        user_agent: r.get(13)?,
                        cookies: r.get(14)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Startup recovery: every job left in an active state becomes
    /// `interrupted`, and its bookkeeping is clamped to the bytes actually on
    /// disk (disk is truth for bytes, DB is truth for the plan). Returns the
    /// interrupted jobs.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn recover_interrupted(&self) -> Result<Vec<JobRow>, StoreError> {
        let jobs = self.jobs_in_states(&["probing", "downloading", "verifying"])?;
        for job in &jobs {
            self.set_state(&job.id, "interrupted", None)?;
            let file_len = std::fs::metadata(&job.part_path).map_or(0, |m| m.len());
            self.clamp_to_file(&job.id, file_len)?;
        }
        Ok(jobs)
    }

    /// Clamps segment bookkeeping to `file_len`: any bytes claimed beyond the
    /// file's actual size are forgotten (the reverse — extra file bytes — is
    /// harmless and simply re-downloaded over).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn clamp_to_file(&self, job_id: &str, file_len: u64) -> Result<(), StoreError> {
        let segments = self.load_segments(job_id)?;
        let claimed: u64 = segments.iter().map(|s| s.done).sum();
        if claimed <= file_len {
            return Ok(());
        }
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute("BEGIN IMMEDIATE", ())?;
            let result = (|| {
                let mut new_total: i64 = 0;
                for s in &segments {
                    let new_done = s.done.min(file_len.saturating_sub(s.start));
                    new_total += to_i64(new_done);
                    let state = if new_done == s.done {
                        s.state.as_str()
                    } else {
                        "queued"
                    };
                    conn.execute(
                        "UPDATE segments SET done = ?3, state = ?4 WHERE job_id = ?1 AND idx = ?2",
                        rusqlite::params![job_id, s.idx, to_i64(new_done), state],
                    )?;
                }
                conn.execute(
                    "UPDATE downloads SET done_bytes = ?2, updated_at = ?3 WHERE id = ?1",
                    rusqlite::params![job_id, new_total, now()],
                )?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    conn.execute("COMMIT", ())?;
                }
                Err(err) => {
                    let _ = conn.execute("ROLLBACK", ());
                    return Err(err);
                }
            }
            Ok(())
        })
    }

    /// Records a completed download in the history table.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on SQL failure.
    pub fn record_history(
        &self,
        url: &str,
        final_path: &std::path::Path,
        size_bytes: Option<u64>,
        mime_type: Option<&str>,
    ) -> Result<(), StoreError> {
        let store = self.locked();
        store.with_conn(|conn| {
            conn.execute(
                "INSERT INTO history (id, url, final_path, size_bytes, mime_type, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    url,
                    final_path.display().to_string(),
                    opt_to_i64(size_bytes),
                    mime_type,
                    now(),
                ],
            )?;
            Ok(())
        })
    }
}
