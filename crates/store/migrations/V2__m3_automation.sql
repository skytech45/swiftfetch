-- SwiftFetch schema v2 (Milestone 3): automation plumbing.
-- The CLI process and the GUI app share one WAL database: the CLI mutates
-- only under BEGIN IMMEDIATE (single-writer discipline, system-design §5),
-- the app consumes these rows and acts through the in-process engine.

-- Downloads staged by out-of-process clients (CLI). The app probes, plans
-- and inserts them through the normal engine path (probe, dedup, journal).
CREATE TABLE staged_downloads (
    id           TEXT PRIMARY KEY,
    url          TEXT NOT NULL,
    dest_dir     TEXT,                        -- NULL = default download dir
    filename     TEXT,                        -- NULL = derive from probe
    queue_id     TEXT REFERENCES queues(id),
    start_paused INTEGER NOT NULL DEFAULT 1,  -- staged adds start paused
    source       TEXT NOT NULL DEFAULT 'cli', -- cli|clipboard|api
    consumed_at  TEXT,                        -- NULL = pending
    created_at   TEXT NOT NULL
);
CREATE INDEX idx_staged_pending ON staged_downloads(consumed_at);

-- Control commands from out-of-process clients targeting live jobs.
CREATE TABLE cli_commands (
    id          TEXT PRIMARY KEY,
    job_id      TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    action      TEXT NOT NULL,               -- pause|resume|cancel
    consumed_at TEXT,                        -- NULL = pending
    created_at  TEXT NOT NULL
);
CREATE INDEX idx_cli_cmd_pending ON cli_commands(consumed_at);
