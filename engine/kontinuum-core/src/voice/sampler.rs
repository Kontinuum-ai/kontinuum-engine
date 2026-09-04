//! Sampler voice (#19 v0): plays one shared PCM buffer, pitch-shifted by the
//! note, gated loop or one-shot. The buffer is shared via Arc so every pool
//! voice plays the same loaded sample. Voices wired to the same
//! [`ChokeState`] group fast-fade each other on retrigger (hat logic).

use std::sync::Arc;

use super::choke::ChokeState;
use super::flush_denormal;
use super::{HitJitter, HitMod};
use crate::{Voice, SILENCE_ABS};

/// Choke fade length: ~10 ms at the voice's rate, matching the offline
/// renderer's choke ceiling.
const CHOKE_FADE_MS: f32 = 10.0;

pub struct Sampler {
    sr: f32,
    sample: Option<Arc<[f32]>>,
    sample_sr: f32,
    /// Sorted slice start frames in the buffer; an empty table is one
    /// full-buffer slice.
    slices: Arc<[usize]>,
    rate: f32,
    /// Slot-level tuning multiplier (transpose + fine detune), set once at
    /// attach time; every note's rate is multiplied by it.
    tune_mul: f32,
    /// Playback stop bound for the current note (last interpolable frame of
    /// the buffer, or of the slice region during sliced playback).
    end: f32,
    pos: f32,
    amp: f32,
    gate: bool,
    jitter: HitJitter,
    active: bool,
    choke: Option<(Arc<ChokeState>, u8)>,
    /// Epoch this voice's current note owns; 0 = no choke assignment.
    epoch: u64,
    group: u8,
    fading: bool,
    fade_pos: usize,
    fade_len: usize,
}

impl Sampler {
    pub fn new(sample_rate: u32) -> Self {
        Sampler {
            sr: sample_rate as f32,
            sample: None,
            sample_sr: sample_rate as f32,
            slices: Arc::from(Vec::new()),
            rate: 1.0,
            tune_mul: 1.0,
            end: 0.0,
            pos: 0.0,
            amp: 0.0,
            gate: false,
            jitter: HitJitter::new(),
            active: false,
            choke: None,
            epoch: 0,
            group: 0,
            fading: false,
            fade_pos: 0,
            fade_len: ((CHOKE_FADE_MS / 1000.0) * sample_rate as f32).round().max(1.0) as usize,
        }
    }

    /// Swap the buffer all pool voices play. Position resets on next note.
    pub fn set_sample(&mut self, data: Arc<[f32]>, sample_rate: u32) {
        self.sample = Some(data);
        self.sample_sr = sample_rate as f32;
    }

    /// Swap the slice table (sorted frame offsets, first entry 0). An empty
    /// table means one full-buffer slice. Offsets are in the buffer's own
    /// frames; the engine rate conversion rides the playback speed.
    pub fn set_slices(&mut self, slices: Arc<[usize]>) {
        self.slices = slices;
    }

    /// Slot-level tuning (#19): `transpose` in semitones, `fine` in cents.
    /// Control-thread only; applied on top of every note's pitch-derived
    /// rate (the per-hit pitch stays authoritative, this shifts the slot).
    pub fn set_tune(&mut self, transpose_semitones: f32, fine_cents: f32) {
        self.tune_mul =
            ((transpose_semitones + fine_cents / 100.0) / 12.0).exp2();
    }

    /// Join a choke group. Voices sharing `state` in the same non-zero
    /// group fade each other out on retrigger.
    pub fn set_choke(&mut self, state: Arc<ChokeState>, group: u8) {
        self.choke = Some((state, group));
        self.group = group;
    }

    /// Slice region for `slice`: (start, end) frames in the buffer.
    /// Out-of-range indices clamp to the last slice; an empty table is one
    /// full-buffer slice.
    fn slice_region(&self, slice: u16, len: usize) -> (usize, usize) {
        let idx = (slice as usize).min(self.slices.len().saturating_sub(1));
        let start = self.slices.get(idx).copied().unwrap_or(0).min(len);
        let end = self.slices.get(idx + 1).copied().unwrap_or(len).clamp(start, len);
        (start, end.max(start + 1))
    }
}

impl Voice for Sampler {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        if let Some((state, group)) = &self.choke {
            self.epoch = state.trigger(*group);
        }
        self.fading = false;
        self.fade_pos = 0;
        let HitMod { amp, pitch: pitch_mul, variant, .. } =
            self.jitter.next_hit((0.75, 1.15), 12.0, 0.0, (1.0, 1.0));
        self.rate = ((pitch.clamp(12.0, 108.0) - 60.0) / 12.0).exp2() * pitch_mul * self.tune_mul;
        self.amp = velocity.clamp(0.0, 1.0) * amp;
        // Round-robin start offset: up to 2% into the buffer, variant-cycled.
        self.pos = match self.sample.as_ref() {
            Some(s) => {
                let len = s.len() as f32;
                (len * 0.004 * variant as f32).min(len * 0.02)
            }
            None => 0.0,
        };
        self.end = self.sample.as_ref().map_or(0.0, |s| (s.len().saturating_sub(1)) as f32);
        self.gate = true;
        self.active = self.sample.is_some() && self.amp > 0.0;
    }

    /// One-shot playback of one slice region: starts at the slice's frame
    /// offset (with the same round-robin anti-grid offset inside the
    /// region), stops at the next boundary, never loops. `rate_mul`
    /// multiplies the pitch-derived rate.
    fn note_on_slice(&mut self, pitch: f32, velocity: f32, slice: u16, rate_mul: f32) {
        if let Some((state, group)) = &self.choke {
            self.epoch = state.trigger(*group);
        }
        self.fading = false;
        self.fade_pos = 0;
        let HitMod { amp, pitch: pitch_mul, variant, .. } =
            self.jitter.next_hit((0.75, 1.15), 12.0, 0.0, (1.0, 1.0));
        self.rate =
            ((pitch.clamp(12.0, 108.0) - 60.0) / 12.0).exp2() * pitch_mul * rate_mul * self.tune_mul;
        self.amp = velocity.clamp(0.0, 1.0) * amp;
        let region = self.sample.as_ref().map(|s| self.slice_region(slice, s.len()));
        self.pos = match (region, variant as f32) {
            (Some((start, end)), variant) => {
                let region_len = (end - start) as f32;
                start as f32 + (region_len * 0.004 * variant).min(region_len * 0.02)
            }
            (None, _) => 0.0,
        };
        self.end = region.map_or(0.0, |(_, end)| (end - 1) as f32);
        self.gate = false; // sliced playback is one-shot by definition
        self.active = self.sample.is_some() && self.amp > 0.0;
    }

    fn note_off(&mut self) {
        self.gate = false; // one-shot finishes; no retrigger
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        let Some(sample) = self.sample.as_ref() else {
            out.fill(0.0);
            return;
        };
        let len = sample.len();
        if len < 2 {
            out.fill(0.0);
            return;
        }
        let speed = self.rate * self.sample_sr / self.sr;
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            let mut gain = 1.0;
            if self.fading {
                if self.fade_pos >= self.fade_len {
                    self.active = false;
                    *slot = 0.0;
                    continue;
                }
                self.fade_pos += 1;
                gain = 1.0 - self.fade_pos as f32 / self.fade_len as f32;
            } else if self.epoch > 0 {
                if let Some((state, group)) = &self.choke {
                    if state.current(*group) != self.epoch {
                        self.fading = true;
                        self.fade_pos = 0;
                    }
                }
            }
            let i = self.pos as usize;
            let frac = self.pos - i as f32;
            let a = sample[i];
            let b = sample[(i + 1).min(len - 1)];
            let mut v = (a + (b - a) * frac) * self.amp * gain;
            if v.abs() < SILENCE_ABS {
                v = 0.0;
            }
            self.pos += speed;
            if self.pos >= self.end {
                if self.gate {
                    self.pos = 0.0; // gated: loop
                } else {
                    self.active = false;
                }
            }
            *slot = v;
        }
        self.amp = flush_denormal(self.amp);
    }

    fn set_param(&mut self, _param: crate::ParamId, _value: f32) {}

    fn reset(&mut self) {
        self.pos = 0.0;
        self.end = 0.0;
        self.amp = 0.0;
        self.gate = false;
        self.active = false;
        self.fading = false;
        self.fade_pos = 0;
        self.epoch = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BLOCK_FRAMES;

    fn sampler_with(state: Arc<ChokeState>, group: u8) -> Sampler {
        let mut s = Sampler::new(48_000);
        s.set_sample(vec![0.5f32; 48_000].into(), 48_000);
        s.set_choke(state, group);
        s
    }

    #[test]
    fn same_group_voices_choke_within_10ms() {
        let state = ChokeState::shared();
        let mut hat = sampler_with(Arc::clone(&state), 1);
        let mut next = sampler_with(Arc::clone(&state), 1);
        hat.note_on(60.0, 1.0);
        next.note_on(60.0, 1.0);

        let mut buf = [0.0f32; BLOCK_FRAMES];
        hat.render(&mut buf);
        assert!(buf.iter().any(|&s| s != 0.0), "hat sounds before the choke");

        // Retrigger chokes the first voice; the fade finishes inside 10 ms
        // (480 frames at 48 kHz), so a later block is exactly silent.
        let mut silenced = [1.0f32; 512];
        for _ in 0..480 / BLOCK_FRAMES {
            hat.render(&mut buf);
        }
        hat.render(&mut buf[..480 % BLOCK_FRAMES]);
        hat.render(&mut silenced);
        assert!(silenced.iter().all(|&s| s == 0.0), "choked voice must hit exact zero");

        hat.note_on(60.0, 1.0);
        let mut fresh = [0.0f32; BLOCK_FRAMES];
        hat.render(&mut fresh);
        assert!(fresh.iter().any(|&s| s != 0.0), "a new note escapes the stale epoch");
    }

    #[test]
    fn other_groups_are_unaffected() {
        let state = ChokeState::shared();
        let mut hat = sampler_with(Arc::clone(&state), 1);
        let mut snare_ish = sampler_with(Arc::clone(&state), 2);
        hat.note_on(60.0, 1.0);
        snare_ish.note_on(60.0, 1.0);

        let mut buf = [0.0f32; BLOCK_FRAMES];
        hat.render(&mut buf);
        let before = buf;
        snare_ish.render(&mut buf);
        hat.render(&mut buf);
        assert_eq!(before[..BLOCK_FRAMES / 2], buf[..BLOCK_FRAMES / 2]);
    }

    #[test]
    fn choke_is_deterministic() {
        let run = || {
            let state = ChokeState::shared();
            let mut a = sampler_with(Arc::clone(&state), 1);
            let mut b = sampler_with(Arc::clone(&state), 1);
            a.note_on(60.0, 1.0);
            let mut out = [0.0f32; BLOCK_FRAMES];
            a.render(&mut out);
            b.note_on(60.0, 1.0);
            let mut tail = [0.0f32; 1024];
            a.render(&mut tail);
            tail
        };
        let x = run();
        let y = run();
        assert!(x.iter().zip(y.iter()).all(|(p, q)| p.to_bits() == q.to_bits()));
    }

    /// Two DC halves: the rendered level names the region being played.
    fn step_sample() -> Arc<[f32]> {
        let mut data = vec![0.25f32; 48_000];
        for v in data[24_000..].iter_mut() {
            *v = 0.75;
        }
        data.into()
    }

    #[test]
    fn slice_playback_stays_inside_its_region() {
        let mut s = Sampler::new(48_000);
        s.set_sample(step_sample(), 48_000);
        s.set_slices(vec![0, 24_000].into());
        // Out-of-range index clamps to the last slice (the 0.75 half).
        s.note_on_slice(60.0, 1.0, 7, 1.0);
        assert!(s.is_active());

        let mut out = Vec::new();
        let mut buf = [0.0f32; BLOCK_FRAMES];
        let mut blocks = 0usize;
        while s.is_active() {
            s.render(&mut buf);
            out.extend_from_slice(&buf);
            blocks += 1;
            assert!(blocks < 1000, "sliced playback never stopped");
        }
        let level = out.iter().find(|&&v| v != 0.0).expect("slice rendered silence");
        assert!(
            out.iter().all(|&v| v == 0.0 || v.to_bits() == level.to_bits()),
            "slice output must be the constant second-half level"
        );
        // 0.75 * jitter amp (0.75..1.15) — the 0.25 half would stay under 0.3.
        assert!(*level > 0.5 && *level < 1.0, "wrong region: {level}");
        // Region is 24_000 frames and the round-robin offset eats into it.
        assert!(out.len() < 24_000, "slice overran its boundary: {}", out.len());

        let mut tail = [1.0f32; BLOCK_FRAMES];
        s.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0), "tail must be exactly zero");
    }

    #[test]
    fn empty_table_slice_zero_is_the_full_buffer_one_shot() {
        let run_sliced = || {
            let mut s = Sampler::new(48_000);
            s.set_sample(step_sample(), 48_000);
            s.note_on_slice(60.0, 1.0, 0, 1.0);
            let mut out = Vec::new();
            let mut buf = [0.0f32; BLOCK_FRAMES];
            while s.is_active() {
                s.render(&mut buf);
                out.extend_from_slice(&buf);
            }
            out
        };
        let run_gated = || {
            let mut s = Sampler::new(48_000);
            s.set_sample(step_sample(), 48_000);
            // The pre-slice full-buffer one-shot: note_on + immediate
            // note_off, so the gate loop never engages.
            s.note_on(60.0, 1.0);
            s.note_off();
            let mut out = Vec::new();
            let mut buf = [0.0f32; BLOCK_FRAMES];
            while s.is_active() {
                s.render(&mut buf);
                out.extend_from_slice(&buf);
            }
            out
        };
        let sliced = run_sliced();
        let one_shot = run_gated();
        assert_eq!(sliced.len(), one_shot.len());
        assert!(
            sliced.iter().zip(one_shot.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
            "slice 0 on an empty table diverged from the full-buffer one-shot"
        );
    }
}
