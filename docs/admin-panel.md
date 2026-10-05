# SwiftFetch — Admin Panel & Services (Web Dashboard)

**Status:** roadmap (track "A"). Sachin supplied a founder's 16-module admin
panel checklist on 2026-10-05 with the instruction: *build a web dashboard to
control the application — feature updates, version control, and the other
capabilities in the checklist.* This document maps every module to what
SwiftFetch actually needs, defines the architecture, and slots the work into
a milestone track parallel to the desktop milestones (M1–M6, see
[system-design.md §10](system-design.md)).

> Principle carried over from the desktop contract: **no fake UI.** Admin
> modules ship only when the data behind them exists (license server,
> telemetry ingest, payment provider). The dashboard is how the business runs
> — it is not an afterthought bolted on after launch, and not a demo.

## 1. What the desktop app needs from the admin side

The freemium model (PRD §6) and the release plan (M5 packaging + Tauri
updater) imply a small set of backend services. The admin dashboard is the
control surface for them:

| Service | Desktop touchpoint | Admin module(s) |
| --- | --- | --- |
| Update feed + version policy | Tauri updater (M5), "check on startup" | 16 Force-Update & Version Control |
| Remote config / feature flags | settings fetch at startup, clipboard prompt defaults, category presets | 11 Feature Flags & App Config |
| License activation + device binding | Pro trial/activation, offline activation, 3-PC limit | 1 Users, 15 Sessions & Devices |
| Payments | Pro purchase, upgrades, regional pricing | 3 Payments, 12 Coupons |
| Telemetry ingest (opt-in) | anonymized counts only (PRD NFR-S05) | 9 Analytics, 5 Reports |
| Announcements | in-app "what's new" / offer banner | 4 Notifications, 2 Content (thin) |

## 2. The 16 checklist modules → SwiftFetch disposition

| # | Module | Verdict | What it becomes for SwiftFetch |
| --- | --- | --- | --- |
| 1 | User Management | **Build** | License-holder/customer registry: search, view profile (license tier, devices, activation history), suspend a license for fraud/abuse. Not "app users" — the free tier is anonymous by design (PRD FR-065). |
| 2 | Content Management | **Thin slice** | Release notes per update-feed entry, in-app announcement copy, "what's new" text. A full CMS is unnecessary — no banners/listings in a download manager. |
| 3 | Payments & Transactions | **Build** | Transaction dashboard for Pro sales (provider-backed: Stripe/Paddle-style with merchant-of-record for regional pricing), refunds, chargeback status, revenue breakdown by region. |
| 4 | Notifications | **Build** | Compose + schedule in-app announcements (all users / by app version / by locale). Privacy rule: delivery is pull-based at app startup; no per-user delivery tracking without opt-in. |
| 5 | Reports & Exports | **Build** | Pre-built exportable reports: revenue, trial→paid conversion (M-11), version adoption, opt-in telemetry summaries, crash-free rate (M-05). CSV/JSON export. |
| 6 | Roles & Permissions | **Build (foundation)** | RBAC for admin team: Owner / Admin / Finance / Support / Viewer. Ships with M-A0 — every module sits behind it. |
| 7 | Order / Booking Management | **N/A** | No orders or bookings exist in a download manager. Skipped. |
| 8 | Support & Ticket Management | **Defer** | Post-v1.0: start with email + community forum linked from the app ("Send diagnostics" prefills a ticket). Build an in-panel ticket view only if volume demands it. |
| 9 | Live Analytics Dashboard | **Build** | At-a-glance: daily/monthly active installs (opt-in), downloads completed, bytes moved, error-code histogram, version adoption, revenue today. Real-time = ingest freshness, no per-second surveillance. |
| 10 | Audit Logs & Activity History | **Build (foundation)** | Every admin mutation (license revoke, refund, flag change, release publish) recorded with actor, timestamp, before/after. Immutable. Ships with M-A0. |
| 11 | Feature Flags & App Configuration | **Build** | Remote config keys the app reads at startup: default max connections, clipboard-prompt default, announcement banner, feed channel (stable/beta), min-required-version. Typed schema, staged rollout by percentage. |
| 12 | Coupons & Discount Management | **Build** | Create/schedule/retire Pro discount codes with usage tracking and regional pricing overrides (launch discount, ₹999 India anchor). |
| 13 | Vendor / Partner Management | **N/A** | No vendors or sellers. Skipped. |
| 14 | Content Moderation Tools | **N/A** | No user-generated content. Skipped. |
| 15 | Session & Device Management | **Build** | Per-license device list (PRD anti-abuse: key bound to ≤ 3 PCs), revoke device, force-deactivate; supports offline activation with signed keys. |
| 16 | Force-Update & App Version Control | **Build (highest priority)** | The update feed doubles as version policy: per-channel latest version, min-required version, prompt vs force semantics (force = app blocks until updated; prompt = banner). Critical security fixes reach every user. |

## 3. Architecture

```
┌─────────────────────────────┐        ┌──────────────────────────────┐
│ apps/admin (React 18+Vite)  │ HTTPS  │ services/api (Rust, axum)    │
│ admin dashboard SPA         │───────▶│ admin auth (RBAC), audit log │
└─────────────────────────────┘        │ license/device/payment APIs  │
                                       │ telemetry ingest (opt-in)    │
┌─────────────────────────────┐        │ update feed + feature flags  │
│ swiftfetch desktop app      │ HTTPS──▶ (public, unauthenticated,   │
│ (Tauri updater, config,     │        │  anonymous — no PII)          │
│  activation)                │        └───────────┬──────────────────┘
└─────────────────────────────┘                    │
                                          PostgreSQL (business data)
```

- **Same stack ethos as the desktop:** Rust (axum) API + TypeScript React
  dashboard; `crates/common` types shared where useful. Server persistence is
  PostgreSQL — the desktop's SQLite single-writer discipline does not apply
  to a multi-user cloud service.
- **Privacy invariants (PRD §7/NFR-S05):** telemetry is opt-in and anonymized;
  no download URLs, filenames, or IPs stored; the public update/config
  endpoints learn nothing about users (no accounts for the free tier).
- **Security:** admin auth = email + password (argon2) + TOTP 2FA; sessions
  short-lived; audit log immutable; payment provider webhooks signature-
  verified; update feed artifacts Ed25519-signed (pubkey pinned in the
  desktop binary — unchanged from M5 design).
- **Hosting:** single deployable (API + static dashboard), Docker image,
  staging + production; secrets in CI secret store (same discipline as
  desktop signing).

## 4. Milestone track A (parallel to desktop M1–M6)

| Milestone | Scope | Exit criteria |
| --- | --- | --- |
| **M-A0 — Foundations** | repo layout (`apps/admin`, `services/api`), deploy pipeline, admin auth + RBAC (module 6), audit log core (module 10), dashboard shell | admin login with roles works; every mutation writes an audit row; staging deploy green |
| **M-A1 — Distribution control** (must be live **before v1.0 public release**) | update feed API + version policy (module 16): channels, min-required version, force/prompt; release management UI (publish signed artifacts + notes — module 2 thin slice); feature flags/remote config (module 11) | staging desktop app updates via the feed; force-update blocks below min version; flag rollout visible in app |
| **M-A2 — Licensing** | license keys, device binding ≤ 3 PCs, activation + offline activation APIs, device revoke (modules 1, 15), trial provisioning | trial→activation round-trip from a real desktop build; device revoke enforced at next launch |
| **M-A3 — Commerce** | payment provider integration, transactions dashboard, refunds, coupons + regional pricing (modules 3, 12) | buy Pro end-to-end on staging; refund reflects in license state |
| **M-A4 — Insights** | opt-in telemetry ingest, live analytics dashboard, reports & exports, announcements composer (modules 9, 5, 4, plus 1 user views) | funnel M-11, crash-free M-05, version adoption visible; CSV export matches dashboard |

**Sequencing rule:** M-A0 may start any time (it has no desktop dependency);
M-A1 must land before the first public signed release (desktop M5); M-A2
before the M5 paywall wiring; M-A3 before launch; M-A4 after launch with
opt-in telemetry flowing. Track A never blocks a desktop milestone except at
those two gates.

## 5. Deferred / not applicable (from the checklist)

- **Support & Ticket Management** — deferred post-v1.0 (module 8 verdict above).
- **Order/Booking (7), Vendor/Partner (13), Content Moderation (14)** — no
  corresponding SwiftFetch concept; revisit only if the product adds those
  surfaces.
