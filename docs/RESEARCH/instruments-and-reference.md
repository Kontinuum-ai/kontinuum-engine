# Research: Professional instrument collection, Settings-based instrument management, and YouTube reference-track analysis/remix

Status: research (issue briefs #14/#30 instruments, #21 reference analysis, #5 analysis pipeline, new epic candidates). This document deliberately does not redesign the app; everything maps onto the existing machine architecture (`Box<dyn Voice>` factories + IR `InstrumentDef` kinds + taste pipeline).

---

## Part 1 — The professional instrument collection

### 1.1 What the classics actually were (and what we emulate)

| Pioneer / canon | Machine | Character | Our digital equivalent (existing → to build) |
|---|---|---|---|
| Kraftwerk | Custom oscillators, Orchestron, Minimoog, Synthanorma sequencer | Melodic, vocal-ish synths, precise repetition | Have: kick/hat/bass/pad. **Add: mono lead (Minimoog-style: 3 VCO, filter drift, glide), vocoder/string pad** |
| Juan Atkins / Model 500 | Roland TR-808, TB-303, Oberheim DMX | Electro stabs, 808 congas/snares | **Add: 808 kit (conga, marimba-ish toms, cowbell = dual square + BP)** |
| Derrick May | TR-909 + 303 | It Is What It Is / Strings of Life stabs | **Add: 909 snare/rim/clap variants (already partial), 303 acid (have)** |
| Joey Beltram / R&S era | 909, hoover sound (Alpha Juno "Hoover") | Rave hoover pads | **Add: supersaw/hoover pad (5-7 detuned saws + LP sweep)** |
| Richie Hawtin / Plus 8 | 909, 303, SP-1 samplers | Dry, precise, clicky | **Add: sampler track (#19) — this is the biggest single unlock** |
| Deep house (Larry Heard) | Rhodes/EP (Rhodes Mk I, Wurlitzer), Juno chords, SP-1200 | EP 7th/9th chords, warm sub | Have: EP (FM). **Add: FM Rhodes variant (tine + body tone), Juno pad (single-osc chorus pad)** |
| Moodymann / LOFTstyle | Samples, vinyl | swung, dusty | **Add: sampler + vinyl noise texture generator** |
| Modern minimal (Rødhåd, Vril) | Modular (Eurorack), rumble bass | Rumble = kick → reverb → LP → sub; evolving textures | **Add: rumble generator (kick-triggered reverb tail + resonant LP), granular texture voice** |

### 1.2 The complete catalog to build (priority order)

**Tier 1 — the missing canon (DSP, cheap in our architecture):**
1. **Sampler track** (#19) — the Ableton Simpler equivalent. One sample, slice, pitch, gate. Unlocks: real 909/808 samples, vocal chops, any texture. *Highest priority; #19 already planned.*
2. **Mono lead** (Minimoog-ish: 2-3 detuned saw/square, resonant LP, glide, moderate drive) — leads, acid-adjacent melodies, Kraftwerk lines.
3. **Hoover/supersaw pad** (5-7 detuned saws, HP+LP, slow sweep) — rave/hard techno pads.
4. **Rumble generator** — kick-sidechained noise/reverb tail through resonant LP (the modern minimal bass staple; our kick-duck pipeline is already the keying source).
5. **808 percussion set** — congas (2-3 pitched), cowbell (dual square + BP), marimba/claves.
6. **FM Rhodes** — EP variant with tine+body: two FM pairs (tine: fast decay, high ratio; body: slow, low ratio).
7. **Vocoder-ish string/choir pad** — formant-filtered saw stack (for Kraftwerk-flavored pads).

**Tier 2 — texture and production tools:**
8. **Noise/sweep generator** (white/pink, LP/HP sweeps, riser envelope) — transitions.
9. **Granular texture voice** — grain cloud from a sample or noise (evolving atmospheres).
10. **Vinyl/tape noise bed** — constant low-level texture + crackle (deterministic pseudo-random).
11. **Percussion synth expansions** — rim (short dual-square), tick, metal (FM cluster).

**Tier 3 — extras after Tier 1/2 land:** Wavetable voice (morphing single-wave), chord memory machine (freeze voicings), drone voice.

With Tier 1+2 the rack is: **12 existing + ~11 new ≈ 23 instruments** — comparable to a full 909+808+303+Juno+DX7+Simpler rig, all `InstrumentDef` kinds in the IR (schema+bounds+factory per kind ≈ 60 lines each in our architecture).

### 1.3 Instrument management through Settings (keeping the main UI simple)

Design that fits the existing code without redesign:

- **Settings → Instruments**: a registry list. Each instrument: full name, machine family badge (909/303/FM/Modular/Sampler), enabled toggle, and a mini-preview (plays a one-shot render via the offline renderer).
- **The main rack is a *view* over the enabled set.** Concretely: `Session` gains `enabled_instruments: Vec<String>` (default: the 10 shipped). The machine strip and the live grid render only enabled families; the genre/taste pipeline only writes patterns for enabled instruments.
- **Track count**: 12 today. With samplers the practical ceiling is CPU, not code (pool sizes are per-track; the cost model in the validator already estimates per-voice cost — raise the ceiling only with the CPU budget check enabled). Plan: 12 → 16 when the sampler lands, enforced by the existing `E_TOO_MANY_TRACKS` + CPU estimate.
- **Why this stays simple**: the main interface keeps exactly three surfaces (waveform, grid, code stream); Settings only changes *which instruments exist in those surfaces*.

---

## Part 2 — Reference-track analysis via YouTube link

### 2.1 The three-way legal/technical distinction (this is the load-bearing decision)

| Mode | What happens | Legal status | Our stance |
|---|---|---|---|
| **1. Analyze** | Extract features only (BPM, structure, energy, spectral stats). No audio stored, no audio redistributed. | Feature data is factual, not copyrightable (US: facts/doctrine; EU: no originality in measurements). Downloading still violates YouTube ToS — so this must run on a server the operator controls and accepts ToS risk for, or on user-supplied files. | Ship as: (a) **file import = fully safe**, (b) YouTube link = server-side job, features-only, audio discarded immediately. |
| **2. Inspired-by generation** | Features → taste profile → our generator produces an **original** composition. | Clean. Style is not copyrightable; we never touch the recording in generation. | This is the default and the product's core loop. The existing `TasteProfile`/`session_from_taste` pipeline is exactly the right shape — reference analysis just produces richer profiles. |
| **3. Remix (uses the recording)** | Actual stems/audio from the original appear in output. | Requires clearance: master use license (label/distributor) + sync license if paired with visuals; on YouTube a Content ID claim is near-certain otherwise. | **Out of scope for generated output.** Only support when the user uploads audio they own/licensed. The app must never fetch-and-include YouTube audio. |

**Rule for the app:** Mode 1 and 2 for any link; Mode 3 only for user-provided files, with a written clearance checkbox. This distinction must be visible in the UI (issue #33 transparency surfaces).

### 2.2 Technical pipeline

**Stage A — acquisition (server-side only, never on-device):**
- `yt-dlp` to pull the audio stream for analysis, then **immediately discard** (retention policy in writing). YouTube ToS §2T prohibits downloading outside the API — the mitigation is operator-owned infrastructure, no redistribution, ephemeral storage. Alternative that avoids the gray zone entirely: user uploads the file, or pastes a Spotify/Beatport link for metadata-only taste.
- Resolve once via oEmbed (title/artist/length, fully sanctioned) to display what's being analyzed.

**Stage B — MIR feature extraction (the state of the art, all open source):**
| Feature | Tool | Notes |
|---|---|---|
| BPM + beat grid | `librosa.beat` / `madmom` (DBN beat tracker — best-in-class) | madmom: research-only license! Prefer librosa or Essentia for a product |
| Downbeat/structure (intro/verse/drop/outro boundaries) | **allin1 (All-in-One Music Structure Analyzer)** — does beat/downbeat/segment/function in one pass | MIT, GPU-friendly, exactly our "structure" requirement |
| Stems (for instrumentation analysis only) | `demucs` (htdemucs) | Features-from-stems OK; do not ship stems |
| Key/chroma | Essentia `KeyExtractor`, or librosa chroma + template match | Feeds `key_root`/`minor` in TasteProfile |
| Groove | madmom/onset microtiming histogram vs grid | Feeds `swing` + humanize depth directly |
| Energy curve | RMS/LUFS per bar (we already have this analyzer shape in-house) | Feeds `energy_curve` per section |
| Spectral balance / production character | Essentia spectral descriptors (centroid, tilt, HF/LF ratio) | Feeds brightness + palette selection |
| Instrumentation | demucs stems + per-stem Essentia stats ("heavy 909-ish kick: LF thump ratio", "hats present: HF transients") | Feeds palette selection |

**Stage C — mapping into our existing pipeline (no redesign):**
The analysis output is a **superset of `TasteProfile`**. Concretely: `ReferenceAnalysis { bpm, beat_phase_hist, key, sections: [(kind, bars, energy)], swing, brightness, stems_present, per_bar_energy }` → `taste_from_reference()` → existing `session_from_taste()`. The sections array maps onto our `Section { bars, energy_curve }` **directly** — the reference's arrangement becomes our arrangement's skeleton. This is the integration: one new adapter module, zero redesign.

### 2.3 Local vs server (per PLAN §2.4 device/cloud policy)

| Component | Runs where | Why |
|---|---|---|
| File import analysis (WAV/AIFF/FLAC/MP3 decode + librosa-lite features) | **On-device**, Rust port or Core ML | Airplane-mode rule; symphonia (decode) already proven in our stack; a small Rust feature set (RMS bands, onset autocorr BPM, chroma) is implementable without Python |
| YouTube acquisition + allin1/demucs analysis | **Server microservice** (Python; GPU optional — allin1 runs CPU, slower) | YouTube ToS risk belongs to the operator; heavy models don't fit the phone budget (PLAN: single-digit % CPU sustained) |
| Features → session generation | **On-device, always** | It's our existing deterministic pipeline; the server only ever sends the (non-copyrightable) feature JSON |
| Remix (Mode 3) with user files | On-device stems (demucs mobile is not viable → server job returning stems only to the paying/licensed user) | Deferred until Modes 1-2 prove out |

**Recommended phased build:**
1. **Phase R1:** File-import reference analysis on-device (Rust: symphonia + BPM/chroma/energy) → `ReferenceAnalysis` → session adaptation. Fully legal, no server, immediately demoable.
2. **Phase R2:** Server analysis service: URL in → oEmbed resolve → yt-dlp → allin1+demucs → features JSON out → audio deleted. app pastes link, gets features, generates inspired-by session.
3. **Phase R3:** Remix mode for user-owned files only: demucs stems server-side, user clears rights in-app, stems load into sampler tracks (#19).
4. **Phase R4:** Adaptive *live* reaction: re-analyze in 8-bar windows and steer the running session via the existing diff pipeline (`apply_diff` at boundaries — #13's machinery is already the transport for this).

### 2.4 Existing open-source stack summary
- **Decode**: symphonia (Rust, on-device) / ffmpeg (server)
- **MIR**: librosa (BSD), Essentia (AGPL — server-side only, never linked into the app!), madmom (research license — server-side only), **allin1** (MIT — structure), **demucs** (MIT — stems)
- **Warning already in PLAN §7**: Essentia is AGPL — keep it in the server process only, or replace with librosa/Essentia-free descriptors
- **Generation stays ours**: features never become audio; our deterministic engine + variation pass is the "remix brain"

---

## 3. Immediate actionable next steps (mapped to issues)
1. New epic: **Reference analysis & adaptation** (Modes 1-2 above), first milestone = Phase R1 file import.
2. New epic: **Instrument collection Tier 1** (sampler #19 first, then mono lead, hoover, rumble, 808 perc, FM Rhodes) — each is one `Voice` + one `InstrumentDef` + one factory in the existing rack.
3. Settings → Instruments registry (enable/disable) gating the machine strip, grid, and generation palette.
4. Legal copy for the UI: three-mode distinction shown at the link-input surface (analyze / inspired-by / remix-with-your-files).
