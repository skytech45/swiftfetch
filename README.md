# SwiftFetch

> **SwiftFetch is a working title** — the final name is pending trademark
> clearance. All branding is original; no third-party download-manager assets
> or code are used anywhere in this project.

A production-quality, cross-platform desktop download manager for
Windows 10/11, macOS and Linux. It matches Internet Download Manager's core
behaviors — dynamic multi-connection segmentation, pause/resume with
crash-safe journaling, queues + scheduler, browser capture, video grabbing —
and exceeds it with cross-platform support, BitTorrent, checksum verification
and an open plugin surface.

**Status:** Milestone 6 — torrents, checksums, plugin surface, hardening complete. The build contract is
the "SwiftFetch Build Prompt" document; the architecture contract is
[docs/system-design.md](docs/system-design.md); product scope is
[docs/PRD.md](docs/PRD.md).

## Tech stack (locked)

| Layer | Technology |
| --- | --- |
| Download engine | Rust (edition 2024), tokio |
| HTTP client | reqwest (rustls) |
| Desktop shell | Tauri v2 (Rust backend + OS webview) |
| UI | TypeScript (strict), React 18 + Vite |
| State persistence | SQLite WAL via rusqlite, refinery migrations |
| Media merge | ffmpeg sidecar binary (argv only, no shell) |
| Browser extensions | MV3 (Chrome/Edge/Opera) + WebExtensions (Firefox) |
| Extension bridge | Rust native-messaging host |
| Torrents | librqbit (Milestone 6) |
| Installers | NSIS / DMG / AppImage, signed (Milestone 5) |

## Repo layout

```
swiftfetch/
├── apps/desktop/       Tauri v2 app (React+TS UI, Rust commands)
├── crates/             engine · net · store · scheduler · media · grabber
│                       sites/youtube · torrent · native-host · cli · common
├── extensions/         shared TS core + chrome/firefox manifests (M4)
├── sidecars/ffmpeg/    per-platform ffmpeg binaries (M4)
├── scripts/            dev utilities (icon generator, test server, …)
└── docs/               system design, PRD, threat model, i18n guide
```

## Development quickstart

Prerequisites: Rust (stable, MSVC on Windows), Node ≥ 20.

```sh
npm install                 # install frontend deps
npm run build               # vite build → apps/desktop/dist (required before cargo build)
cargo build --workspace     # build all Rust crates + the desktop shell
cargo test --workspace      # run Rust tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
npm run typecheck && npm run lint

cargo run -p swiftfetch-desktop   # launch the desktop app window
```

## Milestones

| # | Name | Scope |
| --- | --- | --- |
| M0 | Project scaffolding | workspace, CI, docs skeleton, SQLite store, Tauri shell ✅ |
| M1 | Download engine core | segmentation, resume, speed limiter, test server ✅ |
| M2 | Desktop app shell | main window, dialogs, categories, queues UI, tray ✅ |
| M3 | Scheduler + automation | scheduler, quotas, clipboard, drag-drop, AV hook, CLI ✅ |
| M4 | Browser + media | MV3/Firefox extensions, native host, HLS/DASH grabber, YouTube one-click (§12.4) ✅ |
| M5 | Depth + packaging | site grabber, mirrors, i18n, updater, installers ✅ |
| M6 | Differentiators | BitTorrent, checksums, plugin API, hardening ✅ |
| A0–A4 | Admin panel & services | web dashboard: update feed + version control/force-update, feature flags, licensing + device binding, payments + coupons, opt-in analytics — see [docs/admin-panel.md](docs/admin-panel.md) |

## License

Dual-licensed under [MIT](LICENSE) OR [Apache-2.0](LICENSE-APACHE).

Dependency licenses are allowlisted (MIT / Apache-2.0 / BSD / ISC and
similar permissive licenses only — no GPL in the core); `deny.toml`
enforces this via `cargo deny check`. The `ffmpeg` sidecar is a separate
GPLv3 binary (Gyan essentials build, Windows x64) fetched on first media
use with SHA-256 verification — never bundled, never linked, invoked only
as its own process via argv arrays.
