//! `SwiftFetch` media engine: HLS (`.m3u8`) and DASH (`.mpd`) playlist capture,
//! ffmpeg sidecar invocation (argv arrays only — never a shell), audio+video
//! merge and subtitle extraction.
//!
//! Implementation lands in Milestone 4 (Build Prompt §12.3; design contract
//! in docs/system-design.md §4.4). DRM-protected streams (Widevine,
//! `PlayReady`, `FairPlay`) are explicitly out of scope and must abort with a
//! clear "protected content — not supported" message.
