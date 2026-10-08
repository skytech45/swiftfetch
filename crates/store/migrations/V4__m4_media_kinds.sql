-- SwiftFetch schema v4 (Milestone 4): media job staging.
-- Extension captures declare their pipeline kind (file|hls|dash|youtube)
-- and carry structured meta (e.g. the picked YouTube quality).

ALTER TABLE staged_downloads ADD COLUMN kind TEXT NOT NULL DEFAULT 'file';
ALTER TABLE staged_downloads ADD COLUMN meta_json TEXT;
