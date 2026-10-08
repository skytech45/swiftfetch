//! `SwiftFetch` media engine (Milestone 4): HLS (`.m3u8`) and DASH (`.mpd`)
//! playlist capture, ffmpeg sidecar invocation (argv arrays only — never a
//! shell), audio+video merge and subtitle extraction.
//!
//! DRM-protected streams (Widevine, `PlayReady`, `FairPlay` — signaled by
//! `SAMPLE-AES`/session keys in HLS or `ContentProtection` in DASH) abort
//! with [`MediaError::Drm`]: no CDM, no license-server calls, ever.

pub mod dash;
pub mod ffmpeg;
pub mod hls;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

pub use dash::{DashManifest, Representation};
pub use ffmpeg::Ffmpeg;
pub use hls::{MasterVariant, MediaPlaylist};

/// Media-engine errors surfaced to the UI.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// Protected content (`SAMPLE-AES`, `ContentProtection`, keyserver) — abort,
    /// never probe a license server.
    #[error("protected content — not supported")]
    Drm,
    /// Playlist/manifest could not be parsed.
    #[error("media parse error: {0}")]
    Parse(String),
    /// Playlist/segment fetch failed.
    #[error("media fetch error: {status} for {url}")]
    Fetch {
        /// HTTP status (0 when the transport itself failed).
        status: u16,
        /// URL that failed.
        url: String,
    },
    /// ffmpeg sidecar missing, failed, or stalled.
    #[error("ffmpeg error: {0}")]
    Ffmpeg(String),
    /// Local filesystem failure.
    #[error("media io error: {0}")]
    Io(#[from] std::io::Error),
    /// The job was cancelled.
    #[error("cancelled")]
    Cancelled,
}

impl MediaError {
    /// Machine-readable code for the UI/journal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Drm => "E_DRM",
            Self::Parse(_) => "E_MEDIA_PARSE",
            Self::Fetch { .. } => "E_MEDIA_FETCH",
            Self::Ffmpeg(_) => "E_FFMPEG",
            Self::Io(_) => "E_IO",
            Self::Cancelled => "E_CANCELLED",
        }
    }
}

/// Progress/stage events emitted while a media job runs.
#[derive(Debug, Clone)]
pub enum MediaEvent {
    /// Which stage the job entered (probing/downloading/merging/subtitles).
    Stage(&'static str),
    /// Bytes completed so far.
    Progress(u64),
    /// Sidecar written (name, path).
    Sidecar(String, PathBuf),
}

/// Request context applied to every media request.
#[derive(Debug, Clone, Default)]
pub struct MediaContext {
    /// Cookie header value (capture-forwarded).
    pub cookies: Option<String>,
    /// Referer header value.
    pub referer: Option<String>,
}

async fn fetch_bytes(
    client: &reqwest::Client,
    ctx: &MediaContext,
    url: &str,
) -> Result<Vec<u8>, MediaError> {
    let mut request = client.get(url);
    if let Some(cookies) = &ctx.cookies {
        request = request.header(reqwest::header::COOKIE, cookies.clone());
    }
    if let Some(referer) = &ctx.referer {
        request = request.header(reqwest::header::REFERER, referer.clone());
    }
    let response = request.send().await.map_err(|_| MediaError::Fetch {
        status: 0,
        url: url.to_owned(),
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(MediaError::Fetch {
            status: status.as_u16(),
            url: url.to_owned(),
        });
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|_| MediaError::Fetch {
            status: 0,
            url: url.to_owned(),
        })
}

async fn fetch_text(
    client: &reqwest::Client,
    ctx: &MediaContext,
    url: &str,
) -> Result<String, MediaError> {
    let bytes = fetch_bytes(client, ctx, url).await?;
    String::from_utf8(bytes).map_err(|_| MediaError::Parse("playlist is not UTF-8".into()))
}

/// Fetches a 16-byte AES-128 key.
async fn fetch_key(
    client: &reqwest::Client,
    ctx: &MediaContext,
    uri: &str,
) -> Result<[u8; 16], MediaError> {
    let bytes = fetch_bytes(client, ctx, uri).await?;
    bytes
        .try_into()
        .map_err(|_| MediaError::Parse(format!("AES key at {uri} is not 16 bytes")))
}

/// Decrypts one AES-128-CBC segment. `iv` falls back to the segment's media
/// sequence number as a 128-bit big-endian value.
fn decrypt_aes128(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>, MediaError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    let plain = Aes128CbcDec::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| MediaError::Parse("segment decryption failed".into()))?;
    Ok(plain)
}

fn sequence_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

/// Fetches one segment, decrypting when a key is provided.
async fn fetch_segment(
    client: reqwest::Client,
    ctx: MediaContext,
    index: u64,
    uri: String,
    key: Option<Arc<[u8; 16]>>,
    iv: [u8; 16],
) -> Result<(u64, Vec<u8>), MediaError> {
    let mut data = fetch_bytes(&client, &ctx, &uri).await?;
    if let Some(key) = key {
        data = decrypt_aes128(&data, &key, &iv)?;
    }
    Ok((index, data))
}

/// Downloads an HLS media playlist's segments, decrypting AES-128 when the
/// playlist declares it, writing them sequentially to `out`. Concurrency is
/// bounded (4 in flight) while preserving on-disk order via an ordering
/// buffer.
///
/// # Errors
///
/// See [`MediaError`]; `Err` also covers cancellation.
#[allow(clippy::too_many_lines)] // one cohesive pipeline
pub async fn download_hls(
    client: &reqwest::Client,
    ctx: &MediaContext,
    playlist: &MediaPlaylist,
    out: &Path,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(u64),
) -> Result<(), MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }

    let key: Option<Arc<[u8; 16]>> = match &playlist.encryption {
        hls::Encryption::None => None,
        hls::Encryption::Aes128 { key_uri, .. } => {
            Some(fetch_key(client, ctx, key_uri).await?.into())
        }
    };
    let file = tokio::fs::File::create(out).await?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut written: u64 = 0;
    // (index, bytes) completion buffer; flushed in order.
    let mut completed: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut next_to_write: u64 = 0;
    let mut in_flight = futures_util::stream::FuturesUnordered::new();
    let mut queued: usize = 0;
    let total = playlist.segments.len() as u64;

    for (index, segment) in playlist.segments.iter().enumerate() {
        let index = index as u64;
        let iv = match &playlist.encryption {
            hls::Encryption::Aes128 { iv: Some(iv), .. } => *iv,
            _ => sequence_iv(playlist.media_sequence + index),
        };
        in_flight.push(fetch_segment(
            client.clone(),
            ctx.clone(),
            index,
            segment.uri.clone(),
            key.clone(),
            iv,
        ));
        queued += 1;
        if queued >= 4
            && let Some(result) = tokio::select! {
                () = cancel.cancelled() => return Err(MediaError::Cancelled),
                result = in_flight.next() => result,
            }
        {
            let (index, data) = result?;
            completed.insert(index, data);
            queued -= 1;
        }
        // Flush the contiguous prefix to disk.
        while let Some(data) = completed.remove(&next_to_write) {
            writer.write_all(&data).await?;
            written += data.len() as u64;
            on_progress(written);
            next_to_write += 1;
        }
    }
    while let Some(result) = tokio::select! {
        () = cancel.cancelled() => return Err(MediaError::Cancelled),
        result = in_flight.next() => result,
    } {
        let (index, data) = result?;
        completed.insert(index, data);
        while let Some(data) = completed.remove(&next_to_write) {
            writer.write_all(&data).await?;
            written += data.len() as u64;
            on_progress(written);
            next_to_write += 1;
        }
    }
    writer.flush().await?;
    if next_to_write != total {
        return Err(MediaError::Parse(format!(
            "segment gap: wrote {next_to_write} of {total}"
        )));
    }
    Ok(())
}

use futures_util::StreamExt;

/// Downloads one DASH representation (init + `$Number$` segments) to `out`.
///
/// # Errors
///
/// See [`MediaError`].
pub async fn download_dash_representation(
    client: &reqwest::Client,
    ctx: &MediaContext,
    rep: &Representation,
    segment_count: u64,
    out: &Path,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(u64),
) -> Result<(), MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let file = tokio::fs::File::create(out).await?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut written: u64 = 0;
    let mut urls: Vec<String> = Vec::new();
    if let Some(init) = rep.init_url() {
        urls.push(init);
    }
    for number in rep.start_number..rep.start_number + segment_count {
        urls.push(rep.segment_url(number));
    }
    for url in urls {
        let data = tokio::select! {
            () = cancel.cancelled() => return Err(MediaError::Cancelled),
            data = fetch_bytes(client, ctx, &url) => data?,
        };
        writer.write_all(&data).await?;
        written += data.len() as u64;
        on_progress(written);
    }
    writer.flush().await?;
    Ok(())
}

/// High-level capture: fetches the playlist/manifest, picks representations,
/// downloads streams, writes subtitles and merges/remuxes via ffmpeg.
///
/// * HLS: best variant (by bandwidth, or `prefer_height` when given) →
///   segments → remux to `out` (mp4); subtitle renditions are saved as
///   `.vtt` sidecars next to `out`.
/// * DASH: best video + best audio → merge to `out`.
///
/// # Errors
///
/// See [`MediaError`]; [`MediaError::Drm`] for protected content.
#[allow(clippy::too_many_lines)] // one pipeline per format family
#[allow(clippy::too_many_arguments)] // one pipeline signature for two format families
pub async fn capture(
    client: &reqwest::Client,
    ctx: &MediaContext,
    kind: &str,
    playlist_url: &str,
    out: &Path,
    prefer_height: Option<u32>,
    cancel: &CancellationToken,
    mut on_event: impl FnMut(MediaEvent),
) -> Result<(), MediaError> {
    let ffmpeg = Ffmpeg::locate()?;
    let stage_dir = out
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join(format!(".sfmedia-{}", std::process::id()));
    tokio::fs::create_dir_all(&stage_dir).await?;
    let result = capture_inner(
        client,
        ctx,
        kind,
        playlist_url,
        out,
        prefer_height,
        cancel,
        &stage_dir,
        &ffmpeg,
        &mut on_event,
    )
    .await;
    let _ = tokio::fs::remove_dir_all(&stage_dir).await;
    result
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
async fn capture_inner(
    client: &reqwest::Client,
    ctx: &MediaContext,
    kind: &str,
    playlist_url: &str,
    out: &Path,
    prefer_height: Option<u32>,
    cancel: &CancellationToken,
    stage_dir: &Path,
    ffmpeg: &Ffmpeg,
    on_event: &mut impl FnMut(MediaEvent),
) -> Result<(), MediaError> {
    match kind {
        "hls" => {
            on_event(MediaEvent::Stage("probing"));
            let text = fetch_text(client, ctx, playlist_url).await?;
            // Master playlist: pick the variant, capture subtitles, then
            // descend into the variant playlist. A bare media playlist is
            // used as-is.
            let variant_playlist_url: String = if text.contains("#EXT-X-STREAM-INF") {
                let (variants, renditions) = hls::parse_master(&text, playlist_url)?;
                let variant = variants
                    .iter()
                    .filter(|v| prefer_height.is_none_or(|h| v.height.is_none_or(|vh| vh <= h)))
                    .max_by_key(|v| v.bandwidth)
                    .ok_or(MediaError::Parse(
                        "no variant matches the requested quality".into(),
                    ))?
                    .clone();
                for (name, uri) in hls::subtitle_renditions(&renditions) {
                    on_event(MediaEvent::Stage("subtitles"));
                    if let Ok(sub_text) = fetch_text(client, ctx, &uri).await {
                        let safe = sanitize(&name);
                        let sidecar = out.with_file_name(format!(
                            "{}.{}.vtt",
                            out.file_stem().map_or_else(
                                || "subtitles".into(),
                                |s| s.to_string_lossy().into_owned()
                            ),
                            safe
                        ));
                        tokio::fs::write(&sidecar, sub_text).await?;
                        on_event(MediaEvent::Sidecar(name, sidecar));
                    }
                }
                variant.uri
            } else {
                playlist_url.to_owned()
            };
            on_event(MediaEvent::Stage("downloading"));
            let media_text = fetch_text(client, ctx, &variant_playlist_url).await?;
            let playlist = hls::parse_media(&media_text, &variant_playlist_url)?;
            let staged = stage_dir.join("stream.ts");
            download_hls(client, ctx, &playlist, &staged, cancel, |_| {}).await?;
            on_event(MediaEvent::Stage("merging"));
            ffmpeg.remux(&staged, out).await?;
            Ok(())
        }
        "dash" => {
            on_event(MediaEvent::Stage("probing"));
            let xml = fetch_text(client, ctx, playlist_url).await?;
            let manifest = dash::parse_mpd(&xml, playlist_url)?;
            let video = manifest.video.first().cloned();
            let audio = manifest.audio.first().cloned();
            // Segment count from the fixed duration template: ceil(total /
            // per-segment). MPD@mediaPresentationDuration parsing is out of
            // scope for the fixture set; the number of segments is instead
            // discovered by probing until 404 when duration is unknown.
            let known_count = manifest
                .video
                .first()
                .or_else(|| manifest.audio.first())
                .and_then(|rep| rep.segment_count);
            let segment_count = match known_count {
                Some(count) => count,
                // Unknown count (no timeline, no duration): probe forward.
                None => {
                    probe_segment_count(
                        client,
                        ctx,
                        manifest.video.first().or(manifest.audio.first()),
                        cancel,
                    )
                    .await?
                }
            };
            on_event(MediaEvent::Stage("downloading"));
            let mut video_part: Option<PathBuf> = None;
            if let Some(v) = &video {
                let path = stage_dir.join("video.m4s");
                download_dash_representation(client, ctx, v, segment_count, &path, cancel, |_| {})
                    .await?;
                video_part = Some(path);
            }
            let mut audio_part: Option<PathBuf> = None;
            if let Some(a) = &audio {
                let path = stage_dir.join("audio.m4s");
                download_dash_representation(client, ctx, a, segment_count, &path, cancel, |_| {})
                    .await?;
                audio_part = Some(path);
            }
            on_event(MediaEvent::Stage("merging"));
            match (video_part, audio_part) {
                (Some(v), Some(a)) => ffmpeg.merge(&v, &a, out).await?,
                (Some(v), None) | (None, Some(v)) => ffmpeg.remux(&v, out).await?,
                (None, None) => {
                    return Err(MediaError::Parse("no representations to download".into()));
                }
            }
            Ok(())
        }
        other => Err(MediaError::Parse(format!("unknown media kind {other}"))),
    }
}

async fn probe_segment_count(
    client: &reqwest::Client,
    ctx: &MediaContext,
    rep: Option<&Representation>,
    _cancel: &CancellationToken,
) -> Result<u64, MediaError> {
    let Some(rep) = rep else {
        return Ok(0);
    };
    // Probe forward until a segment 404s (bounded — VOD fixtures are small).
    let mut count: u64 = 0;
    let mut number = rep.start_number;
    while count < 10_000 {
        let url = rep.segment_url(number);
        match fetch_bytes(client, ctx, &url).await {
            Ok(_) => {
                count += 1;
                number += 1;
            }
            Err(MediaError::Fetch { status: s, .. }) if s == 404 || s == 410 => break,
            Err(other) => return Err(other),
        }
    }
    if count == 0 {
        return Err(MediaError::Parse("no DASH segments found".into()));
    }
    Ok(count)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Streams a direct URL to a file (used by site modules and the media
/// pipeline for large stream downloads). Returns the bytes written.
///
/// # Errors
///
/// See [`MediaError`].
pub async fn download_to_file(
    client: &reqwest::Client,
    ctx: &MediaContext,
    url: &str,
    out: &Path,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(u64),
) -> Result<u64, MediaError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let mut request = client.get(url);
    if let Some(cookies) = &ctx.cookies {
        request = request.header(reqwest::header::COOKIE, cookies.clone());
    }
    if let Some(referer) = &ctx.referer {
        request = request.header(reqwest::header::REFERER, referer.clone());
    }
    let response = request.send().await.map_err(|_| MediaError::Fetch {
        status: 0,
        url: url.to_owned(),
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(MediaError::Fetch {
            status: status.as_u16(),
            url: url.to_owned(),
        });
    }
    let file = tokio::fs::File::create(out).await?;
    let mut writer = tokio::io::BufWriter::new(file);
    let mut written: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::select! {
        () = cancel.cancelled() => return Err(MediaError::Cancelled),
        chunk = stream.next() => chunk,
    } {
        let chunk = chunk.map_err(|_| MediaError::Fetch {
            status: 0,
            url: url.to_owned(),
        })?;
        writer.write_all(&chunk).await?;
        written += chunk.len() as u64;
        on_progress(written);
    }
    writer.flush().await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    #[test]
    fn sequence_iv_is_big_endian_tail() {
        let iv = sequence_iv(7);
        assert_eq!(&iv[8..], &7u64.to_be_bytes());
        assert!(iv[..8].iter().all(|b| *b == 0));
    }

    #[test]
    fn sanitizer_replaces_path_characters() {
        assert_eq!(sanitize("English (US)"), "English__US_");
    }
}
