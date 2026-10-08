//! `SwiftFetch` `BitTorrent` support via the `librqbit` crate: `.torrent` files
//! and magnet links as queue items, per-torrent speed limits, DHT, and a
//! seeding-ratio policy (default: seed to 1.0 then stop).
//!
//! Module map: [`bencode`] (total parser, also a fuzz target) →
//! [`metainfo`] (`.torrent` validation) + [`magnet`] (link parsing) →
//! [`session`] ([`TorrentEngine`](session::TorrentEngine), the managed
//! librqbit session the app and CLI drive).
//!
//! No bundled trackers and no piracy facilitation — see the PRD legal
//! guardrails.
#![forbid(unsafe_code)]

pub mod bencode;
pub mod magnet;
pub mod metainfo;
pub mod session;

pub use magnet::{Magnet, MagnetError, parse_magnet};
pub use metainfo::{MetaError, TorrentFile, TorrentMeta, parse_torrent};
pub use session::{TorrentEngine, TorrentError, TorrentId, TorrentStatus, seeding_complete};
