//! `SwiftFetch` Supabase Auth (Track A, M-A2 lite): email sign-up/sign-in
//! against Supabase Auth REST, session persistence in the OS keychain, and
//! tier + device-binding reads the desktop app enforces.
//!
//! The publishable key ships in the binary by design (Supabase keys of this
//! class are public — RLS owns authorization). The service key never leaves
//! the admin backend.
//!
//! Design contract: `docs/admin-panel.md` track A; server schema in the
//! `swiftfetch-admin` repo (`supabase/migrations/0002_app_users_devices.sql`).
#![forbid(unsafe_code)]

pub mod client;
pub mod session;

pub use client::{
    AuthClient, AuthConfig, AuthError, AuthTokens, DeviceRegistration, UserProfile, default_config,
};
pub use session::{KeyringStore, MemoryStore, SessionStore};
