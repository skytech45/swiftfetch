# SwiftFetch — PRD (v1.0 condensed)

**Status:** living document; condensed from the SwiftFetch PRD v1.0 PDF for
the repo (source of truth for product scope). Feature work follows the Build
Prompt milestones M0–M6. "SwiftFetch" is a working title pending trademark
clearance; all branding is original (clean-room).

## 1. Vision

*The fastest, most dependable way to get any file onto your PC — on any OS,
without the guesswork.*

Large-file downloads on PC remain slow and fragile: browsers download on a
single connection, fail silently on flaky networks, and offer no scheduling,
speed control, or video-stream capture. SwiftFetch matches IDM-class
download-engine behavior while exploiting its gaps: cross-platform
(Windows first, then macOS/Linux), BitTorrent, open architecture, remote
management, and checksum verification — freemium, priced under IDM.

## 2. Target users

| Persona | Profile | Core needs |
| --- | --- | --- |
| Ravi, the bulk downloader (primary) | 24, engineering student / IT pro, inconsistent broadband | max speed via segmentation; pause/resume across power cuts; overnight scheduling with auto-shutdown; speed limiter |
| Meera, the content saver (primary) | 31, educator/creator | one-click video capture with quality choice; automatic audio+video merge; subtitle download; auto-categorization |
| Arjun, the power user / IT admin (secondary) | 38, developer/sysadmin | PAC-aware proxies with NTLM/Kerberos; CLI; mirror URLs; checksums; remote queue management |

**Non-persona (out of scope):** users seeking DRM circumvention or piracy
tools. No DRM-circumvention features will ever be built.

## 3. Phased scope

### MVP v0.1 — "Engine + Capture" (M0–M2, Must)

Segmented engine (dynamic re-segmentation, default 8 conns, 1–32, HTTP/HTTPS/
FTP) · pause/resume with Yes/No/Unknown resume-capability detection · named
queues + scheduler (start/stop times, on-completion shutdown/hibernate/
disconnect/launch) · auto-categorization with per-category folders · browser
extensions (Chrome/Edge/Firefox) + native host passing URL+cookies+referer+
UA · global + per-download speed limiter · clipboard URL monitoring · tray
app · main window + add-URL + progress dialogs · auto-update check.

**MVP exit:** ≥ 2.5× median speedup; ≥ 98% resume success in chaos tests;
≥ 90% capture reliability; installer ≤ 15 MB.

### v1.0 — "IDM parity" (M3–M5, Should)

Floating video grabber (HLS `.m3u8` + DASH `.mpd`, audio+video merge,
subtitle SRT/TTML, quality selection) · site grabber/spider with filters +
scheduling · Download-All-links · expired-URL auto-refresh · mirror URLs ·
full proxy support (HTTP/FTP/SOCKS, PAC, Basic/Digest/NTLM/Negotiate/
Kerberos) + site login manager · drag-and-drop basket · AV auto-scan hook ·
hourly quotas · MMS (best-effort) · CLI · customizable toolbar/skins/dark
theme · 12+ languages · weekly Quick Update · 30-day Pro trial + freemium
paywall.

**v1.0 exit:** all metrics below at target; benchmark parity vs IDM (±10%);
zero critical data-loss bugs; privacy policy published.

### v2.0 — "Beyond IDM" (M6+, gap exploiters)

BitTorrent (magnet + .torrent, per-torrent limits) · checksum verification
UI (MD5/SHA-1/SHA-256 auto-verify, mismatch alert) · remote management (web
UI / mobile) · macOS + Linux ports at parity · plugin/extension API · 30+
languages.

### Future (post-v2.0)

Cloud sync of queues/history · team/shared queues · smart bandwidth
scheduler · 50+ languages · enterprise policy (GPO/MDM) · optional
cloud-accelerated sources (with explicit consent + legal review).

## 4. Success metrics

| # | Metric | Target |
| --- | --- | --- |
| M-01 | Multi-connection throughput vs single-connection baseline | ≥ 3× median speedup on 1 GB / ≥50 Mbps (claim only what we measure) |
| M-02 | Installer size (Windows) | ≤ 15 MB |
| M-03 | Idle memory footprint | < 80 MB RAM (tray + extensions connected) |
| M-04 | Active download memory overhead | < 150 MB with 8 segments on a 4 GB file |
| M-05 | Crash-free sessions | ≥ 99.5% |
| M-06 | Resume success rate | ≥ 99% without restart-from-zero |
| M-07 | Browser capture reliability | ≥ 95% (URL + cookies + referer + UA intact) |
| M-08 | Video grabber detection rate | ≥ 90% on HTML5/HLS/DASH benchmark set (excl. DRM) |
| M-09 | Cold start time | < 2 s to usable main window |
| M-10 | Localization coverage | 100% strings externalized by v1.0; ≥ 12 languages |
| M-11 | Trial→paid conversion (Pro) | ≥ 4% of 30-day trial users |
| M-12 | NPS / user rating | ≥ 4.5/5 on distribution channels |

## 5. Non-functional requirements (summary)

- **Performance:** ≥ 3× median speedup; engine overhead ≤ 5% on
  single-segment throughput; ≤ 15% of one core per segmented download at
  100 Mbps; progress refresh ≤ 2 Hz per row; table updates never block the
  UI thread.
- **Reliability:** write-ahead per-segment journal; byte-exact final
  assembly (failed assembly re-downloads affected segments, never silent
  corruption); exponential backoff with jitter; per-segment retry budget
  (default 10); failed auto-updates roll back.
- **Compatibility:** Windows 10 21H2+/11 x64 at MVP (ARM64 best-effort
  v1.0); TLS 1.2+; no-Range servers degrade gracefully to single connection.
- **Security:** credentials only in the OS credential store; native host
  validates extension origins; signed updates; telemetry opt-in and
  anonymized; code-signed binaries.
- **Usability:** first segmented download within 60 s of install, unaided;
  destructive actions confirmed or undoable; WCAG AA contrast including dark
  mode; keyboard-navigable.
- **Maintainability:** engine decoupled from UI (stable internal API);
  unit coverage ≥ 70% on engine (build prompt requires ≥ 80% on sf-core
  crates); E2E matrix across protocols and resume cases.

## 6. Monetization

Freemium: **free core forever** (segmented engine to 8 connections,
pause/resume, queues + basic scheduler, categories, extensions, clipboard
monitor, tray, speed limiter). **Pro one-time ≈ €14.95** (regional pricing,
e.g. India ≈ ₹999; undercuts IDM's reference pricing): 16–32 connections,
video grabber, site grabber, expired-URL refresh, mirror multi-source,
hourly quotas, PAC + enterprise auth, CLI, remote management, checksum UI.
30-day full Pro trial, no credit card; graceful downgrade, no data loss.

## 7. Legal guardrails (hard rules)

1. **Clean-room branding** — original name, logo, icons, copy; no IDM
   assets; speed claims capped at measured results.
2. **Behavior ≠ expression** — reimplementing behaviors is fine; copying
   code, binaries, artwork, help text, or translations is prohibited.
3. **No DRM circumvention, ever** — Widevine/PlayReady/FairPlay off-limits;
   grabber detects DRM and refuses; CI banned-API check.
4. **No piracy facilitation** — no infringing-source indexes or bundled
   trackers.
5. **Third-party IP** — LGPL ffmpeg dynamically linked with attribution;
   credits screen; all licenses honored.
6. **Privacy** — cookies/referer stay on-device; telemetry opt-in only;
   plain-language privacy policy before v1.0.
7. **Politeness** — robots.txt respected by default; polite connection
   defaults (8/download, ≤ 1 req/s spider); honor Retry-After.

## 8. Key risks (top 5)

| Risk | Mitigation |
| --- | --- |
| Segmentation underperforms on throttled/per-IP-limited servers | benchmark corpus vs IDM early (M1); adaptive connection scaling; honest marketing |
| Browser vendors restrict interception APIs | ship all three browsers; fallbacks (manual add-URL, clipboard monitor); watch deprecation channels |
| Video sites change players/formats | modular site handlers + quick updates; DRM explicitly unsupported (smaller breakage surface) |
| AV/EDR false positives on our binaries | EV code signing; reproducible builds; SmartScreen reputation; boring installer |
| Single-founder bandwidth | MVP ruthlessly scoped; milestone-by-milestone build with hard acceptance gates |

## 9. Admin panel & services (web dashboard)

Per Sachin's 16-module admin checklist (2026-10-05), a web admin dashboard
and its backing services are a committed part of the product — the control
surface for version control/force-update, feature flags, licensing/device
management, payments/coupons, and opt-in analytics. Full mapping and the
track-A milestone plan live in [admin-panel.md](admin-panel.md). Summary of
verdicts: **build** modules 1, 3, 4, 5, 6, 9, 10, 11, 12, 15, 16 (16 =
highest priority, required before the first public release); **thin slice**
module 2; **defer** module 8; **not applicable** modules 7, 13, 14.

## 10. Open product questions

1. Final product name (trademark/domain check before Phase 2 marketing).
2. ~~Tech stack~~ — resolved: Rust + Tauri v2 (React 18 UI). Native C/C++ and
   Electron were rejected per the System Design.
3. FFmpeg strategy: bundle vs download-on-first-use (LGPL + installer-size
   trade-off) — decide by M4.
4. Pro feature line: is the video grabber Pro-only or free? (Pricing
   experiment — decide by M4.)
5. Regional pricing tiers beyond India — post-M5.
6. Telemetry vendor: minimal in-house vs off-the-shelf — post-M5.
7. Support model, macOS/Linux sequencing, enterprise licensing — post-v1.0.
