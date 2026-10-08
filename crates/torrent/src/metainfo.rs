//! `.torrent` metainfo parsing (BEP 3): info hash, file layout, trackers.
//!
//! Only v1 (`info` dict) torrents are supported — the format every public
//! legal torrent (e.g. Linux ISOs) ships. Hybrid/v2-only files are rejected
//! with a clear error instead of misbehaving.

use std::collections::BTreeMap;

use sha1::{Digest as _, Sha1};

use crate::bencode::{self, Value};

/// Parsed `.torrent` metainfo.
#[derive(Debug, Clone)]
pub struct TorrentMeta {
    /// Torrent display name (`info.name`).
    pub name: String,
    /// SHA-1 of the bencoded `info` dict (the v1 info hash).
    pub info_hash_hex: String,
    /// Raw bencoded `info` dict (for re-hashing / debugging).
    pub info_bytes: Vec<u8>,
    /// Total content length in bytes.
    pub total_length: u64,
    /// Piece length in bytes.
    pub piece_length: u64,
    /// Files as relative paths with lengths (single-file torrents yield one).
    pub files: Vec<TorrentFile>,
    /// Announce URLs (`announce` + `announce-list`), deduplicated.
    pub trackers: Vec<String>,
    /// `info.private` flag.
    pub is_private: bool,
}

/// One file inside a torrent.
#[derive(Debug, Clone)]
pub struct TorrentFile {
    /// Relative path segments joined with `/`.
    pub path: String,
    /// File length in bytes.
    pub length: u64,
}

/// Metainfo failure.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// The bytes are not bencode at all.
    #[error("not a bencoded torrent: {0}")]
    Bencode(#[from] crate::bencode::ParseError),
    /// The structure is bencode but not a v1 torrent.
    #[error("invalid torrent metainfo: {0}")]
    Invalid(String),
}

fn dict<'a>(value: &'a Value, what: &str) -> Result<&'a BTreeMap<Vec<u8>, Value>, MetaError> {
    match value {
        Value::Dict(map) => Ok(map),
        _ => Err(MetaError::Invalid(format!("`{what}` is not a dict"))),
    }
}

fn get<'a>(map: &'a BTreeMap<Vec<u8>, Value>, key: &str) -> Result<&'a Value, MetaError> {
    map.get(key.as_bytes())
        .ok_or_else(|| MetaError::Invalid(format!("missing `{key}`")))
}

fn as_int(value: &Value, key: &str) -> Result<i64, MetaError> {
    match value {
        Value::Int(n) => Ok(*n),
        _ => Err(MetaError::Invalid(format!("`{key}` is not an integer"))),
    }
}

fn as_str(value: &Value, key: &str) -> Result<String, MetaError> {
    match value {
        Value::Bytes(b) => String::from_utf8(b.clone())
            .map_err(|_| MetaError::Invalid(format!("`{key}` is not UTF-8"))),
        _ => Err(MetaError::Invalid(format!("`{key}` is not a string"))),
    }
}

/// Parses `.torrent` bytes into [`TorrentMeta`].
///
/// # Errors
///
/// Returns [`MetaError`] when the input is not bencode or not a v1 torrent.
pub fn parse_torrent(bytes: &[u8]) -> Result<TorrentMeta, MetaError> {
    let parsed = bencode::parse(bytes)?;
    let root = dict(&parsed, "root")?;
    let info_value = get(root, "info")?;
    let info = dict(info_value, "info")?;

    // Canonical info hash: SHA-1 over the bencoded info dict. Re-encoding
    // from the parsed map is canonical (sorted keys), matching the original
    // whenever the original was canonical — which every real tracker
    // requires. For byte-exactness on odd inputs we re-encode.
    let mut info_bytes = Vec::new();
    bencode::encode(info_value, &mut info_bytes);
    let digest = Sha1::digest(&info_bytes);
    let info_hash_hex = hex_bytes(digest.as_slice());

    let name = as_str(get(info, "name")?, "info.name")?;
    let piece_length = as_int(get(info, "piece length")?, "piece length")?;
    if piece_length <= 0 {
        return Err(MetaError::Invalid("bad piece length".into()));
    }
    let pieces = match get(info, "pieces")? {
        Value::Bytes(b) => b.clone(),
        _ => return Err(MetaError::Invalid("`pieces` is not a string".into())),
    };
    if pieces.len() % 20 != 0 {
        return Err(MetaError::Invalid(
            "`pieces` is not a multiple of 20".into(),
        ));
    }
    let is_private = info
        .get(b"private".as_slice())
        .is_some_and(|v| matches!(v, Value::Int(1)));

    let mut files = Vec::new();
    let single_length: Option<i64> = info
        .get(b"length".as_slice())
        .map(|v| as_int(v, "length"))
        .transpose()?;
    if let Some(length) = single_length {
        files.push(TorrentFile {
            path: name.clone(),
            length: u64::try_from(length)
                .map_err(|_| MetaError::Invalid("negative length".into()))?,
        });
    } else {
        let Value::List(items) = get(info, "files")? else {
            return Err(MetaError::Invalid("`files` is not a list".into()));
        };
        for entry in items {
            let entry = dict(entry, "file entry")?;
            let length = as_int(get(entry, "length")?, "file length")?;
            let length = u64::try_from(length)
                .map_err(|_| MetaError::Invalid("negative file length".into()))?;
            let segments = match get(entry, "path")? {
                Value::List(segs) => segs
                    .iter()
                    .map(|s| as_str(s, "path segment"))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(MetaError::Invalid("`path` is not a list".into())),
            };
            if segments
                .iter()
                .any(|s| s == ".." || s.contains('/') || s.contains('\\'))
            {
                return Err(MetaError::Invalid("unsafe path segment".into()));
            }
            files.push(TorrentFile {
                path: segments.join("/"),
                length,
            });
        }
        if files.is_empty() {
            return Err(MetaError::Invalid("no files listed".into()));
        }
    }
    let total_length = files.iter().map(|f| f.length).sum();

    // Trackers: announce + announce-list (list of lists), deduplicated.
    let mut trackers = Vec::new();
    if let Ok(Value::Bytes(a)) = get(root, "announce") {
        push_tracker(&mut trackers, a);
    }
    if let Ok(Value::List(tiers)) = get(root, "announce-list") {
        for tier in tiers {
            if let Value::List(urls) = tier {
                for url in urls {
                    if let Value::Bytes(b) = url {
                        push_tracker(&mut trackers, b);
                    }
                }
            }
        }
    }

    Ok(TorrentMeta {
        name,
        info_hash_hex,
        info_bytes,
        total_length,
        piece_length: u64::try_from(piece_length)
            .map_err(|_| MetaError::Invalid("bad piece length".into()))?,
        files,
        trackers,
        is_private,
    })
}

/// Pushes a tracker URL unless empty, undecodable or already listed.
fn push_tracker(trackers: &mut Vec<String>, raw: &[u8]) {
    if let Ok(url) = String::from_utf8(raw.to_vec())
        && !url.is_empty()
        && !trackers.contains(&url)
    {
        trackers.push(url);
    }
}

/// Lowercase hex encoding (the `sha1` 0.11 digest array has no `LowerHex`).
fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation
    )]

    use super::*;

    /// Builds a minimal single-file torrent for `data` and returns the bytes.
    pub fn build_test_torrent(name: &str, data: &[u8]) -> Vec<u8> {
        use sha1::Digest as _;
        let piece_length: usize = 16384;
        let mut pieces = Vec::new();
        for chunk in data.chunks(piece_length) {
            let mut h = Sha1::new();
            h.update(chunk);
            pieces.extend_from_slice(h.finalize().as_slice());
        }
        let info = Value::Dict(BTreeMap::from([
            (b"length".to_vec(), Value::Int(data.len() as i64)),
            (b"name".to_vec(), Value::Bytes(name.as_bytes().to_vec())),
            (b"piece length".to_vec(), Value::Int(piece_length as i64)),
            (b"pieces".to_vec(), Value::Bytes(pieces)),
        ]));
        let root = Value::Dict(BTreeMap::from([
            (
                b"announce".to_vec(),
                Value::Bytes(b"http://tracker.example/announce".to_vec()),
            ),
            (b"info".to_vec(), info),
        ]));
        let mut out = Vec::new();
        bencode::encode(&root, &mut out);
        out
    }

    #[test]
    fn parses_single_file_torrent() {
        let data = vec![7u8; 40_000];
        let bytes = build_test_torrent("payload.bin", &data);
        let meta = parse_torrent(&bytes).expect("must parse");
        assert_eq!(meta.name, "payload.bin");
        assert_eq!(meta.total_length, 40_000);
        assert_eq!(meta.files.len(), 1);
        assert_eq!(meta.trackers.len(), 1);
        assert_eq!(meta.info_hash_hex.len(), 40);
    }

    #[test]
    fn rejects_garbage_and_v2() {
        assert!(parse_torrent(b"not bencode").is_err());
        assert!(parse_torrent(b"i42e").is_err()); // valid bencode, not a torrent
        // Dict without info.
        let mut buf = Vec::new();
        bencode::encode(
            &Value::Dict(BTreeMap::from([(
                b"announce".to_vec(),
                Value::Bytes(b"x".to_vec()),
            )])),
            &mut buf,
        );
        assert!(parse_torrent(&buf).is_err());
    }
}
