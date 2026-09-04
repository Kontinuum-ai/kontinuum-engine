# Starter sample library (issue #19)

Self-generated curated one-shots — the sourcing route locked in the issue
comments: packs we render ourselves, zero clearance risk, fully owned.

- **Content**: 6 one-shots (kick, closed/open hat choke pair, click,
  shaker, granular texture bed) rendered from the engine's own synth
  voices through `kontinuum_samples::render_recipe`.
- **Provenance**: `manifest.json` (`license`, `source`, per-entry
  `pcm_hash`, engineered features). License is CC0-1.0 / self-generated.
- **Choke groups**: the hat pair shares `choke_group: 1` (909 open/closed
  convention); assignment rides `choke:N` tags in the recipes.
- **Regeneration** (recipe + seed = bit-identical WAVs and manifest):

  ```sh
  cargo run --release -p kontinuum-offline --bin genpack \
      assets/samples/recipes assets/samples
  ```

Integrity is pinned by `engine/kontinuum-samples/tests/library.rs`.
