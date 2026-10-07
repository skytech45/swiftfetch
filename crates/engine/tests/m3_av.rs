//! M3 integration: the antivirus hook. A download whose content carries the
//! (benign, synthetic) malware marker must be flagged by the scanner,
//! deleted and fail with `E_MALWARE_DETECTED`; a clean file downloads
//! normally. The real-EICAR check against live Windows Defender is
//! local-only (`--ignored`): the marker below is deliberately NOT the real
//! EICAR string so that live antivirus software on dev machines never
//! interferes with the deterministic test.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

use std::path::Path;
use std::time::Duration;

use swiftfetch_engine::{AvScanner, Engine, EngineConfig, JobEvent, JobSpec, ScanOutcome};
use swiftfetch_store::Store;
use swiftfetch_test_server::{Route, TestServer};

/// Synthetic stand-in for the industry-standard EICAR test string — benign
/// and invisible to real antivirus products.
const MARKER: &str = "SWIFTFETCH-SIMULATED-MALWARE-MARKER-FOR-AV-HOOK-TEST";

/// The real EICAR antivirus test string (industry-standard AV test payload;
/// harmless by design — no actual malware anywhere in this repo).
const EICAR: &str = "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";

/// Content-inspecting fake: flags the file when it contains the marker.
#[derive(Debug)]
struct ContentScanner {
    needle: String,
}

impl AvScanner for ContentScanner {
    fn name(&self) -> &'static str {
        "content-fake"
    }

    fn scan(&self, path: &Path) -> ScanOutcome {
        let Ok(bytes) = std::fs::read(path) else {
            return ScanOutcome::Skipped {
                reason: "unreadable".into(),
            };
        };
        if bytes
            .windows(self.needle.len())
            .any(|w| w == self.needle.as_bytes())
        {
            ScanOutcome::Flagged {
                detection: "SWIFTFETCH-TEST-DETECTION".to_owned(),
            }
        } else {
            ScanOutcome::Clean
        }
    }
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        min_segment: 512 * 1024,
        tick: Duration::from_millis(250),
        ..EngineConfig::default()
    }
}

fn init_log() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("swiftfetch=debug"))
            .init();
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn marked_download_is_flagged_deleted_and_errors() {
    init_log();
    let server = TestServer::start().await.expect("server");
    server.set_route("/report.zip", Route::new(MARKER.as_bytes().to_vec()));
    let url = server.url("/report.zip");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("db.sqlite")).expect("store");
    let engine = Engine::open_with_av_scanner(
        engine_config(),
        store,
        std::sync::Arc::new(ContentScanner {
            needle: MARKER.to_owned(),
        }),
    )
    .expect("engine");

    let (id, mut rx) = engine
        .start_job(JobSpec::new(&url, dir.path().to_path_buf()))
        .await
        .expect("start_job");

    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("event")
            .expect("open");
        match ev {
            JobEvent::Failed { code, .. } => {
                assert_eq!(code, "E_MALWARE_DETECTED");
                break;
            }
            JobEvent::Completed { .. } => panic!("flagged download must not complete"),
            _ => {}
        }
    }

    // The flagged file never reached the destination and no part file left.
    let row = engine.snapshot(&id).expect("snapshot");
    assert!(
        !row.final_path.exists(),
        "flagged file must be deleted, found {}",
        row.final_path.display()
    );
    assert!(
        !Path::new(&format!("{}.sfpart", row.final_path.display())).exists(),
        "part file must be gone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_download_completes_under_scanner() {
    let len = 1024 * 1024;
    let server = TestServer::start().await.expect("server");
    server.set_route("/clean.bin", Route::new(vec![0x44u8; len]));
    let url = server.url("/clean.bin");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("db.sqlite")).expect("store");
    let engine = Engine::open_with_av_scanner(
        engine_config(),
        store,
        std::sync::Arc::new(ContentScanner {
            needle: MARKER.to_owned(),
        }),
    )
    .expect("engine");

    let (_, mut rx) = engine
        .start_job(JobSpec::new(&url, dir.path().to_path_buf()))
        .await
        .expect("start_job");
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("event")
            .expect("open");
        match ev {
            JobEvent::Completed { path } => {
                assert_eq!(std::fs::metadata(&path).expect("meta").len(), len as u64);
                break;
            }
            JobEvent::Failed { code, message } => panic!("failed: {code}: {message}"),
            _ => {}
        }
    }
}

/// Live Windows Defender check (local-only: CI runners may lack Defender).
/// Defender flags the EICAR test string either through real-time protection
/// (write blocked with os error 225, or the file silently quarantined away)
/// or through the on-demand `MpCmdRun` scan.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "local-only: requires Windows Defender; run with --ignored"]
async fn live_windows_defender_flags_eicar() {
    init_log();
    let Some(defender) = swiftfetch_engine::WindowsDefender::detect() else {
        panic!("Windows Defender CLI not present on this machine");
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("eicar.com");
    match std::fs::write(&path, EICAR) {
        Ok(()) => {
            if !path.exists() {
                // Real-time protection silently quarantined the file.
                return;
            }
            match defender.scan(&path) {
                ScanOutcome::Flagged { .. } => {}
                ScanOutcome::Clean if !path.exists() => {
                    // Quarantined between the exists-check and the scan.
                }
                other => panic!("expected Flagged, got {other:?}"),
            }
        }
        Err(err) if err.raw_os_error() == Some(225) => {
            // Real-time protection blocked the EICAR write — Defender is
            // active and flagged the content at the filesystem level.
        }
        Err(err) => panic!("unexpected write failure: {err}"),
    }
}
