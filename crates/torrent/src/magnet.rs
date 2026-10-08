//! Magnet-link parsing (BEP 9 + BEP 53 subset): `xt` (v1 hex / base32),
//! `dn` (display name), `tr` (trackers). Only `urn:btih:` topics are
//! accepted — v2-only (`urn:btmh:`) magnets are rejected with a clear error.

use std::fmt::Write as _;

/// A parsed magnet link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Magnet {
    /// 40-char lowercase v1 info hash (hex).
    pub info_hash_hex: String,
    /// Display name (`dn`), when present.
    pub name: Option<String>,
    /// Tracker URLs (`tr`), deduplicated, in order.
    pub trackers: Vec<String>,
}

/// Magnet-link failure.
#[derive(Debug, thiserror::Error)]
pub enum MagnetError {
    /// Not a magnet link at all.
    #[error("not a magnet link")]
    NotAMagnet,
    /// Missing or unusable `xt` (exact topic).
    #[error("magnet has no usable urn:btih: topic")]
    NoInfoHash,
    /// The info hash is malformed.
    #[error("bad info hash `{0}`")]
    BadHash(String),
}

/// Parses a magnet link into [`Magnet`].
///
/// # Errors
///
/// Returns [`MagnetError`] when the link is not a magnet or carries no v1
/// info hash.
pub fn parse_magnet(link: &str) -> Result<Magnet, MagnetError> {
    let query = link
        .strip_prefix("magnet:?")
        .ok_or(MagnetError::NotAMagnet)?;
    let mut info_hash_hex: Option<String> = None;
    let mut name: Option<String> = None;
    let mut trackers: Vec<String> = Vec::new();

    for param in query.split('&') {
        let Some((key, value)) = param.split_once('=') else {
            continue;
        };
        match key {
            "xt" => {
                let hash = value
                    .strip_prefix("urn:btih:")
                    .ok_or(MagnetError::NoInfoHash)?;
                info_hash_hex = Some(normalize_hash(hash)?);
            }
            "dn" => {
                if name.is_none() {
                    name = Some(percent_decode(value));
                }
            }
            "tr" => {
                let tracker = percent_decode(value);
                if !tracker.is_empty() && !trackers.contains(&tracker) {
                    trackers.push(tracker);
                }
            }
            _ => {}
        }
    }

    Ok(Magnet {
        info_hash_hex: info_hash_hex.ok_or(MagnetError::NoInfoHash)?,
        name,
        trackers,
    })
}

fn normalize_hash(hash: &str) -> Result<String, MagnetError> {
    // 40 hex chars (v1).
    if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(hash.to_ascii_lowercase());
    }
    // 32 base32 chars (v1, RFC 4648, no padding).
    if hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return base32_to_hex(hash).ok_or_else(|| MagnetError::BadHash(hash.to_owned()));
    }
    Err(MagnetError::BadHash(hash.to_owned()))
}

fn base32_to_hex(input: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits: u32 = 0;
    let mut bit_count = 0;
    let mut bytes = Vec::with_capacity(20);
    for ch in input.bytes() {
        let upper = ch.to_ascii_uppercase();
        let pos = ALPHABET.iter().position(|&c| c == upper)?;
        let val = u32::try_from(pos).unwrap_or(u32::MAX);
        if val >= 32 {
            return None;
        }
        bits = (bits << 5) | val;
        bit_count += 5;
        while bit_count >= 8 {
            bit_count -= 8;
            bytes.push(u8::try_from((bits >> bit_count) & 0xFF).unwrap_or(0));
        }
    }
    if bytes.len() != 20 {
        return None;
    }
    let mut hex = String::with_capacity(40);
    for b in &bytes {
        let _ = write!(hex, "{b:02x}");
    }
    Some(hex)
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_hex_magnet() {
        let m = parse_magnet(
            "magnet:?xt=urn:btih:A1B2C3D4E5F60718293A4B5C6D7E8F9012345678&dn=Ubuntu+24.04&tr=http%3A%2F%2Ftracker.example%2Fannounce&tr=http://tracker.example/announce",
        )
        .expect("must parse");
        assert_eq!(m.info_hash_hex, "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678");
        assert_eq!(m.name.as_deref(), Some("Ubuntu 24.04"));
        assert_eq!(m.trackers.len(), 1);
    }

    #[test]
    fn parses_base32_magnet() {
        // Base32 of 20 zero bytes = 32 'A's.
        let m = parse_magnet("magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("must parse");
        assert_eq!(m.info_hash_hex, "0".repeat(40));
    }

    #[test]
    fn rejects_non_magnets_and_v2() {
        assert!(parse_magnet("https://example.com/x.torrent").is_err());
        assert!(parse_magnet("magnet:?dn=noname").is_err());
        assert!(parse_magnet("magnet:?xt=urn:btmh:1220abcdef").is_err());
        assert!(parse_magnet("magnet:?xt=urn:btih:tooshort").is_err());
        // Never panics on garbage either.
        for garbage in [
            "magnet:?",
            "magnet:?xt=",
            "magnet:?tr=%zz",
            "magnet:?xt=urn:btih:%41",
        ] {
            let _ = parse_magnet(garbage);
        }
    }
}
