//! Secret storage seam.
//!
//! Convention from the app's `KeychainStore` (shared/Kontinuum, issue
//! #36's hard rule): one Keychain service per feature, secrets keyed by
//! account name, **never** in UserDefaults, session files, exports or
//! logs. The importer keeps the same shape on the Rust side: tokens go
//! through this trait and nowhere else. iOS binds it to `KeychainStore`
//! (service `dev.kontinuum.app.taste`, accounts
//! `spotify/access-token` / `spotify/refresh-token`); tests and non-
//! Keychain hosts use the in-memory store.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Keychain-shaped secret storage: put/get/delete by account.
pub trait SecretStore: Send + Sync {
    fn set(&self, account: &str, value: &str);
    fn get(&self, account: &str) -> Option<String>;
    fn delete(&self, account: &str);
}

/// Process-memory store. Tests, and hosts that keep their own secure
/// storage and hand tokens in per session.
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemorySecretStore {
    fn set(&self, account: &str, value: &str) {
        if let Ok(mut m) = self.map.lock() {
            m.insert(account.to_string(), value.to_string());
        }
    }

    fn get(&self, account: &str) -> Option<String> {
        self.map.lock().ok().and_then(|m| m.get(account).cloned())
    }

    fn delete(&self, account: &str) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(account);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_deletes() {
        let s = MemorySecretStore::new();
        assert!(s.is_empty());
        s.set("spotify/access-token", "at");
        assert_eq!(s.get("spotify/access-token").as_deref(), Some("at"));
        s.set("spotify/access-token", "at2");
        assert_eq!(s.get("spotify/access-token").as_deref(), Some("at2"));
        s.delete("spotify/access-token");
        assert_eq!(s.get("spotify/access-token"), None);
        assert!(s.is_empty());
    }
}
