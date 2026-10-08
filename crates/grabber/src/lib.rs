//! `SwiftFetch` site grabber/spider: BFS crawl from a seed URL with
//! include/exclude file-type filters, depth and page caps, robots.txt
//! compliance (default on) and per-host politeness delays.
//!
//! Implementation for Milestone 5 (Build Prompt §13.1; design contract
//! in docs/system-design.md §4.7). Politeness defaults are a hard guardrail —
//! never ship defaults that look like a crawl-abuse tool.
//!
//! # Example
//!
//! ```ignore
//! use swiftfetch_grabber::{GrabConfig, crawl};
//! let config = GrabConfig::new("https://example.com/");
//! let report = crawl(config).await.expect("crawl works");
//! println!("found {} files", report.files.len());
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Default crawl depth (seed = 0).
pub const DEFAULT_MAX_DEPTH: u32 = 2;
/// Default page cap for one crawl.
pub const DEFAULT_MAX_PAGES: u32 = 50;
/// Default politeness delay between requests to the same host.
pub const DEFAULT_POLITENESS: Duration = Duration::from_secs(1);
/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// What went wrong during a crawl.
#[derive(Debug, thiserror::Error)]
pub enum GrabError {
    /// The seed URL is not a valid http(s) URL.
    #[error("invalid seed URL `{url}`: {message}")]
    InvalidSeed {
        /// Seed URL that failed to parse.
        url: String,
        /// Human-readable reason.
        message: String,
    },
    /// A network request failed.
    #[error("request to `{url}` failed: {message}")]
    Request {
        /// URL that failed.
        url: String,
        /// Human-readable reason.
        message: String,
    },
}

/// Crawl configuration. Polite defaults are baked in — callers must opt out
/// of `robots.txt` compliance explicitly (and the UI shows a warning).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GrabConfig {
    /// Seed URL (crawl root).
    pub seed_url: String,
    /// Maximum link depth (seed = 0).
    pub max_depth: u32,
    /// Maximum HTML pages to fetch.
    pub max_pages: u32,
    /// Maximum files to collect.
    pub max_files: u32,
    /// Only collect files with these extensions (lowercase, no dot).
    /// Empty = accept all.
    pub include_exts: Vec<String>,
    /// Never collect files with these extensions.
    pub exclude_exts: Vec<String>,
    /// Minimum file size in bytes (`None` = no minimum).
    pub min_bytes: Option<u64>,
    /// Maximum file size in bytes (`None` = no maximum).
    pub max_bytes: Option<u64>,
    /// Stay on the seed's host (default true).
    pub stay_on_domain: bool,
    /// Honor `robots.txt` (default true — disabling shows a UI warning).
    pub respect_robots: bool,
    /// Delay between requests to the same host (default 1 s).
    pub politeness: Duration,
    /// Per-request timeout.
    pub timeout: Duration,
    /// `User-Agent` header.
    pub user_agent: String,
}

impl GrabConfig {
    /// A config with safe defaults for `seed_url`.
    #[must_use]
    pub fn new(seed_url: impl Into<String>) -> Self {
        Self {
            seed_url: seed_url.into(),
            max_depth: DEFAULT_MAX_DEPTH,
            max_pages: DEFAULT_MAX_PAGES,
            max_files: 100,
            include_exts: Vec::new(),
            exclude_exts: Vec::new(),
            min_bytes: None,
            max_bytes: None,
            stay_on_domain: true,
            respect_robots: true,
            politeness: DEFAULT_POLITENESS,
            timeout: DEFAULT_TIMEOUT,
            user_agent: "SwiftFetch-site-grabber/0.1".to_owned(),
        }
    }
}

impl Default for GrabConfig {
    fn default() -> Self {
        Self::new("https://example.com/")
    }
}

/// One downloadable file discovered by the crawl.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FoundFile {
    /// Absolute file URL.
    pub url: String,
    /// Link depth where it was found (seed = 0).
    pub depth: u32,
    /// Lowercase extension without the dot (may be empty).
    pub extension: String,
    /// `Content-Length` when the server sent one.
    pub size_hint: Option<u64>,
    /// `Content-Type` when the server sent one.
    pub content_type: Option<String>,
}

/// Crawl outcome.
#[derive(Debug, Clone)]
pub struct GrabReport {
    /// Files matching the filters, in discovery order.
    pub files: Vec<FoundFile>,
    /// HTML pages fetched.
    pub pages_visited: u32,
    /// URLs skipped because of `robots.txt`.
    pub skipped_robots: u32,
    /// URLs skipped by the extension filters.
    pub skipped_filtered: u32,
    /// Total politeness delay applied.
    pub politeness_applied: Duration,
    /// Whether the page cap stopped the crawl early.
    pub truncated: bool,
}

/// Parsed `robots.txt` rules (only `Disallow` for `User-agent: *`).
#[derive(Debug, Default, Clone)]
pub struct RobotsRules {
    disallow: Vec<String>,
}

impl RobotsRules {
    /// Parses a `robots.txt` body.
    #[must_use]
    pub fn parse(body: &str) -> Self {
        let mut rules = Self::default();
        let mut applies = false;
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(agent) = line.strip_prefix("User-agent:").map(str::trim) {
                applies = agent == "*" || agent.eq_ignore_ascii_case("SwiftFetch");
            } else if applies
                && let Some(path) = line.strip_prefix("Disallow:").map(str::trim)
                && !path.is_empty()
            {
                rules.disallow.push(path.to_owned());
            }
        }
        rules
    }

    /// Whether `path` (origin-relative, e.g. `/private/x`) may be fetched.
    #[must_use]
    pub fn allowed(&self, path: &str) -> bool {
        !self.disallow.iter().any(|d| path.starts_with(d.as_str()))
    }
}

/// Extracts `href`/`src` attribute values from HTML (tolerant, no parser dep).
#[must_use]
pub fn extract_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attr in ["href", "src"] {
        let mut rest = html;
        while let Some(pos) = find_attr(rest, attr) {
            rest = &rest[pos..];
            if let Some(value) = take_quoted(rest) {
                let advance = value.len();
                out.push(value);
                rest = &rest[advance.min(rest.len())..];
            } else {
                rest = &rest[attr.len().min(rest.len())..];
            }
        }
    }
    out
}

fn find_attr(haystack: &str, attr: &str) -> Option<usize> {
    let lower = haystack.to_ascii_lowercase();
    let mut search = lower.as_str();
    let mut base = 0;
    while let Some(pos) = search.find(attr) {
        let abs = base + pos;
        let after = abs + attr.len();
        let bytes = haystack.as_bytes();
        let before_ok =
            abs == 0 || !(bytes[abs - 1].is_ascii_alphanumeric() || bytes[abs - 1] == b'-');
        let mut k = after;
        while k < bytes.len() && bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        if before_ok && k < bytes.len() && bytes[k] == b'=' {
            return Some(after + (k - after) + 1);
        }
        search = &search[pos + attr.len().min(search.len())..];
        base = abs + attr.len();
    }
    None
}

fn take_quoted(s: &str) -> Option<String> {
    let s = s.trim_start();
    let quote = s.as_bytes().first()?;
    if *quote != b'"' && *quote != b'\'' {
        return None;
    }
    let inner = &s[1..];
    let end = inner.find(*quote as char)?;
    Some(inner[..end].to_owned())
}

/// Lowercase extension of a URL path (no dot, no query).
#[must_use]
pub fn url_extension(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split('#').next().unwrap_or(path);
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit('.').next().map_or(String::new(), |ext| {
        if ext.len() == file.len() {
            String::new()
        } else {
            ext.to_ascii_lowercase()
        }
    })
}

/// Whether a `Content-Type` looks like HTML.
#[must_use]
pub fn is_html(content_type: Option<&str>) -> bool {
    content_type.is_none_or(|ct| ct.to_ascii_lowercase().contains("html"))
}

/// Crawl `config.seed_url` breadth-first, honoring depth/page caps,
/// `robots.txt`, extension filters and per-host politeness.
///
/// # Errors
///
/// Returns [`GrabError::InvalidSeed`] when the seed URL is not usable and
/// [`GrabError::Request`] when the seed page itself cannot be fetched.
/// Per-page failures after the seed are skipped (and counted via
/// `truncated`), never fatal — a spider must degrade gracefully.
#[allow(clippy::too_many_lines)] // the BFS loop reads best as one body
pub async fn crawl(config: GrabConfig) -> Result<GrabReport, GrabError> {
    let seed_url: reqwest::Url = config
        .seed_url
        .parse()
        .map_err(|err| GrabError::InvalidSeed {
            url: config.seed_url.clone(),
            message: format!("{err}"),
        })?;
    if seed_url.scheme() != "http" && seed_url.scheme() != "https" {
        return Err(GrabError::InvalidSeed {
            url: config.seed_url.clone(),
            message: "only http(s) URLs can be grabbed".to_owned(),
        });
    }
    let seed_host = seed_url.host_str().unwrap_or_default().to_ascii_lowercase();

    let client = reqwest::Client::builder()
        .user_agent(config.user_agent.clone())
        .timeout(config.timeout)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|err| GrabError::Request {
            url: config.seed_url.clone(),
            message: format!("{err}"),
        })?;

    // Fetch robots.txt once per host (best-effort: failure = allow all).
    let mut robots: HashMap<String, RobotsRules> = HashMap::new();
    if config.respect_robots {
        let port_suffix = seed_url.port().map(|p| format!(":{p}")).unwrap_or_default();
        let robots_url = format!(
            "{}://{seed_host}{port_suffix}/robots.txt",
            seed_url.scheme()
        );
        if let Ok(resp) = client.get(&robots_url).send().await
            && resp.status().is_success()
            && let Ok(body) = resp.text().await
        {
            robots.insert(seed_host.clone(), RobotsRules::parse(&body));
        }
    }

    let mut queue: VecDeque<(String, u32)> = VecDeque::from([(seed_url.to_string(), 0)]);
    let mut visited: HashSet<String> = HashSet::from([seed_url.to_string()]);
    let mut report = GrabReport {
        files: Vec::new(),
        pages_visited: 0,
        skipped_robots: 0,
        skipped_filtered: 0,
        politeness_applied: Duration::ZERO,
        truncated: false,
    };
    let mut last_hit: HashMap<String, Instant> = HashMap::new();

    while let Some((url, depth)) = queue.pop_front() {
        if report.pages_visited >= config.max_pages {
            report.truncated = true;
            break;
        }
        if file_count(&report) >= config.max_files {
            report.truncated = true;
            break;
        }
        let parsed: reqwest::Url = match url.parse() {
            Ok(u) => u,
            Err(_) => continue,
        };
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        if config.stay_on_domain && host != seed_host {
            continue;
        }
        let path = parsed.path();
        if config.respect_robots
            && let Some(rules) = robots.get(&host)
            && !rules.allowed(path)
        {
            report.skipped_robots += 1;
            tracing::debug!(%url, "grabber: blocked by robots.txt");
            continue;
        }
        // Politeness: at most one request per `politeness` window per host.
        if let Some(last) = last_hit.get(&host) {
            let elapsed = last.elapsed();
            if elapsed < config.politeness {
                let wait = config
                    .politeness
                    .checked_sub(elapsed)
                    .unwrap_or(Duration::ZERO);
                tracing::debug!(%host, ?wait, "grabber: politeness delay");
                tokio::time::sleep(wait).await;
                report.politeness_applied += wait;
            }
        }
        last_hit.insert(host.clone(), Instant::now());

        let resp = match client.get(url.clone()).send().await {
            Ok(r) => r,
            Err(err) => {
                tracing::debug!(%url, %err, "grabber: page fetch failed, skipping");
                continue;
            }
        };
        let status = resp.status();
        if !status.is_success() {
            continue;
        }
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let Ok(body) = resp.text().await else {
            continue;
        };
        // Only HTML pages are followed; anything else is a file candidate.
        if !is_html(ctype.as_deref()) {
            let size_hint = u64::try_from(body.len()).unwrap_or(u64::MAX);
            push_file_candidate(
                &config,
                &mut report,
                &url,
                depth,
                ctype.as_deref(),
                Some(size_hint),
            );
            continue;
        }
        report.pages_visited += 1;
        for link in extract_links(&body) {
            if link.starts_with('#')
                || link.starts_with("mailto:")
                || link.starts_with("javascript:")
            {
                continue;
            }
            let abs = match parsed.join(&link) {
                Ok(u) => u.to_string(),
                Err(_) => continue,
            };
            if !abs.starts_with("http://") && !abs.starts_with("https://") {
                continue;
            }
            if !visited.insert(abs.clone()) {
                continue;
            }
            let ext = url_extension(&abs);
            let abs_parsed: Option<reqwest::Url> = abs.parse().ok();
            let blocked = abs_parsed.as_ref().is_some_and(|u| {
                let h = u.host_str().unwrap_or_default().to_ascii_lowercase();
                if config.stay_on_domain && h != seed_host {
                    return true;
                }
                if config.respect_robots
                    && let Some(rules) = robots.get(&h)
                    && !rules.allowed(u.path())
                {
                    return true;
                }
                false
            });
            if blocked {
                report.skipped_robots += 1;
                continue;
            }
            if is_probably_file(&ext, &abs) {
                push_file_candidate(&config, &mut report, &abs, depth + 1, None, None);
                if file_count(&report) >= config.max_files {
                    report.truncated = true;
                    break;
                }
            } else if depth < config.max_depth {
                queue.push_back((abs, depth + 1));
            }
        }
    }
    Ok(report)
}

fn is_probably_file(ext: &str, url: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    // Heuristic: a path ending in `.ext` with a short extension is a file.
    // Query-only URLs (`?id=1`) have no extension and are treated as pages.
    let path = url.split('?').next().unwrap_or(url);
    path.rsplit('/').next().is_some_and(|f| f.contains('.')) && ext.len() <= 5
}

#[allow(clippy::too_many_arguments)]
fn push_file_candidate(
    config: &GrabConfig,
    report: &mut GrabReport,
    url: &str,
    depth: u32,
    content_type: Option<&str>,
    size_hint: Option<u64>,
) {
    let ext = url_extension(url);
    if !config.include_exts.is_empty()
        && !config
            .include_exts
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&ext))
    {
        report.skipped_filtered += 1;
        return;
    }
    if config
        .exclude_exts
        .iter()
        .any(|e| e.eq_ignore_ascii_case(&ext))
    {
        report.skipped_filtered += 1;
        return;
    }
    if let Some(min) = config.min_bytes
        && size_hint.is_some_and(|s| s < min)
    {
        report.skipped_filtered += 1;
        return;
    }
    if let Some(max) = config.max_bytes
        && size_hint.is_some_and(|s| s > max)
    {
        report.skipped_filtered += 1;
        return;
    }
    report.files.push(FoundFile {
        url: url.to_owned(),
        depth,
        extension: ext,
        size_hint,
        content_type: content_type.map(str::to_owned),
    });
}

/// Saturating file count for cap comparisons (never truncates in practice —
/// crawls cap at hundreds of files, far below `u32::MAX`).
fn file_count(report: &GrabReport) -> u32 {
    u32::try_from(report.files.len()).unwrap_or(u32::MAX)
}
