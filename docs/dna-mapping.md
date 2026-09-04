# DNA → engine mapping (#21)

Every field of the canonical on-device taste profile
(`kontinuum-compose::taste::TasteProfile` — **the** musical-DNA struct,
schema version `DNA_VERSION = 2`) and where it lands in the engine.
Acceptance rule from #21: **every DNA field either maps to a knob or is
explicitly marked unused** — this document is that accounting. Audited
against source on 2026-09-04 (feat/taste-importer-21).

## One struct, shared

- **#21 importer** (`kontinuum-taste`) — writes the profile from
  connectors + on-device audio analysis.
- **#22 composer context** — consumes `TasteProfile::summary()` as
  `ContextInputs.taste_summary` (a compact ≤320-char line the context
  clamps anyway). The composer stays string-typed by design; the summary
  is produced from the canonical struct, not a second definition.
- **#24 learner ladder** — `kontinuum-taste::map::taste_priors_for_dna`
  expands the profile's points into `TastePriors` bands via the existing
  `from_profile_point` (±0.2 half-width). B0 keeps passing the DNA
  through unchanged.
- Back-compat: v1 documents (no `dna_version` field, no v2 fields)
  deserialize as the v2 default profile; the bridge's
  `kontinuum_generate_session_from_taste` JSON path is unchanged.

## Profile fields → knobs

| DNA field | Engine knob | Path | Status |
|---|---|---|---|
| `bpm` | `GenParams.bpm` → tempo lane | `gen_params_for_taste` → `generate_session` | ✅ mapped |
| `tempo_dispersion` | — (informational) | reported in `summary()`; a tempo-*range* lane is a `tempo_lane` follow-up | ⚠️ **marked unused downstream v2** — dispersion widens the summary, not yet the lane |
| `energy` | `GenParams.intensity` → section energy curves (`plan_structure`), groove-pick intensity | `gen_params_for_taste` → `arrangement.rs` | ✅ mapped |
| `darkness` | `GenParams.darkness` → progression bias toward all-minor templates | `gen_params_for_taste` | ✅ mapped |
| `density` | `GenParams.density` → binding odds + onset budgets | `gen_params_for_taste` | ✅ mapped |
| `variation` | `GenParams.variation` → cross-section fills/redraws | `gen_params_for_taste` | ✅ mapped |
| `genres` | (a) `apply_genre_nudge` seasoning on the profile itself; (b) `GenParams.genre` → genre profiles + palette/rack selection | `taste.rs` + `gen_params_for_taste` | ✅ mapped |
| `genre_mix` | ranks `genres` (top-8 → genre knob above); mix itself is the transparency surface (#33) | `model::EntityGraph::into_profile` | ✅ mapped (via `genres`) |
| `swing` (Stat) | `GenParams.groove` — nearest-swing template from `groove::ALL` (`groove::nearest_swing`) | `gen_params_for_taste` (v2 wiring) | ✅ mapped — audio DNA lands in the session |
| `swing.dispersion` | — | reported; wide-swing ⇒ wider groove sampling is a `groove_bank` follow-up | ⚠️ **marked unused downstream v2** |
| `brightness` (Stat) | — | #27's mix targets are per-genre; a taste-level tilt override is a follow-up | ⚠️ **marked unused downstream v2** (learned + surfaced, not wired) |
| `adventurousness` | composer exploration budget | `kontinuum-taste::map::composer_bias_for_dna` → `reward::ComposerBias.exploration_budget` (0.1..=0.4, mirroring `reward::evaluate`) | ✅ mapped (v2 wiring) |
| `era_weights` | — | scene/era → `GenParams.world` selection (#30) is designed but not taste-weighted yet | ⚠️ **marked unused downstream v2** (learned + surfaced, not wired) |
| `scene_weights` | — | same as era_weights | ⚠️ **marked unused downstream v2** |
| `section_bars` (Stat) | — | `GenParams.structure` is artifact-built (`StructureParams::load_json`); a taste-built structure needs a constructor | ⚠️ **marked unused downstream v2** (learned + surfaced, not wired) |
| `dna_version` | — | schema gate (meta field, not a knob) | ℹ️ meta |

## Where the profile comes from

- `kontinuum-taste::spotify::SpotifySource` — the reference
  [`TasteSource`](../engine/kontinuum-taste/src/source.rs): Auth Code +
  PKCE, refresh, paginated playlist/saved/top/recently-played pulls with
  429/5xx backoff and incremental cursors; disconnect = full purge.
  Metadata-only by design (PLAN §4): no audio-features, no previews.
- `kontinuum-taste::model` — entity graph (artists/genres/labels,
  weighted saved 1.0 > top 0.9 > playlisted 0.6 > recently-played 0.3,
  90-day recency half-life) → genre-mix, era/scene weights,
  adventurousness (normalized Shannon entropy of the genre mix).
- `kontinuum-taste::enrich` — MusicBrainz (keyless, 1 req/s enforced)
  behind `EnrichmentProvider` on the same transport seam; Discogs
  interface ready, disabled without a token.
- `kontinuum-taste::audio` — per-track DNA through the #5 on-device
  subset (`kontinuum-analysis::corpus`): tempo, swing, brightness, energy,
  density, section stats. User DNA = weighted mean + dispersion;
  pinned references weigh 4× (`PIN_WEIGHT`). **Abstract features only —
  no audio retention** (enforced in `tests/privacy.rs`).
- `kontinuum-compose::reference` — unchanged single-WAV path for
  Settings → "Adapt to a WAV reference file".

## Available knobs taste does not set yet

| Knob | Exists | Intended taste source | Status |
|---|---|---|---|
| `GenParams.groove_bank` | yes (#23 artifacts) | swing dispersion → bank sampling | 🔜 wired-inactive |
| `GenParams.structure` | yes (#23/#16) | `section_bars` stat → taste-built structure | 🔜 needs a `StructureParams` constructor |
| `GenParams.world` | yes (#30) | era/scene weights → world selection | 🔜 wired-inactive |
| #27 mix targets | per-genre | `brightness` stat → tilt override | 🔜 follow-up on #27 |

## Privacy invariant

Every field above is computed **on-device**; nothing in this mapping
performs or requires network I/O during playback — the playback path
(`map::session_from_dna` and everything it calls) takes no transport at
all, regression-tested with a fail-loud transport
(`kontinuum-taste/tests/privacy.rs::playback_never_touches_the_transport`).
Import-time network use is governed by per-source consent
(`store::Consent`, checked before any request), tokens live only in the
Keychain-bound secret store, and the cloud heavy-analysis tier of
PLAN §2.4 does not exist in this work (on-device-first).
