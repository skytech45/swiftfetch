//! Update feed client (Track A, M-A1): polls the admin panel's public
//! `/api/updates/<channel>` feed and reports availability + force policy.
//! Installing still goes through the Tauri updater once release signing
//! provisions keys; until then `Install` opens the artifact URL.

use std::sync::Arc;

use serde::Serialize;

use crate::state::AppState;

/// Placeholder baked in until the admin URL is configured.
const PLACEHOLDER_FEED: &str = "https://updates.swiftfetch.app";

/// Compile-time admin base URL (override with `SWIFTFETCH_ADMIN_URL`).
fn default_feed_base() -> String {
    option_env!("SWIFTFETCH_ADMIN_URL")
        .unwrap_or(PLACEHOLDER_FEED)
        .to_owned()
}

/// Feed base: `update.feedUrl` setting wins, else the baked default.
async fn feed_base(state: &Arc<AppState>) -> String {
    let store = Arc::clone(&state.store);
    tokio::task::spawn_blocking(move || {
        let guard = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        swiftfetch_store::repos::get_setting(&guard, "update.feedUrl")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str::<String>(&v).ok())
            .filter(|v| !v.trim().is_empty())
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_else(default_feed_base)
}

/// Update check outcome for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    /// A feed is configured and reachable.
    pub configured: bool,
    /// A newer version exists.
    pub available: bool,
    /// Newest published version (empty when none).
    pub version: String,
    /// Release notes of the newest version.
    pub notes: String,
    /// Current install is below min-required → must update.
    pub force: bool,
    /// Direct artifact URL for this platform, when published.
    pub download_url: Option<String>,
}

/// Compares dotted versions: negative when `a < b`.
fn compare_versions(a: &str, b: &str) -> i32 {
    let parts = |v: &str| {
        v.split('.')
            .map(|n| n.parse::<i64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (
            pa.get(i).copied().unwrap_or(0),
            pb.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    0
}

/// Current app version.
fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// This OS's Tauri target triple as used in feed artifacts.
fn platform_target() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return "windows-x86_64";
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return "darwin-x86_64";
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return "darwin-aarch64";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "linux-x86_64";
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    return "unknown";
}

/// Polls the update feed. Never fails the app: unreachable feeds report
/// `configured: false` instead of erroring.
#[tauri::command]
pub async fn check_for_updates(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<UpdateCheck, String> {
    let idle = UpdateCheck {
        configured: false,
        available: false,
        version: String::new(),
        notes: String::new(),
        force: false,
        download_url: None,
    };
    let base = feed_base(&state).await;
    if base == PLACEHOLDER_FEED {
        return Ok(idle);
    }
    let channel: String = {
        let store = Arc::clone(&state.store);
        tokio::task::spawn_blocking(move || {
            let guard = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            swiftfetch_store::repos::get_setting(&guard, "update.channel")
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str::<String>(&v).ok())
                .unwrap_or_else(|| "stable".to_owned())
        })
        .await
        .unwrap_or_else(|_| "stable".to_owned())
    };
    let current = current_version();
    let url = format!(
        "{}/api/updates/{}?current={}",
        base.trim_end_matches('/'),
        channel,
        current
    );
    let response = match reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(idle),
    };
    // 204 = current.
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(UpdateCheck {
            configured: true,
            ..idle
        });
    }
    if !response.status().is_success() {
        return Ok(idle);
    }
    let bytes = response.bytes().await.map_err(|_| "bad feed".to_owned())?;
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "bad feed".to_owned())?;
    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    if version.is_empty() || compare_versions(&current, &version) >= 0 {
        return Ok(UpdateCheck {
            configured: true,
            ..idle
        });
    }
    let min_required = body
        .get("min_required")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0");
    let download_url = body
        .get("platforms")
        .and_then(|p| p.get(platform_target()))
        .and_then(|p| p.get("url"))
        .and_then(|u| u.as_str())
        .map(str::to_owned);
    Ok(UpdateCheck {
        configured: true,
        available: true,
        notes: body
            .get("notes")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        force: compare_versions(&current, min_required) < 0,
        version,
        download_url,
    })
}

/// Fetches remote config flags for this install (best-effort, `{}` offline).
#[tauri::command]
pub async fn get_remote_config(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let base = feed_base(&state).await;
    if base == PLACEHOLDER_FEED {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    // Stable install bucket: reuse the device install id.
    let install: String = {
        let store = Arc::clone(&state.store);
        tokio::task::spawn_blocking(move || {
            let guard = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            swiftfetch_store::repos::get_setting(&guard, "device.install_id")
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str::<String>(&v).ok())
                .unwrap_or_else(|| "anonymous".to_owned())
        })
        .await
        .unwrap_or_else(|_| "anonymous".to_owned())
    };
    let url = format!(
        "{}/api/config?install={install}",
        base.trim_end_matches('/')
    );
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
