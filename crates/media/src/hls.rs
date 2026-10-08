//! HLS (`EXT-X-*`) playlist parsing and capture (system-design §4.4).
//!
//! Scope: VOD master + media playlists, AES-128 keys the server serves
//! directly. `SAMPLE-AES`, `EXT-X-SESSION-KEY` with a keyserver, or any
//! DRM signal aborts with [`MediaError::Drm`] — no CDM, no license calls.

use std::collections::HashMap;

use crate::MediaError;

/// One variant entry of a master playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterVariant {
    /// Variant playlist URI (absolute).
    pub uri: String,
    /// `BANDWIDTH` (bps).
    pub bandwidth: u64,
    /// `RESOLUTION` width, when advertised.
    pub width: Option<u32>,
    /// `RESOLUTION` height, when advertised.
    pub height: Option<u32>,
    /// `CODECS`, when advertised.
    pub codecs: Option<String>,
}

/// An alternate rendition declared with `EXT-X-MEDIA` in a master playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    /// TYPE (AUDIO, SUBTITLES, CLOSED-CAPTIONS).
    pub kind: String,
    /// NAME attribute.
    pub name: String,
    /// LANGUAGE attribute, when advertised.
    pub language: Option<String>,
    /// URI, when the rendition is external (not muxed in).
    pub uri: Option<String>,
}

/// Key state of a media playlist.
#[derive(Debug, Clone, PartialEq)]
pub enum Encryption {
    /// No `EXT-X-KEY` or `METHOD=NONE`.
    None,
    /// `METHOD=AES-128` with a directly served key (16 bytes) — the only
    /// encryption we decrypt; the key URI is fetched at download time.
    Aes128 {
        /// Key URI (absolute).
        key_uri: String,
        /// Explicit IV from the playlist, when present.
        iv: Option<[u8; 16]>,
    },
}

/// One `EXTINF` entry of a media playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentSpec {
    /// Segment URI (absolute).
    pub uri: String,
    /// `EXTINF` duration in seconds.
    pub duration_secs: f64,
}

/// A parsed media (variant) playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaylist {
    /// Segments in playback order.
    pub segments: Vec<SegmentSpec>,
    /// Key state (single key per playlist — enough for VOD captures).
    pub encryption: Encryption,
    /// `MEDIA-SEQUENCE`, used as the implicit IV when no explicit IV.
    pub media_sequence: u64,
}

fn absolute(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_owned();
    }
    match reqwest::Url::parse(base)
        .ok()
        .and_then(|base| base.join(uri).ok())
    {
        Some(resolved) => resolved.to_string(),
        None => uri.to_owned(),
    }
}

/// Parses an `#EXT-X-KEY`/`#EXT-X-MEDIA`-style attribute list into a map.
pub(crate) fn parse_attr_list(value: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in value.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                attrs.insert_attr(&current);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    attrs.insert_attr(&current);
    attrs
}

trait InsertAttr {
    fn insert_attr(&mut self, pair: &str);
}

impl InsertAttr for HashMap<String, String> {
    fn insert_attr(&mut self, pair: &str) {
        if let Some((key, value)) = pair.split_once('=') {
            self.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
}

fn parse_hex16(value: &str) -> Option<[u8; 16]> {
    let hex = value.trim_start_matches("0x").trim_start_matches("0X");
    if hex.len() != 32 {
        return None;
    }
    let mut iv = [0u8; 16];
    for (i, byte) in hex.as_bytes().chunks(2).enumerate() {
        iv[i] = u8::from_str_radix(std::str::from_utf8(byte).ok()?, 16).ok()?;
    }
    Some(iv)
}

/// Parses a master playlist into its variants and alternate renditions.
///
/// # Errors
///
/// Returns [`MediaError::Drm`] when a session key requires DRM, and
/// [`MediaError::Parse`] on malformed playlists.
pub fn parse_master(
    text: &str,
    base_url: &str,
) -> Result<(Vec<MasterVariant>, Vec<Rendition>), MediaError> {
    let mut variants = Vec::new();
    let mut renditions = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_attr_list(rest);
            let bandwidth = attrs
                .get("BANDWIDTH")
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| MediaError::Parse("EXT-X-STREAM-INF without BANDWIDTH".into()))?;
            let (width, height) =
                attrs
                    .get("RESOLUTION")
                    .map_or((None, None), |res| match res.split_once('x') {
                        Some((w, h)) => (w.parse().ok(), h.parse().ok()),
                        None => (None, None),
                    });
            // The URI is the next non-comment line; stash a marker.
            variants.push(MasterVariant {
                uri: String::new(),
                bandwidth,
                width,
                height,
                codecs: attrs.get("CODECS").cloned(),
            });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = parse_attr_list(rest);
            renditions.push(Rendition {
                kind: attrs.get("TYPE").cloned().unwrap_or_default(),
                name: attrs.get("NAME").cloned().unwrap_or_default(),
                language: attrs.get("LANGUAGE").cloned(),
                uri: attrs.get("URI").map(|u| absolute(base_url, u)),
            });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            let attrs = parse_attr_list(rest);
            let method = attrs.get("METHOD").map_or("", String::as_str);
            if method != "NONE" {
                // Session keys cover the whole presentation; anything but a
                // directly served AES-128 key is treated as protected.
                if method != "AES-128" {
                    return Err(MediaError::Drm);
                }
            }
        } else if !line.starts_with('#')
            && !line.is_empty()
            && let Some(last) = variants.last_mut()
            && last.uri.is_empty()
        {
            last.uri = absolute(base_url, line);
        }
    }
    variants.retain(|v| !v.uri.is_empty());
    if variants.is_empty() {
        return Err(MediaError::Parse("master playlist has no variants".into()));
    }
    Ok((variants, renditions))
}

/// Parses a media (variant) playlist into ordered segments + key state.
///
/// # Errors
///
/// Returns [`MediaError::Drm`] for `SAMPLE-AES` (protected content) and
/// [`MediaError::Parse`] on malformed playlists.
pub fn parse_media(text: &str, base_url: &str) -> Result<MediaPlaylist, MediaError> {
    let mut segments = Vec::new();
    let mut encryption = Encryption::None;
    let mut media_sequence: u64 = 0;
    let mut pending_duration: Option<f64> = None;
    let mut saw_segment = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let duration = rest
                .split(',')
                .next()
                .and_then(|d| d.trim().parse().ok())
                .ok_or_else(|| MediaError::Parse("EXTINF without duration".into()))?;
            pending_duration = Some(duration);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attr_list(rest);
            let method = attrs.get("METHOD").map_or("NONE", String::as_str);
            match method {
                "NONE" => encryption = Encryption::None,
                "AES-128" => {
                    let key_uri = attrs
                        .get("URI")
                        .map(|u| absolute(base_url, u))
                        .ok_or_else(|| MediaError::Parse("AES-128 key without URI".into()))?;
                    let iv = attrs.get("IV").and_then(|v| parse_hex16(v));
                    encryption = Encryption::Aes128 { key_uri, iv };
                }
                // SAMPLE-AES and friends are protected content.
                _ => return Err(MediaError::Drm),
            }
        } else if !line.starts_with('#') && !line.is_empty() {
            let duration = pending_duration.take().unwrap_or(0.0);
            segments.push(SegmentSpec {
                uri: absolute(base_url, line),
                duration_secs: duration,
            });
            saw_segment = true;
        }
    }
    if !saw_segment {
        return Err(MediaError::Parse("media playlist has no segments".into()));
    }
    Ok(MediaPlaylist {
        segments,
        encryption,
        media_sequence,
    })
}

/// Extracts the subtitle renditions from a master playlist result.
#[must_use]
pub fn subtitle_renditions(renditions: &[Rendition]) -> Vec<(String, String)> {
    renditions
        .iter()
        .filter(|r| r.kind.eq_ignore_ascii_case("SUBTITLES"))
        .filter_map(|r| r.uri.clone().map(|uri| (r.name.clone(), uri)))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    #[test]
    fn parses_master_with_variants_and_renditions() {
        let base = "https://example.com/vod/master.m3u8";
        let text = "#EXTM3U\n\
                    #EXT-X-MEDIA:TYPE=SUBTITLES,NAME=\"English\",LANGUAGE=\"en\",URI=\"subs_en.m3u8\"\n\
                    #EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=640x360,CODECS=\"avc1.42001e\"\n\
                    v360.m3u8\n\
                    #EXT-X-STREAM-INF:BANDWIDTH=4128000,RESOLUTION=1920x1080,CODECS=\"avc1.640028\"\n\
                    v1080.m3u8\n";
        let (variants, renditions) = parse_master(text, base).unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1].uri, "https://example.com/vod/v1080.m3u8");
        assert_eq!(variants[1].height, Some(1080));
        assert_eq!(subtitle_renditions(&renditions).len(), 1);
        assert_eq!(
            subtitle_renditions(&renditions)[0].1,
            "https://example.com/vod/subs_en.m3u8"
        );
    }

    #[test]
    fn parses_media_playlist_with_aes128() {
        let text = "#EXTM3U\n\
                    #EXT-X-VERSION:3\n\
                    #EXT-X-MEDIA-SEQUENCE:7\n\
                    #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090a0b0c0d0e0f\n\
                    #EXTINF:4.0,\nseg0.ts\n#EXTINF:4.0,\nseg1.ts\n";
        let playlist = parse_media(text, "https://example.com/v/v.m3u8").unwrap();
        assert_eq!(playlist.segments.len(), 2);
        assert_eq!(playlist.media_sequence, 7);
        match playlist.encryption {
            Encryption::Aes128 { key_uri, iv } => {
                assert_eq!(key_uri, "https://example.com/v/key.bin");
                assert_eq!(iv.unwrap()[0], 0x00);
                assert_eq!(iv.unwrap()[15], 0x0f);
            }
            Encryption::None => panic!("expected AES-128, got None"),
        }
    }

    #[test]
    fn sample_aes_aborts_as_drm() {
        let text =
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\"\n#EXTINF:4.0,\nseg0.ts\n";
        assert!(matches!(
            parse_media(text, "https://example.com/v.m3u8"),
            Err(MediaError::Drm)
        ));
    }
}
