# Reference corpus for learned arrangement (issue #23)

This directory owns the reference corpus that #16's arrangement planner
and #17's groove layer learn from. **No licensed audio is ever committed
here** — the manifest references audio outside the repo, and the only
in-repo tracks are clearly-labeled synthetic fixtures.

## Layout

- `manifest.csv` — the single source of truth for corpus membership.
- `features/` — pipeline OUTPUT (JSONL observations, segmentation report).
  Never audio. Gitignored until a run is archived; regenerate any time.
- `annotations/` — hand-annotation files (`annotations/{track_id}.json`,
  format in `kontinuum-corpus/src/eval.rs`). TODO(#23): the 20 human
  annotations that gate segmentation trust.

## Manifest schema

Header is validated exactly (see `kontinuum_corpus::manifest::HEADER`):

```
track_id,artist,label,year,bpm,subgenre,why_included,file_path,file_hash,hash_algo,license_proof,synthetic,synth_spec
```

- Plain CSV, no quoting; fields must not contain commas, quotes, tabs, or
  control characters.
- `file_path` — ABSOLUTE path to audio outside the repo (private bucket
  mount or local purchase dir). Real rows without it fail loudly.
- `file_hash` + `hash_algo` — file integrity, `fnv1a64` hex only (the
  same FNV-1a convention as the samples store's integrity hashes). This
  is an INTEGRITY check, not cryptographic provenance.
- `license_proof` — the audit trail that a track is legally in the corpus
  (order id / receipt reference). Required on every row; audited under
  the #6 legal memo (EU TDM exception + opt-out reality, US fair use).
- `synthetic` + `synth_spec` — `true` rows render deterministically
  in-process from a preset id (`kontinuum_analysis::synthgen`); they need
  no file and carry the in-repo generator statement as license proof.

Every violation is a typed error (`kontinuum_corpus::ManifestError`),
printed per track with the track id — a bad row never silently skips.

## Pipeline

Versioned and rerunnable; local-Mac-shaped (the cloud batch in PLAN §2.4
is this binary against mounted bucket storage):

```
cargo run --release -p kontinuum-analysis --bin corpus-batch -- \
    --manifest corpus/manifest.csv --out corpus/features
```

Per track: tempo + beat grid (kick-band autocorrelation anchored to the
manifest's declared BPM, then a grid-fit refinement), key (bass-register
chroma vs scale masks), structural segmentation (per-bar
energy/density/brightness novelty, 4-bar minimum sections), boundary-type
classification (silence / filter_sweep / fill / hard_cut), groove stats
(percussive-band 4–10 kHz onsets against the 16th grid → microtiming,
velocity profile, swing). Deterministic end to end: same bytes in, same
observations out. Bump `kontinuum_analysis::PIPELINE_VERSION` when
feature definitions change; a corpus re-run is always a full re-run.

Outputs: `features/observations-{subgenre}.jsonl` (the
`TrackObservation` records the fitters consume),
`features/segmentation-report.json` (per-track boundary F1 vs the
`SEGMENTATION_F1_GATE` = 0.7 from the issue), and the synthetic fixtures'
ground-truth annotations.

## Distribution fitting → shipped artifacts

`kontinuum-corpus` fits per subgenre (Laplace-smoothed section-kind
transition matrix, section-length params, transition-type tables
conditioned on from→to, k≈5 energy-arc centroids + spread, named groove
templates) and emits `arrangement-params-{subgenre}.json` +
`groove-templates-{subgenre}.json` — the exact files the #16 planner
(`kontinuum-compose::structure::StructureParams`) and the #17 groove bank
(`kontinuum-compose::groove::GrooveBank`) already load with zero code
change. Documented deviation from the issue text: the artifacts ship as
JSON, not `.toml`/`.bin` — the workspace forbids new dependencies, `toml`
would be one, and JSON is serde-native for both consumers and
review-diffable; `artifact_version` replaces the binary magic number.

## Selection spec (real corpus, 100–300 tracks)

- ≥ 3 subgenre buckets (minimal techno, microhouse, deep house; optional
  dub techno, ambient), ≥ 30 tracks each.
- Spread across eras, labels, and scenes — no single-label or single-era
  clusters; `why_included` states each track's contribution.
- Analysis-only purchase (Beatport/Bandcamp), stored in the private
  access-logged bucket; legal memo with #6 signed BEFORE batch
  processing.
- A row is appended to `manifest.csv` only with: absolute file path,
  fnv1a64 hash of the purchased file, and a license-proof reference.

### When the purchased corpus lands — exact commands

```sh
# 1. append real rows to corpus/manifest.csv (schema above)
# 2. batch-analyze (fails loudly, per track, on anything missing)
cargo run --release -p kontinuum-analysis --bin corpus-batch -- \
    --manifest corpus/manifest.csv --out corpus/features \
    --annotations corpus/annotations
# 3. human-validate segmentation on the 20 annotated tracks:
#    segmentation-report.json must show F1 >= 0.7 on every annotated track
# 4. fit + emit shipped artifacts
#    (fit_subgenre + write_artifacts over features/*.jsonl; wired in
#    kontinuum-analysis::tests::corpus_pipeline)
# 5. statistical validation: 100 sampled arrangements vs corpus stats
#    (kontinuum-corpus::grammar_sampling style checks)
```

## Detected-label → grammar mapping (honest version)

Detected sections never label themselves "reintro" and never see #16's
grammar. The detector emits roles; `kontinuum-compose::structure::map_kind`
maps them:

| detector label | grammar kind | honest meaning |
|---|---|---|
| `intro` | `Intro` | first section, below-peak energy |
| `build` | `Dev` | after intro, rising, sub-peak |
| `drop` | `Dev` | full-energy arrival after a build/intro |
| `groove` | `Dev` | other full-energy sections |
| `break` | `Breakdown` | mean energy < 0.45 of track max |
| `outro` | `Outro` | declining tail |
| `reintro` | `Reintro` | **never emitted by the detector** — the planner's reintro behavior stays hand-seeded until the detector earns it |

Boundary types (`silence`, `filter_sweep`, `fill`, `hard_cut`) are
heuristic readings of boundary features (energy dip; sustained brightness
rise without collapse; transient/flux spike; step change). They are
counts evidence, not ground truth; the confusion is visible in the
synthetic-corpus report.

## Honesty: what the fit does NOT capture

- **Sound design.** The fit sees bar-level energy/density/brightness and
  boundary shape — not patches, mix texture, or arrangement micro-craft
  (fills' actual drum patterns, automation moves, ear candy). Expectations
  for #26/#27: don't ask arrangement distributions to reproduce "the drop
  sounds expensive"; that is sound design's job.
- **Micro-timing of the arrangement itself**: groove stats capture the
  average 16th grid feel of drums, not per-section performance changes.
- **Section internal structure**: an 8-bar `build` is a mean-energy
  number plus a boundary type, not an 8-step plan.
- **Long-range record-level dramaturgy** beyond the transition matrix and
  arc clusters: energy arcs are k≈5 centroid shapes, not per-track
  narratives.
- **Detector bias**: labels are heuristic roles (see mapping). Until the
  20-track human annotation gate passes on the real corpus, treat every
  fitted number as shape-proven, content-unproven. Until the purchased
  corpus lands, ALL fitted artifacts derive from the synthetic fixtures
  and prove pipeline shape only — consumers must gate on `corpus_size`.
- **Key detection** is bass-register chroma vs scale masks: robust for
  bass-driven dance material, wrong for modal/ambiguous or
  melodically-driven material (documented in `kontinuum-analysis::
  corpus::key`).
