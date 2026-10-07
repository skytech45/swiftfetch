//! Public engine API: [`Engine`] owns the HTTP client, the SQLite journal,
//! the speed limiter and the set of live jobs; job execution lives in
//! [`crate::supervisor`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use swiftfetch_net::HttpConfig;
use swiftfetch_store::Store;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::checksum::Digest;
use crate::connection::ResumeCap;
use crate::disk::DiskWriter;
use crate::errors::EngineError;
use crate::journal::Journal;
use crate::limiter::{Bucket, SpeedLimiter};
use crate::supervisor::StartMode;

/// Engine-wide tunables. Defaults mirror the system design (500 ms supervisor
/// tick, 1 MiB minimum segment, 8 KiB/s stall floor, 5 s stall detection).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Network-layer configuration (proxy, user agent, timeouts).
    pub net: HttpConfig,
    /// Minimum segment size when splitting (bytes).
    pub min_segment: u64,
    /// Throughput below which an active segment counts as stalled (bytes/s).
    pub stall_floor_bps: u64,
    /// How long a segment must be below the stall floor before its range is
    /// reassigned.
    pub stall_after: Duration,
    /// How long a segment must stay below 50% of the job's mean speed before
    /// the largest remaining range is split to a free connection.
    pub slow_window: Duration,
    /// Supervisor tick (EWMA accounting + progress events).
    pub tick: Duration,
    /// Maximum progress-event rate (events are also bounded by the tick).
    pub progress_interval: Duration,
    /// Per-segment retry budget before surfacing a failure.
    pub retry_budget: u32,
    /// Base backoff for segment retries (doubles per retry, capped at 8 s).
    pub retry_backoff: Duration,
    /// Maximum URL refresh attempts on expired/forbidden links.
    pub max_refresh: u32,
    /// Master switch for dynamic rebalancing (tests disable it to measure the
    /// improvement it provides).
    pub rebalancing_enabled: bool,
    /// Global speed limit (KiB/s); `None` = unlimited.
    pub global_speed_limit_kib: Option<u64>,
    /// Test hook: hard-exit the process (`kill -9` equivalent, no cleanup)
    /// once this many bytes are confirmed written. Never set in production.
    pub debug_crash_after_bytes: Option<u64>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            net: HttpConfig::default(),
            min_segment: crate::segmenter::MIN_SEGMENT_BYTES,
            stall_floor_bps: 8 * 1024,
            stall_after: Duration::from_secs(5),
            slow_window: Duration::from_secs(2),
            tick: Duration::from_millis(500),
            progress_interval: Duration::from_millis(250),
            retry_budget: 10,
            retry_backoff: Duration::from_millis(500),
            max_refresh: 3,
            rebalancing_enabled: true,
            global_speed_limit_kib: None,
            debug_crash_after_bytes: None,
        }
    }
}

/// Produces a fresh URL for a job whose URL expired or was rejected
/// (403/404/410, dropped sessions). Implemented by the site-login manager in
/// later milestones; tests provide closures.
pub trait UrlRefresher: Send + Sync {
    /// Returns the replacement URL, or `None` to give up.
    fn refresh(&self) -> Option<String>;
}

impl<F> UrlRefresher for F
where
    F: Fn() -> Option<String> + Send + Sync,
{
    fn refresh(&self) -> Option<String> {
        self()
    }
}

/// One download request. Field semantics mirror the build prompt §9.1.
#[derive(Clone)]
pub struct JobSpec {
    /// Source URL.
    pub url: String,
    /// Directory the file lands in.
    pub dest_dir: PathBuf,
    /// Filename override; resolved from the server response when absent.
    pub filename: Option<String>,
    /// Parallel connections for this file (1–32, default 8).
    pub max_conns: u8,
    /// Per-download speed limit (KiB/s); `None` inherits the global cap.
    pub speed_limit_kib: Option<u64>,
    /// `Referer` header for every request.
    pub referer: Option<String>,
    /// Raw `Cookie` header for every request.
    pub cookies: Option<String>,
    /// Per-job `User-Agent` override.
    pub user_agent: Option<String>,
    /// URL refresher used on expired/forbidden links.
    pub url_refresher: Option<std::sync::Arc<dyn UrlRefresher>>,
    /// Probe and persist the job row but do not spawn the supervisor —
    /// the job waits in `paused` for an explicit resume (queue-later).
    pub start_paused: bool,
}

impl JobSpec {
    /// A spec with sensible defaults (8 connections, unlimited speed).
    #[must_use]
    pub fn new(url: impl Into<String>, dest_dir: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest_dir: dest_dir.into(),
            filename: None,
            max_conns: 8,
            speed_limit_kib: None,
            referer: None,
            cookies: None,
            user_agent: None,
            url_refresher: None,
            start_paused: false,
        }
    }
}

/// Lifecycle states persisted in `downloads.state`; string forms match the
/// schema comment and `swiftfetch_common::DOWNLOAD_STATES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting to start.
    Queued,
    /// Probing the server.
    Probing,
    /// Actively downloading.
    Downloading,
    /// Paused by the user (or interrupted by a crash).
    Paused,
    /// Verifying size/digest before the final rename.
    Verifying,
    /// Completed successfully.
    Done,
    /// Failed with an error code.
    Error,
    /// Interrupted by an unclean shutdown; resumable.
    Interrupted,
    /// Cancelled by the user.
    Cancelled,
}

impl JobState {
    /// Column string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Probing => "probing",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Verifying => "verifying",
            Self::Done => "done",
            Self::Error => "error",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses a column string.
    #[must_use]
    pub fn from_db_str(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "probing" => Self::Probing,
            "downloading" => Self::Downloading,
            "paused" => Self::Paused,
            "verifying" => Self::Verifying,
            "done" => Self::Done,
            "error" => Self::Error,
            "interrupted" => Self::Interrupted,
            _ => Self::Cancelled,
        }
    }
}

/// Streamed job updates.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobEvent {
    /// Progress tick (≤ 4 Hz per job).
    Progress {
        /// Confirmed bytes written.
        done: u64,
        /// Total size when known.
        total: Option<u64>,
        /// Smoothed bytes/s.
        bps: f64,
    },
    /// State transition, optionally with a human-readable notice (e.g.
    /// "resume not supported by server").
    State {
        /// New state.
        state: JobState,
        /// User-facing notice.
        notice: Option<String>,
    },
    /// File completed and renamed into place.
    Completed {
        /// Final path.
        path: PathBuf,
    },
    /// Job failed; `code` is the machine-readable `E_*` code.
    Failed {
        /// Machine-readable code.
        code: String,
        /// Human sentence.
        message: String,
    },
}

/// One segment as seen in a snapshot.
#[derive(Debug, Clone)]
pub struct SegmentSnapshot {
    /// Segment index.
    pub idx: u32,
    /// First byte (inclusive).
    pub start: u64,
    /// Last byte (inclusive).
    pub end: u64,
    /// Confirmed bytes.
    pub done: u64,
    /// `queued` | `active` | `stalled` | `done` | `failed`.
    pub state: String,
}

/// Point-in-time view of a job.
#[derive(Debug, Clone)]
pub struct JobSnapshot {
    /// Job id.
    pub id: String,
    /// Current state.
    pub state: JobState,
    /// Current URL.
    pub url: String,
    /// Final destination.
    pub final_path: PathBuf,
    /// Total size when known.
    pub total: Option<u64>,
    /// Confirmed bytes.
    pub done: u64,
    /// Smoothed speed (bytes/s).
    pub bps: f64,
    /// Resume capability.
    pub resume_cap: ResumeCap,
    /// Per-segment view.
    pub segments: Vec<SegmentSnapshot>,
}

/// Mutable per-job identity/data guarded by one mutex.
#[derive(Clone)]
pub(crate) struct JobMeta {
    pub url: String,
    pub total_len: Option<u64>,
    pub resume_cap: ResumeCap,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub expected_digest: Option<Digest>,
}

/// Runtime segment state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegState {
    Queued,
    Active,
    Stalled,
    Done,
    Failed,
}

impl SegState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Active => "active",
            Self::Stalled => "stalled",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// In-memory segment mirror; shared between the supervisor and the segment
/// task (`end` may shrink via splits, `done` advances via writer acks).
pub(crate) struct SegRt {
    pub idx: u32,
    pub start: u64,
    pub end: Mutex<u64>,
    pub done: AtomicU64,
    pub state: Mutex<SegState>,
    pub ewma: AtomicU64,
    pub since: AtomicU64,
    pub slow_ticks: AtomicU32,
    /// Retry attempts for this work lineage; survives stall reassignment
    /// (the replacement segment inherits it) so a persistently failing
    /// range cannot retry forever.
    pub retries: AtomicU32,
    pub active_since: Mutex<Option<tokio::time::Instant>>,
    pub task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SegRt {
    /// Bytes left in this segment. Saturating: after a split shrinks the
    /// segment, `done` may transiently sit past the new end until the task
    /// observes the shrink — that counts as zero remaining.
    pub(crate) fn remaining(&self) -> u64 {
        let end = *self
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        end.saturating_sub(self.start + self.done.load(Ordering::Relaxed))
    }
}

/// Shared per-job state; the supervisor and segment tasks coordinate through
/// it plus the writer channel.
pub(crate) struct JobShared {
    pub id: String,
    pub spec: JobSpec,
    pub final_path: PathBuf,
    pub part_path: PathBuf,
    pub max_conns: u8,
    pub meta: Mutex<JobMeta>,
    pub state: Mutex<JobState>,
    pub segments: Mutex<Vec<std::sync::Arc<SegRt>>>,
    pub done: AtomicU64,
    pub speed_bps: AtomicU64,
    pub events: broadcast::Sender<JobEvent>,
    pub pause: CancellationToken,
    pub cancel: CancellationToken,
    pub delete_partial_on_cancel: AtomicBool,
    pub job_bucket: Mutex<Option<Bucket>>,
    pub writer: Mutex<Option<DiskWriter>>,
    pub refreshes: AtomicU32,
    /// Serializes URL refresh attempts so concurrent segment failures do not
    /// burn the refresh budget.
    pub refresh_lock: tokio::sync::Mutex<()>,
    pub config: EngineConfig,
}

impl JobShared {
    pub(crate) fn emit(&self, event: JobEvent) {
        let _ = self.events.send(event);
    }

    pub(crate) fn set_state(&self, state: JobState, notice: Option<String>) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
        self.emit(JobEvent::State { state, notice });
    }

    pub(crate) fn state_now(&self) -> JobState {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn maybe_crash(&self) {
        if let Some(threshold) = self.config.debug_crash_after_bytes
            && self.done.load(Ordering::Relaxed) >= threshold
        {
            // Test hook: emulate SIGKILL — no cleanup, no checkpoint.
            tracing::error!(
                bytes = self.done.load(Ordering::Relaxed),
                "debug crash hook"
            );
            std::process::exit(9);
        }
    }
}

pub(crate) struct EngineInner {
    pub config: EngineConfig,
    pub client: reqwest::Client,
    pub journal: Journal,
    pub limiter: SpeedLimiter,
    pub jobs: Mutex<HashMap<String, std::sync::Arc<JobShared>>>,
}

/// The download engine. Clone-cheap; all methods are runtime-safe.
#[derive(Clone)]
pub struct Engine {
    inner: std::sync::Arc<EngineInner>,
}

impl Engine {
    /// Opens the engine over a store and runs crash recovery: jobs left in
    /// active states become `interrupted` with bookkeeping clamped to the
    /// bytes actually on disk.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] for client-configuration or journal failures.
    pub fn open(config: EngineConfig, store: Store) -> Result<Self, EngineError> {
        let client = config
            .net
            .client()
            .map_err(|err| EngineError::Config(format!("http client: {err}")))?;
        let journal = Journal::new(store);
        let limiter = SpeedLimiter::new(config.global_speed_limit_kib.map(|kib| kib * 1024));
        let recovered = journal.recover_interrupted()?;
        if !recovered.is_empty() {
            tracing::info!(count = recovered.len(), "recovered interrupted downloads");
        }
        Ok(Self {
            inner: std::sync::Arc::new(EngineInner {
                config,
                client,
                journal,
                limiter,
                jobs: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Starts a fresh download. The server is probed (with URL refresh
    /// attempts when configured) before the job row is created; the resolved
    /// destination never overwrites an existing file.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] when probing or destination resolution fails.
    #[allow(clippy::too_many_lines)] // one cohesive probe->plan->spawn flow
    pub async fn start_job(
        &self,
        spec: JobSpec,
    ) -> Result<(String, broadcast::Receiver<JobEvent>), EngineError> {
        if spec.max_conns == 0 || spec.max_conns > 32 {
            return Err(EngineError::Config(format!(
                "max_conns must be 1..=32, got {}",
                spec.max_conns
            )));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let ctx = crate::connection::RequestContext {
            referer: spec.referer.clone(),
            cookies: spec.cookies.clone(),
            user_agent: spec.user_agent.clone(),
        };

        // Probe, refreshing the URL first when the initial one is rejected.
        let mut url = spec.url.clone();
        let probe = loop {
            match crate::connection::probe(&self.inner.client, &url, &ctx).await {
                Ok(probe) => break probe,
                Err(err) if err.is_refresh_worthy() => match refreshed_url(&spec) {
                    Some(new_url) => url = new_url,
                    None => {
                        return Err(EngineError::UrlRefreshExhausted { url });
                    }
                },
                Err(err) => return Err(err),
            }
        };

        // Paths already claimed by active rows (paused jobs have no file
        // on disk yet, so a filesystem check alone would let two queued
        // copies of the same URL collide).
        let claimed: std::collections::HashSet<String> = {
            let journal = self.inner.journal.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(journal.claimed_final_paths());
            });
            rx.await
                .map_err(|err| EngineError::Config(format!("claim query: {err}")))?
                .map_err(EngineError::from)?
        };

        let filename = match spec.filename.clone() {
            Some(name) => crate::connection::sanitize_filename(&name),
            None => probe
                .filename
                .clone()
                .unwrap_or_else(|| format!("download-{}.bin", &id[..8])),
        };
        if filename.is_empty() {
            return Err(EngineError::Config("resolved filename is empty".into()));
        }
        let final_path = resolve_destination(&spec.dest_dir, &filename, &claimed).await?;
        let mut part_os = final_path.clone().into_os_string();
        part_os.push(".sfpart");
        let part_path = PathBuf::from(part_os);
        crate::disk::remove_partial(&part_path)?;

        let row = crate::journal::JobRow {
            id: id.clone(),
            url: url.clone(),
            final_path: final_path.clone(),
            part_path: part_path.clone(),
            total_len: probe.total_len,
            done_bytes: 0,
            resume_cap: probe.resume_cap.as_str().to_owned(),
            etag: probe.etag.clone(),
            last_modified: probe.last_modified.clone(),
            content_type: probe.content_type.clone(),
            state: "downloading".to_owned(),
            max_conns: spec.max_conns,
            referer: spec.referer.clone(),
            user_agent: spec.user_agent.clone(),
            cookies: spec.cookies.clone(),
        };
        let initial_state = if spec.start_paused {
            "paused"
        } else {
            "downloading"
        };
        self.inner.journal.create_job(&row, initial_state)?;

        let (events, _) = broadcast::channel(256);
        let job_bucket = self
            .inner
            .limiter
            .job_bucket(spec.speed_limit_kib.map(|kib| kib * 1024));
        let shared = std::sync::Arc::new(JobShared {
            id: id.clone(),
            spec,
            final_path,
            part_path,
            max_conns: row.max_conns,
            meta: Mutex::new(JobMeta {
                url,
                total_len: probe.total_len,
                resume_cap: probe.resume_cap,
                etag: probe.etag,
                last_modified: probe.last_modified,
                expected_digest: probe.advertised_digest,
            }),
            state: Mutex::new(JobState::Downloading),
            segments: Mutex::new(Vec::new()),
            done: AtomicU64::new(0),
            speed_bps: AtomicU64::new(0),
            events,
            pause: CancellationToken::new(),
            cancel: CancellationToken::new(),
            delete_partial_on_cancel: AtomicBool::new(false),
            job_bucket: Mutex::new(job_bucket),
            writer: Mutex::new(None),
            refreshes: AtomicU32::new(0),
            refresh_lock: tokio::sync::Mutex::new(()),
            config: self.inner.config.clone(),
        });
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone(), shared.clone());
        let events = shared.events.subscribe();
        if !spec_ref_start_paused(&shared.spec) {
            tokio::spawn(crate::supervisor::run(
                self.inner.clone(),
                shared,
                StartMode::Fresh,
            ));
        }
        Ok((id, events))
    }

    /// Resumes a paused or interrupted job from its journaled state,
    /// re-probing the server and restarting from zero when the entity
    /// changed or the server cannot resume.
    ///
    /// # Errors
    ///
    /// Returns the job's event stream (subscribed before the supervisor
    /// spawns, so no event is lost) or [`EngineError`] for unknown jobs,
    /// non-resumable states, or journal failures.
    pub fn resume(&self, id: &str) -> Result<broadcast::Receiver<JobEvent>, EngineError> {
        let row = self
            .inner
            .journal
            .load_job(id)?
            .ok_or_else(|| EngineError::Config(format!("unknown job {id}")))?;
        let state = JobState::from_db_str(&row.state);
        if !matches!(
            state,
            crate::engine::JobState::Paused
                | crate::engine::JobState::Interrupted
                | crate::engine::JobState::Queued
        ) {
            return Err(EngineError::Config(format!(
                "job {id} is not resumable (state {})",
                row.state
            )));
        }
        let spec = JobSpec {
            url: row.url.clone(),
            dest_dir: row
                .final_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            filename: row
                .final_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            max_conns: row.max_conns,
            speed_limit_kib: None,
            referer: row.referer.clone(),
            cookies: row.cookies.clone(),
            user_agent: row.user_agent.clone(),
            url_refresher: None,
            start_paused: false,
        };
        let (events, _) = broadcast::channel(256);
        let shared = std::sync::Arc::new(JobShared {
            id: id.to_owned(),
            spec,
            final_path: row.final_path.clone(),
            part_path: row.part_path.clone(),
            max_conns: row.max_conns,
            meta: Mutex::new(JobMeta {
                url: row.url.clone(),
                total_len: row.total_len,
                resume_cap: ResumeCap::from_db_str(&row.resume_cap),
                etag: row.etag.clone(),
                last_modified: row.last_modified.clone(),
                expected_digest: None,
            }),
            state: Mutex::new(state),
            segments: Mutex::new(Vec::new()),
            done: AtomicU64::new(0),
            speed_bps: AtomicU64::new(0),
            events,
            pause: CancellationToken::new(),
            cancel: CancellationToken::new(),
            delete_partial_on_cancel: AtomicBool::new(false),
            job_bucket: Mutex::new(None),
            writer: Mutex::new(None),
            refreshes: AtomicU32::new(0),
            refresh_lock: tokio::sync::Mutex::new(()),
            config: self.inner.config.clone(),
        });
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned(), shared.clone());
        let events = shared.events.subscribe();
        tokio::spawn(crate::supervisor::run(
            self.inner.clone(),
            shared,
            StartMode::Resume,
        ));
        Ok(events)
    }

    /// Pauses a job (graceful: tasks checkpoint, the writer flushes). The
    /// [`JobEvent::State`] event with [`JobState::Paused`] confirms it.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Config`] for unknown jobs.
    pub fn pause(&self, id: &str) -> Result<(), EngineError> {
        let shared = self.lookup(id)?;
        shared.pause.cancel();
        Ok(())
    }

    /// Cancels a job. With `delete_partial`, the `.sfpart` is removed.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Config`] for unknown jobs.
    pub fn cancel(&self, id: &str, delete_partial: bool) -> Result<(), EngineError> {
        let shared = self.lookup(id)?;
        shared
            .delete_partial_on_cancel
            .store(delete_partial, Ordering::Relaxed);
        shared.cancel.cancel();
        Ok(())
    }

    /// Subscribes to a job's events.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Config`] for unknown jobs.
    pub fn subscribe(&self, id: &str) -> Result<broadcast::Receiver<JobEvent>, EngineError> {
        Ok(self.lookup(id)?.events.subscribe())
    }

    /// Snapshots one job.
    #[must_use]
    pub fn snapshot(&self, id: &str) -> Option<JobSnapshot> {
        let shared = self.lookup(id).ok()?;
        Some(crate::supervisor::snapshot_of(&shared))
    }

    /// Snapshots every live job known to this engine instance.
    #[must_use]
    pub fn list_jobs(&self) -> Vec<JobSnapshot> {
        let jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        jobs.values()
            .map(|s| crate::supervisor::snapshot_of(s))
            .collect()
    }

    /// Changes the global speed limit live; `None` removes the cap.
    pub async fn set_global_speed_limit(&self, kib_per_s: Option<u64>) {
        let rate = kib_per_s.map_or(0, |kib| kib * 1024);
        self.inner.limiter.global().set_rate(rate).await;
    }

    /// Changes a job's per-download limit live; `None` inherits the global
    /// cap.
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::Config`] for unknown jobs.
    pub async fn set_job_speed_limit(
        &self,
        id: &str,
        kib_per_s: Option<u64>,
    ) -> Result<(), EngineError> {
        let shared = self.lookup(id)?;
        let existing = shared
            .job_bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match (existing, kib_per_s) {
            (Some(bucket), Some(kib)) => bucket.set_rate(kib * 1024).await,
            (Some(_), None) => {
                *shared
                    .job_bucket
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            }
            (None, Some(kib)) => {
                *shared
                    .job_bucket
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(Bucket::new(kib * 1024));
            }
            (None, None) => {}
        }
        Ok(())
    }

    /// Ids of jobs in a given state (from the journal, so it works across
    /// restarts).
    ///
    /// # Errors
    ///
    /// Returns [`EngineError`] on journal failure.
    pub fn job_ids_in_state(
        &self,
        state: crate::engine::JobState,
    ) -> Result<Vec<String>, EngineError> {
        let rows = self.inner.journal.jobs_in_states(&[state.as_str()])?;
        Ok(rows.into_iter().map(|r| r.id).collect())
    }

    fn lookup(&self, id: &str) -> Result<std::sync::Arc<JobShared>, EngineError> {
        self.inner
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| EngineError::Config(format!("unknown job {id}")))
    }
}

/// Resolves `dest_dir/filename` without overwriting: appends ` (2)`, ` (3)`…
/// before the extension when the path exists on disk OR is claimed by an
/// active download row.
async fn resolve_destination(
    dest_dir: &Path,
    filename: &str,
    claimed: &std::collections::HashSet<String>,
) -> Result<PathBuf, EngineError> {
    let dir = dest_dir.to_path_buf();
    let name = filename.to_owned();
    let claimed = claimed.clone();
    tokio::task::spawn_blocking(move || -> Result<PathBuf, EngineError> {
        std::fs::create_dir_all(&dir)?;
        let candidate = dir.join(&name);
        if !candidate.exists() && !claimed.contains(&candidate.display().to_string()) {
            return Ok(candidate);
        }
        let stem = Path::new(&name)
            .file_stem()
            .map_or_else(|| name.clone(), |s| s.to_string_lossy().into_owned());
        let ext = Path::new(&name)
            .extension()
            .map(|s| format!(".{}", s.to_string_lossy()));
        for n in 2..10_000u32 {
            let candidate = dir.join(format!("{stem} ({n}){}", ext.clone().unwrap_or_default()));
            if !candidate.exists() && !claimed.contains(&candidate.display().to_string()) {
                return Ok(candidate);
            }
        }
        Err(EngineError::Config(format!(
            "could not find a free name for {} in {}",
            name,
            dir.display()
        )))
    })
    .await
    .map_err(|err| EngineError::Config(format!("dest resolution task: {err}")))?
}

/// Returns the replacement URL configured on the spec, if any.
fn refreshed_url(spec: &JobSpec) -> Option<String> {
    spec.url_refresher.as_ref().and_then(|r| r.refresh())
}

fn spec_ref_start_paused(spec: &JobSpec) -> bool {
    spec.start_paused
}
