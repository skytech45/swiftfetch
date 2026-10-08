//! Digest verification: streaming SHA-256/MD5 during download plus parsing of
//! `Digest:` (RFC 3230) and `Content-MD5` headers.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use md5::Md5;
use sha2::{Digest as _, Sha256};

/// A digest advertised by the server or supplied by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Digest {
    /// SHA-256 (32 bytes).
    Sha256([u8; 32]),
    /// MD5 (16 bytes, e.g. `Content-MD5`).
    Md5([u8; 16]),
}

impl Digest {
    /// The lower-case algorithm name as used in `Digest:` headers.
    #[must_use]
    pub fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha256(_) => "sha-256",
            Self::Md5(_) => "md5",
        }
    }

    /// Raw digest bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Sha256(b) => b,
            Self::Md5(b) => b,
        }
    }
}

/// Streaming hasher used by the disk writer; always computes SHA-256 and MD5.
#[derive(Default)]
pub struct StreamHasher {
    sha: Sha256,
    md5: Md5,
}

impl StreamHasher {
    /// Feeds a chunk into both hashers.
    pub fn update(&mut self, data: &[u8]) {
        self.sha.update(data);
        self.md5.update(data);
    }

    /// Finalizes into the two digests.
    #[must_use]
    pub fn finalize(self) -> (Digest, Digest) {
        let sha = self.sha.finalize().into();
        let md5 = self.md5.finalize().into();
        (Digest::Sha256(sha), Digest::Md5(md5))
    }
}

/// Parses a `Digest:` header value (RFC 3230), e.g.
/// `SHA-256=base64blob` or `MD5=base64blob`; hex is accepted as a fallback.
/// Returns the first recognizable entry; a known algorithm with an
/// undecodable blob invalidates the header.
#[must_use]
pub fn parse_digest_header(value: &str) -> Option<Digest> {
    for item in value.split(',') {
        let item = item.trim();
        let Some((algo, blob)) = item.split_once('=') else {
            continue;
        };
        let algo = algo.trim();
        let blob = blob.trim().trim_matches('"');
        let algo = algo.to_ascii_lowercase();
        let expected = match algo.as_str() {
            "sha-256" | "sha256" => 32,
            "md5" => 16,
            _ => continue,
        };
        return digest_from_blob(blob, expected);
    }
    None
}

/// Parses a bare base64/hex blob (used for `Content-MD5`).
#[must_use]
pub fn parse_md5_content_header(value: &str) -> Option<Digest> {
    digest_from_blob(value.trim(), 16)
}

/// Decodes a digest blob trying base64 first, then hex, accepting only the
/// expected byte length (a hex string is often also valid base64, so length
/// is the discriminator).
fn digest_from_blob(blob: &str, expected: usize) -> Option<Digest> {
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(blob)
        && bytes.len() == expected
    {
        return match expected {
            32 => <[u8; 32]>::try_from(bytes).ok().map(Digest::Sha256),
            _ => <[u8; 16]>::try_from(bytes).ok().map(Digest::Md5),
        };
    }
    let bytes = hex_decode(blob)?;
    if bytes.len() != expected {
        return None;
    }
    match expected {
        32 => <[u8; 32]>::try_from(bytes).ok().map(Digest::Sha256),
        _ => <[u8; 16]>::try_from(bytes).ok().map(Digest::Md5),
    }
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Hex-encodes digest bytes for display and comparison.
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Computes the SHA-256 of a file by streaming it from disk.
///
/// # Errors
///
/// Returns [`std::io::Error`] when the file cannot be read.
pub fn sha256_file(path: &Path) -> std::io::Result<Digest> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = StreamHasher::default();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().0)
}

/// Verifies a file against an expected hex digest (SHA-256 or MD5).
///
/// # Errors
///
/// Returns [`std::io::Error`] when the file cannot be read; returns
/// `Ok(false)` (not an error) when the hash simply does not match, or when
/// `expected_hex` is not a valid 32/64-char hex digest.
pub fn verify_expected(path: &Path, expected_hex: &str) -> std::io::Result<bool> {
    let expected = expected_hex.trim().to_ascii_lowercase();
    if expected.len() != 64 && expected.len() != 32 {
        return Ok(false);
    }
    if !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(false);
    }
    let actual = sha256_file(path)?;
    if expected.len() == 64 {
        return Ok(to_hex(actual.bytes()) == expected);
    }
    // MD5 expectation: compare against the streamed MD5.
    let mut file = std::fs::File::open(path)?;
    let mut hasher = StreamHasher::default();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let (_, md5) = hasher.finalize();
    Ok(to_hex(md5.bytes()) == expected)
}

/// Looks for a `<stem>.sha256` / `<stem>.md5` sidecar next to `file_path`
/// and parses `<hex> [*]<filename>` (coreutils format, first line).
/// Returns the expected hex digest, if any.
#[must_use]
pub fn find_sidecar_hex(file_path: &Path) -> Option<String> {
    let stem = file_path.file_name()?.to_str()?;
    for ext in ["sha256", "md5"] {
        let sidecar = file_path.with_file_name(format!("{stem}.{ext}"));
        if let Ok(body) = std::fs::read_to_string(&sidecar)
            && let Some(first) = body.lines().next()
        {
            let hex = first.split_whitespace().next().unwrap_or("").trim();
            if (hex.len() == 64 || hex.len() == 32) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Some(hex.to_ascii_lowercase());
            }
        }
    }
    None
}

/// Renames `path` to `<name>.badhash` (appending a counter when taken)
/// for checksum-mismatched files. Returns the new path.
///
/// # Errors
///
/// Returns [`std::io::Error`] when the rename fails.
pub fn quarantine_badhash(path: &Path) -> std::io::Result<PathBuf> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let mut unique = path.with_file_name(format!("{file_name}.badhash"));
    let mut i = 0u32;
    while unique.exists() {
        i += 1;
        unique = path.with_file_name(format!("{file_name}.badhash.{i}"));
    }
    std::fs::rename(path, &unique)?;
    Ok(unique)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn empty_input_hash_matches_known_vector() {
        let (sha, _md5) = StreamHasher::default().finalize();
        assert_eq!(to_hex(sha.bytes()), EMPTY_SHA256);
    }

    #[test]
    fn parses_digest_header_base64_and_hex() {
        let value = "SHA-256=47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=, qop=auth";
        let digest = parse_digest_header(value).expect("sha-256 entry must parse");
        assert_eq!(to_hex(digest.bytes()), EMPTY_SHA256);

        let hex_value = "sha-256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let digest = parse_digest_header(hex_value).expect("hex entry must parse");
        assert_eq!(to_hex(digest.bytes()), EMPTY_SHA256);
    }

    #[test]
    fn parses_content_md5() {
        // MD5 of empty input.
        let digest = parse_md5_content_header("1B2M2Y8AsgTpgAmY7PhCfg==").expect("must parse");
        assert_eq!(digest.algorithm(), "md5");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_digest_header("nonsense"), None);
        assert_eq!(parse_digest_header("sha-256=!!!not-base64!!!"), None);
    }
}
