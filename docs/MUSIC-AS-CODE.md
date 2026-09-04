# Music as code — the vision, the architecture, the path from here

Status: roadmap direction (2026-09-04). This document sets the long-term shape every
phase must converge on. Issues remain the ground truth for what is built; this is the
ground truth for where it is going.

## 1. The vision

We are not building a Suno competitor and not an Ableton clone. We are building a
**code-native, generative way of creating music**.

- **Music as code.** Tracks are visualizations of executable musical code. The structured
  representation, not audio clips and not MIDI clips, is the source of truth for everything:
  instruments, synthesis, samples, patterns, melodies, effects, automation, arrangement,
  mixing, modulation, probability and generative rules.
- **A DAW that nobody has to code in.** The interface feels like a modern DAW. Moving a
  note, changing an instrument, drawing automation, muting a bar: every gesture rewrites the
  underlying code. The code view and the track view are two projections of one document.
- **Generative is architectural, not an AI button.** A composition carries rules,
  probabilities, controlled randomness and transformations, so the same composition generates
  performances that are unique every time and recognizably the same piece.
- **AI edits the representation.** The composer proposes changes to the code, validated like
  any other edit. It never outputs flattened audio and never touches the audio thread.

Loop: `Code ↔ Visual Tracks ↔ Generative Engine ↔ Audio`.

## 2. Where the implementation already is

Most of the foundation exists; the gap is that the *generative rules* live in engine code
rather than in the musical document.

| Already true | Where |
|---|---|
| The document is strict, typed, seed-rooted code | `ir/kontinuum-ir`: `Session { seed, tempo_lane, key, sections, tracks, palette, souls }`, `deny_unknown_fields`, JSON Schema export |
| Patterns are partly generative | `Pattern::{Steps, Euclidean, ProbabilityMask}` |
| Samples are code | deterministic recipes rendered from JSON + seed (#53) |
| Instruments are typed data, custom instruments are patch graphs | `InstrumentDef::*`, `InstrumentDef::Custom` (#37, #43) |
| Mutation is an API, not a UI side effect | `IrDiff` (10 ops), future-anchored, validated, machine-readable repair codes; the LLM already writes through it (#22) |
| Determinism is enforced | seed hierarchy in `kontinuum-clock`, bit-identical renders, CI golden hash, critic ratchet |
| Arrangement is generative | `kontinuum-compose`: 8-kind section grammar, coupled energy curves, transition catalog, motif memory (#16) |

| Not yet true | Gap |
|---|---|
| The code view shows the source | the app's code stream is a performance *log* in glyphs, not the document |
| Generative rules are in the document | genre templates, concurrency caps, energy→density mappings, the section grammar all live in Rust |
| Patterns compose | no expression layer: no `every`, `sometimes`, `rotate`, `degrade`, `choose`, no melodic generators |
| Edits on generated material survive regeneration | no override or lock nodes; "locked from the AI" exists as an idea, not a schema |
| Every parameter has an address | automation, modulation, AI diffs and UI each address parameters differently |
| UI editors write through the diff API | some editors mutate state directly |

## 3. The layered architecture

```
SCORE          generative source: generators, rules, probabilities, transformations,
               overrides, locks, seed.  This is "the code".
   │ expand    pure, seeded, on the control thread, at lookahead (bar boundaries)
   ▼
PERFORMANCE    concrete IR — today's Session: literal patterns, lanes, sections.
               What will actually play. What the track view draws.
   │ compile   exists: incremental compiler → CompiledBlocks
   ▼
COMPILED BLOCKS  sample-accurate events
   │ render    exists: alloc-free, lock-free real-time core
   ▼
AUDIO
```

Projections over the Score:

- **Code view** renders the Score in its text form. Round-trippable to JSON.
- **Track view** (the DAW surface) draws the Performance and edits the Score.
- **The composer** (any LLM) proposes Score diffs.
- **DJ controls, Shortcuts, AirPods, scripts** are Score diffs too.

**One mutation API.** `ScoreDiff` (a superset of today's `IrDiff`) is the only write path.
Human gestures, machine rules and model proposals are peers: same validator, same repair codes,
same future-anchoring, same determinism.

### 3.1 Four additions to the IR

1. **Generators as nodes.** `Pattern::Expr` — a combinator tree over pattern values, seeded
   per node path so expansion is pure. Rhythm: `steps("x..x..x.")`, `euclid(k, n, rot)`,
   `every(n, f)`, `sometimes(p, f)`, `degrade(p)`, `rotate(n)`, `offset(beats)`,
   `choose([...], weights)`, `chain`, `stack`, `humanize(ms)`. Melody: `fit(scale)`,
   `walk(scale, range, step_dist)`, `arp(mode)`, `motif(id).transform(invert | retro |
   transpose(n))`. Today's `Euclidean` and `ProbabilityMask` become expressions.
2. **Rules in the document.** The arrangement grammar, per-style concurrency caps,
   energy→density mappings and the transition catalog move out of `kontinuum-compose` Rust
   into Score data. A style pack (#56) is a Score fragment. `kontinuum-compose` becomes an
   interpreter of rules, not their author.
3. **Overrides and locks.** `Override { path, at: musical-time range, value }` is a concrete
   edit that survives regeneration. `Lock { path, until }` excludes a region from generators
   and from the composer. A gesture on generated material produces an override, unless the
   gesture maps cleanly onto a generator parameter, in which case it edits the parameter.
   This is how the DAW stays intuitive while the source stays generative.
4. **Uniform parameter addressing.** Every mutable value has a path:
   `tracks/bass/instrument/filter/cutoff`, `sections/A/energy`,
   `tracks/hat/pattern/args/density`. Automation lanes, modulation sources (LFO, envelope
   follower, sidechain, random walk, macro), overrides, AI diffs and the UI all target paths.
   Modulation nodes map source→path with depth and expand at lookahead into automation lanes,
   so the real-time core is untouched.

### 3.2 Text form

JSON stays canonical (validation, diffs, storage, LLM emission under guided decoding). The
code view renders a **text projection**: compact, readable, round-trippable.
`kontinuum-ir` gains `to_text` / `from_text` with a golden round-trip corpus. Constraint:
every text construct maps 1:1 onto a JSON node; no semantics exist only in text. Step
mini-notation (`"x . . x . . x ."`) follows the Strudel/Tidal convention because it is the
proven human-readable form.

### 3.3 Invariants that do not move

- The audio thread sees only compiled blocks. Expansion never runs in render.
- Every `ScoreDiff` passes the validator: bounds, schema, CPU budget, repair loop.
- `hash(Score + seed) → identical Performance → identical audio`. Expansion is pure; every
  generator draws from `Rng::derive(path)`.
- Edits only touch music that has not played. Overrides carry musical-time anchors.
- Degrade, never silence: an unresolvable generator falls back to its last good expansion,
  then to the watchdog's fallback arrangement.
- A today's `Session` loads unchanged as a Score containing only literal patterns.
  Zero-migration is guaranteed through M2.

## 4. The progressive path

Each milestone ships something a user can see, and follows the repo's own rule: IR and
validator first, surface second.

| Milestone | Ships | Proves |
|---|---|---|
| **M0 — Truth in the code view** | `to_text`; the code view renders the real Session IR, read-only, with the playing bar highlighted. Replaces the glyph log. | Tracks are visualizations of code, with zero new semantics. |
| **M1 — One mutation API** | Every UI editor (drum grid, manual editor, drum machines, instrument params) emits `IrDiff` through the validated path. Direct-mutation paths deleted. `apply_diff` is the only write across the FFI. | Human, UI and AI are peers. DJ controls become diffs for free. |
| **M2 — Generators as nodes** | `Pattern::Expr`, evaluator in compose, validator and schema coverage, adversarial fixtures, determinism test. Tap a line in the code view, change it, hear it next bar. | Generative is architectural. |
| **M3 — Overrides and locks** | First-class nodes; gestures on generated material produce overrides; regeneration preserves them; the composer respects locks. | Bidirectional: the DAW edits the code and the code keeps generating. |
| **M4 — Rules move into the document** | Grammar, caps, energy mappings, transition catalog exported from Rust into Score data; the eight built-in genres become Score fragments (#56). | A style is code. Compose is an interpreter. |
| **M5 — Everything addressable** | Parameter paths everywhere; modulation nodes; automation UI edits lanes by path; patch graphs (#37/#43) and sample recipes (#53) referenced by path. | Instruments, samples, modulation and automation are one graph. |
| **M6 — Melody and harmony as generators** | Scale and chord track (#35); melodic generators and motif transforms in the Score; piano-roll edits become overrides or parameter edits. | Melody is code, and still playable by hand. |
| **M7 — Mix and master as rules** | Bus targets, auto-mix decisions (#27) and the mastering profile as Score nodes; the critic's targets live in the document. | The whole signal chain is in the document. |
| **M8 — The whole production is code** | A project is one Score plus packs. Export a performance (#102) or share the Score. Play is always a fresh, recognizable performance. | The vision. |

### 4.1 What this reorders

- M0, M1 and M2 come before DJ mode. After M1, DJ controls are `ScoreDiff`s and DJ mode is a
  surface, not a subsystem.
- The composer's job is precisely defined: propose `ScoreDiff`s. Its quality becomes
  measurable in-document by the critic rather than judged by ear alone.
- Coincides with, and gives a single frame to: #56 styles as packs, #22/#42 the composer,
  #37/#43 instruments as code, #53 samples as code, #16 grammar as data, #39/#45 Produce as
  the projections, #35 harmony.

## 5. Open design questions

- Text grammar: custom, with a Strudel-compatible step mini-notation subset. Decide before M0.
- Override granularity: per step vs per region. Start per region; per step is a region of one.
- Overrides under generator changes: anchor to musical time, re-validate on regeneration,
  surface conflicts in the code view rather than silently dropping.
- `IR_VERSION` policy for the Score: additive fields only through M2; a versioned migration
  path from M3.
