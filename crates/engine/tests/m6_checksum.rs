#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: setup may panic"
)]

//! Milestone 6 — checksum verification acceptance (Build Prompt §14.2):
//! matching hashes verify, mismatches quarantine to `.badhash`, sidecars
//! are detected, and garbage expectations fail closed (no false verified).

use std::io::Write as _;

use swiftfetch_engine::checksum::{
    find_sidecar_hex, quarantine_badhash, sha256_file, to_hex, verify_expected,
};

fn write_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

#[test]
fn matching_hash_verifies_mismatch_quarantines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"swiftfetch checksum fixture";
    let path = write_file(&dir, "fixture.bin", bytes);

    let digest = sha256_file(&path).expect("hash works");
    let hex = to_hex(digest.bytes());
    assert!(verify_expected(&path, &hex).expect("verify works"));
    // Case-insensitive match also verifies.
    assert!(verify_expected(&path, &hex.to_ascii_uppercase()).expect("verify works"));

    assert!(!verify_expected(&path, &"0".repeat(64)).expect("verify works"));
    // Garbage expectations fail closed (false, never an error, never true).
    for bad in ["", "xyz", "0".repeat(63).as_str(), "0".repeat(65).as_str()] {
        assert!(!verify_expected(&path, bad).expect("verify works"));
    }

    let quarantined = quarantine_badhash(&path).expect("quarantine works");
    assert!(!path.exists(), "original must be gone");
    assert!(quarantined.exists(), "quarantined file must exist");
    assert_eq!(
        std::fs::read(&quarantined).expect("read back"),
        bytes,
        "quarantined bytes must be intact for re-download comparison"
    );
}

#[test]
fn sidecar_detected_and_honored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"sidecar fixture";
    let path = write_file(&dir, "movie.mp4", bytes);
    let digest = sha256_file(&path).expect("hash works");
    let hex = to_hex(digest.bytes());

    // Coreutils format: "<hex>  <filename>".
    let mut sidecar = std::fs::File::create(dir.path().join("movie.mp4.sha256")).expect("sidecar");
    writeln!(sidecar, "{hex}  movie.mp4").expect("write sidecar");

    let found = find_sidecar_hex(&path).expect("sidecar must be found");
    assert_eq!(found, hex);
    assert!(verify_expected(&path, &found).expect("verify works"));
}

#[test]
fn no_sidecar_yields_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_file(&dir, "plain.bin", b"data");
    assert_eq!(find_sidecar_hex(&path), None);
}
