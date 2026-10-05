//! `SwiftFetch` download engine: HTTP(S) probing, dynamic multi-connection
//! segmentation, crash-safe resume through the SQLite journal, token-bucket
//! speed limiting and digest verification.
//!
//! Design contract: docs/system-design.md §4.2, §8–§10. The engine requires
//! a multi-threaded tokio runtime (the disk writer uses
//! `tokio::task::block_in_place` for positioned writes).
#![forbid(unsafe_code)]

pub mod checksum;
pub mod connection;
pub mod disk;
pub mod engine;
pub mod errors;
pub mod journal;
pub mod limiter;
pub mod segment;
pub mod segmenter;
mod supervisor;

pub use engine::{
    Engine, EngineConfig, JobEvent, JobSnapshot, JobSpec, JobState, SegmentSnapshot, UrlRefresher,
};
pub use errors::EngineError;
pub use journal::{JobRow, ProbeUpdate, SegmentRow};
pub use segmenter::MIN_SEGMENT_BYTES;
