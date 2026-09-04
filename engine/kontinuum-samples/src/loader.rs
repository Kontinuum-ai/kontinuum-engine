//! Control-thread pack loading (#53 step 3): recipes render off the audio
//! thread into cached PCM keyed by recipe hash; the engine hot-loads the
//! Arc at the next musical boundary. Rendering far outpaces real time, so
//! a micro-kit loads in well under a second without touching the RT path.

use std::collections::BTreeMap;
use std::sync::Arc;

use kontinuum_ir::SampleSlot;

use crate::schema::{recipe_hash, RecipeError, SampleRecipe};
use crate::render::render_recipe;
use crate::stretch::{stretch, StretchMode};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("recipe invalid: {0}")]
    Invalid(#[from] RecipeError),
    #[error("recipe parse failed: {0}")]
    Parse(String),
}

/// Deduplicating cache of rendered packs. Same hash → same bytes, so a
/// session importing ten tracks that share a kit renders it once.
#[derive(Default)]
pub struct PackLoader {
    cache: BTreeMap<u64, Arc<[f32]>>,
    sample_rate: u32,
}

impl PackLoader {
    pub fn new(sample_rate: u32) -> Self {
        PackLoader { cache: BTreeMap::new(), sample_rate }
    }

    /// Parse, validate, render (unless cached), and store. Returns the hash.
    pub fn load(&mut self, recipe_json: &str) -> Result<u64, LoadError> {
        let recipe: SampleRecipe =
            serde_json::from_str(recipe_json).map_err(|e| LoadError::Parse(e.to_string()))?;
        let hash = recipe_hash(&recipe);
        if !self.cache.contains_key(&hash) {
            let rendered = render_recipe(&recipe)?;
            if rendered.sample_rate != self.sample_rate {
                return Err(LoadError::Parse(format!(
                    "recipe sample rate {} != engine {}",
                    rendered.sample_rate, self.sample_rate
                )));
            }
            self.cache.insert(hash, rendered.pcm.into());
        }
        Ok(hash)
    }

    /// [`PackLoader::load`] for a session's sample slot (issue #19 v1):
    /// the slot's `stretch` field time-stretches the rendered pack
    /// control-side (WSOLA — pitch preserved) before it enters the cache,
    /// so the RT path stays repitch-only. Tuned variants cache under a key
    /// derived from the recipe hash + factor, so the untouched pack and
    /// every stretch factor coexist.
    pub fn load_for_slot(&mut self, recipe_json: &str, slot: &SampleSlot) -> Result<u64, LoadError> {
        let recipe: SampleRecipe =
            serde_json::from_str(recipe_json).map_err(|e| LoadError::Parse(e.to_string()))?;
        let base_hash = recipe_hash(&recipe);
        let key = match slot.stretch {
            None => base_hash,
            Some(factor) => {
                let mut bytes = Vec::with_capacity(12);
                bytes.extend_from_slice(&base_hash.to_le_bytes());
                bytes.extend_from_slice(&factor.to_bits().to_le_bytes());
                kontinuum_core::fnv1a64(&bytes)
            }
        };
        if !self.cache.contains_key(&key) {
            let rendered = render_recipe(&recipe)?;
            if rendered.sample_rate != self.sample_rate {
                return Err(LoadError::Parse(format!(
                    "recipe sample rate {} != engine {}",
                    rendered.sample_rate, self.sample_rate
                )));
            }
            let pcm = match slot.stretch {
                None => rendered.pcm,
                Some(factor) => stretch(
                    &rendered.pcm,
                    self.sample_rate,
                    StretchMode::Wsola,
                    factor,
                )?,
            };
            self.cache.insert(key, pcm.into());
        }
        Ok(key)
    }

    /// Shared PCM for a loaded pack, ready for `AudioGraph::attach_sampler`.
    pub fn pcm(&self, hash: u64) -> Option<Arc<[f32]>> {
        self.cache.get(&hash).cloned()
    }

    pub fn loaded(&self) -> Vec<u64> {
        self.cache.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIT: &str = r#"{
        "version": 1, "seed": 7, "name": "kit",
        "tail_ms": 100,
        "voices": [{"id": "cl", "instrument": {"kind": "hat", "decay_ms": 30}}],
        "hits": [{"voice": "cl", "at_ms": 0.0, "velocity": 0.9}]
    }"#;

    #[test]
    fn loads_once_and_shares_bytes() {
        let mut loader = PackLoader::new(48_000);
        let h1 = loader.load(KIT).expect("load");
        let h2 = loader.load(KIT).expect("reload");
        assert_eq!(h1, h2, "identical recipes share a cache entry");
        assert_eq!(loader.loaded().len(), 1);
        let pcm = loader.pcm(h1).expect("pcm");
        assert!(!pcm.is_empty());
        let again = loader.pcm(h1).expect("pcm");
        assert!(Arc::ptr_eq(&pcm, &again), "cache hands out the same Arc");
    }

    #[test]
    fn bad_documents_are_rejected_without_polluting_the_cache() {
        let mut loader = PackLoader::new(48_000);
        assert!(matches!(loader.load("{not json"), Err(LoadError::Parse(_))));
        assert!(loader.load(r#"{"version":2,"seed":1,"name":"x","voices":[],"hits":[]}"#).is_err());
        assert!(loader.loaded().is_empty());
    }

    #[test]
    fn sample_rate_mismatch_is_rejected() {
        let mut loader = PackLoader::new(44_100);
        assert!(loader.load(KIT).is_err(), "engine rate must match recipe");
    }

    #[test]
    fn slot_stretch_changes_length_and_caches_per_factor() {
        use kontinuum_ir::SampleSlot;
        let slot = |stretch: Option<f32>| SampleSlot {
            kind: kontinuum_ir::SampleTag::Sample,
            query: None,
            id: None,
            recipe_hash: None,
            transpose: None,
            fine: None,
            stretch,
            choke_group: None,
            granular: None,
        };
        let mut loader = PackLoader::new(48_000);

        let base = loader.load_for_slot(KIT, &slot(None)).expect("load");
        let plain = loader.load(KIT).expect("load");
        assert_eq!(base, plain, "unstretched slot shares the pack's cache entry");

        let fast = loader.load_for_slot(KIT, &slot(Some(2.0))).expect("load");
        let slow = loader.load_for_slot(KIT, &slot(Some(0.5))).expect("load");
        assert_ne!(fast, base);
        assert_ne!(slow, base);
        assert_eq!(loader.loaded().len(), 3, "base + two factors coexist");

        let pcm_base = loader.pcm(base).expect("pcm");
        let pcm_fast = loader.pcm(fast).expect("pcm");
        let pcm_slow = loader.pcm(slow).expect("pcm");
        // WSOLA: 2x tempo halves the duration; 0.5x doubles it.
        assert!(
            (pcm_fast.len() as f32 - pcm_base.len() as f32 / 2.0).abs() < 2_048.0,
            "2x stretch length off: {} vs {}",
            pcm_fast.len(),
            pcm_base.len()
        );
        assert!(
            (pcm_slow.len() as f32 - pcm_base.len() as f32 * 2.0).abs() < 4_096.0,
            "0.5x stretch length off: {} vs {}",
            pcm_slow.len(),
            pcm_base.len()
        );

        // Deterministic: same slot → same key → same bytes.
        let again = loader.load_for_slot(KIT, &slot(Some(2.0))).expect("reload");
        assert_eq!(again, fast);
        assert!(Arc::ptr_eq(
            loader.pcm(fast).as_ref().unwrap(),
            loader.pcm(again).as_ref().unwrap()
        ));
    }
}
