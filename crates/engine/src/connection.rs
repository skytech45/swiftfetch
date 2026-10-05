//! HTTP probing and ranged requests: HEAD + range probe for resume
//! capability, ranged GETs with `If-Range`, and header parsing helpers.

use futures_util::Stream;
use reqwest::{Client, RequestBuilder, StatusCode, Url};

use crate::checksum::Digest;
use crate::errors::EngineError;

/// Result of probing a URL before any byte is written.
#[derive(Debug, Clone, Default)]
pub struct ProbeResult {
    /// Total length from `Content-Length` / `Content-Range`, when known.
    pub total_len: Option<u64>,
    /// Resume capability: `ranges` > `ifrange` > `none`.
    pub resume_cap: ResumeCap,
    /// `ETag` (strong identity used for `If-Range`).
    pub etag: Option<String>,
    /// `Last-Modified` fallback identity.
    pub last_modified: Option<String>,
    /// `Content-Type`.
    pub content_type: Option<String>,
    /// Filename from `Content-Disposition`, else from the URL path.
    pub filename: Option<String>,
    /// Parsed `Digest:` or `Content-MD5` header, when present.
    pub advertised_digest: Option<Digest>,
}

/// Resume capability of a server for one download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResumeCap {
    /// Server supports byte ranges — full segmented downloading.
    Ranges,
    /// No ranges, but `ETag`/`Last-Modified` exist — single connection with
    /// conditional restart.
    IfRangeOnly,
    /// No resume support at all — single connection, restart from 0.
    #[default]
    None,
}

impl ResumeCap {
    /// String stored in the `downloads.resume_cap` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ranges => "ranges",
            Self::IfRangeOnly => "ifrange",
            Self::None => "none",
        }
    }

    /// Parses the column value back.
    #[must_use]
    pub fn from_db_str(value: &str) -> Self {
        match value {
            "ranges" => Self::Ranges,
            "ifrange" => Self::IfRangeOnly,
            _ => Self::None,
        }
    }
}

/// Extra request context captured from the browser or the job spec.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// `Referer` header.
    pub referer: Option<String>,
    /// Raw `Cookie` header.
    pub cookies: Option<String>,
    /// Per-job `User-Agent` override.
    pub user_agent: Option<String>,
}

/// Type-erased body stream from a response.
pub type ByteStream = std::pin::Pin<Box<dyn Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

/// A ranged GET response, classified per the engine's handling table.
pub enum RangeResponse {
    /// 206 with a verified start offset; body streams from there.
    Partial {
        /// Body stream.
        stream: ByteStream,
        /// Start offset the server confirmed via `Content-Range`.
        verified_start: u64,
    },
    /// 200 — the server ignored the Range header.
    Full {
        /// Body stream starting at byte 0.
        stream: ByteStream,
    },
    /// 416 — our offsets are stale (the entity likely changed).
    NotSatisfiable,
}

fn apply_context(builder: RequestBuilder, ctx: &RequestContext) -> RequestBuilder {
    let builder = match &ctx.user_agent {
        Some(ua) => builder.header(reqwest::header::USER_AGENT, ua.clone()),
        None => builder,
    };
    let builder = match &ctx.referer {
        Some(r) => builder.header(reqwest::header::REFERER, r.clone()),
        None => builder,
    };
    match &ctx.cookies {
        Some(c) => builder.header(reqwest::header::COOKIE, c.clone()),
        None => builder,
    }
}

fn if_range_builder(builder: RequestBuilder, if_range: Option<&str>) -> RequestBuilder {
    match if_range {
        Some(tag) => builder.header(reqwest::header::IF_RANGE, tag),
        None => builder,
    }
}

async fn send(builder: RequestBuilder, url: &str) -> Result<reqwest::Response, EngineError> {
    builder.send().await.map_err(|err| map_transport(err, url))
}

fn map_transport(err: reqwest::Error, url: &str) -> EngineError {
    if let Some(status) = err.status() {
        return EngineError::Http {
            url: url.to_owned(),
            status: status.as_u16(),
        };
    }
    EngineError::Transport(err)
}

/// Probes a URL: HEAD first, falling back to `GET Range: bytes=0-0` when HEAD
/// is unsupported, then a confirming range probe when resume support is
/// unclear. Records everything the engine needs before any byte is written.
///
/// # Errors
///
/// Returns [`EngineError::Probe`] / [`EngineError::Http`] when the server
/// cannot be probed.
#[allow(clippy::too_many_lines)] // HEAD + range probe in one documented flow
pub async fn probe(
    client: &Client,
    url: &str,
    ctx: &RequestContext,
) -> Result<ProbeResult, EngineError> {
    let parsed = Url::parse(url).map_err(|_| EngineError::Config(format!("invalid URL {url}")))?;
    let mut result = ProbeResult::default();

    let head = send(apply_context(client.head(parsed.clone()), ctx), url).await?;
    let status = head.status();
    if status.is_success() {
        let headers = head.headers();
        result.total_len = headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        result.etag = headers
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        result.last_modified = headers
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        result.content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        result.advertised_digest = headers
            .get(reqwest::header::HeaderName::from_static("digest"))
            .and_then(|v| v.to_str().ok())
            .and_then(crate::checksum::parse_digest_header)
            .or_else(|| {
                headers
                    .get(reqwest::header::HeaderName::from_static("content-md5"))
                    .and_then(|v| v.to_str().ok())
                    .and_then(crate::checksum::parse_md5_content_header)
            });
        let accepts_ranges = headers
            .get(reqwest::header::HeaderName::from_static("accept-ranges"))
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));
        result.filename = headers
            .get(reqwest::header::HeaderName::from_static(
                "content-disposition",
            ))
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_disposition);

        if accepts_ranges && result.total_len.is_some() {
            result.resume_cap = ResumeCap::Ranges;
        }
    } else if !matches!(
        status,
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
    ) {
        // HEAD rejected for a real reason — surface it (refresh-worthy).
        return Err(EngineError::Http {
            url: url.to_owned(),
            status: status.as_u16(),
        });
    }

    // Confirming range probe: needed when HEAD said nothing about ranges
    // (or HEAD failed with 405/501 and we know nothing yet).
    if result.resume_cap != ResumeCap::Ranges {
        let range = send(
            if_range_builder(
                apply_context(client.get(parsed.clone()), ctx)
                    .header(reqwest::header::RANGE, "bytes=0-0"),
                result.etag.as_deref(),
            ),
            url,
        )
        .await?;
        match range.status() {
            StatusCode::PARTIAL_CONTENT => {
                if let Some(cr) = range
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(parse_content_range)
                    && let Some(total) = cr.total
                {
                    result.total_len = Some(total);
                }
                result.resume_cap = ResumeCap::Ranges;
            }
            StatusCode::OK => {
                if result.total_len.is_none() {
                    result.total_len = range
                        .headers()
                        .get(reqwest::header::CONTENT_LENGTH)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                }
                result.resume_cap = if result.etag.is_some() || result.last_modified.is_some() {
                    ResumeCap::IfRangeOnly
                } else {
                    ResumeCap::None
                };
            }
            code if code.is_success() => {
                return Err(EngineError::Http {
                    url: url.to_owned(),
                    status: code.as_u16(),
                });
            }
            // 4xx/5xx on the probe: HEAD may still have told us everything we
            // need; degrade to whatever HEAD produced instead of failing.
            _ => {}
        }
    }

    if result.filename.is_none() {
        result.filename = url_path_filename(&parsed);
    }
    Ok(result)
}

/// Opens a ranged GET for `[start, end]` (inclusive). `if_range` enables
/// conditional resume: a changed entity comes back as a full 200.
///
/// # Errors
///
/// Returns [`EngineError::Http`] for unexpected statuses and
/// [`EngineError::Transport`] for network failures.
pub async fn open_range(
    client: &Client,
    url: &str,
    start: u64,
    end: u64,
    if_range: Option<&str>,
    ctx: &RequestContext,
) -> Result<RangeResponse, EngineError> {
    let parsed = Url::parse(url).map_err(|_| EngineError::Config(format!("invalid URL {url}")))?;
    let range = format!("bytes={start}-{end}");
    let response = send(
        if_range_builder(
            apply_context(client.get(parsed), ctx).header(reqwest::header::RANGE, range),
            if_range,
        ),
        url,
    )
    .await?;

    match response.status() {
        StatusCode::PARTIAL_CONTENT => {
            let cr = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            match cr {
                Some(cr) if cr.first == start => Ok(RangeResponse::Partial {
                    stream: Box::pin(response.bytes_stream()),
                    verified_start: cr.first,
                }),
                _ => Err(EngineError::RangeMismatch {
                    url: url.to_owned(),
                }),
            }
        }
        StatusCode::OK => Ok(RangeResponse::Full {
            stream: Box::pin(response.bytes_stream()),
        }),
        StatusCode::RANGE_NOT_SATISFIABLE => Ok(RangeResponse::NotSatisfiable),
        code => Err(EngineError::Http {
            url: url.to_owned(),
            status: code.as_u16(),
        }),
    }
}

/// Opens a full (non-ranged) GET — used for unknown-length downloads.
///
/// # Errors
///
/// Returns [`EngineError::Http`] for unexpected statuses and
/// [`EngineError::Transport`] for network failures.
pub async fn open_full(
    client: &Client,
    url: &str,
    ctx: &RequestContext,
) -> Result<ByteStream, EngineError> {
    let parsed = Url::parse(url).map_err(|_| EngineError::Config(format!("invalid URL {url}")))?;
    let response = send(apply_context(client.get(parsed), ctx), url).await?;
    match response.status() {
        StatusCode::OK => Ok(Box::pin(response.bytes_stream())),
        code => Err(EngineError::Http {
            url: url.to_owned(),
            status: code.as_u16(),
        }),
    }
}

/// `Content-Range` as parsed from a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    /// First byte of the returned range (inclusive).
    pub first: u64,
    /// Last byte of the returned range (inclusive), when given.
    pub last: Option<u64>,
    /// Total entity size, when known.
    pub total: Option<u64>,
}

/// Parses `Content-Range: bytes first-last/total` (also `bytes */total`).
#[must_use]
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim();
    let (range_part, total_part) = rest.split_once('/')?;
    let total = total_part
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|t| *t > 0)
        .or_else(|| (total_part.trim() == "*").then_some(None).flatten());
    if range_part.trim() == "*" {
        return Some(ContentRange {
            first: 0,
            last: None,
            total,
        });
    }
    let (first, last) = range_part.trim().split_once('-')?;
    Some(ContentRange {
        first: first.trim().parse().ok()?,
        last: last.trim().parse().ok(),
        total,
    })
}

/// Parses `Content-Disposition` and returns the filename, preferring
/// `filename*` (RFC 5987) over `filename`.
#[must_use]
pub fn parse_content_disposition(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';') {
        let part = part.trim();
        if let Some(raw) = part.strip_prefix("filename*=") {
            // charset'lang'percent-encoded-value
            let value = raw.splitn(3, '\'').nth(2).unwrap_or(raw);
            if let Some(name) = percent_decode(value) {
                let cleaned = sanitize_filename(&name);
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        } else if let Some(raw) = part.strip_prefix("filename=") {
            let unquoted = raw.trim().trim_matches('"');
            let unescaped = unquoted.replace("\\\"", "\"").replace("\\\\", "\\");
            plain = Some(unescaped);
        }
    }
    plain
        .map(|p| {
            let cleaned = sanitize_filename(&p);
            if cleaned.is_empty() {
                String::new()
            } else {
                cleaned
            }
        })
        .filter(|s| !s.is_empty())
}

/// Sanitizes a filename: last path component only, control characters
/// stripped, Windows reserved names defused, trailing dots/spaces trimmed.
#[must_use]
pub fn sanitize_filename(input: &str) -> String {
    const RESERVED: [&str; 12] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "LPT1", "LPT2", "LPT3", "COM",
    ];
    let last_component = input
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(input);
    let mut cleaned: String = last_component
        .chars()
        .filter(|c| !c.is_ascii_control())
        .collect();
    cleaned = cleaned.trim().to_string();
    let upper = cleaned.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    if RESERVED.contains(&stem) {
        cleaned = format!("_{cleaned}");
    }
    while cleaned.ends_with('.') || cleaned.ends_with(' ') {
        cleaned.pop();
    }
    if cleaned.chars().count() > 200 {
        cleaned = cleaned.chars().take(200).collect();
    }
    cleaned
}

fn url_path_filename(url: &Url) -> Option<String> {
    let segment = url.path().rsplit('/').find(|s| !s.is_empty())?;
    let decoded = percent_decode(segment)?;
    let cleaned = sanitize_filename(&decoded);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(0));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_content_range_forms() {
        let cr = parse_content_range("bytes 0-0/2147483648").expect("parse");
        assert_eq!(cr.first, 0);
        assert_eq!(cr.last, Some(0));
        assert_eq!(cr.total, Some(2_147_483_648));

        let cr = parse_content_range("bytes */123").expect("parse");
        assert_eq!(cr.total, Some(123));

        assert_eq!(parse_content_range("garbage"), None);
    }

    #[test]
    fn parses_content_disposition_variants() {
        assert_eq!(
            parse_content_disposition("attachment; filename=\"ubuntu-24.04.iso\"").as_deref(),
            Some("ubuntu-24.04.iso")
        );
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''na%C3%AFve%20file.txt")
                .as_deref(),
            Some("naïve file.txt")
        );
        assert_eq!(
            parse_content_disposition("attachment; filename*=UTF-8''a.txt; filename=\"b.txt\"")
                .as_deref(),
            Some("a.txt"),
            "filename* wins"
        );
        assert_eq!(parse_content_disposition("inline"), None);
    }

    #[test]
    fn sanitizes_filenames() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(
            sanitize_filename("C:\\Users\\evil\\..\\file.zip"),
            "file.zip"
        );
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("name...   "), "name");
        assert_eq!(sanitize_filename("a\x01b"), "ab");
        assert_eq!(sanitize_filename(""), "");
    }

    #[test]
    fn percent_decodes_paths() {
        assert_eq!(
            url_path_filename(&Url::parse("https://x.io/a/b/My%20File.zip").expect("url"))
                .as_deref(),
            Some("My File.zip")
        );
    }
}
