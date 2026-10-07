//! M2 integration: the add-URL → queue → download → rehydrate flow through
//! the engine + repos, mirroring the Tauri command logic 1:1 (the commands
//! are thin wrappers around exactly these calls).

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

use std::time::Duration;

use swiftfetch_engine::{Engine, EngineConfig, JobEvent, JobSpec, JobState};
use swiftfetch_store::Store;
use swiftfetch_store::repos;
use swiftfetch_test_server::{Route, TestServer};

fn init_log() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("swiftfetch=debug"))
            .init();
    });
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        min_segment: 512 * 1024,
        tick: Duration::from_millis(250),
        ..EngineConfig::default()
    }
}

/// Mirrors the `add_url` command: start (or park) a job, assign the
/// extension-mapped category, optionally enqueue.
async fn add_url(
    engine: &Engine,
    store: &Store,
    dest_dir: &std::path::Path,
    url: &str,
    start_now: bool,
    queue_id: Option<&str>,
) -> String {
    let mut spec = JobSpec::new(url, dest_dir.to_path_buf());
    spec.max_conns = 8;
    spec.start_paused = !start_now;
    let (id, _rx) = engine.start_job(spec).await.expect("start_job");
    let cats = repos::list_categories(store).expect("categories");
    let filename = url.rsplit('/').next().expect("path").to_owned();
    let category = repos::categorize(&cats, &filename);
    if let Some(cat) = category {
        repos::set_category(store, &id, Some(&cat)).expect("set category");
    }
    if let Some(q) = queue_id {
        repos::enqueue(store, q, &id).expect("enqueue");
    }
    id
}

/// Mirrors one queue-runner tick: start the next queued job of each active
/// queue while under its concurrency limit.
async fn queue_tick(engine: &Engine, store: &Store) -> usize {
    let queues = repos::list_queues(store).expect("queues");
    let mut started = 0usize;
    for queue in queues {
        if queue.is_active == 0 {
            continue;
        }
        let members = repos::queue_order(store, &queue.id);
        let mut in_flight = 0u32;
        let mut next: Option<String> = None;
        for job in &members {
            let Some(row) = repos::get_download(store, job).ok().flatten() else {
                continue;
            };
            match row.state.as_str() {
                "downloading" | "probing" | "verifying" => in_flight += 1,
                "queued" | "paused" | "interrupted" if next.is_none() => next = Some(job.clone()),
                _ => {}
            }
        }
        let limit = u32::try_from(queue.max_concurrent.clamp(1, 5)).unwrap_or(2);
        while in_flight < limit {
            let Some(job) = next.take() else {
                break;
            };
            if engine.resume(&job).is_ok() {
                started += 1;
                in_flight += 1;
            } else {
                break;
            }
            if in_flight < limit {
                next = members
                    .iter()
                    .find(|id| {
                        id.as_str() != job
                            && repos::get_download(store, id)
                                .ok()
                                .flatten()
                                .is_some_and(|r| {
                                    matches!(r.state.as_str(), "queued" | "paused" | "interrupted")
                                })
                    })
                    .cloned();
            }
        }
    }
    started
}

#[tokio::test(flavor = "multi_thread")]
async fn add_download_complete_and_rehydrate() {
    let len = 4 * 1024 * 1024;
    let data = vec![0x5Au8; len];
    let server = TestServer::start().await.expect("server");
    server.set_route("/file.zip", Route::new(data.clone()));
    let url = server.url("/file.zip");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("db.sqlite")).expect("store");
    let repos_store = Store::open(&dir.path().join("db.sqlite")).expect("repos store");
    repos::seed_default_categories(&repos_store, dir.path()).expect("seed");
    repos::create_queue(&repos_store, "main", "Main", 2).expect("queue");
    let engine = Engine::open(engine_config(), store).expect("engine");

    // 1. Add (start now) — lands in Video via the extension map.
    let id = add_url(&engine, &repos_store, dir.path(), &url, true, None).await;
    let row = repos::get_download(&repos_store, &id)
        .expect("row")
        .expect("some");
    assert_eq!(
        row.category_id.as_deref(),
        Some("compressed"),
        "auto-categorized"
    );
    assert_eq!(row.state, "downloading");

    // 2. Live progress via the engine subscription (what the UI receives).
    let mut rx = engine.subscribe(&id).expect("subscribe");
    let mut saw_progress = false;
    let path = loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("event")
            .expect("open");
        match ev {
            JobEvent::Progress { done, .. } if done > 0 => saw_progress = true,
            JobEvent::Completed { path } => break path,
            JobEvent::Failed { code, message } => panic!("failed: {code}: {message}"),
            _ => {}
        }
    };
    assert!(saw_progress, "live progress events reached the subscriber");
    assert!(path.exists(), "completed file exists");
    assert_eq!(std::fs::metadata(&path).expect("meta").len(), len as u64);

    // 3. DB row reflects completion (state rehydrated from SQLite).
    let row = repos::get_download(&repos_store, &id)
        .expect("row")
        .expect("some");
    assert_eq!(row.state, "done");
    assert_eq!(row.done_bytes, len as i64);

    // 4. Pause → app "restart" (new Engine over the same DB) → resume.
    let id2 = add_url(&engine, &repos_store, dir.path(), &url, true, None).await;
    engine.pause(&id2).expect("pause");
    // Wait for the paused state to land.
    for _ in 0..40 {
        let row = repos::get_download(&repos_store, &id2)
            .expect("row")
            .expect("some");
        if row.state == "paused" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(engine);
    let store2 = Store::open(&dir.path().join("db.sqlite")).expect("relaunch store");
    let engine2 = Engine::open(engine_config(), store2).expect("relaunch");
    let paused = engine2.job_ids_in_state(JobState::Paused).expect("list");
    assert!(paused.contains(&id2), "paused job rehydrated from SQLite");
    let mut rx = engine2.resume(&id2).expect("resume");
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("event")
            .expect("open");
        match ev {
            JobEvent::Completed { path } => {
                assert!(path.exists());
                break;
            }
            JobEvent::Failed { code, message } => panic!("failed: {code}: {message}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_runner_respects_concurrency_and_order() {
    init_log();
    let len = 512 * 1024;
    let data = vec![0x33u8; len];
    let server = TestServer::start().await.expect("server");
    server.set_route(
        "/file.zip",
        Route::new(data.clone()).throttle_bps(1024 * 1024),
    );
    let url = server.url("/file.zip");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("db.sqlite")).expect("store");
    let repos_store = Store::open(&dir.path().join("db.sqlite")).expect("repos store");
    repos::seed_default_categories(&repos_store, dir.path()).expect("seed");
    repos::create_queue(&repos_store, "main", "Main", 2).expect("queue");
    let engine = Engine::open(engine_config(), store).expect("engine");

    // Queue 5 downloads, none started.
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(add_url(&engine, &repos_store, dir.path(), &url, false, Some("main")).await);
    }
    repos::set_queue_active(&repos_store, "main", true).expect("activate");

    // First tick starts exactly 2 (the concurrency limit).
    let started = queue_tick(&engine, &repos_store).await;
    assert_eq!(
        started, 2,
        "first tick starts exactly `max_concurrent` jobs"
    );

    // Wait for both to finish; each completion frees a slot.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let _ = queue_tick(&engine, &repos_store).await;
        let done = ids
            .iter()
            .filter(|id| {
                repos::get_download(&repos_store, id)
                    .expect("row")
                    .expect("some")
                    .state
                    == "done"
            })
            .count();
        if done == 5 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queue did not drain in time"
        );
        // Invariant while draining: never more than 2 in flight.
        let in_flight = ids
            .iter()
            .filter(|id| {
                repos::get_download(&repos_store, id)
                    .expect("row")
                    .expect("some")
                    .state
                    == "downloading"
            })
            .count();
        assert!(in_flight <= 2, "concurrency limit violated ({in_flight})");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        repos::queue_order(&repos_store, "main").len(),
        5,
        "order preserved"
    );
}
