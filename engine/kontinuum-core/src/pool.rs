//! Fixed-capacity voice pools with quietest-recent-peak stealing.
//!
//! `render` ADDS into `out` — the caller zeroes the buffer first (the graph
//! does). Recent peaks decay per block so releasing voices are preferred
//! steal victims. Scratch rendering is per-voice into a stack buffer, so no
//! allocation occurs on the render path.

use crate::{ParamId, Voice, BLOCK_FRAMES};

impl Voice for Box<dyn Voice> {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        (**self).note_on(pitch, velocity)
    }
    fn note_on_slice(&mut self, pitch: f32, velocity: f32, slice: u16, rate_mul: f32) {
        (**self).note_on_slice(pitch, velocity, slice, rate_mul)
    }
    fn note_off(&mut self) {
        (**self).note_off()
    }
    fn is_active(&self) -> bool {
        (**self).is_active()
    }
    fn render(&mut self, out: &mut [f32]) {
        (**self).render(out)
    }
    fn set_param(&mut self, param: ParamId, value: f32) {
        (**self).set_param(param, value)
    }
    fn reset(&mut self) {
        (**self).reset()
    }
}

pub struct VoicePool<V: Voice> {
    voices: Box<[V]>,
    peaks: Box<[f32]>,
}

impl<V: Voice> VoicePool<V> {
    pub fn new(capacity: usize, make: impl Fn() -> V) -> Self {
        let voices: Vec<V> = (0..capacity.max(1)).map(|_| make()).collect();
        let peaks = vec![0.0f32; voices.len()];
        VoicePool { voices: voices.into_boxed_slice(), peaks: peaks.into_boxed_slice() }
    }

    pub fn capacity(&self) -> usize {
        self.voices.len()
    }

    /// Free slot first, else steal the voice with the lowest recent peak.
    /// Returns the slot that received the note.
    pub fn note_on(&mut self, pitch: f32, velocity: f32) -> usize {
        let slot = self.pick_slot();
        self.voices[slot].note_on(pitch, velocity);
        self.peaks[slot] = 0.0;
        slot
    }

    /// Sliced one-shot via the same slot policy as [`VoicePool::note_on`].
    /// Returns the slot that received the trigger.
    pub fn trigger_sample(&mut self, pitch: f32, velocity: f32, slice: u16, rate_mul: f32) -> usize {
        let slot = self.pick_slot();
        self.voices[slot].note_on_slice(pitch, velocity, slice, rate_mul);
        self.peaks[slot] = 0.0;
        slot
    }

    fn pick_slot(&self) -> usize {
        self.voices
            .iter()
            .position(|v| !v.is_active())
            .unwrap_or_else(|| {
                let mut best = 0usize;
                let mut best_peak = f32::INFINITY;
                for (i, &p) in self.peaks.iter().enumerate() {
                    if p < best_peak {
                        best_peak = p;
                        best = i;
                    }
                }
                best
            })
    }

    pub fn note_off(&mut self, slot: usize) {
        if let Some(v) = self.voices.get_mut(slot) {
            v.note_off();
        }
    }

    /// Sum all voices into `out` (which must be pre-zeroed, len ≤ BLOCK_FRAMES).
    pub fn render(&mut self, out: &mut [f32]) {
        let n = out.len();
        debug_assert!(n <= BLOCK_FRAMES);
        let mut scratch = [0.0f32; BLOCK_FRAMES];
        for i in 0..self.voices.len() {
            if !self.voices[i].is_active() {
                self.peaks[i] *= 0.5;
                continue;
            }
            scratch[..n].fill(0.0);
            self.voices[i].render(&mut scratch[..n]);
            let mut peak = 0.0f32;
            for (o, &s) in out[..n].iter_mut().zip(scratch[..n].iter()) {
                *o += s;
                peak = peak.max(s.abs());
            }
            let decayed = self.peaks[i] * 0.5;
            self.peaks[i] = if peak > decayed { peak } else { decayed };
        }
    }

    pub fn set_param(&mut self, param: ParamId, value: f32) {
        for v in self.voices.iter_mut() {
            v.set_param(param, value);
        }
    }

    pub fn active_count(&self) -> usize {
        self.voices.iter().filter(|v| v.is_active()).count()
    }

    pub fn is_active(&self, slot: usize) -> bool {
        self.voices.get(slot).is_some_and(|v| v.is_active())
    }

    pub fn reset(&mut self) {
        for v in self.voices.iter_mut() {
            v.reset();
        }
        self.peaks.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Voice;

/// Pool mechanics don't need a real instrument — a deterministic minimal
/// voice keeps these tests harness-only (issue #51: no instrument code in
/// core, not even in tests).
struct DummyVoice {
    active: bool,
}

impl DummyVoice {
    fn new() -> Self {
        DummyVoice { active: false }
    }
}

impl Voice for DummyVoice {
    fn note_on(&mut self, _pitch: f32, _velocity: f32) {
        self.active = true;
    }
    fn note_off(&mut self) {
        self.active = false;
    }
    fn is_active(&self) -> bool {
        self.active
    }
    fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
    }
    fn set_param(&mut self, _param: crate::ParamId, _value: f32) {}
    fn reset(&mut self) {
        self.active = false;
    }
}

    #[test]
    fn stealing_caps_active_voices_at_capacity() {
        let mut pool = VoicePool::new(4, DummyVoice::new);
        for _ in 0..8 {
            pool.note_on(60.0, 1.0);
            pool.render(&mut [0.0f32; BLOCK_FRAMES]);
        }
        assert_eq!(pool.active_count(), 4);
        assert_eq!(pool.capacity(), 4);
    }

    #[test]
    fn free_slot_is_preferred_over_stealing() {
        let mut pool = VoicePool::new(4, DummyVoice::new);
        let a = pool.note_on(60.0, 1.0);
        let b = pool.note_on(60.0, 1.0);
        assert_ne!(a, b);
        assert_eq!(pool.active_count(), 2);
        pool.note_off(b);
        let c = pool.note_on(60.0, 1.0);
        assert_ne!(c, a, "freed slot should be reused before stealing");
    }
}
