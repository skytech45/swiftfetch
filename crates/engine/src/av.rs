//! Antivirus auto-scan hook (PRD "AV auto-scan hook", M3): every completed
//! download is handed to the system scanner before it reaches the final
//! destination. A flagged file is deleted and the job fails with
//! `E_MALWARE_DETECTED`; a scanner that cannot run (not installed, exec
//! failure) yields `Skipped` and never blocks the download.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Result of scanning one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanOutcome {
    /// No threat found.
    Clean,
    /// The scanner flagged the file.
    Flagged {
        /// Human-readable detection name.
        detection: String,
    },
    /// The scanner could not run (not installed, spawn/timeout failure).
    /// Downloads proceed — a broken scanner must never block the app.
    Skipped {
        /// Why the scan did not run.
        reason: String,
    },
}

/// A virus scanner backend.
pub trait AvScanner: Send + Sync {
    /// Human-readable scanner name (logs + error messages).
    fn name(&self) -> &'static str;

    /// Scans a file. Implementations must be argument-vector-only (no
    /// shell) and should avoid blocking for long.
    fn scan(&self, path: &Path) -> ScanOutcome;
}

/// Shared scanner handle (engine config).
pub type AvScannerRef = Arc<dyn AvScanner>;

/// Windows Defender scanner via `MpCmdRun.exe` (command-line scanner shipped
/// with Defender). Located at runtime; never vendored or downloaded.
#[derive(Debug)]
pub struct WindowsDefender {
    exe: PathBuf,
}

impl WindowsDefender {
    /// Locates `MpCmdRun.exe` (stable install first, then the newest
    /// versioned Platform directory). `None` = Defender CLI not present.
    #[must_use]
    pub fn detect() -> Option<Self> {
        let program_files = std::env::var_os("ProgramFiles")
            .map_or_else(|| PathBuf::from("C:\\Program Files"), PathBuf::from);
        let stable = program_files.join("Windows Defender").join("MpCmdRun.exe");
        if stable.is_file() {
            return Some(Self { exe: stable });
        }
        let platform_dir = std::env::var_os("ProgramData")
            .map(PathBuf::from)?
            .join("Microsoft")
            .join("Windows Defender")
            .join("Platform");
        let mut candidates: Vec<PathBuf> = std::fs::read_dir(&platform_dir)
            .ok()?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path().join("MpCmdRun.exe"))
            .filter(|p| p.is_file())
            .collect();
        // Highest version directory wins (Platform subdirs are named like
        // `4.18.24090.11`), so a plain string sort on the parent works.
        candidates.sort_by(|a, b| b.parent().cmp(&a.parent()));
        candidates.into_iter().next().map(|exe| Self { exe })
    }
}

impl AvScanner for WindowsDefender {
    fn name(&self) -> &'static str {
        "windows-defender"
    }

    fn scan(&self, path: &Path) -> ScanOutcome {
        // MpCmdRun exit codes: 0 = no threat, 2 = threat found, anything
        // else = the scan itself failed (treated as skipped).
        let output = std::process::Command::new(&self.exe)
            .args(["-Scan", "-ScanType", "3", "-File"])
            .arg(path)
            .output();
        match output {
            Ok(output) if output.status.code() == Some(0) => ScanOutcome::Clean,
            Ok(output) if output.status.code() == Some(2) => ScanOutcome::Flagged {
                detection: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            },
            Ok(output) => ScanOutcome::Skipped {
                reason: format!("MpCmdRun exited with {:?}", output.status.code()),
            },
            Err(err) => ScanOutcome::Skipped {
                reason: format!("MpCmdRun spawn failed: {err}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    #[test]
    fn defender_detect_is_safe_when_absent() {
        // Must never panic regardless of environment; Some is fine when the
        // machine has Defender (the dev box does), None elsewhere.
        let _ = WindowsDefender::detect();
    }
}
