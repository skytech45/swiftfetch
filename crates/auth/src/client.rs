//! Supabase Auth + `PostgREST` client over `reqwest` (no SDK exists for Rust).
//!
//! Endpoints used: `/auth/v1/signup`, `/auth/v1/token?grant_type=password`,
//! `/auth/v1/token?grant_type=refresh_token`, `/auth/v1/logout`,
//! `/rest/v1/app_users`, `/rest/v1/rpc/register_device`.
//!
//! The workspace `reqwest` has no `json` feature, so bodies go out as
//! strings and responses are parsed from bytes (see [`read_json`]).

use serde::{Deserialize, Serialize};

/// Connection config. Values bake in at compile time with `option_env`
/// overrides so release builds can point at staging without code changes.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Supabase project URL.
    pub url: String,
    /// Publishable (anon) key — public by design.
    pub anon_key: String,
}

/// Compile-time config with the production project baked in.
#[must_use]
pub fn default_config() -> AuthConfig {
    AuthConfig {
        url: option_env!("SUPABASE_URL")
            .unwrap_or("https://lbfkjfbeyxbrrpktenyp.supabase.co")
            .to_owned(),
        anon_key: option_env!("SUPABASE_ANON_KEY")
            .unwrap_or("sb_publishable_yaCsoJ6ztxmon7haOL2lqw_4Qf-lTC3")
            .to_owned(),
    }
}

/// Auth failure.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Network failure.
    #[error("auth request failed: {0}")]
    Transport(String),
    /// The server rejected the credentials/request (message surfaced to UI).
    #[error("{0}")]
    Rejected(String),
    /// No session stored (signed out).
    #[error("not signed in")]
    SignedOut,
    /// Stored session is expired and refresh failed.
    #[error("session expired — please sign in again")]
    Expired,
}

/// Tokens from sign-in/sign-up/refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    /// JWT for authenticated PostgREST/Auth calls.
    pub access_token: String,
    /// Long-lived token used to mint fresh access tokens.
    pub refresh_token: String,
    /// User id (`sub`).
    pub user_id: String,
    /// User email.
    pub email: String,
    /// Unix time when the access token expires.
    pub expires_at: u64,
}

/// The signed-in user's app profile (tier + status from `app_users`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    /// `free` or `pro`.
    pub tier: String,
    /// `active` or `suspended`.
    pub status: String,
    /// Trial expiry (ISO-8601), if any.
    pub trial_ends_at: Option<String>,
}

/// Outcome of `register_device`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRegistration {
    /// Whether this PC may proceed.
    pub ok: bool,
    /// `device_limit` / `suspended` / `not_signed_in` when refused.
    pub reason: Option<String>,
}

/// Supabase Auth client (holds no secrets itself).
#[derive(Debug, Clone)]
pub struct AuthClient {
    config: AuthConfig,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: Option<u64>,
    user: Option<AuthUser>,
}

#[derive(Debug, Deserialize)]
struct AuthUser {
    id: String,
    email: Option<String>,
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Reads a JSON body without the `reqwest/json` feature.
async fn read_json(response: reqwest::Response) -> Result<serde_json::Value, AuthError> {
    let bytes = response
        .bytes()
        .await
        .map_err(|e| AuthError::Transport(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| AuthError::Transport(e.to_string()))
}

fn json_body(value: &serde_json::Value) -> Result<String, AuthError> {
    serde_json::to_string(value).map_err(|e| AuthError::Transport(e.to_string()))
}

fn decode_tokens(body: serde_json::Value) -> Result<TokenResponse, AuthError> {
    serde_json::from_value(body).map_err(|e| AuthError::Transport(e.to_string()))
}

impl AuthClient {
    /// Builds a client with default timeouts.
    #[must_use]
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    fn post(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::RequestBuilder, AuthError> {
        Ok(self
            .http
            .post(url)
            .header("apikey", &self.config.anon_key)
            .header("Content-Type", "application/json")
            .body(json_body(body)?))
    }

    /// Registers a new account (also provisions the free-tier `app_users` row
    /// server-side). Returns tokens when email confirmation is off.
    ///
    /// # Errors
    ///
    /// [`AuthError::Rejected`] on invalid input or an existing account;
    /// [`AuthError::Transport`] on network failure.
    pub async fn signup(
        &self,
        name: &str,
        email: &str,
        password: &str,
    ) -> Result<AuthTokens, AuthError> {
        let url = format!("{}/auth/v1/signup", self.config.url);
        let response = self
            .post(
                &url,
                &serde_json::json!({
                    "email": email,
                    "password": password,
                    "data": {"display_name": name},
                }),
            )?
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        self.token_or_reject(response, email).await
    }

    /// Signs in with email + password.
    ///
    /// # Errors
    ///
    /// Same as [`AuthClient::signup`].
    pub async fn signin(&self, email: &str, password: &str) -> Result<AuthTokens, AuthError> {
        let url = format!("{}/auth/v1/token?grant_type=password", self.config.url);
        let response = self
            .post(
                &url,
                &serde_json::json!({"email": email, "password": password}),
            )?
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        self.token_or_reject(response, email).await
    }

    /// Refreshes an expired access token.
    ///
    /// # Errors
    ///
    /// [`AuthError::Expired`] when the refresh token is rejected.
    pub async fn refresh(&self, refresh_token: &str) -> Result<AuthTokens, AuthError> {
        let url = format!("{}/auth/v1/token?grant_type=refresh_token", self.config.url);
        let response = self
            .post(&url, &serde_json::json!({"refresh_token": refresh_token}))?
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if !response.status().is_success() {
            return Err(AuthError::Expired);
        }
        let body = decode_tokens(read_json(response).await?)?;
        Ok(Self::to_tokens(body))
    }

    /// Signs out (best-effort server revocation; the caller clears storage).
    pub async fn signout(&self, access_token: &str) {
        let url = format!("{}/auth/v1/logout", self.config.url);
        let _ = self
            .http
            .post(&url)
            .header("apikey", &self.config.anon_key)
            .bearer_auth(access_token)
            .send()
            .await;
    }

    /// Ensures the caller's `app_users` row exists (idempotent insert,
    /// first writer wins). Called after every sign-up/sign-in so a missing
    /// or failed server trigger can never leave the account unprovisioned.
    ///
    /// # Errors
    ///
    /// [`AuthError::Transport`] on network failure. Row-level denials are
    /// returned as [`AuthError::Rejected`].
    pub async fn provision_profile(
        &self,
        tokens: &AuthTokens,
        display_name: Option<&str>,
    ) -> Result<(), AuthError> {
        let url = format!("{}/rest/v1/app_users?on_conflict=id", self.config.url);
        let name = display_name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&tokens.email);
        let response = self
            .http
            .post(&url)
            .header("apikey", &self.config.anon_key)
            .header("Content-Type", "application/json")
            .header("Prefer", "resolution=ignore-duplicates")
            .bearer_auth(&tokens.access_token)
            .body(json_body(&serde_json::json!({
                "id": tokens.user_id,
                "email": tokens.email,
                "display_name": name,
            }))?)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(AuthError::Expired);
        }
        if !response.status().is_success() {
            let message = read_json(response)
                .await
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
                .unwrap_or_else(|| "profile provisioning failed".to_owned());
            return Err(AuthError::Rejected(message));
        }
        Ok(())
    }

    /// Reads the caller's `app_users` row (tier + status). Suspended users
    /// are reported (not errored) so the UI can show the reason.
    ///
    /// # Errors
    ///
    /// [`AuthError::Transport`] on network failure; [`AuthError::Rejected`]
    /// when the row is missing (auth user without a provisioned profile).
    pub async fn profile(&self, tokens: &AuthTokens) -> Result<UserProfile, AuthError> {
        let url = format!(
            "{}/rest/v1/app_users?id=eq.{}&select=tier,status,trial_ends_at",
            self.config.url, tokens.user_id
        );
        let response = self
            .http
            .get(&url)
            .header("apikey", &self.config.anon_key)
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(AuthError::Expired);
        }
        let body = read_json(response).await?;
        let rows: Vec<serde_json::Value> =
            serde_json::from_value(body).map_err(|e| AuthError::Transport(e.to_string()))?;
        let row = rows.into_iter().next().ok_or_else(|| {
            AuthError::Rejected("account is not provisioned yet — try again shortly".into())
        })?;
        Ok(UserProfile {
            tier: row
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("free")
                .to_owned(),
            status: row
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active")
                .to_owned(),
            trial_ends_at: row
                .get("trial_ends_at")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        })
    }

    /// Activates a Pro license key (`activate_license` RPC validates,
    /// binds and upgrades server-side).
    ///
    /// # Errors
    ///
    /// [`AuthError::Rejected`] with `invalid_key` / `revoked` / `expired` /
    /// `already_claimed`; [`AuthError::Transport`] on network failure.
    pub async fn activate_license(&self, tokens: &AuthTokens, key: &str) -> Result<(), AuthError> {
        let url = format!("{}/rest/v1/rpc/activate_license", self.config.url);
        let response = self
            .post(&url, &serde_json::json!({"p_key": key.trim()}))?
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(AuthError::Expired);
        }
        let body = read_json(response).await?;
        if body
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(());
        }
        let reason = body
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("rejected");
        Err(AuthError::Rejected(match reason {
            "invalid_key" => "unknown license key".to_owned(),
            "revoked" => "this license was revoked".to_owned(),
            "expired" => "this license expired".to_owned(),
            "already_claimed" => "this key is already used on another account".to_owned(),
            other => format!("activation refused: {other}"),
        }))
    }

    /// Registers this PC (`register_device` RPC enforces the 3-PC cap and
    /// the suspension check server-side).
    ///
    /// # Errors
    ///
    /// [`AuthError::Transport`] on network failure.
    pub async fn register_device(
        &self,
        tokens: &AuthTokens,
        device_id: &str,
        label: Option<&str>,
    ) -> Result<DeviceRegistration, AuthError> {
        let url = format!("{}/rest/v1/rpc/register_device", self.config.url);
        let response = self
            .post(
                &url,
                &serde_json::json!({"p_device_id": device_id, "p_label": label}),
            )?
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(|e| AuthError::Transport(e.to_string()))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(AuthError::Expired);
        }
        let body = read_json(response).await?;
        Ok(DeviceRegistration {
            ok: body
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            reason: body
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        })
    }

    async fn token_or_reject(
        &self,
        response: reqwest::Response,
        email: &str,
    ) -> Result<AuthTokens, AuthError> {
        if !response.status().is_success() {
            let message = read_json(response)
                .await
                .ok()
                .and_then(|v| {
                    v.get("msg")
                        .or_else(|| v.get("error_description"))
                        .or_else(|| v.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "sign-in failed".to_owned());
            return Err(AuthError::Rejected(message));
        }
        let body = decode_tokens(read_json(response).await?)?;
        // Email-confirmation projects return a user with no session here.
        if body.access_token.is_empty() {
            return Err(AuthError::Rejected(
                "check your inbox to confirm this email, then sign in".into(),
            ));
        }
        let fallback = email.to_owned();
        let email = body
            .user
            .as_ref()
            .and_then(|u| u.email.clone())
            .unwrap_or(fallback);
        let user_id = body.user.as_ref().map(|u| u.id.clone()).unwrap_or_default();
        Ok(Self::to_tokens_with(body, email, user_id))
    }

    fn to_tokens(body: TokenResponse) -> AuthTokens {
        let email = body
            .user
            .as_ref()
            .and_then(|u| u.email.clone())
            .unwrap_or_default();
        let user_id = body.user.as_ref().map(|u| u.id.clone()).unwrap_or_default();
        Self::to_tokens_with(body, email, user_id)
    }

    fn to_tokens_with(body: TokenResponse, email: String, user_id: String) -> AuthTokens {
        AuthTokens {
            access_token: body.access_token,
            refresh_token: body.refresh_token,
            user_id,
            email,
            expires_at: now_epoch().saturating_add(body.expires_in.unwrap_or(3600)),
        }
    }
}
