# Blind A/B listening protocol (#32)

Methodology for every listening evaluation in this project — release-candidate
regression rounds, mastering gates (#28), and any "does it sound professional"
question we answer with humans instead of the critic (#25). Pair *generation*
is automated (`kontinuum-offline` `render_ab`, issues #28/#32); this document
fixes how humans judge the pairs.

## 1. Stimuli (automated before any human is involved)

- **Loudness matching is mandatory.** Pairs are matched to ±0.2 LU integrated
  (BS.1770) by `render_ab`. Level differences masquerade as quality; an
  unmatched pair invalidates the trial set. The match value is recorded in the
  pair manifest.
- **Pairs shipped per round** (each = condition A, condition B, shared seed):
  - Regression: new build vs previous build, same fixture sessions
    (4 genres × ≥3 seeds), 10 s section stimuli.
  - Mastering gate (#28): unmastered mix vs mastered render; plus commercial
    reference excerpts (purchased/licensed per #6, same matching).
- **Naming**: files are content-hash only. No condition labels, build ids, or
  timestamps in filenames — the presenter script maps hashes to conditions and
  keeps the mapping sealed until results are submitted.
- **Critic pre-gate**: both stimuli must pass their own `CriticVerdict`
  tolerances (#25) before a round runs. A stimulus that fails its own targets
  is garbage-in and voids the trial.

## 2. Listening procedure

- **Task**: forced choice. Each trial plays X, then A, then B (X is A or B,
  randomized); the listener answers "was X A or B?". Preference share and
  discrimination (percent correct) come from the same trials — discrimination
  near 50% means the conditions are indistinguishable, which for a *regression*
  round is the pass condition.
- **Order**: trial order and A/B assignment come from a seeded RNG
  (`xorshift`, seed logged in the manifest) — reproducible and blind.
- **Playback**: wired headphones, fixed output level on a calibrated device,
  ≥5 s silence between stimuli, listener may replay the trial once. No
  loudness compensation by hand — matching already handled it.
- **Panel size**: engineering rounds ≥3 team listeners, ≥12 trials per genre;
  formal gates (#28) 15 listeners, ≥180 judgments per comparison. Report
  per-genre splits only when each has ≥60 judgments.
- **Optional scales**: after the forced choice, a 1–10 "professional finish"
  rating per condition may be collected. Scales are secondary evidence; the
  forced choice is the primary metric.

## 3. Hypotheses and pass/fail (pre-registered before the round)

- **Regression round (new build vs previous)**: pass if preference share for
  the new build is ≥ 55% with two-sided binomial p < 0.05, OR discrimination
  is at chance (|share − 50%| within the 95% binomial interval) — i.e. no
  audible regression. A loss of > 5 preference points vs the archived previous
  round without a documented, deliberate sound change (#52 ratchet) fails the
  round.
- **Mastering gate (#28)**: mastered preferred over unmastered mix at
  share ≥ 55%, p < 0.05; "professionally finished" median ≥ 7/10; the
  vs-reference gap is reported per axis, not thresholded (it feeds the Phase 6
  decision).
- **Report**: per-comparison table (judgments, share, p, median ratings,
  per-listener breakdown). Raw submissions archived next to the manifest under
  `renders/ab/`.

## 4. Integrity rules

- Listeners never learn condition identities until after submission.
- The presenter/researcher may know, but must not comment during sessions.
- Trials where the listener reports playback problems (dropouts, wrong level)
  are discarded and logged, not silently kept.
- Result files are append-only; corrections are new submissions with a note.

## 5. Tooling status

| Step | Status |
|---|---|
| Pair generation, loudness matching, manifest | shipped (`ab.rs`, #28/#32) |
| Critic pre-gate | shipped (#25 `CriticVerdict`) |
| Trial presenter script | manual v0 — scripted playlist + spreadsheet form; automating the presenter is a follow-up |
| Binomial analysis | manual v0 — any stats package; a small analyzer is a follow-up |

## 6. Cadence

Every release candidate: one regression round (3 listeners, 4 genres × 3
seeds). Formal 15-listener gates: at #28's mastering acceptance and before
#31's TestFlight external beta. Results are archived with the pair manifests;
the #52 ratchet compares rounds over time.
