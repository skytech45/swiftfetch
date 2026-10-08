-- SwiftFetch schema v6 (Milestone 6): checksum expectations + torrents.
-- `expected_sha256` holds the user-pasted or sidecar-detected hash a
-- download must match; `checksum_state` records the outcome
-- (unverified|verified|failed). Torrents live in their own table and join
-- queues as items like normal downloads.

ALTER TABLE downloads ADD COLUMN expected_sha256 TEXT;
ALTER TABLE downloads ADD COLUMN checksum_state TEXT NOT NULL DEFAULT 'unverified';

CREATE TABLE torrents (
    id            TEXT PRIMARY KEY,
    info_hash     TEXT NOT NULL UNIQUE,
    magnet        TEXT,
    torrent_file  BLOB,
    name          TEXT NOT NULL,
    output_dir    TEXT NOT NULL,
    queue_id      TEXT REFERENCES queues(id),
    state         TEXT NOT NULL DEFAULT 'downloading',
        -- downloading|paused|seeding|done|error
    seed_ratio    REAL NOT NULL DEFAULT 1.0,
    error_msg     TEXT,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_torrents_state ON torrents(state);
