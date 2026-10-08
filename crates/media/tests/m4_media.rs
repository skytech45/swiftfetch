//! M4 integration: the media pipeline against ffmpeg-generated fixtures.
//!
//! HLS: master + variant playlist with an AES-128 key → segments decrypt
//! and the remuxed MP4 lands. DASH: an ffmpeg-generated MPD downloads and
//! merges video+audio. DRM (`ContentProtection`) aborts cleanly.
//! ffmpeg/ffprobe come from `PATH` (installed in CI; winget/choco locally).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::format_push_string
)] // test fixtures: bounded buffers, display-only formatting

use std::path::{Path, PathBuf};
use std::process::Stdio;

use swiftfetch_media::{Ffmpeg, MediaContext, capture};
use swiftfetch_test_server::{Route, TestServer};
use tokio_util::sync::CancellationToken;

fn ffmpeg_exe() -> PathBuf {
    // Prefer PATH; fall back to the winget layout used on the dev box.
    if let Ok(path) = which("ffmpeg") {
        return path;
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    if let Some(local) = local {
        let mut candidates = std::fs::read_dir(local.join("Microsoft/WinGet/Packages"))
            .map(|entries| {
                entries
                    .filter_map(std::result::Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with("Gyan.FFmpeg"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        candidates.sort();
        if let Some(dir) = candidates.pop() {
            let exe = dir.join("ffmpeg-9.0-full_build/bin/ffmpeg.exe");
            if exe.is_file() {
                return exe;
            }
        }
    }
    panic!("ffmpeg is required for this suite (winget install ffmpeg / choco / brew / apt)");
}

fn which(name: &str) -> std::io::Result<PathBuf> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        let with_ext = dir.join(format!("{name}.exe"));
        if with_ext.is_file() {
            return Ok(with_ext);
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::NotFound, name))
}

async fn run_ffmpeg(exe: &Path, args: &[&str], out: &Path) {
    let status = tokio::process::Command::new(exe)
        .args(args)
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .expect("ffmpeg spawn");
    assert!(status.success(), "ffmpeg fixture generation failed");
}

/// A 1-second 128x96 MP4 with video + audio.
async fn sample_mp4(dir: &Path, name: &str) -> PathBuf {
    let exe = ffmpeg_exe();
    let out = dir.join(name);
    run_ffmpeg(
        &exe,
        &[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=128x96:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-shortest",
        ],
        &out,
    )
    .await;
    out
}

async fn sample_ts(dir: &Path, name: &str) -> PathBuf {
    let exe = ffmpeg_exe();
    let out = dir.join(name);
    run_ffmpeg(
        &exe,
        &[
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=128x96:rate=10",
            "-c:v",
            "mpeg4",
            "-f",
            "mpegts",
        ],
        &out,
    )
    .await;
    out
}

fn is_mp4(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes.windows(4).any(|w| w == b"ftyp"))
}

#[tokio::test(flavor = "multi_thread")]
async fn hls_aes128_captures_and_remuxes() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Segments are generated inside sample_ts; no direct ffmpeg use here.
    let _ = ffmpeg_exe();

    // Two clear TS segments, one AES-128-CBC encrypted with a known key/IV.
    let seg0 = sample_ts(dir.path(), "seg0.ts").await;
    let seg1 = sample_ts(dir.path(), "seg1.ts").await;
    let key: [u8; 16] = core::array::from_fn(|i| i as u8);
    let iv: [u8; 16] = core::array::from_fn(|i| (i as u8) ^ 0x5A);
    encrypt_aes128(&seg0, &dir.path().join("seg0.enc"), &key, &iv);
    encrypt_aes128(&seg1, &dir.path().join("seg1.enc"), &key, &iv);

    let server = TestServer::start().await.expect("server");
    server.set_route("/key.bin", Route::new(key.to_vec()));
    server.set_route(
        "/seg0.ts",
        Route::new(std::fs::read(dir.path().join("seg0.enc")).expect("seg0")),
    );
    server.set_route(
        "/seg1.ts",
        Route::new(std::fs::read(dir.path().join("seg1.enc")).expect("seg1")),
    );
    let mut iv_hex = String::with_capacity(32);
    for b in iv {
        iv_hex.push_str(&format!("{b:02x}"));
    }
    let playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"{key_url}\",IV=0x{iv_hex}\n\
         #EXTINF:1.0,\n{base}/seg0.ts\n#EXTINF:1.0,\n{base}/seg1.ts\n#EXT-X-ENDLIST\n",
        key_url = server.url("/key.bin"),
        base = server.url("")
    );
    server.set_route("/media.m3u8", Route::new(playlist.into_bytes()));

    let out = dir.path().join("out.mp4");
    let client = reqwest::Client::new();
    capture(
        &client,
        &MediaContext::default(),
        "hls",
        &server.url("/media.m3u8"),
        &out,
        None,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("hls capture");

    assert!(is_mp4(&out), "output must be an MP4 (ftyp box)");
    assert!(std::fs::metadata(&out).expect("meta").len() > 0);
}

fn encrypt_aes128(input: &Path, output: &Path, key: &[u8; 16], iv: &[u8; 16]) {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
    let data = std::fs::read(input).expect("segment");
    let enc = Aes128CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&data);
    std::fs::write(output, enc).expect("write encrypted");
}

const CHAR_BS: char = '\\';

#[tokio::test(flavor = "multi_thread")]
async fn dash_captures_and_merges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = ffmpeg_exe();

    // ffmpeg's dash muxer generates the MPD + init/segments fixture.
    let source = sample_mp4(dir.path(), "source.mp4").await;
    let dash_dir = dir.path().join("dash");
    std::fs::create_dir_all(&dash_dir).expect("dash dir");
    let status = tokio::process::Command::new(&exe)
        .args(["-y", "-i"])
        .arg(&source)
        .args([
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-f",
            "dash",
            "-seg_duration",
            "0.5",
        ])
        .arg(
            dash_dir
                .join("manifest.mpd")
                .to_string_lossy()
                .replace(CHAR_BS, "/"),
        )
        .stderr(Stdio::inherit())
        .status()
        .await
        .expect("ffmpeg dash");
    eprintln!(
        "dash ffmpeg status: {:?} exe: {:?} source size: {:?}",
        status.code(),
        exe,
        std::fs::metadata(&source).map(|m| m.len())
    );
    assert!(status.success(), "dash fixture generation failed");
    let generated: Vec<String> = std::fs::read_dir(&dash_dir)
        .expect("dash files")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    eprintln!("dash fixture files: {generated:?}");

    // Serve every generated file at its relative name; the MPD's relative
    // URLs resolve against the server root.
    let server = TestServer::start().await.expect("server");
    for entry in std::fs::read_dir(&dash_dir).expect("dash files") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        eprintln!("registered: /{name}");
        let body = std::fs::read(&path).expect("file body");
        server.set_route(&format!("/{name}"), Route::new(body));
    }

    let out = dir.path().join("out.mp4");
    let client = reqwest::Client::new();
    capture(
        &client,
        &MediaContext::default(),
        "dash",
        &server.url("/manifest.mpd"),
        &out,
        None,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("dash capture");

    assert!(is_mp4(&out), "output must be an MP4 (ftyp box)");
}

#[tokio::test(flavor = "multi_thread")]
async fn drm_manifest_aborts_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = TestServer::start().await.expect("server");
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static">
  <Period>
    <AdaptationSet contentType="video">
      <ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/>
      <SegmentTemplate timescale="12800" duration="128000" media="seg-$Number$.m4s"/>
      <Representation id="v1" bandwidth="800000" width="640" height="360" mimeType="video/mp4"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
    server.set_route("/drm.mpd", Route::new(mpd.as_bytes().to_vec()));
    let out = dir.path().join("out.mp4");
    let client = reqwest::Client::new();
    let err = capture(
        &client,
        &MediaContext::default(),
        "dash",
        &server.url("/drm.mpd"),
        &out,
        None,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect_err("DRM must abort");
    assert_eq!(err.code(), "E_DRM");
    assert!(err.to_string().contains("protected content"));
    assert!(!out.exists(), "no output for protected content");
}

/// Sanity for the sidecar locator used by the app (explicit path wins).
#[test]
fn ffmpeg_locates_from_env_override() {
    let exe = ffmpeg_exe();
    // SAFETY: test-only, exclusive access to this var.
    unsafe {
        std::env::set_var("SWIFTFETCH_FFMPEG", &exe);
    }
    assert!(Ffmpeg::locate().is_ok());
}
