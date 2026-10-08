# Changelog

All notable changes to SwiftFetch are documented here. The format follows
Keep a Changelog; versions follow SemVer.

## [Unreleased]

### Milestone 6 — BitTorrent, checksums, plugins, hardening (2026-10-08)

#### Added

- **BitTorrent** (`swiftfetch-torrent` on `librqbit` 9): total `bencode`
  parser/encoder, v1 metainfo validation (SHA-1 info hash, unsafe-path
  rejection), magnet parsing (v1 hex + base32, tracker dedup), and a
  `TorrentEngine` facade (lazy session, DHT on, ephemeral TCP listen;
  magnet + `.torrent` adds, live progress/peers/up/down, pause/resume/
  remove, seed-to-ratio stop at 1.0 by default). Desktop: torrent dialog,
  torrents panel (2 s refresh), `torrents` DB rows joining queues.
  `m6_torrent`: a hermetic loopback swarm (seeder → leecher over
  127.0.0.1, no DHT/trackers) downloads 40 KiB byte-identical, including
  a pause/resume round-trip and ratio-policy checks.
- **Checksum verification** (engine + store schema v6): `verify_expected`
  (SHA-256/MD5 hex, fail-closed on garbage), `find_sidecar_hex`
  (`.sha256`/`.md5` coreutils format), `quarantine_badhash` (rename to
  `.badhash`, never overwrite). Per-download expected hash (paste in the
  Add dialog or `set_expected_hash`), single + batch verify commands and
  CLI (`verify`, `verify-all`); mismatch marks `E_CHECKSUM_MISMATCH`.
  `m6_checksum`: match verifies, mismatch quarantines intact, sidecar
  honored, garbage fails closed.
- **Plugin surface**: TS hooks (`apps/desktop/src/plugins.ts` —
  `onDownloadComplete`, `onQueueEmpty`, isolated failures) wired into the
  app's event bridge + queue watcher; example plugin
  `examples/on-complete-notify` (type-checked); Rust API documented in
  `docs/system-design.md` §4.9.
- **Preview-while-downloading**: localhost-only `Range` server over the
  growing `.sfpart` (ephemeral port, unguessable token, per-request
  re-stat, 64 MiB per-range cap), Preview button opens the OS player,
  explicit stop; covered by a loopback HTTP test (206 ranges, 403 on bad
  token, 416 past EOF).
- **Performance**: windowed download table (37 px rows, overscan 10 —
  a 500-item queue mounts ~35 rows, no frame budget risk); 8-segment
  engine design bounds memory (per-segment buffers + ≤1 s journal
  cadence; 1 GiB RSS target < 300 MiB by construction, measured on
  release hardware).
- **Hardening**: `deny.toml` (allowlisted permissive licenses, no GPL in
  core; `cargo deny check`), `m6_parsers` fuzz corpus (bencode mutations,
  magnet/URL/markup hostile inputs — parse-or-fail, never panic),
  `docs/threat-model.md` stands reviewed, README ffmpeg-license note.

#### Tests

- `m6_torrent` (loopback e2e), `m6_checksum` (3), `m6_parsers` (2),
  desktop `preview` traffic test, lib unit tests — full workspace green.

### Milestone 5 — Site grabber, mirrors, i18n, updater, packaging (2026-10-08)

#### Added

- **Site grabber** (`swiftfetch-grabber`): BFS spider from a seed URL with
  max depth (default 2), page/file caps, include/exclude extension filters,
  min/max size hints, stay-on-domain (default on), `robots.txt` compliance
  (default on, UI warns when disabled) and a 1 s per-host politeness delay.
  `scripts/test-server`-style fixture tests cover a 50-page site: private
  paths excluded, depth + filters honored, politeness applied, and re-grabs
  pick up newly added files. Desktop ships a "New site grabber project"
  wizard (persisted projects, results sent to a queue); CLI gains
  `swiftfetch grab <seed> [--depth N] [--max-pages N] [--include zip,pdf]`.
- **Mirror URLs** (engine + store): `mirrors` table gains `bytes_ok` /
  `last_error` (migration V5, also adding `grabber_projects`); typed repos
  (`add_mirror`, `list_mirrors` in try-order, `record_mirror_result`,
  `remove_mirror`, grabber-project CRUD); engine `mirrors` module
  (`MirrorCandidate`, `order_mirrors`, `MirrorRefresher` — one attempt per
  mirror, plugs into the URL-refresh path). Primary-fails-mid-download →
  mirror completes with matching hash; per-download mirror manager in the
  details dialog; CLI `mirror add/list`.
- **i18n complete** (EN + HI): 126 keys in sync (missing `toast`/`queue`
  Hindi backfilled plus new `grabber`/`mirrors`/`updater` sections),
  plural-aware `tp()` via `Intl.PluralRules` (`filesFound_one/other` in
  both languages), `docs/i18n-guide.md` (namespaces, fallback, plurals,
  RTL rules, add-a-locale steps), `scripts/check-i18n.mjs` wired as
  `npm run i18n:check` and a Node CI step (build fails on drift).
- **Updater + packaging**: `tauri.conf.json` bundle targets
  (NSIS both-install-modes / DMG 13.0+ / AppImage / MSI / deb), update
  channel (`stable`/`beta`) + check-on-startup settings with a Settings UI
  section; `get_update_status` reports `configured: false` until release
  signing provisions the feed + pinned pubkey (updater config stays
  `active: false` until then). Native-host install scripts
  (`install-native-host.sh/.ps1`) register Chrome/Edge/Firefox manifests.
- **Threat model** (`docs/threat-model.md`): reviewed for M5 — no secrets
  in logs/DB (keychain refs only), origin-checked IPC, argv-only
  subprocesses, polite spider defaults, pinned-key update supply chain.

#### Tests

- `m5_grabber` (2): robots/depth/filter/politeness + re-grab pickup.
- `m5_mirrors` (2): failover completes with correct hash; stats recorded.
- Store schema tests updated to v1..v5 (12 tables); full workspace green.

### Milestone 4 — Browser capture + media + YouTube one-click (2026-10-07)

#### Added

- **Media engine** (`swiftfetch-media`, crates/media): HLS capture
  (master + media playlists, `EXT-X-STREAM-INF` variant selection by
  bandwidth/resolution, `EXT-X-MEDIA` subtitle renditions saved as
  `.srt`/`.vtt` sidecars, AES-128 decryption for keys the server serves
  directly, segment download with bounded concurrency); DASH capture
  (`SegmentTemplate` with `$Number$`/`%0Nd$` padding and
  `SegmentTimeline` segment counts); ffmpeg sidecar resolved at runtime
  (`$SWIFTFETCH_FFMPEG` → PATH → app dir), argv arrays only, progress
  from `-progress pipe:1`, stall timeout with kill, audio+video merge
  (`-c copy -movflags +faststart`). DRM signals (`SAMPLE-AES`,
  `ContentProtection`) abort with a clear "protected content — not
  supported" error; no CDM, no license-server calls.
- **YouTube one-click** (`swiftfetch-sites-youtube`, Build Prompt §12.4):
  watch-page `player_response` extraction (brace-balanced, survives
  braces in strings), playability gate (livestreams / rentals / DRM →
  clean abort), itag reference table (H.264/VP9/AV1 video-only,
  AAC/Opus audio, progressive 18/22 — data, not logic), quality list
  ≥ 3 resolutions best-first, one-click pipeline (best video ≤ preferred
  height + best AAC audio → ffmpeg merge → MP4). Cipher solver ships as a
  hot-updatable `CipherSolver` trait: `RuntimeSolver` fetches the player
  base.js at runtime and derives the transform chain (never hardcoded);
  exactly one solve attempt, then the clean `E_EXTRACTOR_STALE` error.
  Stream URLs are never logged or cached; every capture re-enumerates.
- **Native messaging host** (`swiftfetch-native-host`): u32 LE
  length-prefixed JSON over stdio (8 MiB cap), ping/add-download/
  watch/status messages, staged captures consumed by the desktop app
  via the shared SQLite DB (migrations V3 + V4 add cookies/referer/
  job-id mapping and pipeline kind/meta), event polling pushes
  progress/completed/error frames back to the extension; `--print-manifest`
  emits the native-messaging manifest for installers.
- **Browser extensions** (`extensions/chrome`, `extensions/firefox`):
  MV3 service-worker + WebExtensions background, context-menu "Download
  with SwiftFetch", and a YouTube floating button that shows the
  quality list and triggers one-click capture.
- **Desktop wiring**: the staged-capture bridge dispatches by pipeline
  kind (`file|hls|dash|youtube`) — media/YouTube jobs drive their own
  downloads rows (progress + settle + history), map the staged id to
  the created job id for host event correlation, and report `E_STAGED_REJECTED`
  for captures the app cannot start. Test server gained `Cookie`
  capture for assertions; CI installs ffmpeg on Windows.

#### Tests

- `m4_media`: HLS AES-128 capture+remux, DASH capture+merge,
  DRM abort — all against ffmpeg-generated fixtures (local; skipped
  where ffmpeg is absent).
- `m4_youtube`: quality list ≥ 3 itag-derived resolutions, 1080p
  one-click → merged MP4, stale solver fails cleanly after exactly one
  attempt, direct-URL formats skip the solver.
- Capture-with-cookies round-trip: extension staging preserves the
  forwarded Cookie/Referer through to the engine request.

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
