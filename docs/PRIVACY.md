# SwiftFetch Privacy Policy

**Effective:** October 2026 (v1.0). Plain language, as required by PRD §7.

## What SwiftFetch stores on your PC

- Your download list, queues, categories, settings and the crash-safe
  segment journal — in a local SQLite database. Nothing leaves the device.
- Login session tokens — in the OS keychain/credential store. Proxy and
  site passwords likewise live only in the keychain, referenced by handle.
- Downloaded files, obviously. Browser cookies/UA/referer forwarded for a
  capture go only to the download origin and are cleared when you delete
  the job.

## What SwiftFetch sends over the network

- The files you ask it to download, from the servers you point it at.
- Update checks (version number only) and remote-config fetches, when
  enabled — no URLs, filenames, or personal data.
- BitTorrent traffic you start (inherent to the protocol: your IP is
  visible to swarm peers, as with every torrent client).

## What SwiftFetch never does

- No telemetry without your explicit opt-in (Settings; default off).
- No accounts required for the free tier beyond an email login; guest mode
  needs nothing at all.
- No ads, no bundled offers, no credential exfiltration.

## Account deletion

Delete the app and its data dir (`%APPDATA%\SwiftFetch` on Windows), then
ask support to erase your account row. Backups expire within 30 days.

## Contact

Support details ship with the release. This document also lives at
`/privacy` on the admin dashboard.
