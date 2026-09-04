# Mutable Instruments port evaluation (issue #30, decision doc for Nick)

Status: **evaluation only — nothing ported.** Issue #30 asks for a
port/bind assessment of the Mutable Instruments DSP (Plaits, Rings,
Clouds/Beads) with license verification per file. This document is that
assessment; the shipped #30 roster is the in-house wavetable / FM-perc /
texture engines, so the licensing decision stays fully in Nick's hands.

## License status (verified upstream, 2026-09)

All three projects are © Émilie Gillet and released under the **MIT
license** (per-file headers in each repo):

| project | repo | license | what it is |
|---|---|---|---|
| Plaits | `pichenettes/eurorack` (`plaits/dsp/…`) | MIT | 16 macro-oscillator synthesis models (2-op/FG FM, WTF, harmonic/inharmonic string-ish, physical-model plucks, noise/chiptune, …) |
| Rings | `pichenettes/eurorack` (`rings/dsp/…`) | MIT | resonator bank: modal / sympathetic-strings / modulated-inversion physical models |
| Clouds | `pichenettes/eurorack` (`clouds/dsp/…`) | MIT | granular texture processor (Clouds); Beads is the 2023 rework, also MIT |

MIT obligations: keep the copyright + permission notice in all copies or
substantial portions. Practical shape for us: vendor the ported subtree with
its `LICENSE.txt`, add a licensing row to #6's table, credit in the app's
about screen. **No GPL, no patent encumbrance on these specific modules**
(the recent Mutable *system* firmware is a different story — do not pull
from `mutable-devices`/Daisy spins without re-checking).

One caveat worth an hour of Nick's time: the **sample material** some
Plaits models ship with (e.g. spoken-word in the speech synth) is separate
data, not MIT code. We would port DSP only and synthesize our own table
data (we already do this for the wavetable engine), so the question is
moot unless the speech model is wanted.

## What each would buy us

- **Plaits** — highest value. Its FM-perc and noise models overlap with
  what #30 shipped in-house (fmperc, texture); the genuinely additive parts
  are the physical-model plucks and the "engine" abstraction (16 models
  behind one voice). Cost: the code is C++ with virtuals and float
  frequency tables; a faithful Rust port is a multi-week job including
  re-tuning our voice contract around its parameterization (timbre/morph
  axes, not named params).
- **Rings** — beautiful resonator, niche in our four subgenres (short
  plucks already exist). Port value: pad/pluck one-shots with body. Cost:
  moderate (the modal filter bank is self-contained); benchmark risk low.
- **Clouds/Beads** — overlaps directly with the #19 granular voice and the
  #30 texture voice. We keep **ours** (pick made in #30: texture voice for
  RT beds, #19 granular for sample playback; Clouds adds density/spread
  polish but not a new capability class). Revisit only if the composer's
  query log (#20 gap analysis, now wired) shows texture queries scoring
  poorly.

## Recommendation

1. Plaits is the only port that changes what the product can sound like —
   scope it as its own issue (port the 4 highest-value engines first:
   FM-perc for parity-checking ours, plucks, strings, vox-free noise
   models), behind the plugin registry.
2. Rings as a follow-up if the worlds want physical-model bodies.
3. Skip Clouds for now; let the query-log data argue for it.

All three decisions are license-clean (MIT), so timing is a
priority/craft call, not a legal one. Nothing here blocks #30's
acceptance: the in-house roster shipped and the packs/worlds are
re-derivable without external code.
