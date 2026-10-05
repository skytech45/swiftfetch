# Changelog

All notable changes to SwiftFetch are documented here. The format follows
Keep a Changelog; versions follow SemVer.

## [Unreleased]

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
- **Acceptance suite** (`tests/m1_acceptance.rs`, 11 tests): 100 MiB × 8
  segments with ≥3× measured speedup and exact journal sums; kill -9 at
  50% → resume → SHA-256 match; randomized kill-offset property test;
  non-resumable-server downgrade with user notice; pause→relaunch→resume;
  expiring-URL transparency; global limiter 512 KiB/s within ±10%;
  per-download override; dynamic rebalancing beats static chunking
  (journal shows the splits); mid-download entity change → clean restart
  to the new content; wrong Content-Length fails cleanly.

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
