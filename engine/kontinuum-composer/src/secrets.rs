//! Secret storage seam for composer BYOK keys (issue #36's hard rule).
//!
//! Convention from the app's `KeychainStore` (shared/Kontinuum): one
//! Keychain service per feature, secrets keyed by account name, **never**
//! in UserDefaults, session files, exports or logs. Same shape as the #21
//! taste importer's seam (`kontinuum-taste::secrets`), copied rather than
//! shared so the composer doesn't couple to the Spotify stack (and its
//! bundled sqlite). iOS binds it to `KeychainStore` (service
//! `dev.kontinuum.app.composer`, accounts from [`ComposerSecrets`]); tests
//! and non-Keychain hosts use the in-memory store. The Rust side never
//! persists keys anywhere else.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Keychain-shaped secret storage: put/get/delete by account.
pub trait SecretStore: Send + Sync {
    fn set(&self, account: &str, value: &str);
    fn get(&self, account: &str) -> Option<String>;
    fn delete(&self, account: &str);
}

/// Process-memory store. Tests, and hosts that keep their own secure
/// storage and hand keys in per session.
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
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

/// Keychain account names, one per configured provider.
pub struct ComposerSecrets;

impl ComposerSecrets {
    /// Account for a provider id ("openai_compat" →
    /// `providers/openai_compat/key`), matching the Swift
    /// `KeychainStore.set(_, account:)` calls in Settings.
    pub fn account_for(provider: &str) -> String {
        format!("providers/{provider}/key")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_deletes() {
        let s = MemorySecretStore::new();
        s.set("providers/openai_compat/key", "sk-1");
        assert_eq!(s.get("providers/openai_compat/key").as_deref(), Some("sk-1"));
        s.delete("providers/openai_compat/key");
        assert_eq!(s.get("providers/openai_compat/key"), None);
    }

    #[test]
    fn accounts_are_namespaced_per_provider() {
        assert_eq!(
            ComposerSecrets::account_for("anthropic"),
            "providers/anthropic/key"
        );
    }
}
