//! Shared types and constants used across `SwiftFetch` crates.

/// Human-readable product name. "`SwiftFetch`" is a working title — see
/// docs/PRD.md for the naming note (final name pending trademark clearance).
pub const APP_NAME: &str = "SwiftFetch";

/// Download job states persisted in `downloads.state`.
pub const DOWNLOAD_STATES: [&str; 9] = [
    "queued",
    "probing",
    "downloading",
    "paused",
    "verifying",
    "done",
    "error",
    "interrupted",
    "cancelled",
];

/// Segment states persisted in `segments.state`.
pub const SEGMENT_STATES: [&str; 5] = ["queued", "active", "stalled", "done", "failed"];

/// Resume-capability detection outcomes persisted in `downloads.resume_cap`
/// (`Yes` / `No` / `Unknown` in the PRD map to `ranges` / `none` / `ifrange`).
pub const RESUME_CAPS: [&str; 3] = ["none", "ranges", "ifrange"];

#[cfg(test)]
mod tests {
    #[test]
    fn state_names_are_unique_within_each_set() {
        // Job and segment states are separate state spaces and may share
        // names (e.g. `queued`, `done`); each set must be unique within itself.
        for set in [
            super::DOWNLOAD_STATES.to_vec(),
            super::SEGMENT_STATES.to_vec(),
            super::RESUME_CAPS.to_vec(),
        ] {
            let mut names = set.clone();
            names.sort_unstable();
            let total = names.len();
            names.dedup();
            assert_eq!(total, names.len(), "duplicate names in set: {set:?}");
        }
    }
}
