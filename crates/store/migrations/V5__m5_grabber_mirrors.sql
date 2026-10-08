-- SwiftFetch schema v5 (Milestone 5): site-grabber projects + mirror stats.
-- Grabber projects persist the spider config so periodic re-grabs (via the
-- M3 scheduler) can re-run them; mirrors gain outcome counters that feed
-- the engine's mirror ordering (fewest fails first, most bytes first).

CREATE TABLE grabber_projects (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    seed_url      TEXT NOT NULL,
    config_json   TEXT NOT NULL,
    queue_id      TEXT REFERENCES queues(id),
    last_run_at   TEXT,
    last_found    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL
);

ALTER TABLE mirrors ADD COLUMN bytes_ok INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mirrors ADD COLUMN last_error TEXT;
