//! DASH (`.mpd`) manifest parsing (system-design §4.4).
//!
//! Scope: VOD MPDs using `SegmentTemplate` with `$Number$` addressing —
//! the overwhelmingly common static-VOD layout. Any `ContentProtection`
//! element aborts with [`MediaError::Drm`] (protected content — not
//! supported; no CDM, no license-server calls).

use crate::MediaError;

/// One `Representation` (video or audio) of a DASH manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Representation {
    /// `@id` (used for `$RepresentationID$` substitution).
    pub id: String,
    /// `@bandwidth` (bps).
    pub bandwidth: u64,
    /// `@width` (video only).
    pub width: Option<u32>,
    /// `@height` (video only).
    pub height: Option<u32>,
    /// `@mimeType` (falls back to the `AdaptationSet` value).
    pub mime_type: Option<String>,
    /// `@codecs`, when advertised.
    pub codecs: Option<String>,
    /// Resolved initialization URL template.
    pub initialization: Option<String>,
    /// Resolved media URL template (`$Number$` style).
    pub media_template: String,
    /// `@startNumber`.
    pub start_number: u64,
    /// Per-segment duration in the media timescale (`duration` form).
    pub segment_duration: Option<u64>,
    /// Total segment count (`SegmentTimeline` form).
    pub segment_count: Option<u64>,
    /// `@timescale` (defaults to 1).
    pub timescale: u64,
}

/// A parsed MPD: video + audio representations, best first.
#[derive(Debug, Clone, PartialEq)]
pub struct DashManifest {
    /// Video representations sorted by height desc, then bandwidth desc.
    pub video: Vec<Representation>,
    /// Audio representations sorted by bandwidth desc.
    pub audio: Vec<Representation>,
}

fn resolve(base: &str, uri: &str) -> String {
    if uri.starts_with("http://") || uri.starts_with("https://") {
        return uri.to_owned();
    }
    reqwest::Url::parse(base)
        .ok()
        .and_then(|base| base.join(uri).ok())
        .map_or_else(|| uri.to_owned(), |resolved| resolved.to_string())
}

/// Reads an XML attribute from a tag opening like `<Representation id="x"`.
fn attr_of(opening: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    let start = opening.find(&pattern)? + pattern.len();
    let end = opening[start..].find('"')? + start;
    Some(opening[start..end].to_owned())
}

fn attr_of_tag(xml: &str, tag: &str, name: &str) -> Option<String> {
    let open = String::from("<") + tag;
    let start = xml.find(&open)?;
    let end = xml[start..].find('>')? + start;
    attr_of(&xml[start..end], name)
}

fn parse_representations(
    adaptation: &str,
    base_url: &str,
    set_mime: Option<&str>,
) -> Vec<Representation> {
    let set_template = template_element(adaptation);
    let parse_tpl = |tpl: &str| {
        (
            attr_of_tag(tpl, "SegmentTemplate", "media"),
            attr_of_tag(tpl, "SegmentTemplate", "initialization"),
            attr_of_tag(tpl, "SegmentTemplate", "timescale").and_then(|v| v.parse().ok()),
            attr_of_tag(tpl, "SegmentTemplate", "duration").and_then(|v| v.parse().ok()),
            attr_of_tag(tpl, "SegmentTemplate", "startNumber")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1),
            // `SegmentTimeline` (ffmpeg-style MPDs): total segment count
            // from the `<S d= r=>` entries.
            timeline_segment_count(tpl),
        )
    };
    let mut reps = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = adaptation[cursor..].find("<Representation") {
        let start = cursor + rel;
        let Some(gt) = adaptation[start..].find('>') else {
            break;
        };
        let self_closing = adaptation.as_bytes().get(start + gt - 1) == Some(&b'/');
        let opening = &adaptation[start..start + gt];
        let body_end = if self_closing {
            start + gt
        } else {
            adaptation[start..]
                .find("</Representation>")
                .map_or(start + gt, |e| start + e)
        };
        let body = &adaptation[start..body_end];
        // Per-representation SegmentTemplate wins over the AdaptationSet's.
        let tpl = template_element(body).or_else(|| set_template.clone());
        if let Some(tpl) = tpl {
            let (media, init, timescale, duration, start_number, timeline_count) = parse_tpl(&tpl);
            if let Some(media) = media
                && (duration.is_some() || timeline_count.is_some())
            {
                reps.push(Representation {
                    id: attr_of(opening, "id").unwrap_or_default(),
                    bandwidth: attr_of(opening, "bandwidth")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0),
                    width: attr_of(opening, "width").and_then(|v| v.parse().ok()),
                    height: attr_of(opening, "height").and_then(|v| v.parse().ok()),
                    mime_type: attr_of(opening, "mimeType").or_else(|| set_mime.map(str::to_owned)),
                    codecs: attr_of(opening, "codecs"),
                    initialization: init.map(|i| resolve(base_url, &i)),
                    media_template: resolve(base_url, &media),
                    start_number,
                    segment_duration: duration,
                    segment_count: timeline_count,
                    timescale: timescale.unwrap_or(1),
                });
            }
        }
        cursor = body_end;
    }
    reps
}

/// Extracts the full `<SegmentTemplate>…</SegmentTemplate>` element (or
/// self-closing opening tag).
fn template_element(xml: &str) -> Option<String> {
    let start = xml.find("<SegmentTemplate")?;
    if let Some(len) = xml[start..].find("</SegmentTemplate>") {
        Some(xml[start..start + len + "</SegmentTemplate>".len()].to_owned())
    } else {
        let end = xml[start..].find('>')? + start;
        Some(xml[start..=end].to_owned())
    }
}

/// Segment count from a `<SegmentTimeline>`: sum of `1 + r` per `<S>`.
fn timeline_segment_count(tpl: &str) -> Option<u64> {
    let start = tpl.find("<SegmentTimeline")?;
    let end = tpl[start..].find("</SegmentTimeline>")? + start;
    let timeline = &tpl[start..end];
    let mut count: u64 = 0;
    // The `d` (element duration) is informational here: the segment count is
    // the `1 + r` sum over entries; `d` itself is consumed by the player.
    let mut cursor = 0usize;
    while let Some(rel) = timeline[cursor..].find("<S ") {
        let s = cursor + rel;
        let tag_end = timeline[s..].find("/>")? + s;
        let tag = &timeline[s..=tag_end];
        let repeats = attr_of(tag, "r")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        count = count.saturating_add(1 + repeats);
        cursor = tag_end;
    }
    (count > 0).then_some(count)
}

/// Parses an MPD document. `ContentProtection` anywhere in a
/// `Representation`/`AdaptationSet` aborts with [`MediaError::Drm`].
///
/// # Errors
///
/// Returns [`MediaError::Drm`] for protected content and
/// [`MediaError::Parse`] for malformed/unusable manifests.
pub fn parse_mpd(xml: &str, base_url: &str) -> Result<DashManifest, MediaError> {
    if xml.contains("ContentProtection") {
        return Err(MediaError::Drm);
    }
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = xml[cursor..].find("<AdaptationSet") {
        let start = cursor + rel;
        let Some(body_len) = xml[start..].find("</AdaptationSet>") else {
            break;
        };
        let adaptation = &xml[start..start + body_len];
        let set_mime = attr_of_tag(adaptation, "AdaptationSet", "contentType")
            .or_else(|| attr_of_tag(adaptation, "AdaptationSet", "mimeType"));
        let reps = parse_representations(adaptation, base_url, set_mime.as_deref());
        let is_video = set_mime.as_deref().is_some_and(|m| m.starts_with("video/"))
            || reps.iter().any(|r| r.height.is_some());
        if is_video {
            video.extend(reps);
        } else {
            audio.extend(reps);
        }
        cursor = start + body_len;
    }
    if video.is_empty() && audio.is_empty() {
        return Err(MediaError::Parse(
            "MPD has no usable representations".into(),
        ));
    }
    video.sort_by_key(|r| (std::cmp::Reverse(r.height), std::cmp::Reverse(r.bandwidth)));
    audio.sort_by_key(|r| std::cmp::Reverse(r.bandwidth));
    Ok(DashManifest { video, audio })
}

impl Representation {
    /// Builds the URL for segment `number`, substituting
    /// `$RepresentationID$`, `$Bandwidth$` and `$Number$` (with `%0Nd`
    /// zero padding).
    #[must_use]
    pub fn segment_url(&self, number: u64) -> String {
        self.substitute(&self.media_template, number)
    }

    /// Builds the initialization URL.
    #[must_use]
    pub fn init_url(&self) -> Option<String> {
        self.initialization
            .as_ref()
            .map(|tpl| self.substitute(tpl, self.start_number))
    }

    fn substitute(&self, template: &str, number: u64) -> String {
        let mut out = template.to_string();
        for (token, value) in [
            ("$RepresentationID$", self.id.clone()),
            ("$Bandwidth$", self.bandwidth.to_string()),
            ("$Time$", String::new()),
        ] {
            out = out.replace(token, &value);
        }
        // $Number$ and $Number%0Nd$ (zero-padded) forms.
        while let Some(pos) = out.find("$Number") {
            let Some(rel) = out[pos + 7..].find('$') else {
                break;
            };
            let dollar = pos + 7 + rel;
            let directive = &out[pos + 7..dollar]; // e.g. "%05d" or ""
            let padded = if directive.starts_with("%0") && directive.ends_with('d') {
                let width: usize = directive[2..directive.len() - 1].parse().unwrap_or(0);
                format!("{number:0width$}")
            } else {
                number.to_string()
            };
            out.replace_range(pos..=dollar, &padded);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    const MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static">
  <Period>
    <AdaptationSet contentType="video">
      <SegmentTemplate timescale="12800" duration="128000" startNumber="1"
        initialization="v/$RepresentationID$/init.mp4" media="v/$RepresentationID$/$Number%05d$.m4s"/>
      <Representation id="v360" bandwidth="800000" width="640" height="360" mimeType="video/mp4" codecs="avc1.42001e"/>
      <Representation id="v1080" bandwidth="4000000" width="1920" height="1080" mimeType="video/mp4" codecs="avc1.640028"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio">
      <SegmentTemplate timescale="48000" duration="192000" startNumber="1"
        initialization="a/$RepresentationID$/init.mp4" media="a/$RepresentationID$/$Number$.m4s"/>
      <Representation id="a140" bandwidth="128000" mimeType="audio/mp4" codecs="mp4a.40.2"/>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn parses_representations_and_templates() {
        let manifest = parse_mpd(MPD, "https://example.com/vod/manifest.mpd").unwrap();
        assert_eq!(manifest.video.len(), 2);
        assert_eq!(manifest.audio.len(), 1);
        let best = &manifest.video[0];
        assert_eq!(best.id, "v1080");
        assert_eq!(best.height, Some(1080));
        assert_eq!(
            best.segment_url(1),
            "https://example.com/vod/v/v1080/00001.m4s"
        );
        assert_eq!(
            best.segment_url(12),
            "https://example.com/vod/v/v1080/00012.m4s"
        );
        assert_eq!(
            best.init_url().unwrap(),
            "https://example.com/vod/v/v1080/init.mp4"
        );
        let audio = &manifest.audio[0];
        assert_eq!(audio.segment_url(3), "https://example.com/vod/a/a140/3.m4s");
    }

    #[test]
    fn content_protection_aborts_as_drm() {
        let drm = MPD.replace(
            "<Period>",
            "<Period><ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\"/>",
        );
        assert!(matches!(
            parse_mpd(&drm, "https://example.com/x.mpd"),
            Err(MediaError::Drm)
        ));
    }

    #[test]
    fn garbage_mpd_is_a_parse_error() {
        assert!(matches!(
            parse_mpd("<html>nope</html>", "https://example.com/x.mpd"),
            Err(MediaError::Parse(_))
        ));
    }
}
