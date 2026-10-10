//! `SwiftFetch` networking: HTTP client construction shared by the engine and
//! later milestones (proxy/auth/cookie expansion lands in M4 — design
//! contract in docs/system-design.md §4.5). TLS is rustls; redirects are
//! capped at 10 per the build prompt.

use std::time::Duration;

use reqwest::{Client, Proxy};

pub mod proxy;

pub use proxy::{
    ProxyMode, first_proxy_in_pac, normalize_proxy_url, proxy_from_parts, resolve_pac,
    resolve_proxy_url, system_proxy_url, with_credentials,
};

/// Network-layer configuration for building the shared HTTP client.
#[derive(Debug, Clone, Default)]
pub struct HttpConfig {
    /// Proxy selection (default: OS-configured, direct when unset).
    pub proxy: ProxyMode,
    /// Proxy username for Basic auth (`None` = none).
    pub proxy_username: Option<String>,
    /// Proxy password — the caller loads this from the OS keychain at
    /// startup and never persists or logs it.
    pub proxy_password: Option<String>,
    /// Value for the `User-Agent` header.
    pub user_agent: Option<String>,
    /// Connect timeout (default 10 s). There is deliberately no total-request
    /// timeout: download streams may legitimately run for hours.
    pub connect_timeout: Option<Duration>,
}

impl HttpConfig {
    /// Builds a `reqwest` client from this configuration.
    ///
    /// # Errors
    ///
    /// Returns [`reqwest::Error`] when the client or proxy configuration is
    /// invalid.
    pub fn client(&self) -> Result<Client, reqwest::Error> {
        let mut builder = Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(self.connect_timeout.unwrap_or(Duration::from_secs(10)))
            .tcp_keepalive(Duration::from_secs(30));
        if let Some(ua) = &self.user_agent {
            builder = builder.user_agent(ua.clone());
        }
        if let Some(mut url) = resolve_proxy_url(&self.proxy) {
            if let Some(user) = self.proxy_username.as_deref() {
                let pass = self.proxy_password.as_deref().unwrap_or("");
                if let Some(authed) = with_credentials(&url, user, pass) {
                    url = authed;
                }
            }
            builder = builder.proxy(Proxy::all(url)?);
        }
        builder.build()
    }

    /// Legacy helper: explicit proxy URL, no auth.
    #[must_use]
    pub fn with_proxy_url(url: impl Into<String>) -> Self {
        Self {
            proxy: ProxyMode::Manual(url.into()),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_client_with_defaults() {
        let client = HttpConfig::default().client();
        assert!(client.is_ok(), "default client must build: {client:?}");
    }

    #[test]
    fn builds_client_with_proxy_and_agent() {
        let cfg = HttpConfig {
            proxy: ProxyMode::Manual("http://127.0.0.1:9".into()),
            user_agent: Some("SwiftFetch-test/0.1".into()),
            connect_timeout: Some(Duration::from_secs(1)),
            ..HttpConfig::default()
        };
        let client = cfg.client();
        assert!(client.is_ok(), "configured client must build: {client:?}");
    }

    #[test]
    fn rejects_invalid_proxy() {
        let cfg = HttpConfig::with_proxy_url("http://[::1");
        assert!(cfg.client().is_err(), "invalid proxy must fail");
    }

    #[test]
    fn system_mode_builds() {
        // Registry unset on CI → direct client; must still build.
        let cfg = HttpConfig::default();
        assert!(cfg.client().is_ok());
    }
}
