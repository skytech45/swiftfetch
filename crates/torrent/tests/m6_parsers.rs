#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: setup may panic"
)]

//! Milestone 6 — parser hardening corpus (Build Prompt §14.4).
//!
//! Nightly `cargo fuzz` runs these grammars for an hour each; this test is
//! the checked-in deterministic corpus: every input must parse or fail
//! cleanly — panics are the only failure mode. Covers bencode, magnets,
//! metainfo and the grabber's link extractor + extension splitter.

use swiftfetch_torrent::bencode::{Value, encode, parse};
use swiftfetch_torrent::{parse_magnet, parse_torrent};

#[test]
fn fuzz_corpus_never_panics() {
    let mut corpus: Vec<Vec<u8>> = vec![
        vec![],
        vec![0],
        b"d".to_vec(),
        b"i0e".to_vec(),
        b"0:".to_vec(),
        b"1:a".to_vec(),
        b"le".to_vec(),
        b"de".to_vec(),
        b"i-0e".to_vec(),
        b"i00e".to_vec(),
        b"i9223372036854775808e".to_vec(),
        b"100:".to_vec(),
        b"l".to_vec(),
        b"d3:foo".to_vec(),
        b"malformed bytes \xff\xfe".to_vec(),
    ];
    // Deeply nested + very long inputs.
    corpus.push(vec![b'l'; 200]);
    corpus.push(vec![b'd'; 200]);
    corpus.push(b"i".to_vec().into_iter().chain(b"1".repeat(100)).collect());
    corpus.push(b"999999:".to_vec());
    // A valid torrent, mutated byte-by-byte (first 256 positions).
    let mut valid = Vec::new();
    encode(
        &Value::Dict(
            [
                (b"announce".to_vec(), Value::Bytes(b"http://t/a".to_vec())),
                (
                    b"info".to_vec(),
                    Value::Dict(
                        [
                            (b"length".to_vec(), Value::Int(10)),
                            (b"name".to_vec(), Value::Bytes(b"f".to_vec())),
                            (b"piece length".to_vec(), Value::Int(16384)),
                            (b"pieces".to_vec(), Value::Bytes(vec![0u8; 20])),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                ),
            ]
            .into_iter()
            .collect(),
        ),
        &mut valid,
    );
    for i in 0..valid.len().min(256) {
        let mut mutated = valid.clone();
        mutated[i] = mutated[i].wrapping_add(0x40);
        corpus.push(mutated);
        corpus.push(valid[..i].to_vec());
    }
    for input in &corpus {
        let parsed = parse(input);
        if let Ok(value) = parsed {
            // Re-encoding a parsed value must itself parse.
            let mut buf = Vec::new();
            encode(&value, &mut buf);
            let _ = parse(&buf);
            // And metainfo sees only well-formed input here.
            let _ = parse_torrent(input);
        }
    }
}

#[test]
fn magnet_and_url_corpus_never_panics() {
    let cases = [
        "magnet:?",
        "magnet:?xt=urn:btih:",
        "magnet:?xt=urn:btih:%",
        "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&tr=%ff&dn=+%2",
        "magnet:?xt=urn:btmh:1220abcd",
        "https://example.com/file with spaces.torrent?x=1&y=2",
        "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&dn=%",
        "<a href=\"javascript:alert(1)\">x</a><a HREF='/UPPER.MP4?a=b#c'>y</a>",
        "<img src=noquotes><a href='unclosed>",
    ];
    for case in cases {
        let _ = parse_magnet(case);
        // The grabber's splitters must also hold on hostile markup.
        let _ = swiftfetch_grabber::url_extension(case);
        let _ = swiftfetch_grabber::extract_links(case);
    }
}
