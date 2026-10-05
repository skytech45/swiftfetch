# Changelog

All notable changes to SwiftFetch are documented here. The format follows
Keep a Changelog; versions follow SemVer.

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
