//! Per-role model assignment (issue #36): `quick_moves` (frequent 8-bar
//! diffs) and `deep_planning` (arrangement rewrites, custom instrument
//! design) each map to their own [`BackendConfig`]. The zero-configuration
//! default is on-device for both — the app must work forever with no keys
//! and in airplane mode (PLAN §2.4).

use serde::{Deserialize, Serialize};

use crate::scripted::BackendConfig;

/// The two composer roles (issue #36). Serialized snake_case for settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerRole {
    QuickMoves,
    DeepPlanning,
}

impl ComposerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ComposerRole::QuickMoves => "quick_moves",
            ComposerRole::DeepPlanning => "deep_planning",
        }
    }
}

/// Both role slots. Hosts persist this JSON; defaults build the on-device
/// floor so an untouched install plans without any provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoleConfig {
    #[serde(default = "on_device_default_quick")]
    pub quick_moves: BackendConfig,
    #[serde(default = "on_device_default_deep")]
    pub deep_planning: BackendConfig,
}

fn on_device_default_quick() -> BackendConfig {
    BackendConfig::on_device_default(ComposerRole::QuickMoves.as_str())
}

fn on_device_default_deep() -> BackendConfig {
    BackendConfig::on_device_default(ComposerRole::DeepPlanning.as_str())
}

impl Default for RoleConfig {
    fn default() -> Self {
        RoleConfig {
            quick_moves: on_device_default_quick(),
            deep_planning: on_device_default_deep(),
        }
    }
}

impl RoleConfig {
    pub fn backend_for(&self, role: ComposerRole) -> &BackendConfig {
        match role {
            ComposerRole::QuickMoves => &self.quick_moves,
            ComposerRole::DeepPlanning => &self.deep_planning,
        }
    }

    pub fn backend_for_mut(&mut self, role: ComposerRole) -> &mut BackendConfig {
        match role {
            ComposerRole::QuickMoves => &mut self.quick_moves,
            ComposerRole::DeepPlanning => &mut self.deep_planning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roles_are_on_device() {
        let cfg = RoleConfig::default();
        for role in [ComposerRole::QuickMoves, ComposerRole::DeepPlanning] {
            let backend = cfg.backend_for(role);
            assert_eq!(backend.provider, "heuristic", "{role:?} defaults on-device");
            assert_eq!(backend.role, role.as_str());
        }
    }

    #[test]
    fn roles_serialize_and_parse_legacy_free() {
        let cfg = RoleConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: RoleConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(
            serde_json::to_string(&ComposerRole::QuickMoves).unwrap(),
            "\"quick_moves\""
        );
        // A settings file with only one role slot still parses: the other
        // defaults to on-device.
        let partial: RoleConfig =
            serde_json::from_str(r#"{"quick_moves":{"role":"quick_moves","provider":"heuristic"}}"#).unwrap();
        assert_eq!(partial.deep_planning.provider, "heuristic");
    }

    #[test]
    fn role_slots_are_independently_assignable() {
        let mut cfg = RoleConfig::default();
        cfg.backend_for_mut(ComposerRole::DeepPlanning).provider = "anthropic".into();
        assert_eq!(cfg.backend_for(ComposerRole::QuickMoves).provider, "heuristic");
        assert_eq!(cfg.backend_for(ComposerRole::DeepPlanning).provider, "anthropic");
    }
}
