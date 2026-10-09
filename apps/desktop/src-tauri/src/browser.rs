//! First-run browser integration (installer follow-up).
//!
//! After SwiftFetch is installed, the app registers itself with every
//! supported browser **once**, without admin rights:
//!
//! * **Native host** (fully automatic): writes the
//!   `com.swiftfetch.host` manifest and points the registry (Windows) or
//!   the well-known JSON dirs (macOS/Linux) at this install's
//!   `swiftfetch-native-host` binary.
//! * **Extensions** (browser-gated): writes sideload entries (Windows
//!   registry, Linux/macOS external-extensions JSON) for the packaged
//!   `.crx`/`.xpi` when present, so the extension *appears* on next
//!   browser launch. Browsers deliberately require one user click to
//!   enable sideloaded extensions — silent force-installs are impossible
//!   by design (that would be malware behavior), so the app also offers
//!   one-click Web Store links once published.
//!
//! Extension ids must match the release-signed manifests; until release
//! signing replaces the placeholder key, sideload entries carry the
//! development ids and the setup UI points at the packaged `.zip`s.

use std::path::{Path, PathBuf};

/// Native-messaging host id shared with the manifests.
pub const HOST_ID: &str = "com.swiftfetch.host";

/// Pinned extension ids (release signing replaces the placeholder key and
/// these ids with it — see `docs/system-design.md` §4.6).
pub const CHROME_EXT_ID: &str = "ofinmfgldbecdccimfgknfhjekcioclb";
pub const FIREFOX_EXT_ID: &str = "swiftfetch@skytech45";

/// Per-browser outcome of one integration run.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserReport {
    /// Browser key (`chrome`, `edge`, `firefox`).
    pub browser: String,
    /// Native-host manifest registered.
    pub native_host: bool,
    /// Extension sideload entry written (browser still asks one click).
    pub extension: bool,
    /// Human detail / reason when something was skipped.
    pub note: String,
}

/// Builds the native-messaging manifest JSON for `exe_path`.
#[must_use]
pub fn manifest_json(exe_path: &str) -> String {
    // Backslashes must be JSON-escaped for the manifest path.
    let escaped = exe_path.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\n  \"name\": \"{HOST_ID}\",\n  \"description\": \"SwiftFetch browser integration host\",\n  \"path\": \"{escaped}\",\n  \"type\": \"stdio\",\n  \"allowed_origins\": [\n    \"chrome-extension://{CHROME_EXT_ID}/\"\n  ]\n}}"
    )
}

/// Builds the Firefox native-host manifest (uses `allowed_extensions`).
#[must_use]
pub fn firefox_manifest_json(exe_path: &str) -> String {
    let escaped = exe_path.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\n  \"name\": \"{HOST_ID}\",\n  \"description\": \"SwiftFetch browser integration host\",\n  \"path\": \"{escaped}\",\n  \"type\": \"stdio\",\n  \"allowed_extensions\": [\"{FIREFOX_EXT_ID}\"]\n}}"
    )
}

/// Windows registry sideload entry for a Chromium extension:
/// `(subkey, value_name, value)`.
#[must_use]
pub fn chromium_extension_entry(
    browser: &str,
    ext_id: &str,
    package_path: &str,
    version: &str,
) -> (String, String, String) {
    let vendor = if browser == "edge" {
        "Microsoft\\Edge"
    } else {
        "Google\\Chrome"
    };
    (
        format!("Software\\{vendor}\\Extensions\\{ext_id}"),
        "path".to_owned(),
        format!("{package_path}|{version}"),
    )
}

/// Linux/macOS external-extension JSON (`external_crx` + `external_version`).
#[cfg(not(windows))]
#[must_use]
pub fn external_extension_json(package_path: &str, version: &str) -> String {
    format!(
        "{{\n  \"external_crx\": \"{package_path}\",\n  \"external_version\": \"{version}\"\n}}"
    )
}

/// Where a native-host manifest lives for `browser` on Unix.
/// (Windows finds the manifest through the registry instead.)
#[cfg(not(windows))]
#[must_use]
pub fn manifest_dir(browser: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let dir = match (std::env::consts::OS, browser) {
        ("macos", "firefox") => {
            home.join("Library/Application Support/Mozilla/NativeMessagingHosts")
        }
        ("macos", "edge") => {
            home.join("Library/Application Support/Microsoft Edge/NativeMessagingHosts")
        }
        ("macos", _) => home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts"),
        (_, "firefox") => home.join(".mozilla/native-messaging-hosts"),
        (_, "edge") => home.join(".config/microsoft-edge/NativeMessagingHosts"),
        _ => home.join(".config/google-chrome/NativeMessagingHosts"),
    };
    Some(dir)
}

/// External-extension JSON location for Chromium browsers on Unix.
#[cfg(not(windows))]
#[must_use]
pub fn external_extension_path(browser: &str, ext_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let base = if std::env::consts::OS == "macos" {
        match browser {
            "edge" => home.join("Library/Application Support/Microsoft Edge/External Extensions"),
            _ => home.join("Library/Application Support/Google/Chrome/External Extensions"),
        }
    } else {
        match browser {
            "edge" => home.join(".config/microsoft-edge/External Extensions"),
            _ => home.join(".config/google-chrome/External Extensions"),
        }
    };
    Some(base.join(format!("{ext_id}.json")))
}

/// Writes one registry string value. Windows only.
#[cfg(windows)]
fn reg_write(key: &str, name: &str, value: &str) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let (k, err) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(key)
        .map_err(|e| format!("registry {key}: {e}"))?;
    k.set_value(name, &value.to_string())
        .map_err(|e| format!("registry {key}\\{name}: {e}"))?;
    let _ = err;
    Ok(())
}

/// Runs full first-run integration. `app_dir` is the install dir (holds the
/// native-host binary + packaged extensions); `version` is the app version
/// for sideload entries. Safe to re-run — everything is idempotent.
pub fn integrate(app_dir: &Path, version: &str) -> Vec<BrowserReport> {
    let mut reports = Vec::new();
    let host_exe = host_binary(app_dir);
    for browser in ["chrome", "edge", "firefox"] {
        reports.push(integrate_one(
            browser,
            host_exe.as_deref(),
            app_dir,
            version,
        ));
    }
    reports
}

fn host_binary(app_dir: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let name = "swiftfetch-native-host.exe";
    #[cfg(not(windows))]
    let name = "swiftfetch-native-host";
    let candidate = app_dir.join(name);
    candidate.is_file().then_some(candidate)
}

fn integrate_one(
    browser: &str,
    host_exe: Option<&Path>,
    app_dir: &Path,
    version: &str,
) -> BrowserReport {
    let mut report = BrowserReport {
        browser: browser.to_owned(),
        native_host: false,
        extension: false,
        note: String::new(),
    };
    // 1. Native host.
    if let Some(exe) = host_exe {
        match register_native_host(browser, exe) {
            Ok(()) => report.native_host = true,
            Err(err) => {
                report.note = err;
                return report;
            }
        }
    } else {
        report.note = "native-host binary not bundled".to_owned();
        return report;
    }
    // 2. Extension sideload (only when a signed package ships with the app).
    let package = packaged_extension(app_dir, browser);
    if let Some(pkg) = package {
        match register_extension(browser, &pkg, version) {
            Ok(()) => {
                report.extension = true;
                report.note = "enable it once in the browser".to_owned();
            }
            Err(err) => report.note = err,
        }
    } else {
        report.note =
            "native host registered; install the extension from the setup page".to_owned();
    }
    report
}

fn register_native_host(browser: &str, exe: &Path) -> Result<(), String> {
    // Tauri's resource dir may come back as a verbatim (`\\?\`) path, which
    // browsers reject in manifests — normalize to a plain absolute path.
    let exe_str = normalize_win_path(&exe.to_string_lossy());
    // The manifest must live somewhere the user can write: per-machine
    // installs land in Program Files (read-only), so manifests go to the
    // per-user data dir and only the registry points at them.
    let home = manifest_home()?;
    std::fs::create_dir_all(&home).map_err(|e| format!("mkdir: {e}"))?;
    #[cfg(windows)]
    {
        let body = if browser == "firefox" {
            firefox_manifest_json(&exe_str)
        } else {
            manifest_json(&exe_str)
        };
        let manifest = home.join(format!("{HOST_ID}-{browser}.json"));
        std::fs::write(&manifest, body).map_err(|e| format!("manifest: {e}"))?;
        let manifest_str = manifest.to_string_lossy().into_owned();
        let subkey = match browser {
            "firefox" => format!("SOFTWARE\\Mozilla\\NativeMessagingHosts\\{HOST_ID}"),
            "edge" => format!("SOFTWARE\\Microsoft\\Edge\\NativeMessagingHosts\\{HOST_ID}"),
            _ => format!("SOFTWARE\\Google\\Chrome\\NativeMessagingHosts\\{HOST_ID}"),
        };
        // Default value ("") holds the manifest path.
        reg_write(&subkey, "", &manifest_str)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let Some(dir) = manifest_dir(browser) else {
            return Err("unsupported browser".to_owned());
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
        let body = if browser == "firefox" {
            firefox_manifest_json(&exe_str)
        } else {
            manifest_json(&exe_str)
        };
        let _ = app_dir;
        std::fs::write(dir.join(format!("{HOST_ID}.json")), body)
            .map_err(|e| format!("manifest: {e}"))?;
        Ok(())
    }
}

/// Per-user writable home for generated manifests (`%LOCALAPPDATA%` on
/// Windows, the data dir elsewhere).
fn manifest_home() -> Result<PathBuf, String> {
    let base = dirs::data_dir().ok_or_else(|| "no user data dir".to_owned())?;
    Ok(base.join("SwiftFetch").join("native-host"))
}

/// Strips Windows verbatim (`\\?\`, `\\?\UNC\`) prefixes browsers reject.
fn normalize_win_path(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        raw.strip_prefix(r"\\?\").unwrap_or(raw).to_owned()
    }
}

fn packaged_extension(app_dir: &Path, browser: &str) -> Option<PathBuf> {
    // Release signing drops versioned .crx/.xpi here (see package script).
    let ext = match browser {
        "firefox" => "xpi",
        _ => "crx",
    };
    app_dir
        .join("extensions")
        .read_dir()
        .ok()?
        .filter_map(|e| e.ok().map(|entry| entry.path()))
        .find(|p| p.extension().is_some_and(|e| e == ext))
}

fn register_extension(browser: &str, package: &Path, version: &str) -> Result<(), String> {
    let pkg = package.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        if browser == "firefox" {
            reg_write(
                "SOFTWARE\\Mozilla\\Firefox\\Extensions",
                FIREFOX_EXT_ID,
                &pkg,
            )?;
            return Ok(());
        }
        let ext_id = CHROME_EXT_ID;
        let (subkey, name, value) = chromium_extension_entry(browser, ext_id, &pkg, version);
        // Value encodes path|version; split for the two registry values.
        let mut parts = value.splitn(2, '|');
        let path = parts.next().unwrap_or("");
        let ver = parts.next().unwrap_or(version);
        reg_write(&subkey, &name, path)?;
        reg_write(&subkey, "version", ver)?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if browser == "firefox" {
            return Err(
                "firefox sideload needs a signed .xpi via store/enterprise policy".to_owned(),
            );
        }
        let Some(dest) = external_extension_path(browser, CHROME_EXT_ID) else {
            return Err("unsupported browser".to_owned());
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        std::fs::write(&dest, external_extension_json(&pkg, version))
            .map_err(|e| format!("sideload entry: {e}"))?;
        Ok(())
    }
}

/// Re-runs browser integration on demand (Settings button, upgrades).
#[tauri::command]
pub async fn integrate_browsers(
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> Result<Vec<BrowserReport>, String> {
    let dir = browser_assets_dir(&app);
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let reports = tokio::task::spawn_blocking(move || integrate(&dir, &version))
        .await
        .map_err(|e| format!("integration task: {e}"))?;
    let store = std::sync::Arc::clone(&state.store);
    tokio::task::spawn_blocking(move || {
        let guard = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        swiftfetch_store::repos::set_setting(
            &guard,
            "browser.integrated",
            &serde_json::to_string(&env!("CARGO_PKG_VERSION")).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("db task: {e}"))??;
    Ok(reports)
}

/// Browser-assets dir: `$RESOURCE/browser-assets` when bundled, else the
/// executable's dir (dev / portable).
fn browser_assets_dir(app: &tauri::AppHandle) -> PathBuf {
    use tauri::Manager as _;
    if let Ok(resource) = app.path().resource_dir() {
        let bundled = resource.join("browser-assets");
        if bundled.is_dir() {
            return bundled;
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// First-run hook for `setup`: integrates once per app version.
pub fn integrate_if_needed(app: &tauri::AppHandle, state: &std::sync::Arc<crate::state::AppState>) {
    let guard = state
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let done: Option<String> = swiftfetch_store::repos::get_setting(&guard, "browser.integrated")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<String>(&v).ok());
    if done.as_deref() == Some(env!("CARGO_PKG_VERSION")) {
        return;
    }
    let dir = browser_assets_dir(app);
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let reports = integrate(&dir, &version);
    tracing::info!(?reports, "browser integration ran");
    // Only stamp success when every native host registered — otherwise the
    // next launch retries instead of silently staying unintegrated.
    if reports.iter().all(|r| r.native_host) {
        let _ = swiftfetch_store::repos::set_setting(
            &guard,
            "browser.integrated",
            &serde_json::to_string(&env!("CARGO_PKG_VERSION")).unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn manifest_json_is_valid_and_pinned() {
        let body = manifest_json("C:\\Program Files\\SwiftFetch\\swiftfetch-native-host.exe");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(value["name"], HOST_ID);
        assert!(
            value["path"]
                .as_str()
                .expect("path")
                .contains("swiftfetch-native-host.exe")
        );
        // No single backslashes survive unescaped.
        assert!(
            value["allowed_origins"][0]
                .as_str()
                .expect("origin")
                .contains(CHROME_EXT_ID)
        );
    }

    #[test]
    fn firefox_manifest_uses_allowed_extensions() {
        let body = firefox_manifest_json("/usr/bin/swiftfetch-native-host");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(value["allowed_extensions"][0], FIREFOX_EXT_ID);
    }

    #[test]
    fn win_paths_lose_verbatim_prefix() {
        assert_eq!(
            normalize_win_path(r"\\?\C:\Program Files\SwiftFetch\h.exe"),
            r"C:\Program Files\SwiftFetch\h.exe"
        );
        assert_eq!(
            normalize_win_path(r"\\?\UNC\server\share\h.exe"),
            r"\\server\share\h.exe"
        );
        assert_eq!(normalize_win_path(r"C:\plain\h.exe"), r"C:\plain\h.exe");
    }

    #[test]
    fn chromium_entry_targets_vendor_subkey() {
        let (key, name, value) =
            chromium_extension_entry("chrome", "abc123", "C:\\x\\e.crx", "1.0.0");
        assert_eq!(key, "Software\\Google\\Chrome\\Extensions\\abc123");
        assert_eq!(name, "path");
        assert!(value.contains("C:\\x\\e.crx"));
        let (edge_key, _, _) = chromium_extension_entry("edge", "abc123", "C:\\x\\e.crx", "1.0.0");
        assert!(edge_key.contains("Microsoft\\Edge"));
    }

    #[cfg(not(windows))]
    #[test]
    fn external_extension_json_shape() {
        let body = external_extension_json("/opt/e.crx", "1.0.0");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(value["external_crx"], "/opt/e.crx");
        assert_eq!(value["external_version"], "1.0.0");
    }
}
