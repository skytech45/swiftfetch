//! M4 integration: the `YouTube` one-click pipeline (Build Prompt §12.4)
//! against fixture watch pages served locally.
//!
//! * Quality list from the fixture `player_response`: ≥ 3 itag-derived
//!   resolutions.
//! * One click at 1080p: the 137 video-only + 140 audio streams download
//!   and merge into an MP4.
//! * Stale player JS: exactly one attempt, then the clean
//!   `E_EXTRACTOR_STALE` error (asserted via the server request log).

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

use swiftfetch_media::MediaContext;
use swiftfetch_sites_youtube::{IdentitySolver, YoutubeSite};
use swiftfetch_test_server::{Route, TestServer};
use tokio_util::sync::CancellationToken;

fn ffmpeg_exe() -> PathBuf {
    if let Ok(path) = which("ffmpeg") {
        return path;
    }
    panic!("ffmpeg is required for this suite (winget/choco/brew/apt)");
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

async fn generate_streams(dir: &Path) -> (PathBuf, PathBuf) {
    let exe = ffmpeg_exe();
    let video = dir.join("v.mp4");
    let audio = dir.join("a.m4a");
    let status = tokio::process::Command::new(&exe)
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=128x96:rate=10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(&video)
        .stderr(Stdio::null())
        .status()
        .await
        .expect("video fixture");
    assert!(status.success());
    let status = tokio::process::Command::new(&exe)
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:a",
            "aac",
        ])
        .arg(&audio)
        .stderr(Stdio::null())
        .status()
        .await
        .expect("audio fixture");
    assert!(status.success());
    (video, audio)
}

/// Watch-page fixture with a `player_response` whose formats point at the
/// local server (137/140 obfuscated through `signatureCipher`, 18 direct).
fn watch_page_html(base: &str) -> String {
    format!(
        r#"<html><script>
var ytcfg = {{"jsUrl":"{base}/player-base.js"}};
var ytInitialPlayerResponse = {{"videoDetails":{{"title":"Fixture Video"}},"playabilityStatus":{{"status":"OK"}},"streamingData":{{"formats":[{{"itag":18,"mimeType":"video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"","bitrate":500000,"height":360,"qualityLabel":"360p","url":"{base}/v18.mp4"}}],"adaptiveFormats":[{{"itag":137,"mimeType":"video/mp4; codecs=\"avc1.640028\"","bitrate":4000000,"height":1080,"qualityLabel":"1080p","contentLength":123456,"signatureCipher":"s=ZmFrZQ%3D%3D&sp=sig&url={b64}%2Fv137.mp4"}},{{"itag":136,"mimeType":"video/mp4; codecs=\"avc1.44001F\"","bitrate":2000000,"height":720,"qualityLabel":"720p","contentLength":80000,"signatureCipher":"s=ZmFrZTI%3D&sp=sig&url={b64}%2Fv136.mp4"}},{{"itag":135,"mimeType":"video/mp4","bitrate":1000000,"height":480,"qualityLabel":"480p","url":"{base}/v135.mp4"}},{{"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130000,"contentLength":12000,"url":"{base}/a140.m4a"}}]}}}};
</script></html>
"#,
        base = base,
        b64 = base.replace('/', "%2F"),
    )
}

/// A base.js carrying the standard three-op decipher chain. `PLAYER_JS_URL`
/// is patched per test like a fixture parameter (every quality-list path
/// needs a resolvable player URL or it aborts with a parse error).
const PLAYER_JS: &str = r#"
var Qx = {
  swap: function(a, b) { var c = a[0]; a[0] = a[b % a.length]; a[b % a.length] = c; return a },
  splice: function(a, b) { a.splice(0, b); return a },
  reverse: function(a) { a.reverse(); return a }
};
function decipher(a) {
  a = a.split("");
  a = Qx.reverse(a, 7);
  a = Qx.swap(a, 3);
  return a.join("");
}
"#;

fn is_mp4(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes.windows(4).any(|w| w == b"ftyp"))
}

#[tokio::test(flavor = "multi_thread")]
async fn quality_list_shows_itag_derived_resolutions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (video, audio) = generate_streams(dir.path()).await;
    let server = TestServer::start().await.expect("server");
    server.set_route("/player-base.js", Route::new(PLAYER_JS.as_bytes().to_vec()));
    server.set_route("/v18.mp4", Route::new(std::fs::read(&video).expect("v18")));
    server.set_route(
        "/v137.mp4",
        Route::new(std::fs::read(&video).expect("v137")),
    );
    server.set_route(
        "/v136.mp4",
        Route::new(std::fs::read(&video).expect("v136")),
    );
    server.set_route(
        "/v135.mp4",
        Route::new(std::fs::read(&video).expect("v135")),
    );
    server.set_route(
        "/a140.m4a",
        Route::new(std::fs::read(&audio).expect("a140")),
    );
    server.set_route(
        "/watch",
        Route::new(watch_page_html(&server.url("")).into_bytes()),
    );

    let client = reqwest::Client::new();
    let solver = swiftfetch_sites_youtube::RuntimeSolver::new(client.clone());
    let site = YoutubeSite::new(&client, &solver);
    let qualities = site
        .quality_list(&server.url("/watch"), &MediaContext::default())
        .await
        .expect("quality list");

    let heights: Vec<u32> = qualities.iter().map(|q| q.height).collect();
    assert!(
        qualities.len() >= 3,
        "expected >= 3 itag-derived resolutions, got {heights:?}"
    );
    assert!(heights.contains(&1080), "1080p must be listed: {heights:?}");
    assert!(heights.contains(&720), "720p must be listed: {heights:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_click_1080p_downloads_and_merges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (video, audio) = generate_streams(dir.path()).await;
    let server = TestServer::start().await.expect("server");
    server.set_route("/player-base.js", Route::new(PLAYER_JS.as_bytes().to_vec()));
    server.set_route(
        "/v137.mp4",
        Route::new(std::fs::read(&video).expect("v137")),
    );
    server.set_route(
        "/a140.m4a",
        Route::new(std::fs::read(&audio).expect("a140")),
    );
    server.set_route(
        "/watch",
        Route::new(watch_page_html(&server.url("")).into_bytes()),
    );

    let client = reqwest::Client::new();
    let solver = swiftfetch_sites_youtube::RuntimeSolver::new(client.clone());
    let site = YoutubeSite::new(&client, &solver);
    let out = dir.path().join("merged.mp4");
    site.one_click(
        &server.url("/watch"),
        &out,
        1080,
        &MediaContext::default(),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("one click");

    assert!(out.is_file(), "merged file must exist");
    assert!(is_mp4(&out), "one-click output must be an MP4 (ftyp box)");
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_player_js_fails_cleanly_after_one_attempt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (video, _audio) = generate_streams(dir.path()).await;
    let server = TestServer::start().await.expect("server");
    // A player script with NO decipher chain: the freshness probe must give
    // up after exactly one attempt (the fixture needs jsUrl-bearing direct
    // routes so quality_list reaches the probe at all).
    server.set_route("/player-base.js", Route::new(b"var nothing = 1;".to_vec()));
    server.set_route(
        "/v137.mp4",
        Route::new(std::fs::read(&video).expect("v137")),
    );
    server.set_route(
        "/a140.m4a",
        Route::new(std::fs::read(&video).expect("a140")),
    );
    let html = watch_page_html(&server.url(""));
    server.set_route("/watch", Route::new(html.into_bytes()));

    let client = reqwest::Client::new();
    let solver = swiftfetch_sites_youtube::RuntimeSolver::new(client.clone());
    let site = YoutubeSite::new(&client, &solver);
    let err = site
        .quality_list(&server.url("/watch"), &MediaContext::default())
        .await
        .expect_err("must fail on stale player");
    assert_eq!(err.code(), "E_EXTRACTOR_STALE");
    assert!(
        err.to_string().contains("extractor update required"),
        "must surface the clean stale-extractor message: {err}"
    );

    // Exactly one attempt: the base.js was fetched once, never retried.
    let base_fetches = server
        .request_log()
        .iter()
        .filter(|r| r.path == "/player-base.js")
        .count();
    assert_eq!(base_fetches, 1, "exactly one base.js attempt expected");
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_urls_skip_the_solver() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (video, _audio) = generate_streams(dir.path()).await;
    let server = TestServer::start().await.expect("server");
    server.set_route(
        "/v135.mp4",
        Route::new(std::fs::read(&video).expect("v135")),
    );
    // No jsUrl on purpose: direct URLs skip the solver entirely.
    let html = format!(
        r#"<html><script>
var ytInitialPlayerResponse = {{"playabilityStatus":{{"status":"OK"}},"streamingData":{{"adaptiveFormats":[{{"itag":135,"mimeType":"video/mp4","bitrate":1000000,"height":480,"qualityLabel":"480p","url":"{}"}}]}}}};
</script></html>"#,
        server.url("/v135.mp4")
    );
    server.set_route("/watch", Route::new(html.into_bytes()));
    let client = reqwest::Client::new();
    let site = YoutubeSite::new(&client, &IdentitySolver);
    let qualities = site
        .quality_list(&server.url("/watch"), &MediaContext::default())
        .await
        .expect("quality list");
    assert_eq!(qualities.len(), 1);
    assert_eq!(qualities[0].label, "480p");
}
