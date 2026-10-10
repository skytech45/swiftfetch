//! `SwiftFetch` `YouTube` one-click site module (non-DRM only).
//!
//! Build Prompt §12.4: floating button → quality list → one click → merged
//! MP4. Pipeline: watch-page `player_response` JSON → itag table (data, not
//! logic) → cipher solver (hot-updatable trait) → quality pairing + ffmpeg
//! merge → stream-URL expiry re-resolution (fresh `enumerate` per attempt,
//! never a blind retry of an expired URL).
//!
//! Hard boundaries: DRM/Widevine-signaled content aborts with "protected
//! content — not supported"; livestreams and rental/Premium-only content
//! are "not supported"; license servers are never contacted; on solver
//! failure there is exactly one attempt, then the clean error "`YouTube` player changed — extractor update required" (`E_EXTRACTOR_STALE`).

pub mod cipher;
pub mod itag;
pub mod player;

use std::path::{Path, PathBuf};

use swiftfetch_media::{Ffmpeg, MediaContext, MediaError, ensure_ffmpeg};
use tokio_util::sync::CancellationToken;

pub use cipher::{
    CipherSolver, IdentitySolver, RuntimeSolver, SolverError, build_stream_url, parse_query,
};
pub use itag::itag_label;
pub use player::StreamFormat;

/// `YouTube` module errors surfaced to the UI.
#[derive(Debug, thiserror::Error)]
pub enum YoutubeError {
    /// Livestreams cannot be captured.
    #[error("livestreams are not supported")]
    Live,
    /// The server refuses the capture (rental, protected, Premium-only…).
    #[error("protected content — not supported{reason}")]
    Unplayable {
        /// Whether the refusal looked like DRM/rental protection.
        protected_content: bool,
        /// Server-provided reason, when present.
        reason: String,
    },
    /// Watch page / player response could not be used.
    #[error("YouTube parse error: {0}")]
    Parse(String),
    /// No matching format for the requested quality.
    #[error("no stream matches the requested quality")]
    NoFormat,
    /// The player changed; the cipher solver gave up after one attempt.
    #[error(transparent)]
    ExtractorStale(#[from] SolverError),
    /// Underlying media pipeline failure.
    #[error(transparent)]
    Media(#[from] MediaError),
}

impl YoutubeError {
    /// Machine-readable code for the UI/journal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Live => "E_YT_LIVE",
            Self::Unplayable { .. } => "E_YT_PROTECTED",
            Self::Parse(_) => "E_YT_PARSE",
            Self::NoFormat => "E_YT_NO_FORMAT",
            Self::ExtractorStale(_) => "E_EXTRACTOR_STALE",
            Self::Media(err) => err.code(),
        }
    }
}

/// A video's enumerable qualities (what the extension quality list shows).
#[derive(Debug, Clone)]
pub struct VideoInfo {
    /// Video title, when the page provides one.
    pub title: String,
    /// Player base.js URL for the cipher solver (`None` when the page
    /// advertises none — direct URLs work without a solver).
    pub base_js_url: Option<String>,
    /// Formats sorted best-first (video by height desc, then audio by
    /// bandwidth desc).
    pub formats: Vec<StreamFormat>,
}

/// Site-module client over a watch page.
pub struct YoutubeSite<'a> {
    client: &'a reqwest::Client,
    solver: &'a dyn CipherSolver,
}

impl<'a> YoutubeSite<'a> {
    /// Builds a site module over an HTTP client and a cipher solver.
    #[must_use]
    pub fn new(client: &'a reqwest::Client, solver: &'a dyn CipherSolver) -> Self {
        Self { client, solver }
    }

    async fn fetch_watch_page(
        &self,
        url: &str,
        ctx: &MediaContext,
    ) -> Result<String, YoutubeError> {
        let mut request = self.client.get(url);
        if let Some(cookies) = &ctx.cookies {
            request = request.header(reqwest::header::COOKIE, cookies.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|err| YoutubeError::Parse(format!("watch page fetch: {err}")))?;
        if !response.status().is_success() {
            return Err(YoutubeError::Parse(format!(
                "watch page status {}",
                response.status()
            )));
        }
        response
            .text()
            .await
            .map_err(|err| YoutubeError::Parse(format!("watch page body: {err}")))
    }

    /// Enumerates the video's qualities for the picker. This is the fresh
    /// snapshot used both for the quality list and for one-click picks —
    /// stream URLs expire, so every job re-enumerates instead of caching.
    ///
    /// # Errors
    ///
    /// See [`YoutubeError`].
    pub async fn enumerate(
        &self,
        watch_url: &str,
        ctx: &MediaContext,
    ) -> Result<VideoInfo, YoutubeError> {
        let html = self.fetch_watch_page(watch_url, ctx).await?;
        let response = player::extract_player_response(&html)?;
        player::check_playability(&response)?;
        let mut formats = player::extract_formats(&response)?;
        let base_js_url = player::extract_base_js_url(&html).ok();
        // Cipher freshness probe (part of enumerate): when any ciphered
        // format needs the solver, make exactly one probe call so a stale
        // player fails here — never later, never more than once per call.
        if let Some(base) = base_js_url.clone()
            && formats.iter().any(|f| f.signature_cipher.is_some())
        {
            self.solver.solve("e30=", &base).await?;
        }
        // Resolve obfuscated URLs eagerly? No — resolving is deferred to
        // `resolve_url` so the solver runs at most once per chosen stream.
        let title = response
            .pointer("/videoDetails/title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("youtube-video")
            .to_owned();
        formats.sort_by_key(|f| {
            let height = std::cmp::Reverse(f.height.unwrap_or(0));
            (height, std::cmp::Reverse(f.bitrate))
        });
        Ok(VideoInfo {
            title,
            base_js_url,
            formats,
        })
    }

    /// Resolves a format to a direct stream URL (deciphering through the
    /// solver only when the format is obfuscated).
    ///
    /// # Errors
    ///
    /// Returns [`YoutubeError::ExtractorStale`] after exactly one solver
    /// attempt when the format carries a cipher the solver cannot parse.
    pub async fn resolve_url(
        &self,
        format: &StreamFormat,
        base_js_url: Option<&str>,
    ) -> Result<String, YoutubeError> {
        if let Some(url) = &format.url {
            return Ok(url.clone());
        }
        let Some(cipher) = &format.signature_cipher else {
            return Err(YoutubeError::Parse(format!(
                "format {} has neither url nor signatureCipher",
                format.itag
            )));
        };
        let Some(base) = base_js_url else {
            return Err(YoutubeError::Parse(
                "ciphered format but the watch page has no player script URL".into(),
            ));
        };
        Ok(build_stream_url(self.solver, cipher, base).await?)
    }

    /// The qualities the picker shows: video (and progressive) streams
    /// with a resolvable resolution, best first.
    ///
    /// # Errors
    ///
    /// See [`YoutubeError`].
    pub async fn quality_list(
        &self,
        watch_url: &str,
        ctx: &MediaContext,
    ) -> Result<Vec<QualityOption>, YoutubeError> {
        let info = self.enumerate(watch_url, ctx).await?;
        let mut options = Vec::new();
        for format in &info.formats {
            let Some(height) = format.height else {
                continue;
            };
            if format.has_audio || format.mime.starts_with("video/") {
                let label = format
                    .quality_label
                    .clone()
                    .or_else(|| itag::itag_label(format.itag).map(str::to_owned))
                    .unwrap_or_else(|| format!("{height}p"));
                options.push(QualityOption {
                    itag: format.itag,
                    label,
                    height,
                    has_audio: format.has_audio,
                    container: format.mime.split('/').nth(1).unwrap_or("mp4").to_owned(),
                });
            }
        }
        options.dedup_by(|a, b| a.height == b.height && a.has_audio == b.has_audio);
        Ok(options)
    }

    /// One-click capture (§12.4): picks the best ≤`prefer_height` video
    /// stream plus the best audio stream, downloads both and merges into
    /// an MP4 at `out`. A progressive (audio+video) pick downloads
    /// directly.
    ///
    /// # Errors
    ///
    /// See [`YoutubeError`].
    #[allow(clippy::too_many_arguments)] // one pipeline signature
    pub async fn one_click(
        &self,
        watch_url: &str,
        out: &Path,
        prefer_height: u32,
        ctx: &MediaContext,
        cancel: &CancellationToken,
        mut on_progress: impl FnMut(u64),
    ) -> Result<PathBuf, YoutubeError> {
        let info = self.enumerate(watch_url, ctx).await?;
        let base = info.base_js_url.as_deref();
        // Best video pick: highest height ≤ prefer_height, then bitrate;
        // adaptive (video-only) streams first so one-click tests the merge
        // path, falling back to progressive when no adaptive pick fits.
        let pick_video = |want_audio: bool| {
            info.formats
                .iter()
                .filter(|f| {
                    f.has_audio == want_audio && f.height.is_some() && f.mime.starts_with("video/")
                })
                .filter(|f| f.height.is_some_and(|h| h <= prefer_height))
                .max_by_key(|f| (f.height.unwrap_or(0), f.bitrate))
                .cloned()
        };
        let pick_audio = || {
            info.formats
                .iter()
                .filter(|f| f.has_audio && f.mime.starts_with("audio/") && f.mime.contains("mp4"))
                .max_by_key(|f| f.bitrate)
                .cloned()
        };
        let video = pick_video(false)
            .or_else(|| pick_video(true))
            .ok_or(YoutubeError::NoFormat)?;
        let stage = out
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        if video.has_audio {
            // Progressive: a direct MP4 download.
            let url = self.resolve_url(&video, base).await?;
            swiftfetch_media::download_to_file(self.client, ctx, &url, out, cancel, on_progress)
                .await?;
            return Ok(out.to_path_buf());
        }
        let audio = pick_audio().ok_or(YoutubeError::NoFormat)?;
        let video_url = self.resolve_url(&video, base).await?;
        let audio_url = self.resolve_url(&audio, base).await?;
        let video_part = stage.join("yt-video.mp4");
        let audio_part = stage.join("yt-audio.mp4");
        let mut transferred: u64 = 0;
        swiftfetch_media::download_to_file(
            self.client,
            ctx,
            &video_url,
            &video_part,
            cancel,
            |done| {
                on_progress(transferred + done);
            },
        )
        .await?;
        transferred += std::fs::metadata(&video_part).map_or(0, |m| m.len());
        swiftfetch_media::download_to_file(
            self.client,
            ctx,
            &audio_url,
            &audio_part,
            cancel,
            |done| {
                on_progress(transferred + done);
            },
        )
        .await?;
        let ffmpeg = Ffmpeg::from_exe(ensure_ffmpeg().await?);
        ffmpeg.merge(&video_part, &audio_part, out).await?;
        let _ = std::fs::remove_file(&video_part);
        let _ = std::fs::remove_file(&audio_part);
        Ok(out.to_path_buf())
    }
}

/// One entry of the extension's quality list.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QualityOption {
    /// The itag backing this option.
    pub itag: u32,
    /// Human label (e.g. `1080p`).
    pub label: String,
    /// Vertical resolution.
    pub height: u32,
    /// Whether the option carries audio (progressive).
    pub has_audio: bool,
    /// Container (`mp4`/`webm`).
    pub container: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;
    use crate::itag::{ITAG_18, ITAG_137, ITAG_140};

    #[test]
    fn itag_table_maps_reference_itags() {
        assert_eq!(itag_label(ITAG_137), Some("1080p"));
        assert_eq!(itag_label(ITAG_140), Some("128k audio"));
        assert_eq!(itag_label(ITAG_18), Some("360p progressive"));
    }
}
