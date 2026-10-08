//! M3 integration: the CLI round trip. The CLI writes into the shared
//! database (staged downloads + control commands, exactly what
//! `crates/cli` does); the bridge logic mirrored here 1:1 consumes those
//! rows and drives the live engine. The final read-back is what the CLI's
//! `list`/`status` commands show.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

use std::time::Duration;

use swiftfetch_engine::{Engine, EngineConfig, JobEvent, JobSpec};
use swiftfetch_store::Store;
use swiftfetch_store::repos;
use swiftfetch_test_server::{Route, TestServer};

fn engine_config() -> EngineConfig {
    EngineConfig {
        min_segment: 512 * 1024,
        tick: Duration::from_millis(250),
        ..EngineConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_stage_add_command_round_trip() {
    let len = 2 * 1024 * 1024;
    let server = TestServer::start().await.expect("server");
    server.set_route("/cli.zip", Route::new(vec![0x21u8; len]));
    let url = server.url("/cli.zip");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("db.sqlite")).expect("store");
    let repos_store = Store::open(&dir.path().join("db.sqlite")).expect("repos store");
    repos::seed_default_categories(&repos_store, dir.path()).expect("seed");
    repos::create_queue(&repos_store, "main", "Main", 2).expect("queue");
    let engine = Engine::open(engine_config(), store).expect("engine");

    // ── 1. CLI side: stage a download (what `swiftfetch add` does).
    let staged_id = repos::stage_download(
        &repos_store,
        &repos::StageRequest {
            url,
            dest_dir: Some(dir.path().to_path_buf()),
            queue_id: Some("main".to_owned()),
            start_paused: true,
            source: "cli".to_owned(),
            kind: "file".to_owned(),
            ..repos::StageRequest::default()
        },
    )
    .expect("stage");

    // ── 2. Bridge side: consume the staged row and start the job.
    let staged = repos::take_staged_downloads(&repos_store).expect("take");
    assert_eq!(staged.len(), 1, "the staged row is claimed exactly once");
    let item = &staged[0];
    assert_eq!(item.id, staged_id);
    assert_eq!(item.source, "cli");

    let mut spec = JobSpec::new(&item.url, item.dest_dir.clone().unwrap());
    spec.start_paused = true;
    spec.max_conns = 8;
    let (id, rx) = engine.start_job(spec).await.expect("start_job");
    // Category like the add_url command, then the queue membership.
    let cats = repos::list_categories(&repos_store).expect("categories");
    let category = repos::categorize(&cats, "cli.zip");
    assert_eq!(category.as_deref(), Some("compressed"), "auto-categorized");
    repos::set_category(&repos_store, &id, category.as_deref()).expect("set category");
    repos::enqueue(&repos_store, "main", &id).expect("enqueue");

    // Row is visible to a CLI `list` immediately (paused, queued work).
    let row = repos::get_download(&repos_store, &id)
        .expect("row")
        .expect("some");
    assert_eq!(row.state, "paused");

    // ── 3. CLI side: `swiftfetch resume <id>` writes a control command.
    repos::enqueue_command(&repos_store, &id, "resume").expect("enqueue command");
    let commands = repos::take_pending_commands(&repos_store).expect("take commands");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].action, "resume");

    // Bridge applies the command through the engine.
    let mut rx2 = engine.resume(&id).expect("resume");
    drop(rx);

    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), rx2.recv())
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

    // ── 4. CLI read-back: a fresh `list` sees the completion.
    let rows = repos::list_downloads(&repos_store).expect("list");
    let done = rows.iter().find(|r| r.id == id).expect("row present");
    assert_eq!(done.state, "done");
    assert_eq!(done.done_bytes, len as i64);

    // Consumed rows stay consumed (no double-processing on later ticks).
    assert!(
        repos::take_staged_downloads(&repos_store)
            .expect("take again")
            .is_empty()
    );
    assert!(
        repos::take_pending_commands(&repos_store)
            .expect("commands again")
            .is_empty()
    );
}
