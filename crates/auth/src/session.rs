//! Session persistence: OS keychain in production, memory in tests.
//!
//! Only tokens live here — never passwords. The keychain entry is
//! `SwiftFetch / supabase-session`.

use crate::client::AuthTokens;

/// Stored-session backend.
pub trait SessionStore {
    /// Loads tokens, if any.
    ///
    /// # Errors
    ///
    /// Returns a message when the backend cannot be read.
    fn load(&self) -> Result<Option<AuthTokens>, String>;
    /// Persists tokens.
    ///
    /// # Errors
    ///
    /// Returns a message when the backend cannot be written.
    fn save(&self, tokens: &AuthTokens) -> Result<(), String>;
    /// Clears tokens (sign-out).
    ///
    /// # Errors
    ///
    /// Returns a message when the backend cannot be cleared.
    fn clear(&self) -> Result<(), String>;
}

/// OS-keychain session store (production).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new("SwiftFetch", "supabase-session").map_err(|e| e.to_string())
    }
}

impl SessionStore for KeyringStore {
    fn load(&self) -> Result<Option<AuthTokens>, String> {
        match Self::entry()?.get_password() {
            Ok(secret) => serde_json::from_str(&secret)
                .map(Some)
                .map_err(|e| e.to_string()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(other.to_string()),
        }
    }

    fn save(&self, tokens: &AuthTokens) -> Result<(), String> {
        let secret = serde_json::to_string(tokens).map_err(|e| e.to_string())?;
        Self::entry()?
            .set_password(&secret)
            .map_err(|e| e.to_string())
    }

    fn clear(&self) -> Result<(), String> {
        match Self::entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(other) => Err(other.to_string()),
        }
    }
}

/// In-memory session store (tests and keychain-less environments).
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: std::sync::Mutex<Option<String>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SessionStore for MemoryStore {
    fn load(&self) -> Result<Option<AuthTokens>, String> {
        let guard = self.inner.lock().map_err(|e| e.to_string())?;
        guard
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e: serde_json::Error| e.to_string())
    }

    fn save(&self, tokens: &AuthTokens) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
        *guard = Some(serde_json::to_string(tokens).map_err(|e| e.to_string())?);
        Ok(())
    }

    fn clear(&self) -> Result<(), String> {
        let mut guard = self.inner.lock().map_err(|e| e.to_string())?;
        *guard = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn tokens() -> AuthTokens {
        AuthTokens {
            access_token: "access".into(),
            refresh_token: "refresh".into(),
            user_id: "user-1".into(),
            email: "user@example.com".into(),
            expires_at: 9_999_999_999,
        }
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        assert!(store.load().expect("load").is_none());
        store.save(&tokens()).expect("save");
        let loaded = store.load().expect("load").expect("some");
        assert_eq!(loaded.email, "user@example.com");
        store.clear().expect("clear");
        assert!(store.load().expect("load").is_none());
    }
}
