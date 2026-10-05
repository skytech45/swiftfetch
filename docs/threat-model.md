# SwiftFetch Threat Model

This document is filled in during **Milestone 5** (Build Prompt §13.6)
alongside the hardening pass. Scope it will cover:

- Proxy and site-login credential handling; OS keychain usage (secrets are
  never stored in the database, logs, or telemetry).
- Localhost IPC authentication (bearer token, localhost-only binding).
- Browser extension origin validation (`E_ORIGIN_DENIED` for unknown IDs).
- Site-spider abuse surface (robots.txt compliance, politeness delays,
  page/depth caps as safe defaults).
- AV-hook command injection prevention (argv arrays only, no shell; single
  `{file}` substitution token).
- Cookie lifecycle (memory + job row only, never logged, never leaves the
  device, cleared on job delete).
- Update-channel supply chain (Ed25519-signed feed, pinned public key).

Placeholder created in Milestone 0 as part of the docs skeleton.
