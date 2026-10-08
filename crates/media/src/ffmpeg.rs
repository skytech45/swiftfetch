//! ffmpeg sidecar (system-design §4.4 "ffmpeg discipline"): resolved at
//! runtime, invoked with argv arrays only (never shell strings), progress
//! parsed from `-progress pipe:1`, hard stall timeout with kill.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::MediaError;

/// The resolved ffmpeg sidecar.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    exe: PathBuf,
}

/// Where `Ffmpeg::locate` looks, in order.
const SEARCH_HINT: &str = "$SWIFTFETCH_FFMPEG, PATH, the app directory";

impl Ffmpeg {
    /// Locates the sidecar: `$SWIFTFETCH_FFMPEG`, then `ffmpeg` on `PATH`,
    /// then beside the current executable.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Ffmpeg`] when no usable ffmpeg is found.
    pub fn locate() -> Result<Self, MediaError> {
        if let Some(path) = std::env::var_os("SWIFTFETCH_FFMPEG") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(Self { exe: path });
            }
        }
        let exe_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        if let Some(path) = which_on_path(exe_name) {
            return Ok(Self { exe: path });
        }
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
        {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Ok(Self { exe: candidate });
            }
        }
        Err(MediaError::Ffmpeg(format!(
            "ffmpeg not found (searched: {SEARCH_HINT})"
        )))
    }

    /// The resolved executable path (tests + diagnostics).
    #[must_use]
    pub fn exe(&self) -> &Path {
        &self.exe
    }

    /// Runs ffmpeg with `args`, streaming `-progress pipe:1` to `on_line`.
    /// Kills the process when no progress arrives within `stall_timeout`.
    async fn run(
        &self,
        args: &[std::ffi::OsString],
        stall_timeout: Duration,
        mut on_line: impl FnMut(&str),
    ) -> Result<(), MediaError> {
        let mut child = Command::new(&self.exe)
            .args(args)
            .arg("-progress")
            .arg("pipe:1")
            .arg("-nostats")
            .arg("-loglevel")
            .arg("error")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| MediaError::Ffmpeg(format!("spawn ffmpeg: {err}")))?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill().await;
            return Err(MediaError::Ffmpeg("ffmpeg stdout unavailable".into()));
        };
        let mut reader = BufReader::new(stdout).lines();
        let mut stderr_lines: Vec<String> = Vec::new();
        let mut saw_end = false;
        loop {
            match tokio::time::timeout(stall_timeout, reader.next_line()).await {
                Err(_) => {
                    let _ = child.kill().await;
                    return Err(MediaError::Ffmpeg(format!(
                        "ffmpeg stalled for {stall_timeout:?} without progress — killed"
                    )));
                }
                Ok(Ok(None)) => break,
                Ok(Ok(Some(line))) => {
                    on_line(&line);
                    if line == "progress=end" {
                        saw_end = true;
                    }
                }
                Ok(Err(err)) => {
                    let _ = child.kill().await;
                    return Err(MediaError::Ffmpeg(format!("progress read: {err}")));
                }
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|err| MediaError::Ffmpeg(format!("ffmpeg wait: {err}")))?;
        if !status.success() || !saw_end {
            // Drain stderr (bounded) for the error message.
            if let Some(stderr) = child.stderr.take() {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Ok(Some(line))) =
                    tokio::time::timeout(Duration::from_millis(500), lines.next_line()).await
                {
                    if stderr_lines.len() < 20 {
                        stderr_lines.push(line);
                    } else {
                        break;
                    }
                }
            }
            return Err(MediaError::Ffmpeg(format!(
                "ffmpeg failed ({}): {}",
                status,
                stderr_lines.join(" | ")
            )));
        }
        Ok(())
    }

    /// Remuxes a single stream into `out` (`-c copy`, faststart for MP4).
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Ffmpeg`] on spawn/failure/stall.
    pub async fn remux(&self, input: &Path, out: &Path) -> Result<(), MediaError> {
        let args: Vec<std::ffi::OsString> = vec![
            "-y".into(),
            "-i".into(),
            input.as_os_str().to_owned(),
            "-c".into(),
            "copy".into(),
            "-movflags".into(),
            "+faststart".into(),
            out.as_os_str().to_owned(),
        ];
        self.run(&args, Duration::from_secs(30), |_| {}).await
    }

    /// Merges a video and an audio stream into `out` (`-c copy`, faststart).
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Ffmpeg`] on spawn/failure/stall.
    pub async fn merge(&self, video: &Path, audio: &Path, out: &Path) -> Result<(), MediaError> {
        let args: Vec<std::ffi::OsString> = vec![
            "-y".into(),
            "-i".into(),
            video.as_os_str().to_owned(),
            "-i".into(),
            audio.as_os_str().to_owned(),
            "-c".into(),
            "copy".into(),
            "-movflags".into(),
            "+faststart".into(),
            out.as_os_str().to_owned(),
        ];
        self.run(&args, Duration::from_secs(30), |_| {}).await
    }
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    fn ffmpeg() -> Ffmpeg {
        match Ffmpeg::locate() {
            Ok(f) => f,
            Err(MediaError::Ffmpeg(msg)) => panic!(
                "ffmpeg is required for this test suite (install via winget/choco/brew/apt): {msg}"
            ),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    /// Generates a 1 s 128x96 test-source MP4 (H.264 + AAC) via ffmpeg.
    pub(crate) async fn sample_mp4(dir: &Path, name: &str) -> PathBuf {
        let ffmpeg = ffmpeg();
        let out = dir.join(name);
        let status = tokio::process::Command::new(ffmpeg.exe())
            .args([
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
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&out)
            .status()
            .await
            .expect("ffmpeg run");
        assert!(status.success(), "fixture generation failed");
        out
    }

    #[tokio::test]
    async fn remux_produces_faststart_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = sample_mp4(dir.path(), "in.mp4").await;
        let out = dir.path().join("out.mp4");
        let ffmpeg = ffmpeg();
        ffmpeg.remux(&input, &out).await.expect("remux");
        assert!(out.is_file());
        assert!(std::fs::metadata(&out).expect("meta").len() > 0);
    }

    #[tokio::test]
    async fn merge_combines_video_and_audio() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Two video-only+audio-only files: generate a combined one, then
        // split streams to get separate inputs.
        let combined = sample_mp4(dir.path(), "combined.mp4").await;
        let video_only = dir.path().join("v.mp4");
        let audio_only = dir.path().join("a.m4a");
        let ffmpeg = ffmpeg();
        let status = tokio::process::Command::new(ffmpeg.exe())
            .args(["-y", "-i"])
            .arg(&combined)
            .args(["-map", "0:v:0", "-an", "-c:v", "copy"])
            .arg(&video_only)
            .status()
            .await
            .expect("extract video");
        assert!(status.success());
        let status = tokio::process::Command::new(ffmpeg.exe())
            .args(["-y", "-i"])
            .arg(&combined)
            .args(["-map", "0:a:0", "-vn", "-c:a", "copy"])
            .arg(&audio_only)
            .status()
            .await
            .expect("extract audio");
        assert!(status.success());
        let merged = dir.path().join("merged.mp4");
        ffmpeg
            .merge(&video_only, &audio_only, &merged)
            .await
            .expect("merge");
        assert!(merged.is_file());
        assert!(std::fs::metadata(&merged).expect("meta").len() > 0);
    }
}
