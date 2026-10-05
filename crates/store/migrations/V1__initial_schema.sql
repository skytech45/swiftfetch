-- SwiftFetch schema v1 (Milestone 0).
-- Timestamps are UTC ISO-8601 text, ids are uuid v4 strings.

-- Categories & file organization (the extension-to-category map is data, not code)
CREATE TABLE categories (
    id               TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    extensions       TEXT NOT NULL,           -- JSON array, e.g. ["mp4","mkv"]
    folder           TEXT NOT NULL,           -- absolute default folder
    max_conns        INTEGER,                 -- NULL = inherit global default
    speed_limit_kbps INTEGER                  -- NULL = inherit global default
);

-- Named queues + scheduler (schedule_json NULL = manual, else a Schedule object)
CREATE TABLE queues (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL UNIQUE,
    max_concurrent INTEGER NOT NULL DEFAULT 2,
    schedule_json  TEXT,
    post_action    TEXT NOT NULL DEFAULT 'none', -- none|sleep|hibernate|shutdown
    is_active      INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL
);

-- One row per download job (normal, media, spider-found)
CREATE TABLE downloads (
    id               TEXT PRIMARY KEY,            -- uuid v4
    url              TEXT NOT NULL,
    final_path       TEXT NOT NULL,               -- destination incl. filename
    part_path        TEXT NOT NULL,               -- final_path + '.sfpart' while active
    total_len        INTEGER,                     -- NULL if unknown (chunked)
    done_bytes       INTEGER NOT NULL DEFAULT 0,  -- denormalized SUM(segments.done)
    resume_cap       TEXT NOT NULL DEFAULT 'none',-- none|ranges|ifrange
    etag             TEXT,
    last_modified    TEXT,
    content_type     TEXT,
    category_id      TEXT REFERENCES categories(id),
    queue_id         TEXT REFERENCES queues(id),
    state            TEXT NOT NULL DEFAULT 'queued',
        -- queued|probing|downloading|paused|verifying|done|error|interrupted|cancelled
    error_code       TEXT,                        -- machine-readable, e.g. E_RANGE_MISMATCH
    error_msg        TEXT,                        -- human sentence
    max_conns        INTEGER NOT NULL DEFAULT 8,
    speed_limit_kbps INTEGER,                     -- NULL = inherit global
    referer          TEXT,
    user_agent       TEXT,
    cookies_json     TEXT,                        -- captured request context
    job_kind         TEXT NOT NULL DEFAULT 'file',-- file|hls|dash|spider_batch
    media_meta       TEXT,                        -- JSON: variants, quality, ffmpeg plan
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX idx_dl_state ON downloads(state);
CREATE INDEX idx_dl_queue ON downloads(queue_id, state);

-- Segment journal: the crash-safety core (progress checkpointed at most once
-- per second per active segment, so a kill loses at most ~1 s of bookkeeping)
CREATE TABLE segments (
    job_id  TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    idx     INTEGER NOT NULL,
    start_o INTEGER NOT NULL, -- inclusive byte offset
    end_o   INTEGER NOT NULL, -- inclusive byte offset
    done    INTEGER NOT NULL DEFAULT 0,
    state   TEXT NOT NULL DEFAULT 'queued', -- queued|active|stalled|done|failed
    PRIMARY KEY (job_id, idx)
);
CREATE INDEX idx_seg_job_state ON segments(job_id, state);

-- Queue membership (ordered)
CREATE TABLE queue_items (
    queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
    job_id   TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    added_at TEXT NOT NULL,
    PRIMARY KEY (queue_id, job_id)
);

-- Mirrors: alternate URLs for the same file (per job, engine logic lands in M5)
CREATE TABLE mirrors (
    job_id        TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    url           TEXT NOT NULL,
    priority      INTEGER NOT NULL DEFAULT 0,
    fails         INTEGER NOT NULL DEFAULT 0, -- backoff counter
    backoff_until TEXT,
    PRIMARY KEY (job_id, url)
);

-- Stored site logins (passwords live in the OS keyring, only the ref is here)
CREATE TABLE site_logins (
    id           TEXT PRIMARY KEY,
    host         TEXT NOT NULL UNIQUE,
    username     TEXT NOT NULL,
    keyring_ref  TEXT NOT NULL, -- opaque handle into the OS credential store
    cookies_json TEXT,          -- encrypted where the OS supports it
    updated_at   TEXT NOT NULL
);

-- Settings: key/value store with JSON values
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Completed-download history (retained even when the file is deleted)
CREATE TABLE history (
    id           TEXT PRIMARY KEY,
    url          TEXT NOT NULL,
    final_path   TEXT NOT NULL,
    size_bytes   INTEGER,
    category_id  TEXT REFERENCES categories(id),
    mime_type    TEXT,
    completed_at TEXT NOT NULL,
    file_deleted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_history_completed ON history(completed_at);
