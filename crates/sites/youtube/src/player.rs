//! Watch-page `ytInitialPlayerResponse` extraction and format parsing.

use serde_json::Value;

use crate::YoutubeError;

/// Extracts the `ytInitialPlayerResponse` JSON object from a watch page
/// (brace-balanced; survives nested objects and strings containing braces).
///
/// # Errors
///
/// Returns [`YoutubeError::Parse`] when the page has no player response.
pub fn extract_player_response(html: &str) -> Result<Value, YoutubeError> {
    const MARKERS: [&str; 2] = ["ytInitialPlayerResponse = ", "ytInitialPlayerResponse="];
    for marker in MARKERS {
        let Some(start) = html.find(marker) else {
            continue;
        };
        let json_start = start + marker.len();
        if !html[json_start..].starts_with('{') {
            continue;
        }
        let Some(end) = balanced_brace_end(html, json_start) else {
            continue;
        };
        let json_text = &html[json_start..=end];
        return serde_json::from_str(json_text)
            .map_err(|err| YoutubeError::Parse(format!("player response JSON: {err}")));
    }
    Err(YoutubeError::Parse(
        "watch page has no ytInitialPlayerResponse".into(),
    ))
}

/// Index of the `}` that closes the object opened at `open` (inclusive).
fn balanced_brace_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The player base.js URL (`ytcfg.jsUrl`), needed by the cipher solver.
///
/// # Errors
///
/// Returns [`YoutubeError::Parse`] when the page does not advertise one.
pub fn extract_base_js_url(html: &str) -> Result<String, YoutubeError> {
    const MARKERS: [&str; 2] = ["\"jsUrl\":\"", "\"PLAYER_JS_URL\":\""];
    for marker in MARKERS {
        if let Some(start) = html.find(marker) {
            let rest = &html[start + marker.len()..];
            if let Some(end) = rest.find('"') {
                return Ok(rest[..end].to_owned());
            }
        }
    }
    Err(YoutubeError::Parse("watch page has no jsUrl".into()))
}

/// One stream format from `streamingData`.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamFormat {
    /// `@itag`.
    pub itag: u32,
    /// `mimeType` prefix (`video/mp4`, `audio/mp4`, `video/webm`, …).
    pub mime: String,
    /// `bitrate` (bps), 0 when unadvertised.
    pub bitrate: u64,
    /// `height` (video formats).
    pub height: Option<u32>,
    /// `qualityLabel` (e.g. `1080p`), when advertised.
    pub quality_label: Option<String>,
    /// `contentLength` (bytes), when advertised.
    pub content_length: Option<u64>,
    /// Direct URL (unobfuscated formats).
    pub url: Option<String>,
    /// `signatureCipher` payload (obfuscated formats).
    pub signature_cipher: Option<String>,
    /// `true` when the stream also carries audio (progressive formats).
    pub has_audio: bool,
}

/// Pulls `streamingData.formats` + `adaptiveFormats` out of a parsed
/// player response.
///
/// # Errors
///
/// Returns [`YoutubeError::Parse`] when the structure is unusable.
pub fn extract_formats(player_response: &Value) -> Result<Vec<StreamFormat>, YoutubeError> {
    let Some(streaming) = player_response.get("streamingData") else {
        return Err(YoutubeError::Parse("no streamingData".into()));
    };
    let mut formats = Vec::new();
    for (list, progressive) in [("formats", true), ("adaptiveFormats", false)] {
        let Some(items) = streaming.get(list).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let itag = item
                .get("itag")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| YoutubeError::Parse("format without itag".into()))?;
            let mime = item
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split(';')
                .next()
                .unwrap_or_default()
                .to_owned();
            let height = item
                .get("height")
                .and_then(Value::as_u64)
                .and_then(|v| u32::try_from(v).ok());
            let has_audio = progressive || mime.starts_with("audio/");
            formats.push(StreamFormat {
                itag,
                mime,
                bitrate: item.get("bitrate").and_then(Value::as_u64).unwrap_or(0),
                height,
                quality_label: item
                    .get("qualityLabel")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                content_length: item.get("contentLength").and_then(Value::as_u64),
                url: item.get("url").and_then(Value::as_str).map(str::to_owned),
                signature_cipher: item
                    .get("signatureCipher")
                    .or_else(|| item.get("cipher"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                // Progressive `formats` entries carry audio; adaptive audio
                // entries are audio-only; adaptive video entries are silent.
                has_audio,
            });
        }
    }
    Ok(formats)
}

/// Playability gate: aborts cleanly for livestreams, rentals and other
/// protected/unplayable content.
///
/// # Errors
///
/// Returns [`YoutubeError::Live`] for livestreams and
/// [`YoutubeError::Unplayable`] for anything the server refuses to serve.
pub fn check_playability(player_response: &Value) -> Result<(), YoutubeError> {
    if player_response
        .pointer("/videoDetails/isLiveContent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(YoutubeError::Live);
    }
    let status = player_response.pointer("/playabilityStatus/status");
    match status.and_then(Value::as_str) {
        Some("OK") | None => Ok(()),
        Some(other) => {
            let reason = player_response
                .pointer("/playabilityStatus/reason")
                .and_then(Value::as_str)
                .unwrap_or("");
            let protected = other == "LOGIN_REQUIRED"
                || reason.to_ascii_lowercase().contains("rental")
                || reason.to_ascii_lowercase().contains("protected");
            Err(YoutubeError::Unplayable {
                protected_content: protected,
                reason: reason.to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_player_response_with_braces_in_strings() {
        let html = r#"<script>var ytInitialPlayerResponse = {"a":"}{","streamingData":{"x":1}};var x=1;</script>"#;
        let value = extract_player_response(html).expect("player response");
        assert_eq!(value.pointer("/streamingData/x"), Some(&json!(1)));
    }

    #[test]
    fn missing_player_response_is_parse_error() {
        assert!(extract_player_response("<html>nothing</html>").is_err());
    }

    #[test]
    fn livestreams_abort() {
        let response =
            json!({"videoDetails": {"isLiveContent": true}, "playabilityStatus": {"status": "OK"}});
        assert!(matches!(
            check_playability(&response),
            Err(YoutubeError::Live)
        ));
    }

    #[test]
    fn rentals_abort_as_protected() {
        let response = json!({"playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "This video requires payment"}});
        assert!(matches!(
            check_playability(&response),
            Err(YoutubeError::Unplayable {
                protected_content: true,
                ..
            })
        ));
    }

    #[test]
    fn formats_extracted_from_both_lists() {
        let response = json!({
            "streamingData": {
                "formats": [
                    {"itag": 18, "mimeType": "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"", "bitrate": 500_000, "height": 360, "qualityLabel": "360p", "url": "https://x/18"}
                ],
                "adaptiveFormats": [
                    {"itag": 137, "mimeType": "video/mp4; codecs=\"avc1.640028\"", "bitrate": 4_000_000, "height": 1080, "qualityLabel": "1080p", "contentLength": 123_456, "signatureCipher": "s=ABC&sp=sig&url=https%3A%2F%2Fx%2F137"},
                    {"itag": 140, "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"", "bitrate": 130_000, "contentLength": 12_000, "url": "https://x/140"}
                ]
            }
        });
        let formats = extract_formats(&response).expect("formats");
        assert_eq!(formats.len(), 3);
        let f137 = formats.iter().find(|f| f.itag == 137).expect("137");
        assert_eq!(f137.height, Some(1080));
        assert!(!f137.has_audio);
        assert!(f137.signature_cipher.is_some());
        let f18 = formats.iter().find(|f| f.itag == 18).expect("18");
        assert!(f18.has_audio);
        let f140 = formats.iter().find(|f| f.itag == 140).expect("140");
        assert!(f140.has_audio);
    }
}
