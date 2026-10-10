//! Supabase Auth gate (Track A, M-A2 lite): the app starts behind
//! sign-in/register; sessions persist in the OS keychain and every sign-in
//! registers this PC against the 3-device cap.

use std::sync::Arc;

use serde::Serialize;
use swiftfetch_auth::{
    AuthClient, AuthError, AuthTokens, KeyringStore, SessionStore, default_config,
};
use swiftfetch_store::repos;

use crate::state::AppState;

/// UI-facing signed-in account.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthView {
    /// Account email.
    pub email: String,
    /// `free` or `pro`.
    pub tier: String,
    /// Trial expiry (ISO-8601), if any.
    pub trial_ends_at: Option<String>,
}

fn client() -> AuthClient {
    AuthClient::new(default_config())
}

fn store() -> KeyringStore {
    KeyringStore
}

/// This PC's install id (generated once, stored in settings).
fn install_id(state: &Arc<AppState>) -> Result<String, String> {
    let store = Arc::clone(&state.store);
    tokio::task::block_in_place(|| {
        let guard = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = repos::get_setting(&guard, "device.install_id")
            .map_err(|e| e.to_string())?
            .and_then(|v| serde_json::from_str::<String>(&v).ok())
        {
            return Ok(existing);
        }
        let id = uuid::Uuid::new_v4().to_string();
        repos::set_setting(
            &guard,
            "device.install_id",
            &serde_json::to_string(&id).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        Ok(id)
    })
}

/// Validates stored tokens (refreshing once when expired) and returns the
/// live account view, or `None` when signed out.
async fn live_view(mut tokens: AuthTokens) -> Result<AuthView, String> {
    let api = client();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if tokens.expires_at.saturating_sub(60) <= now {
        tokens = api
            .refresh(&tokens.refresh_token)
            .await
            .map_err(|e| e.to_string())?;
        store()
            .save(&tokens)
            .map_err(|e| format!("keychain: {e}"))?;
    }
    let profile = api.profile(&tokens).await.map_err(|e| e.to_string())?;
    if profile.status == "suspended" {
        return Err("this account is suspended — contact support".to_owned());
    }
    Ok(AuthView {
        email: tokens.email.clone(),
        tier: profile.tier,
        trial_ends_at: profile.trial_ends_at,
    })
}

/// Signs in (or registers) and binds this PC. Shared by signup + signin.
async fn authenticate(
    state: &Arc<AppState>,
    tokens: AuthTokens,
    display_name: Option<&str>,
) -> Result<AuthView, String> {
    let api = client();
    // Idempotent self-provisioning: guarantees the app_users row even if
    // the server trigger ever misfires.
    api.provision_profile(&tokens, display_name)
        .await
        .map_err(|e| e.to_string())?;
    let profile = api.profile(&tokens).await.map_err(|e| e.to_string())?;
    if profile.status == "suspended" {
        return Err("this account is suspended — contact support".to_owned());
    }
    let device = install_id(state)?;
    let registration = api
        .register_device(&tokens, &device, Some("desktop"))
        .await
        .map_err(|e| e.to_string())?;
    if !registration.ok {
        let reason = registration.reason.as_deref().unwrap_or("refused");
        if reason == "device_limit" {
            return Err(
                "device limit reached (3 PCs) — ask an admin to revoke a device".to_owned(),
            );
        }
        return Err(format!("device registration refused: {reason}"));
    }
    store()
        .save(&tokens)
        .map_err(|e| format!("keychain: {e}"))?;
    Ok(AuthView {
        email: tokens.email.clone(),
        tier: profile.tier,
        trial_ends_at: profile.trial_ends_at,
    })
}

/// Current session, if any (refreshes once when expired).
#[tauri::command]
pub async fn auth_session() -> Result<Option<AuthView>, String> {
    let Some(tokens) = store().load().map_err(|e| format!("keychain: {e}"))? else {
        return Ok(None);
    };
    match live_view(tokens).await {
        Ok(view) => Ok(Some(view)),
        Err(err) if err.contains("expired") || err.contains("signed") => {
            let _ = store().clear();
            Ok(None)
        }
        Err(other) => Err(other),
    }
}

/// Registers a new account and signs in.
#[tauri::command]
pub async fn auth_signup(
    state: tauri::State<'_, Arc<AppState>>,
    name: String,
    email: String,
    password: String,
) -> Result<AuthView, String> {
    if name.trim().is_empty() {
        return Err("please enter your name".to_owned());
    }
    if password.len() < 8 {
        return Err("password must be at least 8 characters".to_owned());
    }
    let tokens = client()
        .signup(name.trim(), &email, &password)
        .await
        .map_err(|e| e.to_string())?;
    authenticate(&state, tokens, Some(name.trim())).await
}

/// Signs in and binds this PC.
#[tauri::command]
pub async fn auth_signin(
    state: tauri::State<'_, Arc<AppState>>,
    email: String,
    password: String,
) -> Result<AuthView, String> {
    let tokens = client()
        .signin(&email, &password)
        .await
        .map_err(|e| e.to_string())?;
    authenticate(&state, tokens, None).await
}

/// Signs out (revokes server-side, clears the keychain).
#[tauri::command]
pub async fn auth_signout() -> Result<(), String> {
    if let Ok(Some(tokens)) = store().load() {
        client().signout(&tokens.access_token).await;
    }
    store().clear().map_err(|e| format!("keychain: {e}"))
}

/// Activates a Pro license key for the signed-in account.
#[tauri::command]
pub async fn activate_license(key: String) -> Result<AuthView, String> {
    let Some(tokens) = store().load().map_err(|e| format!("keychain: {e}"))? else {
        return Err(AuthError::SignedOut.to_string());
    };
    let api = client();
    api.activate_license(&tokens, &key)
        .await
        .map_err(|e| e.to_string())?;
    live_view(tokens).await
}
