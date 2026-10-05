//! Test harness binary (not shipped to end users): downloads a URL with the
//! engine against a given database and either completes (printing the
//! SHA-256) or hard-exits with code 9 after N confirmed bytes — a `kill -9`
//! equivalent used by the crash-recovery tests.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test harness binary

use std::path::PathBuf;

use swiftfetch_engine::{Engine, EngineConfig, JobSpec};

struct Args {
    db: PathBuf,
    url: String,
    dest_dir: PathBuf,
    crash_after: Option<u64>,
    max_conns: u8,
}

fn parse_args() -> Args {
    let mut db = None;
    let mut url = None;
    let mut dest_dir = None;
    let mut crash_after = None;
    let mut max_conns = 8u8;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--db" => db = iter.next().map(PathBuf::from),
            "--url" => url = iter.next(),
            "--dest-dir" => dest_dir = iter.next().map(PathBuf::from),
            "--crash-after" => crash_after = iter.next().and_then(|v| v.parse().ok()),
            "--max-conns" => max_conns = iter.next().and_then(|v| v.parse().ok()).unwrap_or(8),
            other => panic!("unknown argument {other}"),
        }
    }
    Args {
        db: db.expect("--db required"),
        url: url.expect("--url required"),
        dest_dir: dest_dir.expect("--dest-dir required"),
        crash_after,
        max_conns,
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::process::ExitCode {
    let args = parse_args();
    let store = swiftfetch_store::Store::open(&args.db).expect("open store");
    let config = EngineConfig {
        debug_crash_after_bytes: args.crash_after,
        ..EngineConfig::default()
    };
    let engine = match Engine::open(config, store) {
        Ok(engine) => engine,
        Err(err) => {
            eprintln!("engine open failed: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // If an interrupted job exists, resume it instead of starting fresh.
    let interrupted = engine
        .job_ids_in_state(swiftfetch_engine::JobState::Interrupted)
        .expect("list interrupted");
    let job_id = if let Some(id) = interrupted.into_iter().next() {
        eprintln!("resuming interrupted job {id}");
        let events = match engine.resume(&id) {
            Ok(events) => events,
            Err(err) => {
                eprintln!("resume failed: {err}");
                return std::process::ExitCode::FAILURE;
            }
        };
        (id, events)
    } else {
        let mut spec = JobSpec::new(args.url.clone(), args.dest_dir.clone());
        spec.max_conns = args.max_conns;
        match engine.start_job(spec).await {
            Ok((id, events)) => (id, events),
            Err(err) => {
                eprintln!("start failed: {err}");
                return std::process::ExitCode::FAILURE;
            }
        }
    };

    let (job_id, mut events) = job_id;
    loop {
        match events.recv().await {
            Ok(swiftfetch_engine::JobEvent::Completed { path }) => {
                let digest =
                    swiftfetch_engine::checksum::sha256_file(&path).expect("hash completed file");
                println!(
                    "{{\"id\":\"{job_id}\",\"path\":\"{}\",\"sha256\":\"{}\"}}",
                    path.display(),
                    swiftfetch_engine::checksum::to_hex(digest.bytes()),
                );
                return std::process::ExitCode::SUCCESS;
            }
            Ok(swiftfetch_engine::JobEvent::Failed { code, message }) => {
                eprintln!("job failed: {code}: {message}");
                return std::process::ExitCode::FAILURE;
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                eprintln!("event channel closed");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
}
