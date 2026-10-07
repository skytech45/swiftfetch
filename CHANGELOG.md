# Changelog

All notable changes to SwiftFetch are documented here. The format follows
Keep a Changelog; versions follow SemVer.

## [Unreleased]

### Milestone 3 — Scheduler + automation (2026-10-07)

#### Added

- **`swiftfetch-scheduler`** (crates/scheduler): `Schedule` model
  (once / daily / daily-window with cross-midnight support / periodic with
  jitter) persisted as JSON on the `queues` table; a single tokio timer
  service owning a min-heap of `(next_fire, queue_id)` with an injectable
  clock; persisted timers survive restart — the missed-fire policy runs an
  overdue open if it is less than 15 minutes late, else marks it skipped;
  hourly + daily **quota windows** (`QuotaLedger`, pure logic, persisted
  snapshot) with a pause-everything gate; post-queue power actions
  (**sleep / hibernate / shutdown**) behind a 60-second cancellable
  countdown — Win32 `SetSuspendState` / `InitiateSystemShutdownExW` with
  per-call `SE_SHUTDOWN_NAME` (never elevated), `osascript` on macOS,
  logind (`loginctl`/`systemctl`, argv-only) on Linux.
- **AV auto-scan hook** (engine): every completed download is scanned
  before the rename into its destination. `AvScanner` trait +
  Windows-Defender implementation via `MpCmdRun.exe` located at runtime
  (stable + Platform paths); flagged files are deleted and fail with
  `E_MALWARE_DETECTED`; a scanner that cannot run never blocks downloads.
  CI tests use a deterministic content scanner; the real-EICAR check runs
  locally (`--ignored`) and passes (Defender real-time blocks the write at
  os error 225 or the on-demand scan flags it).
- **CLI** (`crates/cli` → `swiftfetch` binary): `add / list / status /
  pause / resume / cancel / queues / start-queue / stop-queue` over the
  **same SQLite database** — writes staged under `BEGIN IMMEDIATE`
  (single-writer discipline); the app consumes staged downloads and
  control commands within ~1 s and acts through the live engine.
- **Desktop automation**: queue scheduler service (fires `scheduler://fired`,
  opens/closes queues and pauses in-flight jobs on window close);
  quota gate in the queue runner (pauses everything when a limit is
  exhausted, auto-resumes on window reset, `quota://changed` events, live
  quota readout in the status bar); post-action countdown with a UI cancel
  button; **clipboard URL monitor** (opt-in, surfaces a toast with an Add
  button, `clipboard://url`); **drag-and-drop** URLs onto the window opens
  the add dialog prefilled.
- **UI**: per-queue schedule editor (kind/time/post-action), quota fields
  and clipboard toggle in Settings, toast stack, quota status chip;
  schedule editor button (⏱) in the queue panel.
- **Store**: migration v2 (`staged_downloads`, `cli_commands`);
  `Store::with_conn_immediate` for cross-process writers; queue rows now
  expose `schedule_json` + `post_action`.

#### Notes

- `QuotaConfig` serializes camelCase (`hourlyLimit`/`dailyLimit`) to match
  the UI round-trip; the ledger snapshot persists under `quota.ledger`.
- The clipboard monitor is opt-in and default-off.

### Milestone 2 — Desktop app shell (2026-10-07)

#### Added

- **Desktop app** (Tauri v2 + React 18): main window with toolbar,
  category sidebar, sortable download table with live progress bars,
  status bar; Add-URL dialog (category/queue pickers, connections slider
  1–32, start now / add-to-queue / queue-later), per-download segment
  progress dialog, queue panel (create/start/stop/delete/reorder), and a
  settings dialog (global speed limit, theme).
- **State layer**: typed SQLite repositories (`crates/store` repos module)
  — downloads, categories (6 seeded defaults + extension-based
  auto-categorization), queues (ordering, move clamping, concurrency limit
  1–5), settings; refinery migrations; two-connection split (engine journal
  + UI repos) on one WAL database.
- **Queue runner**: 500 ms tick + wake-notify loop starting queued/paused/
  interrupted jobs up to each active queue's concurrency limit, in job
  order; completion notifications via the OS notification plugin.
- **Event bridge**: engine `JobEvent` broadcast forwarded to the webview as
  `download://event`; UI refreshes are event-driven with a ≤ 1 Hz
  dirty-check poll fallback.
- **Tray**: show/hide, pause-all, resume-all, quit — menu built from live
  engine snapshots.
- **Engine hardening**: resumed jobs persist the `downloading` state on
  entry (queue runners and the UI count in-flight downloads by the
  journaled state); destination resolution consults final paths claimed by
  active rows so paused copies of the same URL never collide; jobs paused
  before their first start are planned fresh on resume (an empty plan no
  longer finalizes an empty part file — E_SIZE_MISMATCH fix); unknown-
  length segments are never split; saturating arithmetic in progress math.
- **i18n + theming**: en/hi locale dictionaries with a React context
  hook; light/dark themes via CSS variables.
- **Integration tests** (`tests/m2_flow.rs`): add → auto-categorize →
  progress events → complete → pause → engine restart → rehydrate →
  resume; and a queue-runner drain test (5 jobs, concurrency 2, order
  preserved) mirroring the Tauri command logic 1:1.

#### Notes

- Tauri e2e (WebDriver) smoke is deferred to M3 tooling — WebView2 exposes
  no accessibility tree for UIA, so the M2 flow is covered by Rust
  integration tests mirroring command logic plus manual launch smoke.

### Milestone 1 — Download engine core (2026-10-05)

#### Added

- **Download engine** (`swiftfetch-engine`): HEAD+range probing with
  Yes/No/Unknown resume-capability detection (incl. `If-RangeOnly`
  conditional-resume class); dynamic multi-connection segmentation
  (default 8, configurable 1–32, 1 MiB minimum segment); 500 ms EWMA
  supervisor with split-largest work-stealing, <50%-of-mean slow-window
  splitting and 8 KiB/s stall reassignment; per-segment retry budgets that
  survive stall reassignment (new ranges inherit the counter; URL-refresh
  requeues reset it); token-bucket speed limiting (global + per-download
  buckets, FIFO waiters, live rate changes, unlimited = rate 0); SHA-256 +
  MD5 streamed during write with `Digest:`/`Content-MD5` verification;
  URL auto-refresh on 403/404/410 with coalescing (≤ 3 attempts);
  unknown-length (chunked) single-connection mode; pre-allocation above
  100 MiB; positioned writes through a single per-job disk writer with
  ≤ 1 s fsync cadence and journal checkpoints at most once per second;
  crash recovery (`Engine::open` marks interrupted + clamps bookkeeping to
  bytes on disk); pause/resume/cancel; atomic rename + history rows.
- **Segment retry policy**: refresh-worthy failures (403/404/410) surface
  immediately to the supervisor for URL refresh — they never consume the
  retry budget; transport/EOF failures retry with quadratic backoff capped
  at 8 s.
- **HTTP client** (`swiftfetch-net`): rustls, ≤ 10 redirects, optional
  proxy and per-job user agent; no total-request timeout by design.
- **Scripted test server** (`scripts/test-server`): fixed-body routes with
  per-connection throttling, slow-Nth-connection, expiring URLs (410),
  truncated-206 resets, wrong Content-Length, ETag swaps, request log.
- **Crash harness** (`sf-engine-child` bin): downloads with a hard-exit
  (kill -9 equivalent) after N bytes for crash-recovery testing.
- **Acceptance suite** (`tests/m1_acceptance.rs`): 100 MiB × 8 segments
  with exact journal sums, 8 participating connections and SHA-256 match
  (runs in CI); the wall-time speedup comparison (≥3× on reference
  hardware, ~5× locally) is a separate `--ignored` test because shared
  3-vCPU CI runners measure as low as 1.3× — per the no-flaky-tests rule.
  Plus: kill -9 at 50% → resume → SHA-256 match; randomized kill-offset
  property test; non-resumable-server downgrade with user notice;
  pause→relaunch→resume; expiring-URL transparency; global limiter
  512 KiB/s within ±10%; per-download override; dynamic rebalancing beats
  static chunking (journal shows the splits); mid-download entity change
  → clean restart to the new content; wrong Content-Length fails cleanly.

### Added

- `docs/admin-panel.md`: roadmap for the admin panel & services track (track
  A) — web dashboard per Sachin's 16-module checklist, mapping each module to
  SwiftFetch (update feed + force-update/version control highest priority,
  feature flags, licensing + device management, payments + coupons, opt-in
  analytics; RBAC + audit log as foundations; orders/vendors/moderation
  marked not applicable). Track-A milestones A0–A4 slotted parallel to the
  desktop M1–M6 with two hard gates (distribution control before v1.0
  release, licensing before the M5 paywall).

## [0.1.0] — 2026-10-05

### Milestone 0 — Project scaffolding

#### Added

- Cargo workspace + npm workspaces implementing the build-prompt repo layout;
  all crates compile: `engine`, `net`, `store`, `scheduler`, `media`,
  `grabber`, `torrent`, `native-host`, `cli`, `common`, and the
  `swiftfetch-desktop` Tauri shell.
- `swiftfetch-store`: SQLite opened in WAL mode (`synchronous=NORMAL`,
  `foreign_keys=ON`), refinery-embedded schema v1 migrations (`downloads`,
  `segments` journal, `queues`, `queue_items`, `categories`, `mirrors`,
  `site_logins`, `settings`, `history`), integration tests for WAL mode,
  schema completeness, migration idempotence and FK enforcement.
- Tauri v2 desktop shell: window titled "SwiftFetch", `ping` command proving
  the TS → Rust round-trip, `db_status` command reporting the store path,
  journal mode and table count; store opens during app setup so the DB file
  is created on first run at the OS app-data directory
  (`%APPDATA%\SwiftFetch\swiftfetch.db` on Windows).
- React 18 + Vite UI scaffold: engine round-trip panel (button-triggered +
  on-mount ping with auto/button invocation counters) and SQLite status card
  (path, WAL badge, table count), light/dark aware styles.
- Placeholder original branding: gradient+bolt icon source
  (`scripts/gen-icon.mjs`) and generated icon set in `src-tauri/icons/`.
- CI matrix (windows/macos/ubuntu): `cargo fmt --check`, clippy
  `-D warnings` (pedantic enabled), `cargo test`, `cargo build`; node job:
  typecheck, ESLint, build.
- Docs: `docs/system-design.md` (architecture contract) and `docs/PRD.md`
  (feature list + metrics); `docs/threat-model.md` and `docs/i18n-guide.md`
  stubs (filled in M5). License: MIT OR Apache-2.0 dual.

#### Decisions (doc conflicts resolved with Sachin's approval)

1. UI framework: **React 18 + Vite** (Build Prompt §4 locked stack) over the
   System Design's Svelte 5.
2. Partial-file suffix: **`.sfpart`** (Build Prompt) over `.part`.
3. **Rust edition 2024** (exceeds the Build Prompt's 2021+ minimum).
4. Rebalancing supervisor tick: **500 ms EWMA** (System Design §5.1 Phase C)
   over the Build Prompt's 2 s window — the finer-grained spec wins.
5. Repo layout: **split crates** (engine/net/store/scheduler/media/grabber/
   torrent/native-host/cli/common) per Build Prompt §6.

Deviation from the tree: `tauri.conf.json` lives at
`apps/desktop/src-tauri/tauri.conf.json` because Tauri v2 tooling requires
the config beside the crate it configures (a root-level file is not read by
`tauri-build`).

#### Deferred

- Engine, scheduler, media, grabber, torrent, native-host and CLI
  implementations land in M1–M6 per the milestone plan.
- Installers, updater and code signing: M5.
- i18n locales (EN + HI): M5.
