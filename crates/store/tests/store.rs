//! Integration tests: WAL mode, schema completeness, migration
//! idempotence and foreign-key enforcement.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

use swiftfetch_store::Store;

const EXPECTED_TABLES: [&str; 13] = [
    "categories",
    "cli_commands",
    "downloads",
    "grabber_projects",
    "history",
    "mirrors",
    "queue_items",
    "queues",
    "segments",
    "settings",
    "site_logins",
    "staged_downloads",
    "torrents",
];

fn temp_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let store = Store::open(&dir.path().join("swiftfetch.db")).expect("open store");
    (dir, store)
}

#[test]
fn opens_in_wal_mode_with_full_schema() {
    let (_dir, store) = temp_store();
    assert_eq!(store.journal_mode().expect("journal mode"), "wal");

    let tables = store.table_names().expect("table names");
    for expected in EXPECTED_TABLES {
        assert!(
            tables.iter().any(|name| name == expected),
            "missing table `{expected}` (found {tables:?})"
        );
    }
}

#[test]
fn reopens_cleanly_with_migrations_applied_once() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("swiftfetch.db");

    drop(Store::open(&path).expect("first open"));
    let store = Store::open(&path).expect("second open");

    let tables = store.table_names().expect("table names");
    assert!(
        tables.iter().any(|name| name == "refinery_schema_history"),
        "refinery bookkeeping table missing (found {tables:?})"
    );

    let applied = store
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM refinery_schema_history", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .expect("count applied migrations");
    assert_eq!(applied, 6, "schema v1..v6 must be applied exactly once");
}

#[test]
fn foreign_keys_are_enforced() {
    let (_dir, store) = temp_store();
    let err = store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO downloads (id, url, final_path, part_path, created_at, updated_at, \
                 category_id)
                 VALUES ('j1', 'https://example.com/f.zip', 'C:\\f.zip', 'C:\\f.zip.sfpart', \
                 '2026-10-05T00:00:00Z', '2026-10-05T00:00:00Z', 'missing-category')",
                (),
            )
        })
        .expect_err("insert with unknown category must violate the foreign key");
    assert!(
        err.to_string().to_lowercase().contains("foreign key"),
        "unexpected error: {err}"
    );
}

#[test]
fn inserts_valid_download_and_segment_rows() {
    let (_dir, store) = temp_store();

    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO categories (id, name, extensions, folder)
                 VALUES ('video', 'Video', '[\"mp4\",\"mkv\"]', 'C:\\Downloads\\Video')",
                (),
            )?;
            conn.execute(
                "INSERT INTO downloads (id, url, final_path, part_path, category_id, created_at, \
                 updated_at)
                 VALUES ('j1', 'https://example.com/f.zip', 'C:\\Downloads\\Video\\f.zip', \
                 'C:\\Downloads\\Video\\f.zip.sfpart', 'video', '2026-10-05T00:00:00Z', \
                 '2026-10-05T00:00:00Z')",
                (),
            )?;
            conn.execute(
                "INSERT INTO segments (job_id, idx, start_o, end_o, done, state)
                 VALUES ('j1', 0, 0, 1048575, 524288, 'active')",
                (),
            )
        })
        .expect("valid inserts");

    let segments = store
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM segments", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .expect("count segments");
    assert_eq!(segments, 1);
}
