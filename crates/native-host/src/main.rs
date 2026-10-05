//! `SwiftFetch` browser native-messaging host.
//!
//! Speaks length-prefixed JSON over stdio with Chrome/Edge/Opera (MV3) and
//! Firefox, validates extension origins against the install-time allowlist,
//! and forwards capture requests to the running app's localhost IPC.
//! Implementation lands in Milestone 4 (Build Prompt §12.1); this binary is a
//! compiling scaffold in Milestone 0.

fn main() {
    // Scaffold only — protocol handling arrives with Milestone 4.
}
