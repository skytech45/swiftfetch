# SwiftFetch Threat Model

Reviewed in **Milestone 5** (Build Prompt §13.6). Scope: the shipped
desktop app, browser bridge, site spider, AV hook, CLI and update channel
as built through M5.

## 1. Assets & trust boundaries

| Asset | Stored | Boundary |
| --- | --- | --- |
| Proxy / site-login secrets | OS keychain only; DB holds `keyring_ref` | Never logged, never in telemetry, TLS-only to the target host |
| Browser cookies / UA / referer | Memory + job row while downloading; cleared on job delete | Forwarded only to the download origin, never logged with secrets |
| Download journal + history | SQLite WAL (`swiftfetch.db`) | Single-writer discipline; CLI writes under `BEGIN IMMEDIATE` |
| Update artifacts | Tauri updater feed (M5: configured at release signing) | Ed25519-signed, public key pinned in the binary |
| Extension messages | Native host stdin (length-prefixed JSON) | Origin allow-list; unknown IDs rejected (`E_ORIGIN_DENIED`) |

## 2. Findings & mitigations (verified in M5)

- **No secret in logs/DB.** Grep gate: `password|secret|cookie|Authorization`
  at INFO+ appears only in redacted form. `site_logins` stores the keychain
  reference, never the password. Telemetry (opt-in, M-A4) carries counts
  only — no URLs, filenames or IPs.
- **IPC requires proof of origin.** The native host validates
  `allowed_origins` against the installed extension IDs before forwarding;
  the localhost app endpoint only accepts payloads launched through the
  registered manifest path (or the `swiftfetch://add` deep link). Fuzzed in
  M4 protocol tests (wrong-origin rejection).
- **AV hook cannot inject commands.** The scanner runs via argv arrays only
  (no shell, single `{file}` substitution token); paths are validated and
  the scan has a timeout. A scanner that cannot run reports `Skipped` and
  never blocks downloads.
- **Spider defaults are polite.** Grabber ships with `respect_robots = true`,
  `stay_on_domain = true`, 1 s per-host politeness and page/file caps; the
  UI warns when robots compliance is disabled. `Retry-After` / 429 backs
  off exponentially; defaults can never look like a DDoS tool.
- **Subprocess hygiene.** ffmpeg, AV scanner, shutdown/sleep helpers and
  the updater all use argv arrays — no interpolated shell strings anywhere
  (`grep -rn "sh -c\|cmd /C\|shell(" crates apps --include=*.rs` is empty).
- **TLS is never weakened.** No `danger_accept_invalid_certs`; per-site
  exceptions require an explicit scary-warning opt-in (not shipped in M5).
- **Supply chain.** Update feed artifacts are Ed25519-signed; the public key
  is pinned in the binary at release time (M5 wires the channel setting +
  feed config; `configured: false` until signing keys are provisioned).
  `cargo deny` / `cargo audit` gate the dependency tree (M6 hardening
  makes it a nightly job).

## 3. Residual risks

- Native-host manifest registration is per-user and relies on the browser's
  profile paths — a malicious local profile could register a rival ID; the
  app still shows every bridged URL before downloading.
- The grabber downloads what the user points it at; the first-run notice
  states the tool is for content the user has the right to download.
- Offline license activation (M-A2) and payment webhooks (M-A3) are
  reviewed with their own track-A milestones, not here.
