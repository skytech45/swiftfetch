# SwiftFetch — System Design (Milestone 0 working copy)

**Status:** living document. Architectural source of truth per the Build
Prompt; the Build Prompt itself is the behavioral/feature source of truth.
This file expands Build Prompt §5 and will grow each milestone. Source
documents: `SwiftFetch System Design (PDF).pdf` and `SwiftFetch Build Prompt
(PDF).pdf` in the parent folder.

## 0. Resolved architecture decisions

The two source documents conflicted on five points; Sachin approved these
resolutions on 2026-10-05 (recorded in CHANGELOG.md):

| # | Topic | Decision | Loser |
| --- | --- | --- | --- |
| 1 | UI framework | **React 18 + Vite** (Build Prompt §4, locked) | Svelte 5 (System Design §4.3) |
| 2 | Partial-file suffix | **`.sfpart`** (Build Prompt §9.1.8) | `.part` (System Design §10) |
| 3 | Rust edition | **2024** (≥ Build Prompt's 2021+) | — |
| 4 | Rebalance supervisor tick | **500 ms EWMA** (System Design §5.1 Phase C) | 2 s / <50%-of-mean (Build Prompt §9.1.3) |
| 5 | Repo layout | **Split crates** (Build Prompt §6) | single `sf-core` crate (System Design §3.3) |

Deviation from the Build Prompt tree: `tauri.conf.json` lives at
`apps/desktop/src-tauri/tauri.conf.json` — Tauri v2 tooling reads the config
from beside the crate it configures, not from the repo root.

## 1. Design goals (ranked)

1. **Tiny footprint** — installer ≤ 15 MB on Windows, resident memory < 80 MB
   with 5 active downloads. No Electron/Chromium bundling (Tauri v2 uses the
   OS webview).
2. **Crash-safe by construction** — a kill -9 at any moment loses at most
   ~1 s of *bookkeeping*; never corrupts state or files. Resume works after
   OS crash, power loss, or user kill.
3. **Cross-platform from day one** — Windows primary; macOS/Linux from the
   same codebase; platform code behind trait boundaries, no `#ifdef` spaghetti.
4. **Clean-room** — no third-party download-manager code, binaries, or
   trademarks. Original names, icons, strings.
5. **Browser-agnostic integration** — Chrome/Edge/Opera (MV3) + Firefox
   (WebExtensions) capture through one native-host protocol.
6. **Ethical media handling** — grab only what servers openly serve. No DRM
   circumvention, ever. Hard boundary, not a backlog item.

## 2. Process & layering model

| Layer | Process | Talks to |
| --- | --- | --- |
| Browser extensions | Browser's extension process | Native host over stdio (length-prefixed JSON) |
| Native host `swiftfetch-native-host` | Short-lived per browser session | Core over localhost IPC; launches app if needed |
| App `swiftfetch-desktop` (Tauri) | One long-lived GUI process | Everything below via in-process calls |
| Crates `engine`, `scheduler`, `media`, `grabber`, `net` | In-process (rlib) | Network, filesystem, ffmpeg sidecar |
| `store` | In-process | SQLite (WAL), OS keyring refs |
| `swiftfetch` CLI | Short-lived commands | Same SQLite DB (single-writer discipline, §5) |
| ffmpeg | Spawned per media job | argv pipes / temp segment files |

**Key layering rule:** workspace crates never import Tauri, UI, or browser
types. The Tauri shell and the CLI are thin callers. This keeps the engine
testable headless and makes a future daemon/service mode trivial.

## 3. Repository layout

```
swiftfetch/
├── Cargo.toml                  # workspace root (edition 2024)
├── package.json                # TS workspace (npm workspaces)
├── README.md · LICENSE · LICENSE-APACHE · CHANGELOG.md
├── docs/                       # system-design.md, PRD.md, threat-model.md, i18n-guide.md
├── apps/
│   └── desktop/                # Tauri v2 app
│       ├── src/                # React+TS UI (components/, locales/, hooks/, stores/, utils/)
│       ├── src-tauri/          # Tauri backend (thin: commands → crates) + tauri.conf.json
│       └── tests/
├── crates/
│   ├── engine/                 # core download engine (M1)
│   ├── net/                    # proxy, auth, cookies, TLS (M1/M4)
│   ├── store/                  # SQLite WAL, migrations, repositories (M0/M1)
│   ├── scheduler/              # queues, cron, quotas, shutdown (M3)
│   ├── media/                  # HLS/DASH parse, ffmpeg wrapper, subtitles (M4)
│   ├── grabber/                # site spider (M5)
│   ├── torrent/                # BitTorrent via librqbit (M6)
│   ├── native-host/            # browser native-messaging host binary (M4)
│   ├── cli/                    # `swiftfetch` CLI binary (M3)
│   └── common/                 # shared types, errors, config
├── extensions/                 # shared/ + chrome/ + firefox/ (M4)
├── sidecars/ffmpeg/            # per-platform ffmpeg binaries (M4)
├── scripts/                    # dev utilities (gen-icon.mjs; test-server M1)
├── .github/workflows/ci.yml    # build+test+lint on win/mac/linux
└── tests/e2e/                  # Tauri e2e smoke tests (M2+)
```

## 4. Component APIs

### 4.1 Store (`swiftfetch-store`, shipped in M0)

SQLite in WAL mode (`journal_mode=WAL`, `synchronous=NORMAL`,
`foreign_keys=ON`). Migrations are embedded via refinery
(`migrations/V1__initial_schema.sql`). Current API:

```rust
pub struct Store { /* Connection + path */ }
impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError>;
    pub fn open_default() -> Result<Self, StoreError>;   // OS app-data dir
    pub fn path(&self) -> &Path;
    pub fn journal_mode(&self) -> Result<String, StoreError>;
    pub fn table_names(&self) -> Result<Vec<String>, StoreError>;
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>)
        -> Result<T, StoreError>;                        // repository escape hatch
}
pub fn default_data_dir() -> Option<PathBuf>;
pub enum StoreError { Open { .. }, Migrate(..), Sql(..) }
```

M1 adds typed repositories (`Downloads`, `Segments`, `Queues`, `Settings`)
that wrap `with_conn` and own all SQL; commands never write SQL inline.

### 4.2 Download engine (`swiftfetch-engine`, planned M1)

Lifecycle of one download: **probe → plan segments → fetch in parallel →
rebalance → verify → finalize.** Core data structures (Rust sketch):

```rust
pub struct DownloadJob {
    pub id: JobId,                       // uuid v4, PK in `downloads`
    pub url: Url,
    pub mirrors: Vec<Url>,
    pub dest: PathBuf,                   // final path (without .sfpart)
    pub total_len: Option<u64>,
    pub resume_cap: ResumeCap,           // None | Ranges | IfRangeOnly
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub segments: Vec<Segment>,
    pub state: JobState,
    pub max_conns: u8,                   // default 8, user 1..=32
}

pub struct Segment {
    pub idx: u32,
    pub start: u64,                      // absolute byte offset (inclusive)
    pub end: u64,                        // absolute byte offset (inclusive)
    pub done: u64,                       // bytes confirmed written (== DB)
    pub state: SegState,                 // Queued|Active|Stalled|Done|Failed
    pub conn_id: Option<u32>,
    pub ewma_bps: f64,                   // throughput EWMA for rebalancing
}

pub enum ResumeCap { None, Ranges, IfRangeOnly }

impl Engine {
    pub async fn probe(url: &Url) -> Result<Probe>;
    pub async fn start(job: DownloadJob) -> Result<JobHandle>;
    pub async fn pause(id: JobId) -> Result<()>;
    pub async fn resume(id: JobId) -> Result<()>;
    pub async fn cancel(id: JobId, delete_partial: bool) -> Result<()>;
    pub fn subscribe(id: JobId) -> broadcast::Receiver<JobEvent>; // @4 Hz
}
pub enum JobEvent {
    Progress { done: u64, total: Option<u64>, bps: f64 },
    StateChanged(JobState),
    Error(EngineError),
}
```

**Phase A — probing.** HEAD (fallback: GET `Range: bytes=0-0` when HEAD is
405/501). Record Content-Length, Accept-Ranges, ETag, Last-Modified,
Content-Type, Content-Disposition. `Accept-Ranges: bytes` or a 206 →
`ResumeCap::Ranges`; else ETag/Last-Modified present → `IfRangeOnly`; else
`None` (single connection, no resume, warn user on large files). Probe
result is persisted before any byte is written.

**Phase B — initial segmentation.** If Ranges and length known: split
`[0, total_len)` into `min(max_conns, ceil(total_len / MIN_SEG))` segments,
`MIN_SEG = 1 MiB` default. One tokio task per segment with its own pooled
connection (`Range: bytes={start+done}-{end}`).

**Phase C — dynamic rebalancing** (the IDM-class algorithm; supervisor ticks
every **500 ms**):

```text
loop every 500ms:
  for each Active segment s:
    s.ewma_bps = 0.7 * s.ewma_bps + 0.3 * (bytes_since_last_tick / 0.5)
  // 1. stall detection
  for each Active s where s.ewma_bps < STALL_FLOOR (8 KiB/s) and active > 5s:
    mark Stalled; abort connection; requeue remaining range
  // 2. split-largest-remaining (work stealing)
  find Active segment L with max remaining; find idle worker capacity
  if L.remaining > 2 * MIN_SEG and idle capacity:
    mid = L.start + L.done + L.remaining / 2
    shrink L to [L.start+done, mid]; spawn new segment [mid+1, L.end]
    atomically update DB rows (single transaction)
    // old connection keeps its (shorter) range — no abort needed
  // 3. keep-alive pool per host; finished tasks return connections
```

Static chunking strands throughput when one connection hits a throttled
route (the classic "7 of 8 parts done, 1 crawling" problem); splitting the
largest remaining segment redirects idle connections at the bottleneck.

**Phase D — Range/206/416 handling.**

| Server response | Engine action |
| --- | --- |
| 206 + Content-Range matches | Normal: stream body, positioned write at start+done |
| 206 but range mismatch | Abort connection, mark segment Failed, retry once with fresh probe; repeat → job Error `E_RANGE_MISMATCH` |
| 200 to a ranged request | If `done == 0` and sole segment: accept full body (downgrade to single-connection). Else discard, retry with If-Range. Never honors ranges → `ResumeCap::None` path |
| 416 Range Not Satisfiable | Re-probe; if length shrank or ETag changed → restart job from 0 with user notice |
| If-Range with stored ETag → 200 | Entity changed: discard segment progress for changed region, restart affected segments |
| If-Range → 206 | Clean conditional resume |

**Resume after restart/crash:** load segment rows; for each non-done segment
re-issue `Range: bytes={start+done}-{end}` with `If-Range: <etag>` (prefer
ETag over Last-Modified). Verify via a 0-0 probe that `total_len` is
unchanged before reusing offsets.

**URL auto-refresh (M1):** on 403/404/410 or connection reset mid-download,
re-resolve the URL (re-issue the original request chain incl. cookies from
the site-login manager / extension capture), then resume from journaled
offsets. Max 3 refresh attempts, then fail with a clear error.

**Mirrors (M5):** each segment task may pull its byte range from any URL in
`mirrors`; per-host repeated failure deprioritizes that mirror (exponential
backoff) and reassigns its ranges to healthy mirrors.

**Integrity (M6):** optional SHA-256/MD5 verify-on-complete — auto-detect
`.sha256`/`.md5` sidecar or Digest header, or user-supplied; mismatch marks
"checksum failed", keeps the file as `.badhash`, offers re-download.

### 4.3 Scheduler & queues (`swiftfetch-scheduler`, planned M3)

```rust
pub struct Queue {
    pub id: QueueId,
    pub name: String,                    // "Main", "Night downloads", …
    pub max_concurrent: u8,              // default 2 (UI-configurable 1..=5)
    pub schedule: Option<Schedule>,
    pub on_complete: PostAction,         // None | Sleep | Hibernate | Shutdown
    pub move_done_to: Option<QueueId>,
}
pub enum Schedule {
    Once { at: DateTime<Utc> },
    Daily { at: NaiveTime },
    Periodic { every: Duration, jitter: Duration },
    StartStop { start: NaiveTime, stop: NaiveTime },
}
```

Timer service: a single tokio task owns a min-heap of `(next_fire,
queue_id)`; persisted timers survive restart (recompute next fire on boot;
missed-fire policy: run if < 15 min overdue, else mark skipped). Periodic
sync queues hold sync entries (URL + local path + last ETag): conditional
HEAD each period; 200 with new ETag → enqueue job; 304 → skip. Post-actions
include OS power actions (Windows `SetSuspendState`/`ExitWindowsEx` with
`SE_SHUTDOWN_NAME` enabled per-call, never run elevated; macOS
`osascript`; Linux logind D-Bus) — always preceded by a **60-second
cancellable countdown**.

### 4.4 Media engine (`swiftfetch-media`, planned M4)

- **HLS:** fetch master `.m3u8` → parse `EXT-X-STREAM-INF` variants →
  quality menu (bandwidth/resolution/codecs, sorted desc; `EXT-X-MEDIA`
  alternate audio/subtitles). User picks quality → enumerate media segments
  → download concurrently through the engine's segmenter → AES-128 handled
  only for clear keys the server serves directly. Remux with ffmpeg
  (`-c copy`) locally.
- **DASH:** parse `.mpd` → pick best video+audio AdaptationSets by bandwidth
  → download init + media segments → merge
  (`ffmpeg -i video -i audio -c copy -movflags +faststart out.mp4`).
- **Subtitles:** `EXT-X-MEDIA:TYPE=SUBTITLES` tracks and DASH text sets are
  saved as `.srt`/`.vtt` sidecars with language suffix.
- **ffmpeg discipline:** sidecar resolved at runtime; args always an argv
  array (never shell strings); progress parsed from `-progress pipe:1`; hard
  timeout + kill on stall.
- **DRM:** DRM-signaled streams abort with a clear "protected content — not
  supported" message. No CDM, no license-server calls, no key extraction.

### 4.5 Net (`swiftfetch-net`, planned M1/M4)

Config model: `{ mode: system|manual|pac|none, manual: {http, https, socks5,
no_proxy}, pac: {url}, auth: {scheme, username, password_ref} }`. Passwords
never touch SQLite plaintext — OS keyring via the `keyring` crate, referenced
by handle. PAC evaluated with an embedded JS engine (boa/rquickjs — decide in
M1; shelling out to system JS is forbidden). NTLM/Kerberos/Negotiate via
system SSPI (Windows) / GSSAPI (Unix); NTLM crypto is never hand-rolled.

### 4.6 Native host (`swiftfetch-native-host`, planned M4)

Launched by browsers per session; length-prefixed JSON over stdio
(4-byte LE length + UTF-8 JSON, max 8 MiB). Validates `allowed_origins`
(extension IDs pinned at build time) before parsing anything else — unknown
origins get `E_ORIGIN_DENIED` and the connection drops. Translates extension
messages → app localhost IPC; launches the app via deep link when not
running; exits on stdin EOF (no orphan processes).

## 5. SQLite schema v1 (shipped in M0)

File: `%APPDATA%\SwiftFetch\swiftfetch.db` (Windows),
`~/Library/Application Support/SwiftFetch/swiftfetch.db` (macOS),
`$XDG_DATA_HOME/SwiftFetch/swiftfetch.db` (Linux). WAL mode,
`foreign_keys=ON`, UTC ISO-8601 text timestamps. Full DDL:
[`crates/store/migrations/V1__initial_schema.sql`](../crates/store/migrations/V1__initial_schema.sql)
— tables: `categories`, `queues`, `downloads`, `segments` (crash-safety
journal), `queue_items`, `mirrors`, `site_logins`, `settings`, `history`.

Write-ahead guarantees (engine contract for M1+):

- Segment progress (`done` bytes) checkpoints to the DB **at most once per
  second per segment** — bounds kill -9 loss to ≤ 1 s of bookkeeping;
  already-fsync'd file bytes are never re-downloaded because `done` only
  advances after write+flush of that chunk.
- Multi-row mutations (split segment, reassign, state change) run in a
  single transaction.
- On startup: `wal_checkpoint(TRUNCATE)` then recovery scan — jobs in
  `downloading`/`verifying`/`probing` move to `interrupted` and are offered
  for resume.
- Recovery ordering (the invariant): **disk is truth for bytes, DB is truth
  for the plan** — if the `.sfpart` is smaller than bookkeeping says,
  truncate bookkeeping to the file size; never trust the DB over disk.
- Single-writer discipline: the GUI process owns the write connection; the
  CLI mutates only under `BEGIN IMMEDIATE`; WAL lets readers proceed.

## 6. Event catalog

Tauri events (engine → UI, M1+; UI never polls faster than 4 Hz):

| Event | Payload | Notes |
| --- | --- | --- |
| `download://progress` | `{ id, done, total, bps, eta }` | throttled to 4 Hz per job |
| `download://state` | `{ id, state, error_code?, error_msg? }` | state machine transitions |
| `queue://changed` | `{ queue_id }` | membership/order changes |
| `scheduler://fired` | `{ queue_id, action }` | timer fired / window opened |

Native-messaging `EVENT` (host → extension, M4): `progress`, `completed`
(with path), `error` (with machine-readable code + human message).

## 7. Native messaging protocol (normative for M4)

Framing (both directions): `u32 LE length N` + `N` bytes UTF-8 JSON, max
8 MiB.

| Type | Direction | Purpose |
| --- | --- | --- |
| `ADD_URL` | ext → host | Single download: url, filename hint, cookies, UA, referer, pageUrl, headers?, startPaused |
| `ADD_BATCH` | ext → host | ≤ 500 items per message (pagination beyond); queue name; startPaused |
| `GRAB_MEDIA` | ext → host | Chosen media candidate: playlistUrl, kind (hls/dash), variant, cookies, UA, referer |
| `QUERY_STATE` | ext → host | Liveness/version probe → `STATE { appVersion, coreAlive, takeoverMode }` |
| `EVENT` | host → ext | Async job progress/completion/error |
| `ERROR` | host → ext | Reply envelope: `{ inReplyTo, code, message }` |

Security notes (normative): the host validates `allowed_origins` before
parsing any other field; cookies/headers reproduce only the exact request
the browser would have made and are never logged or persisted beyond the
job row's `cookies_json` (needed for resume); they never leave the machine.

## 8. Concurrency model (Rust / tokio)

- **One tokio task per active segment** (a task owns its connection for its
  lifetime). Tasks never share mutable state; coordination goes through the
  supervisor via mpsc/oneshot channels.
- **Single disk-writer task per job** serializes all file writes → no torn
  writes; `done` advances only after the writer acks a flushed batch.
- **Backpressure:** the writer's inbound channel is bounded (64 batches ×
  256 KiB); a fast network can't OOM the process through a slow disk.
- **Blocking work** (SQLite writes, ffmpeg spawn/wait, hashing) runs on
  `tokio::task::spawn_blocking` — never on async worker threads.
- **Shutdown:** CancellationToken per job; tasks observe it between chunks,
  checkpoint `done`, and exit within ~100 ms.
- **Rate-limiter integration** (token bucket: global + per-job buckets,
  capacity = 2 s burst, FIFO acquire) sits *before* the writer channel so
  limiting never deadlocks the pipeline. Speed-limit windows just swap the
  global bucket's rate — no connection churn.

## 9. Disk I/O design

- **Pre-allocation** when `total_len` is known (files > 100 MiB) → prevents
  mid-download ENOSPC; sparse fallback where prealloc is unsupported.
- **Positioned writes** (pwrite-style / OVERLAPPED on Windows) — no shared
  file cursor, safe with concurrent segments.
- **Temp `.sfpart` files:** bytes land in `<final>.sfpart`; completion =
  fsync + atomic rename to the final path — a crash can never leave a
  half-file masquerading as complete.
- **Free-space guard:** before starting and every 30 s during, check volume
  free space vs remaining bytes; pause with `E_NO_SPACE` instead of failing
  mid-write.
- **Hash verification (optional per job, M6):** stream-hash during write,
  compare before rename.

## 10. Milestone map

| Milestone | Scope | Exit criteria |
| --- | --- | --- |
| M0 ✅ | Workspace, CI, docs skeleton, SQLite store, Tauri shell | builds on 3 OSes; clippy/fmt clean; ping round-trip; DB created with WAL verified |
| M1 ✅ | Download engine core | 100 MiB × 8-segment speedup; kill -9 resume; expiring-URL refresh; limiter ±10%; rebalancing improves time — all green (2026-10-05) |
| M2 | Desktop app shell | main window/dialogs/categories/queues/tray; E2E smoke green |
| M3 | Scheduler, quotas, clipboard, drag-drop, AV hook, CLI | scheduler fires on time; quota gates; EICAR flagged; CLI round-trips |
| M4 | Browser extensions + native host + media | capture with cookies forwarded; HLS AES-128 + DASH merge; DRM aborts cleanly |
| M5 | Site grabber, mirrors, i18n, updater, packaging | robots honored; mirror failover; EN+HI complete; signed installers smoke-tested |
| M6 | Torrents, checksums, plugin surface, hardening | torrent round-trip; checksum states; fuzz/audit clean; RSS < 300 MiB @ 1 GiB |

## 11. Traceability

Every decision above traces to the source PDFs (section references inline).
The five conflict resolutions in §0 are the only intentional deviations; any
future conflict between the Build Prompt and this document is flagged to
Sachin rather than silently resolved.
