#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    reason = "tests: setup may panic; test data sizes are bounded"
)]

//! Milestone 5 — mirror failover acceptance (Build Prompt §13.2): the
//! primary URL fails mid-download, the engine switches to a mirror through
//! the URL-refresh path and completes with the correct hash; per-mirror
//! stats are recorded in the store.

use std::sync::Arc;
use std::time::{Duration, Instant};

use swiftfetch_engine::{
    Engine, EngineConfig, JobEvent, JobSpec, MirrorCandidate, MirrorRefresher,
};
use swiftfetch_store::Store;
use swiftfetch_store::repos;
use swiftfetch_test_server::{Route, TestServer};

fn make_data(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    let mut state: u64 = 0x1234_5678_9abc_def1;
    for chunk in data.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        for (i, b) in chunk.iter_mut().enumerate() {
            *b = (state >> (i * 8)) as u8;
        }
    }
    data
}

async fn await_settled(
    rx: &mut tokio::sync::broadcast::Receiver<JobEvent>,
    timeout: Duration,
) -> std::path::PathBuf {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("event in time")
            .expect("channel open");
        match event {
            JobEvent::Completed { path } => return path,
            JobEvent::Failed { code, message } => panic!("job failed: {code}: {message}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mirror_failover_completes_with_correct_hash() {
    let data = make_data(2 * 1024 * 1024);
    let expected = {
        use swiftfetch_engine::checksum::{StreamHasher, to_hex};
        let mut h = StreamHasher::default();
        h.update(&data);
        let (sha, _) = h.finalize();
        to_hex(sha.bytes())
    };

    let server = TestServer::start().await.expect("server starts");
    // Primary: serves headers + 64 KiB then drops every connection —
    // the download cannot complete on it.
    server.set_route(
        "/primary.bin",
        Route::new(data.clone()).resets_after_gets(0, 64 * 1024),
    );
    // Mirror: healthy full copy.
    server.set_route("/mirror.bin", Route::new(data.clone()));

    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("dl");
    std::fs::create_dir_all(&dest).expect("dest dir");
    let engine = Engine::open(
        EngineConfig {
            retry_backoff: Duration::from_millis(50),
            ..EngineConfig::default()
        },
        Store::open(&dir.path().join("m5.db")).expect("store"),
    )
    .expect("engine opens");

    let refresher = Arc::new(MirrorRefresher::new(&[MirrorCandidate::new(
        server.url("/mirror.bin"),
        0,
    )]));
    let mut spec = JobSpec::new(server.url("/primary.bin"), dest);
    spec.max_conns = 2;
    spec.url_refresher = Some(refresher);

    let (_id, mut rx) = engine.start_job(spec).await.expect("job starts");
    let path = await_settled(&mut rx, Duration::from_secs(60)).await;

    let bytes = std::fs::read(&path).expect("output readable");
    assert_eq!(bytes.len(), data.len(), "mirror must deliver full file");
    {
        use swiftfetch_engine::checksum::{StreamHasher, to_hex};
        let mut h = StreamHasher::default();
        h.update(&bytes);
        let (sha, _) = h.finalize();
        assert_eq!(to_hex(sha.bytes()), expected, "hash must match mirror");
    }
}

#[tokio::test]
async fn mirror_stats_recorded_in_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("m5stats.db")).expect("store");
    let job_id = "job-m5-mirrors";
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO downloads (id, url, final_path, part_path, created_at, updated_at) \
                 VALUES (?1, 'http://example.com/a', '/tmp/a', '/tmp/a.sfpart', \
                 strftime('%Y-%m-%dT%H:%M:%SZ','now'), strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
                rusqlite::params![job_id],
            )?;
            Ok(())
        })
        .expect("seed job");

    repos::add_mirror(&store, job_id, "http://mirror1/file", 1).expect("add m1");
    repos::add_mirror(&store, job_id, "http://mirror0/file", 0).expect("add m0");
    // mirror0 fails once, mirror1 delivers bytes.
    repos::record_mirror_result(&store, job_id, "http://mirror0/file", 0, Some("reset"))
        .expect("record fail");
    repos::record_mirror_result(&store, job_id, "http://mirror1/file", 1024, None)
        .expect("record ok");

    let mirrors = repos::list_mirrors(&store, job_id).expect("list");
    assert_eq!(mirrors.len(), 2);
    // Priority 0 first despite its failure (ordering is priority-first).
    assert_eq!(mirrors[0].url, "http://mirror0/file");
    assert_eq!(mirrors[0].fails, 1);
    assert_eq!(mirrors[1].bytes_ok, 1024);
}
