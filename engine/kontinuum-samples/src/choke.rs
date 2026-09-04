//! Choke groups (issue #19): hat logic. When a hit assigned to choke group
//! `g` triggers, every still-sounding voice in `g` fast-fades to silence
//! within [`CHOKE_FADE_MS`]. The offline renderer models this with per-hit
//! gain envelopes applied during mixdown — a pure function of the document,
//! so choked renders stay bit-reproducible.

/// Fast-fade ceiling for a choked voice.
pub const CHOKE_FADE_MS: f32 = 10.0;

/// One hit's contribution to the mix: frame offset, choke assignment, and
/// its rendered (chain-processed) buffer. `buf` is modified in place by the
/// choke pass.
pub struct HitPart {
    pub start: usize,
    pub group: Option<u8>,
    pub buf: Vec<f32>,
}

/// Apply a deterministic linear fade starting at `from` frames into `buf`,
/// reaching exact zero after `fade_ms`. Frames past the fade are hard-zeroed.
pub fn choke_fade(buf: &mut [f32], from: usize, fade_ms: f32, sample_rate: u32) {
    if from >= buf.len() {
        return;
    }
    let n = ((fade_ms / 1000.0) * sample_rate as f32).round().max(1.0) as usize;
    let end = (from + n).min(buf.len());
    for (k, s) in buf[from..end].iter_mut().enumerate() {
        *s *= 1.0 - (k + 1) as f32 / (n + 1) as f32;
    }
    for s in buf[end..].iter_mut() {
        *s = 0.0;
    }
}

/// Choke pass + mixdown. Hits are faded by later same-group triggers, then
/// every part is added into `pcm` in document order. With no group
/// assignments this is exactly the plain additive mixdown (gain 1.0 is
/// bit-exact), so choke-free packs render identically to the pre-choke path.
pub fn mix(pcm: &mut [f32], parts: &mut [HitPart], sample_rate: u32) {
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&k| parts[k].start);
    for (pos, &j) in order.iter().enumerate() {
        let Some(group) = parts[j].group else { continue };
        let at = parts[j].start;
        for &i in order.iter().take(pos) {
            if parts[i].group != Some(group) {
                continue;
            }
            let into = at.saturating_sub(parts[i].start);
            choke_fade(&mut parts[i].buf, into, CHOKE_FADE_MS, sample_rate);
        }
    }
    for part in parts {
        let frames = (part.start + part.buf.len()).min(pcm.len());
        if part.start < pcm.len() {
            for (pcm_slot, s) in pcm[part.start..frames].iter_mut().zip(part.buf.iter()) {
                *pcm_slot += *s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(start: usize, group: Option<u8>, len: usize, level: f32) -> HitPart {
        HitPart { start, group, buf: vec![level; len] }
    }

    #[test]
    fn fade_reaches_zero_within_the_ceiling() {
        let mut buf = vec![1.0f32; 48_000];
        choke_fade(&mut buf, 1000, CHOKE_FADE_MS, 48_000);
        let n = (CHOKE_FADE_MS / 1000.0 * 48_000.0) as usize;
        assert!(buf[1000] < 1.0, "fade starts immediately");
        assert!(buf[1000 + n - 1] > 0.0 && buf[1000 + n - 1] < 0.01);
        assert!(buf[1000 + n..].iter().all(|&s| s == 0.0), "tail hard-zeroed");
        assert_eq!(&buf[..1000], &vec![1.0f32; 1000], "pre-choke audio untouched");
    }

    #[test]
    fn same_group_hits_choke_each_other() {
        let mut pcm = vec![0.0f32; 48_000];
        let mut parts = vec![
            part(0, Some(1), 10_000, 1.0),    // closed hat, long tail
            part(4800, Some(1), 10_000, 1.0), // next hat 100 ms later
        ];
        mix(&mut pcm, &mut parts, 48_000);
        let fade = 480; // 10 ms @ 48 kHz
        let choked_at = |f: usize| pcm[f] - 1.0; // subtract the surviving hit
        assert!((pcm[4799] - 1.0).abs() < 1e-6, "choke starts at trigger");
        assert_eq!(choked_at(4800 + fade), 0.0, "choked to exact zero in ≤10 ms");
    }

    #[test]
    fn different_groups_do_not_interact() {
        let mut pcm = vec![0.0f32; 48_000];
        let mut parts = vec![
            part(0, Some(1), 10_000, 1.0),
            part(4800, Some(2), 10_000, 1.0),
        ];
        mix(&mut pcm, &mut parts, 48_000);
        assert!(pcm[..4800].iter().all(|&s| (s - 1.0).abs() < 1e-6));
    }

    #[test]
    fn no_groups_mixes_bit_identically_to_plain_add() {
        let mut pcm = vec![0.0f32; 20_000];
        let mut parts = vec![part(0, None, 10_000, 0.5), part(300, None, 10_000, 0.25)];
        mix(&mut pcm, &mut parts, 48_000);
        let mut plain = vec![0.0f32; 20_000];
        for (start, level) in [(0usize, 0.5f32), (300, 0.25)] {
            for s in plain[start..start + 10_000].iter_mut() {
                *s += level;
            }
        }
        assert!(pcm.iter().zip(plain.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    }

    #[test]
    fn chokes_are_deterministic() {
        let run = || {
            let mut pcm = vec![0.0f32; 24_000];
            let mut parts = vec![
                part(0, Some(3), 20_000, 1.0),
                part(960, Some(3), 20_000, 0.8),
                part(1920, Some(3), 20_000, 0.6),
            ];
            mix(&mut pcm, &mut parts, 48_000);
            pcm
        };
        let (a, b) = (run(), run());
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    }
}
