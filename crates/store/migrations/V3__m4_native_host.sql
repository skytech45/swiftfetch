-- SwiftFetch schema v3 (Milestone 4): native-messaging host plumbing.
-- Staged rows gain the captured request context (cookies/referer are the
-- browser's own values, forwarded per §7 of the design contract) and the
-- app writes back the created job id so the host can push events.

ALTER TABLE staged_downloads ADD COLUMN cookies TEXT;
ALTER TABLE staged_downloads ADD COLUMN referer TEXT;
ALTER TABLE staged_downloads ADD COLUMN job_id TEXT;
