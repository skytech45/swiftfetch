//! `SwiftFetch` networking: HTTP client construction shared by the engine and
//! later milestones (proxy/auth/cookie expansion lands in M4 — design
//! contract in docs/system-design.md §4.5). TLS is rustls; redirects are
//! capped at 10 per the build prompt.

use std::time::Duration;

use reqwest::{Client, Proxy};

/// Network-layer configuration for building the shared HTTP client.
#[derive(Debug, Clone, Default)]
pub struct HttpConfig {
    /// Proxy URL (`http`, `https`, or `socks5`) applied to every request.
    pub proxy_url: Option<String>,
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
        if let Some(proxy) = &self.proxy_url {
            builder = builder.proxy(Proxy::all(proxy)?);
        }
        builder.build()
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
            proxy_url: Some("http://127.0.0.1:9".into()),
            user_agent: Some("SwiftFetch-test/0.1".into()),
            connect_timeout: Some(Duration::from_secs(1)),
        };
        let client = cfg.client();
        assert!(client.is_ok(), "configured client must build: {client:?}");
    }

    #[test]
    fn rejects_invalid_proxy() {
        let cfg = HttpConfig {
            proxy_url: Some("not a proxy".into()),
            ..HttpConfig::default()
        };
        assert!(cfg.client().is_err(), "invalid proxy must fail");
    }
}
