//! `SwiftFetch` `YouTube` one-click site module (non-DRM only).
//!
//! Build Prompt §12.4: floating button → quality list → one click → merged
//! MP4. Pipeline: player data (watch-page `player_response` JSON or Innertube
//! `/player` with browser cookies/UA via the native host) → itag table
//! (data, not logic) → cipher solver → quality pairing + ffmpeg merge →
//! stream-URL expiry re-resolution.
//!
//! Hard boundaries: DRM/Widevine-signaled content aborts with the
//! "protected content not supported" message; livestreams and
//! rental/Premium-only content are "not supported"; license servers are
//! never contacted; stream URLs are never written to disk or logs with
//! secrets attached.
//!
//! Implementation lands in Milestone 4 (cipher solver ships as a
//! hot-updatable module behind the `CipherSolver` trait — never a vendored
//! hardcoded cipher; on solve failure exactly one attempt, then the clean
//! error "`YouTube` player changed — extractor update required"). This crate
//! is a compiling scaffold until then.
