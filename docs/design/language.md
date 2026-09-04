# Kontinuum design language (issue #33, workstream A)

Status: v0.1 — tokens shipped in `shared/Kontinuum/Theme.swift`, living surfaces
in `LivingUI.swift` + `ContentView.swift`. Dark-only by decision: light mode
does not exist and will not be designed.

## Principles (from #33)

1. **The music is the interface.** Every visual element renders engine ground
   truth — hit masks, per-bar onset density, section energy — never FFT
   guesses at a mixed signal.
2. **Restraint.** Near-black base, one accent hue, no gradients, no chrome
   ceremony. Minimal-techno sensibility: negative space, precision.
3. **Beat-locked motion.** The live waveform column pulses with
   `beat_phase`; the step-grid cursor sweeps the 16-slot bar; nothing drifts
   or floats freely. Motion is quantized to the musical grid because the
   engine hands us the grid.
4. **Zero audio-thread cost.** The UI reads atomic telemetry snapshots at
   30 Hz on the control thread; the render thread never touches visuals.

## Tokens

| Token | Value | Use |
|---|---|---|
| `bg` | `#0B0B0D` | base, full-bleed |
| `surface` | `#1C1C1F` | pills, chips, time bubble |
| `ink` | `#D9D9D9` | primary glyph color (waveform columns, text) |
| `dim` | white 42% | secondary text, lane labels |
| `accent(e)` | `#FF3B30 → #FF9F0A` | single living accent; hue drifts with the session's live energy curve |

Typography: **monospaced design** (SF Mono family) for anything that *is*
engine data — the code stream, step grid, telemetry, counters. SF rounded,
thin weights, only for the bar counter. Wordmark: 17pt monospaced, +9
tracking. No other faces.

Glyph vocabulary for patterns: `●` velocity > 0.66, `•` > 0.33, `∙` below,
`·` rest. Information-bearing: glyph weight *is* the recorded velocity.

## Surfaces

- **WaveformBar** — the reference bar (thin symmetric columns over a
  baseline rail, duration chip bottom-right), rebuilt live: one column per
  played bar (energy + onset density), the rightmost column is *now*,
  pulsing with beat phase in the accent hue. 48 columns, 2.5pt bars.
- **LiveGrid** — the bar sounding now: four 16-slot lanes (kick/hat/bass/
  pad) showing the recorded hit masks with a sweeping beat cursor.
- **CodeStream** — the composition written live: per-bar groups of pattern
  rows in mono glyphs, newest in accent, elders dimmed. This is the
  "watch it be coded" surface; it is the actual performance log.
- **Transport** — stop/play circle, bar counter, `block · queue · gaps`
  safety telemetry. The engine's honesty is part of the aesthetic.

## Motion rules

- Quantized: pulses, cursors and column updates derive from `beat_phase`
  (sample-accurate playhead), never from wall-clock timers.
- Continuous 60fps: `TimelineView(.animation)` between 30 Hz snapshot polls.
- Section transitions (v0.2+) will mirror the IR's transition types —
  filter sweeps as brightness ramps on `accent`, mutes as grid blackouts.

## Deferred to later #33 workstreams

Metal centerpiece (prototype directions 1–3), steering gestures with
kick-synced haptics, mood-input dissolve, Dynamic Island glyphs, lock-screen
art. The ground-truth data plumbing shipped here (UiSnapshot FFI) is their
foundation.

## Brand assets

The canonical logo pack lives in `assets/logo-pack/` (16 SVG masters, raster
exports in `png/`, interactive spec sheet in `brand-spec.html`). Working
copies used by the repo and site:

- `assets/logo.svg` / `assets/logo-dark.svg` — primary mark, light/dark
- `assets/app-icon.svg` — source for `scripts/render-app-icons.swift`
  (macOS `.icns` + iOS masters)
- `assets/logo-avatar.svg` — bare mark, avatar/favicon use
- `site/static/logo.svg` — site favicon (app icon)
