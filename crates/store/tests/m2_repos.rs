//! M2 store-repository tests: category assignment, queue ordering and
//! move semantics, settings persistence, delete cascades.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

use swiftfetch_store::Store;
use swiftfetch_store::repos;

fn temp_store(name: &str) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join(format!("{name}.db"))).expect("store");
    (dir, store)
}

fn seeded() -> (tempfile::TempDir, Store) {
    let (dir, store) = temp_store("m2");
    let base = dir.path().join("downloads");
    repos::seed_default_categories(&store, &base).expect("seed");
    (dir, store)
}

#[test]
fn categories_seed_once_and_assign_by_extension() {
    let (_dir, store) = seeded();

    // Seeding twice is a no-op.
    repos::seed_default_categories(&store, std::path::Path::new("/x")).expect("re-seed");
    let cats = repos::list_categories(&store).expect("list");
    assert_eq!(cats.len(), 7, "6 defaults + other");

    // Data-driven extension map: video → mp4, documents → pdf, other fallback.
    assert_eq!(
        repos::categorize(&cats, "movie.mp4").as_deref(),
        Some("video")
    );
    assert_eq!(
        repos::categorize(&cats, "PAPER.PDF").as_deref(),
        Some("documents"),
        "extension match is case-insensitive"
    );
    assert_eq!(
        repos::categorize(&cats, "unknown.xyz").as_deref(),
        Some("other")
    );
}

#[test]
fn queue_order_and_move_are_clamped() {
    let (_dir, store) = seeded();
    repos::create_queue(&store, "q1", "Night", 2).expect("queue");
    for i in 0..5 {
        repos::insert_download(
            &store,
            &format!("j{i}"),
            "https://x/f.zip",
            &format!("C:\\f{i}.zip"),
            None,
            Some("q1"),
            8,
        )
        .expect("insert");
        repos::enqueue(&store, "q1", &format!("j{i}")).expect("enqueue");
    }

    let order = repos::queue_order(&store, "q1");
    assert_eq!(order, vec!["j0", "j1", "j2", "j3", "j4"]);

    // Move j3 up two → [j0, j3, j1, j2, j4]
    repos::move_queue_item(&store, "q1", "j3", -2).expect("move");
    assert_eq!(
        repos::queue_order(&store, "q1"),
        vec!["j0", "j3", "j1", "j2", "j4"]
    );

    // Move clamps at the end.
    repos::move_queue_item(&store, "q1", "j3", 99).expect("move");
    assert_eq!(
        repos::queue_order(&store, "q1"),
        vec!["j0", "j1", "j2", "j4", "j3"]
    );

    // Dequeue removes membership but keeps the download row.
    repos::dequeue(&store, "j0").expect("dequeue");
    assert_eq!(repos::queue_order(&store, "q1").len(), 4);
    assert!(repos::get_download(&store, "j0").expect("get").is_some());
}

#[test]
fn delete_download_cascades_segments_and_queue_membership() {
    let (_dir, store) = seeded();
    repos::create_queue(&store, "q1", "Main", 2).expect("queue");
    repos::insert_download(
        &store,
        "j1",
        "https://x/f.zip",
        "C:\\f.zip",
        Some("video"),
        Some("q1"),
        8,
    )
    .expect("insert");
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO segments (job_id, idx, start_o, end_o, done, state) \
                 VALUES ('j1', 0, 0, 1023, 512, 'active')",
                (),
            )
        })
        .expect("segment");

    repos::delete_download(&store, "j1").expect("delete");
    let segments = repos::list_segments(&store, "j1").expect("segments");
    assert!(segments.is_empty(), "segments cascade");
    assert!(
        repos::queue_order(&store, "q1").is_empty(),
        "membership cascades"
    );
}

#[test]
fn settings_roundtrip() {
    let (_dir, store) = temp_store("settings");
    assert_eq!(repos::get_setting(&store, "ui.theme").expect("get"), None);
    repos::set_setting(&store, "ui.theme", "\"dark\"").expect("set");
    repos::set_setting(&store, "ui.theme", "\"light\"").expect("overwrite");
    assert_eq!(
        repos::get_setting(&store, "ui.theme").expect("get"),
        Some("\"light\"".to_owned())
    );
}
