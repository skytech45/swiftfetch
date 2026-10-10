//! Proxy configuration (Windows production pack): disabled / system /
//! manual / PAC-lite modes with optional Basic credentials.
//!
//! * **System** (default): Windows reads `HKCU\...\Internet Settings`
//!   (`ProxyEnable` + `ProxyServer`); other platforms fall back to direct.
//! * **Manual**: an explicit `http(s)`/`socks5` URL.
//! * **PAC-lite**: fetches the PAC file and takes the first `PROXY host:port`
//!   — full PAC scripts need a JS engine, so complex PACs fall back to
//!   direct with a logged warning (documented limitation).
//! * Credentials ride inside the proxy URL (`http://user:pass@host:port`),
//!   which reqwest honors. Callers must source the password from the OS
//!   keychain and must never log the assembled URL.

/// Proxy selection mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyMode {
    /// No proxy (previous behavior).
    Disabled,
    /// OS-configured proxy (Windows registry; direct elsewhere).
    #[default]
    System,
    /// Explicit proxy URL.
    Manual(String),
    /// PAC file URL (lite parsing).
    Pac(String),
}

/// Resolves the effective proxy URL for `mode`, or `None` for direct.
/// Pure logic except system/PAC fetching (network for PAC).
#[must_use]
pub fn resolve_proxy_url(mode: &ProxyMode) -> Option<String> {
    match mode {
        ProxyMode::Disabled | ProxyMode::Pac(_) => None, // PAC: fetched async via `resolve_pac`
        ProxyMode::System => system_proxy_url(),
        ProxyMode::Manual(url) => normalize_proxy_url(url),
    }
}

/// Normalizes a user-supplied proxy URL (`host:port` gains `http://`).
/// Returns `None` for empty/unsupported schemes.
#[must_use]
pub fn normalize_proxy_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("http://{trimmed}")
    };
    let lower = with_scheme.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("socks5://")
        || lower.starts_with("socks5h://")
    {
        Some(with_scheme)
    } else {
        None
    }
}

/// Embeds Basic credentials into a proxy URL. The result carries a secret —
/// never log it.
#[must_use]
pub fn with_credentials(proxy_url: &str, username: &str, password: &str) -> Option<String> {
    let (scheme, rest) = proxy_url.split_once("://")?;
    if username.is_empty() {
        return Some(proxy_url.to_owned());
    }
    Some(format!(
        "{scheme}://{}:{}@{rest}",
        percent_encode(username),
        percent_encode(password)
    ))
}

fn percent_encode(input: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Scans PAC text for the first routing directive. A leading `DIRECT`
/// yields `None`; a leading `PROXY host:port` yields its URL.
#[must_use]
pub fn first_proxy_in_pac(pac: &str) -> Option<String> {
    let upper = pac.to_ascii_uppercase();
    let upper_bytes = upper.as_bytes();
    let mut i = 0;
    while i < upper_bytes.len() {
        if matches_word_at(upper_bytes, i, "DIRECT") {
            return None;
        }
        if matches_word_at(upper_bytes, i, "PROXY") {
            let Some(after) = pac.get(i + "PROXY".len()..) else {
                i += 1;
                continue;
            };
            let host_port: String = after
                .trim_start_matches([' ', '\t', '=', ':', '"', '\''])
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'' && *c != ';')
                .collect();
            if !host_port.is_empty() {
                return normalize_proxy_url(&host_port);
            }
        }
        i += 1;
    }
    None
}

fn matches_word_at(bytes: &[u8], at: usize, word: &str) -> bool {
    let end = at + word.len();
    if bytes.len() < end || bytes[at..end] != *word.as_bytes() {
        return false;
    }
    let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
    let after_ok = bytes.get(end).is_none_or(|b| !b.is_ascii_alphanumeric());
    before_ok && after_ok
}

/// Fetches a PAC file and resolves its first proxy (async, for the app).
///
/// # Errors
///
/// Returns a message when the fetch fails or no proxy is found.
pub async fn resolve_pac(url: &str) -> Result<Option<String>, String> {
    let body = reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("PAC fetch: {e}"))?
        .text()
        .await
        .map_err(|e| format!("PAC fetch: {e}"))?;
    Ok(first_proxy_in_pac(&body))
}

/// Parses the persisted mode string (`system|disabled|manual|pac`) with an
/// optional URL into a [`ProxyMode`]. Unknown values fall back to system.
#[must_use]
pub fn proxy_from_parts(mode: &str, url: Option<&str>) -> ProxyMode {
    match mode {
        "disabled" => ProxyMode::Disabled,
        "manual" => ProxyMode::Manual(url.unwrap_or_default().to_owned()),
        "pac" => ProxyMode::Pac(url.unwrap_or_default().to_owned()),
        _ => ProxyMode::System,
    }
}
/// Reads the Windows system proxy (`ProxyEnable` + `ProxyServer`).
/// Non-Windows always yields `None` (direct).
#[must_use]
pub fn system_proxy_url() -> Option<String> {
    #[cfg(windows)]
    {
        windows_system_proxy()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_system_proxy() -> Option<String> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    // Forms: `host:port` or `http=host:port;https=host:port;...`.
    if server.contains('=') {
        for part in server.split(';') {
            let (scheme, value) = part.split_once('=').unwrap_or(("", part));
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
                return normalize_proxy_url(value.trim());
            }
        }
        None
    } else {
        normalize_proxy_url(server.trim())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn pac_first_proxy_wins() {
        let pac = r#"function FindProxyForURL(u, h) { if (shExpMatch(h, "*.corp")) return "PROXY p1:8080; DIRECT"; return "DIRECT"; }"#;
        assert_eq!(first_proxy_in_pac(pac).as_deref(), Some("http://p1:8080"));
        assert_eq!(
            first_proxy_in_pac("function f(){ return \"DIRECT\"; }"),
            None
        );
        assert_eq!(first_proxy_in_pac("garbage(((("), None);
    }

    #[test]
    fn normalize_accepts_bare_host() {
        assert_eq!(
            normalize_proxy_url("proxy:8080").as_deref(),
            Some("http://proxy:8080")
        );
        assert_eq!(
            normalize_proxy_url("socks5://h:1080").as_deref(),
            Some("socks5://h:1080")
        );
        assert_eq!(normalize_proxy_url("gopher://h:70"), None);
        assert_eq!(normalize_proxy_url("   "), None);
    }

    #[test]
    fn credentials_embed_without_logging_plain() {
        let url = with_credentials("http://proxy:8080", "user", "p@ss").expect("url");
        assert!(url.starts_with("http://user:p%40ss@proxy:8080"));
    }
}
