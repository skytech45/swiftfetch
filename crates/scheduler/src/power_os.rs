//! Real [`PowerActions`] implementations. Windows uses the documented
//! Win32 calls (`SetSuspendState`, `InitiateSystemShutdownExW` with the
//! `SE_SHUTDOWN_NAME` privilege enabled per-call); never elevated. macOS
//! drives System Events via `osascript`; Linux talks to logind through
//! `loginctl`/`systemctl`. Every invocation is a plain process spawn with an
//! argument vector — never a shell string (Build Prompt §3).

use crate::power::PowerActions;

/// Platform power actions for the current OS.
#[derive(Debug, Default)]
pub struct SystemPower;

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Power::SetSuspendState;
    use windows::Win32::System::Shutdown::{
        InitiateSystemShutdownExW, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION,
        SHTDN_REASON_MINOR_MAINTENANCE, SHUTDOWN_REASON,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{PCWSTR, PWSTR};

    use super::SystemPower;
    use crate::power::PowerActions;

    /// Enables a privilege on the current process token for the duration of
    /// the call (system-design §4.3: `SE_SHUTDOWN_NAME` per-call, never
    /// run elevated).
    fn with_privilege<T>(privilege: &str, f: impl FnOnce() -> T) -> T {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &raw mut token,
            )
            .is_err()
            {
                return f();
            }
            let mut value = privilege.encode_utf16().collect::<Vec<_>>();
            value.push(0);
            let mut luid = LUID::default();
            if LookupPrivilegeValueW(
                PCWSTR::null(),
                PCWSTR::from_raw(value.as_ptr()),
                &raw mut luid,
            )
            .is_err()
            {
                let _ = CloseHandle(token);
                return f();
            }
            let mut tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let _ = AdjustTokenPrivileges(token, false, Some(&raw mut tp), 0, None, None);
            let result = f();
            let _ = CloseHandle(token);
            result
        }
    }

    impl PowerActions for SystemPower {
        fn sleep_system(&self) -> Result<(), String> {
            // SetSuspendState(false) suspends; no privilege required.
            let ok = unsafe { SetSuspendState(false, false, false) };
            if ok {
                Ok(())
            } else {
                Err("SetSuspendState failed (windows)".into())
            }
        }

        fn hibernate(&self) -> Result<(), String> {
            let ok = unsafe { SetSuspendState(true, false, false) };
            if ok {
                Ok(())
            } else {
                Err("SetSuspendState(hibernate) failed (windows)".into())
            }
        }

        fn shutdown(&self) -> Result<(), String> {
            with_privilege("SeShutdownPrivilege", || {
                let reason = SHUTDOWN_REASON(
                    SHTDN_REASON_FLAG_PLANNED.0
                        | SHTDN_REASON_MAJOR_APPLICATION.0
                        | SHTDN_REASON_MINOR_MAINTENANCE.0,
                );
                // Null machine name = local machine; timeout 0 because the
                // user-facing countdown already ran in `countdown_then_action`.
                unsafe {
                    InitiateSystemShutdownExW(PWSTR::null(), PWSTR::null(), 0, false, false, reason)
                }
                .map_err(|err| format!("InitiateSystemShutdownExW failed (windows): {err}"))
            })
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::process::Command;

    use super::SystemPower;
    use crate::power::PowerActions;

    fn run(program: &str, args: &[&str]) -> Result<(), String> {
        // argv array only — no shell (Build Prompt §3).
        match Command::new(program).args(args).status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("{program} exited with {status}")),
            Err(err) => Err(format!("{program}: {err}")),
        }
    }

    impl PowerActions for SystemPower {
        fn sleep_system(&self) -> Result<(), String> {
            if cfg!(target_os = "macos") {
                run(
                    "osascript",
                    &["-e", "tell application \"System Events\" to sleep"],
                )
            } else {
                run("loginctl", &["suspend"])
            }
        }

        fn hibernate(&self) -> Result<(), String> {
            // macOS exposes no supported hibernate path via System Events;
            // fall back to suspend. Linux: logind hibernate.
            if cfg!(target_os = "macos") {
                run(
                    "osascript",
                    &["-e", "tell application \"System Events\" to sleep"],
                )
            } else {
                run("systemctl", &["hibernate"])
            }
        }

        fn shutdown(&self) -> Result<(), String> {
            if cfg!(target_os = "macos") {
                run(
                    "osascript",
                    &["-e", "tell application \"System Events\" to shut down"],
                )
            } else {
                run("loginctl", &["poweroff"])
            }
        }
    }
}

/// Resolves the platform implementation: a real `SystemPower` everywhere.
#[must_use]
pub fn system_power() -> std::sync::Arc<dyn PowerActions> {
    std::sync::Arc::new(SystemPower)
}
