//! `SwiftFetch` `BitTorrent` support via the `librqbit` crate: `.torrent` files
//! and magnet links as queue items, per-torrent speed limits, DHT, and a
//! seeding-ratio policy (default: seed to 1.0 then stop).
//!
//! Implementation lands in Milestone 6 (Build Prompt §14.1). No bundled
//! trackers and no piracy facilitation — see the PRD legal guardrails.
