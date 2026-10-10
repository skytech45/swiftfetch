//! ffmpeg download-on-first-use (Windows production decision, PRD §10 Q3).
//!
//! Bundling a ~110 MB ffmpeg would triple the installer past the 15 MB
//! goal, so the first media job fetches the pinned Gyan essentials build
//! into the per-user data dir, verifies its SHA-256, and extracts
//! `bin/ffmpeg.exe`. The sidecar stays a separate `GPLv3` binary invoked
//! as its own process (never linked) — see the README licensing note.
//!
//! Override for tests/air-gap: `SWIFTFETCH_FFMPEG_URL` +
//! `SWIFTFETCH_FFMPEG_SHA256` point at any zip containing `bin/ffmpeg.exe`.

use std::path::{Path, PathBuf};

use futures_util::StreamExt as _;
use sha2::{Digest as _, Sha256};
use std::io::Write as _;

use crate::{Ffmpeg, MediaError};

/// Pinned ffmpeg release (Gyan essentials, Windows x64).
pub const PINNED_VERSION: &str = "9.0.2";
/// Download URL for the pinned release.
pub const PINNED_URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
/// SHA-256 of the pinned zip (from the `.sha256` sidecar file).
pub const PINNED_SHA256: &str = "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba";

/// Sanity cap for the download (the pinned zip is ~109 MB).
const MAX_ZIP_BYTES: u64 = 300 * 1024 * 1024;

/// Ensures a usable ffmpeg, downloading it on first use.
///
/// Order: `Ffmpeg::locate()` (env/PATH/app dir) → previously bootstrapped
/// copy in the user data dir → pinned download. Returns the exe path.
///
/// # Errors
///
/// [`MediaError::Ffmpeg`] when nothing resolves and the download fails
/// (message tells the user to install ffmpeg manually).
pub async fn ensure_ffmpeg() -> Result<PathBuf, MediaError> {
    if let Ok(found) = Ffmpeg::locate() {
        return Ok(found.exe().to_path_buf());
    }
    let dir = bootstrap_dir()?;
    let exe = dir.join(exe_name());
    if exe.is_file() {
        return Ok(exe);
    }
    download_pinned(&dir).await?;
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(MediaError::Ffmpeg(
            "ffmpeg bootstrap extracted nothing usable — install ffmpeg manually".into(),
        ))
    }
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn bootstrap_dir() -> Result<PathBuf, MediaError> {
    dirs::data_dir()
        .map(|base| base.join("SwiftFetch").join("ffmpeg"))
        .ok_or_else(|| MediaError::Ffmpeg("no user data dir".into()))
}

fn source() -> (String, String) {
    let url = std::env::var("SWIFTFETCH_FFMPEG_URL").unwrap_or_else(|_| PINNED_URL.into());
    let sha = std::env::var("SWIFTFETCH_FFMPEG_SHA256").unwrap_or_else(|_| PINNED_SHA256.into());
    (url, sha.to_ascii_lowercase())
}

async fn download_pinned(dir: &Path) -> Result<(), MediaError> {
    let (url, sha) = source();
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MediaError::Ffmpeg(
            "refusing to fetch: bad SHA-256 pin".into(),
        ));
    }
    tracing::info!(
        version = PINNED_VERSION,
        "fetching ffmpeg (first media use)"
    );
    std::fs::create_dir_all(dir).map_err(|e| MediaError::Ffmpeg(format!("mkdir: {e}")))?;
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| MediaError::Ffmpeg(format!("ffmpeg download: {e}")))?;
    if !response.status().is_success() {
        return Err(MediaError::Ffmpeg(format!(
            "ffmpeg download: HTTP {}",
            response.status()
        )));
    }
    let tmp = dir.join("ffmpeg-dl.zip.part");
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| MediaError::Ffmpeg(format!("tmp: {e}")))?;
    let mut hasher = Sha256::new();
    let mut bytes: u64 = 0;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| MediaError::Ffmpeg(format!("ffmpeg download: {e}")))?;
        bytes = bytes.saturating_add(chunk.len() as u64);
        if bytes > MAX_ZIP_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(MediaError::Ffmpeg(
                "ffmpeg download exceeds size cap".into(),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|e| MediaError::Ffmpeg(format!("tmp: {e}")))?;
    }
    drop(file);
    let digest = format!("{:x}", hasher.finalize());
    if digest != sha {
        let _ = std::fs::remove_file(&tmp);
        return Err(MediaError::Ffmpeg(
            "ffmpeg download failed integrity check — install ffmpeg manually".into(),
        ));
    }
    let dir_owned = dir.to_path_buf();
    let tmp_owned = tmp.clone();
    tokio::task::spawn_blocking(move || extract_ffmpeg(&tmp_owned, &dir_owned))
        .await
        .map_err(|e| MediaError::Ffmpeg(format!("extract task: {e}")))??;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

/// Extracts `bin/ffmpeg(.exe)` from `zip_path` into `dir`.
fn extract_ffmpeg(zip_path: &Path, dir: &Path) -> Result<(), MediaError> {
    let file =
        std::fs::File::open(zip_path).map_err(|e| MediaError::Ffmpeg(format!("zip: {e}")))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| MediaError::Ffmpeg(format!("zip: {e}")))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| MediaError::Ffmpeg(format!("zip: {e}")))?;
        let name = entry.name().replace('\\', "/");
        if name.ends_with(exe_name()) && name.contains("bin/") {
            let dest = dir.join(exe_name());
            let mut out = std::fs::File::create(&dest)
                .map_err(|e| MediaError::Ffmpeg(format!("zip: {e}")))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| MediaError::Ffmpeg(format!("zip: {e}")))?;
            return Ok(());
        }
    }
    Err(MediaError::Ffmpeg("zip has no bin/ffmpeg".into()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn pin_table_is_well_formed() {
        assert!(PINNED_URL.starts_with("https://"));
        assert_eq!(PINNED_SHA256.len(), 64);
        assert!(PINNED_SHA256.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn extracts_ffmpeg_from_a_local_zip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip_path = dir.path().join("fake.zip");
        {
            let file = std::fs::File::create(&zip_path).expect("zip");
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("bin/ffmpeg.exe", zip::write::SimpleFileOptions::default())
                .expect("entry");
            zip.write_all(b"fake-ffmpeg").expect("write");
            zip.finish().expect("finish");
        }
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).expect("mkdir");
        extract_ffmpeg(&zip_path, &out).expect("extract");
        assert_eq!(
            std::fs::read(out.join("ffmpeg.exe")).expect("read"),
            b"fake-ffmpeg"
        );
    }
}
