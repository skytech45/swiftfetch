//! The itag reference table (Build Prompt §12.4): data, not logic — a
//! plain mapping from itag to a human label. Streaming URLs are never
//! derived from this table; it only labels the quality picker.

/// Video-only H.264/MP4 itags.
/// 160 — 144p.
pub const ITAG_160: u32 = 160; // 144p
/// 133 — 240p.
pub const ITAG_133: u32 = 133; // 240p
/// 134 — 360p.
pub const ITAG_134: u32 = 134; // 360p
/// 135 — 480p.
pub const ITAG_135: u32 = 135; // 480p
/// 136 — 720p.
pub const ITAG_136: u32 = 136; // 720p
/// 137 — 1080p.
pub const ITAG_137: u32 = 137; // 1080p

/// Video-only VP9/WebM itags.
/// 242 — 240p.
pub const ITAG_242: u32 = 242; // 240p
/// 243 — 360p.
pub const ITAG_243: u32 = 243; // 360p
/// 244 — 480p.
pub const ITAG_244: u32 = 244; // 480p
/// 247 — 720p.
pub const ITAG_247: u32 = 247; // 720p
/// 248 — 1080p.
pub const ITAG_248: u32 = 248; // 1080p

/// Video-only AV1 itags.
/// 394 — 144p.
pub const ITAG_394: u32 = 394; // 144p
/// 395 — 240p.
pub const ITAG_395: u32 = 395; // 240p
/// 396 — 360p.
pub const ITAG_396: u32 = 396; // 360p
/// 397 — 480p.
pub const ITAG_397: u32 = 397; // 480p
/// 398 — 720p.
pub const ITAG_398: u32 = 398; // 720p
/// 399 — 1080p.
pub const ITAG_399: u32 = 399; // 1080p

/// Audio AAC/MP4 itags.
/// 139 — 48k audio.
pub const ITAG_139: u32 = 139; // 48k audio
/// 140 — 128k audio.
pub const ITAG_140: u32 = 140; // 128k audio

/// Audio Opus/WebM itags.
/// 249 — 50k audio.
pub const ITAG_249: u32 = 249;
/// 250 — 70k audio.
pub const ITAG_250: u32 = 250;
/// 251 — 160k audio.
pub const ITAG_251: u32 = 251;

/// Legacy progressive (audio+video) itags.
/// 18 — 360p progressive.
/// 18 — 360p progressive.
pub const ITAG_18: u32 = 18; // 360p progressive
/// 22 — 720p progressive.
/// 22 — 720p progressive.
pub const ITAG_22: u32 = 22; // 720p progressive

/// The reference table as `(itag, label)` pairs.
const TABLE: &[(u32, &str)] = &[
    (ITAG_160, "144p"),
    (ITAG_133, "240p"),
    (ITAG_134, "360p"),
    (ITAG_135, "480p"),
    (ITAG_136, "720p"),
    (ITAG_137, "1080p"),
    (ITAG_242, "240p"),
    (ITAG_243, "360p"),
    (ITAG_244, "480p"),
    (ITAG_247, "720p"),
    (ITAG_248, "1080p"),
    (ITAG_394, "144p"),
    (ITAG_395, "240p"),
    (ITAG_396, "360p"),
    (ITAG_397, "480p"),
    (ITAG_398, "720p"),
    (ITAG_399, "1080p"),
    (ITAG_139, "48k audio"),
    (ITAG_140, "128k audio"),
    (ITAG_249, "50k audio"),
    (ITAG_250, "70k audio"),
    (ITAG_251, "160k audio"),
    (ITAG_18, "360p progressive"),
    (ITAG_22, "720p progressive"),
];

/// Labels an itag, if it is in the reference table.
#[must_use]
pub fn itag_label(itag: u32) -> Option<&'static str> {
    TABLE
        .iter()
        .find(|(known, _)| *known == itag)
        .map(|(_, label)| *label)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    #[test]
    fn covers_every_table_itag_at_least_once() {
        for (itag, label) in TABLE {
            assert!(!label.is_empty(), "itag {itag} has an empty label");
        }
    }

    #[test]
    fn unknown_itags_return_none() {
        assert_eq!(itag_label(9999), None);
    }
}
