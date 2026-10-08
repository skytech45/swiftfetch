#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "tests: setup may panic; sizes are bounded"
)]

//! Milestone 6 — `BitTorrent` acceptance (Build Prompt §14.1).
//!
//! A hermetic loopback swarm: session A seeds synthetic content over
//! 127.0.0.1, session B downloads it via `initial_peers` (no `DHT`, no
//! trackers, no internet). Asserts byte-identical completion, pause/resume
//! and the seeding-ratio policy. A public-swarm check (magnet + `.torrent`
//! against a legal torrent) is documented in the M6 report as a manual
//! local-only procedure.

use std::time::Duration;

use swiftfetch_torrent::{TorrentEngine, seeding_complete};

fn test_data() -> Vec<u8> {
    // ~3 pieces of non-trivial bytes (piece length is 16 KiB in the builder).
    (0..40_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 8) as u8)
        .collect()
}

fn test_torrent_bytes(name: &str, data: &[u8]) -> Vec<u8> {
    // Reuse the builder from the metainfo unit tests via a local copy of
    // the same construction (kept in sync by the parse test).
    use sha1::Digest as _;
    let piece_length: usize = 16384;
    let mut pieces = Vec::new();
    for chunk in data.chunks(piece_length) {
        let mut h = sha1::Sha1::new();
        h.update(chunk);
        pieces.extend_from_slice(h.finalize().as_slice());
    }
    let mut info = std::collections::BTreeMap::new();
    info.insert(
        b"length".to_vec(),
        swiftfetch_torrent::bencode::Value::Int(data.len() as i64),
    );
    info.insert(
        b"name".to_vec(),
        swiftfetch_torrent::bencode::Value::Bytes(name.as_bytes().to_vec()),
    );
    info.insert(
        b"piece length".to_vec(),
        swiftfetch_torrent::bencode::Value::Int(piece_length as i64),
    );
    info.insert(
        b"pieces".to_vec(),
        swiftfetch_torrent::bencode::Value::Bytes(pieces),
    );
    let mut root = std::collections::BTreeMap::new();
    root.insert(
        b"info".to_vec(),
        swiftfetch_torrent::bencode::Value::Dict(info),
    );
    let mut out = Vec::new();
    swiftfetch_torrent::bencode::encode(&swiftfetch_torrent::bencode::Value::Dict(root), &mut out);
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_swarm_downloads_to_completion() {
    let data = test_data();
    let torrent = test_torrent_bytes("payload.bin", &data);

    let seed_dir = tempfile::tempdir().expect("seed dir");
    let dl_dir = tempfile::tempdir().expect("download dir");
    // The seeder serves the complete file from disk.
    std::fs::write(seed_dir.path().join("payload.bin"), &data).expect("seed file");

    let seeder = TorrentEngine::open(
        seed_dir.path(),
        false,
        true,
        Some("127.0.0.1:0".parse().expect("loopback addr")),
    )
    .await
    .expect("seeder opens");
    let seed_addr = seeder.listen_addr().expect("seeder listens");
    seeder
        .add_torrent_bytes(&torrent, None, Vec::new())
        .await
        .expect("seed added");

    let leecher = TorrentEngine::open(dl_dir.path(), false, true, None)
        .await
        .expect("leecher opens");
    let id = leecher
        .add_torrent_bytes(&torrent, None, vec![seed_addr])
        .await
        .expect("download added");

    // Pause/resume round-trip before completion.
    leecher.pause(id).await.expect("pause works");
    let paused = leecher.status(id).expect("status works");
    assert!(paused.paused, "status must report paused");
    leecher.resume(id).await.expect("resume works");

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let status = leecher.status(id).expect("status works");
        if status.finished && status.progress_bytes == data.len() as u64 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "download did not complete in time: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let got = std::fs::read(dl_dir.path().join("payload.bin")).expect("output readable");
    assert_eq!(got, data, "downloaded bytes must match the seed");

    // Seeding-ratio policy: 1.0 not yet reached with nothing uploaded.
    let status = leecher.status(id).expect("status works");
    assert!(
        !seeding_complete(status.uploaded_bytes, status.progress_bytes, 1.0)
            || status.uploaded_bytes >= status.progress_bytes,
        "ratio policy must hold"
    );
    assert!(
        seeding_complete(100, 100, 1.0),
        "1.0 ratio reached at parity"
    );
    assert!(
        !seeding_complete(50, 100, 1.0),
        "1.0 ratio not reached below parity"
    );
    assert!(
        !seeding_complete(0, 0, 1.0),
        "empty torrent never satisfies"
    );

    leecher.remove(id, true).await.expect("remove works");
    assert!(
        leecher.status(id).is_err(),
        "removed torrent must be unknown"
    );
}
