//! Deliverable encoders (#102): WAV at three depths, MP3 at three CBR rates.
//!
//! **Why a pure-Rust MP3 encoder.** MP3 320 is the press-kit format the
//! industry actually asks for, and it has to come out of the Rust core:
//! AVFoundation/CoreAudio *decode* MP3 on iOS but do not encode it, so there
//! is no host-side path. The usual Rust option is an FFI binding to
//! libmp3lame, which is LGPL and needs a C toolchain in every cross-compile —
//! two problems for a statically linked MIT iOS app. `rusty_mp3` is
//! Apache-2.0, has no C and no dependencies, and therefore builds for
//! device, simulator, macOS and CI off one code path.

use std::path::Path;

use kontinuum_mastering::offline::Dithered16;
use kontinuum_offline::RenderOutput;

use crate::ExportError;

/// Container/precision of one deliverable file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// 32-bit float WAV — the archival master: the graph's own samples, no
    /// quantization, no dither, nothing to reconstruct.
    WavFloat32,
    /// 24-bit PCM WAV — the minimum lossless deliverable.
    WavPcm24,
    /// 16-bit PCM WAV, TPDF-dithered — CD-spec press-kit lossless.
    WavPcm16,
    /// Constant-bitrate MP3 at `kbps` (320 / 256 / 192).
    Mp3Cbr { kbps: u16 },
}

impl Encoding {
    pub fn extension(&self) -> &'static str {
        match self {
            Encoding::Mp3Cbr { .. } => "mp3",
            _ => "wav",
        }
    }

    /// The precision half of the AES filename tag.
    pub fn spec_tag(&self) -> String {
        match self {
            Encoding::WavFloat32 => "32float".to_string(),
            Encoding::WavPcm24 => "24bit".to_string(),
            Encoding::WavPcm16 => "16bit".to_string(),
            Encoding::Mp3Cbr { kbps } => format!("{kbps}kbps"),
        }
    }

    /// Whether this encoding quantizes below float, i.e. wants dither.
    pub fn is_quantized(&self) -> bool {
        matches!(self, Encoding::WavPcm24 | Encoding::WavPcm16)
    }

    /// Whether this encoding can carry samples above full scale. Only the
    /// float WAV can: the PCM depths clip at 0 dBFS, and MP3 quantizes in
    /// its own domain. This is what decides a premium stem's peak-control
    /// stage (#121).
    pub fn exceeds_full_scale(&self) -> bool {
        matches!(self, Encoding::WavFloat32)
    }
}

/// Full-scale for 24-bit signed PCM.
const PCM24_SCALE: f32 = 8_388_607.0;

/// Write a 32-bit float stereo WAV — the samples the graph produced.
pub fn write_wav_f32(path: &Path, out: &RenderOutput) -> Result<(), ExportError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: out.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for (l, r) in out.left.iter().zip(out.right.iter()) {
        writer.write_sample(*l)?;
        writer.write_sample(*r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Write a 24-bit PCM stereo WAV from an already-dithered 24-bit payload.
pub fn write_wav_pcm24(
    path: &Path,
    left: &[i32],
    right: &[i32],
    sample_rate: u32,
) -> Result<(), ExportError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for (l, r) in left.iter().zip(right.iter()) {
        writer.write_sample(*l)?;
        writer.write_sample(*r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Write a 16-bit PCM stereo WAV from a dithered payload.
pub fn write_wav_pcm16(
    path: &Path,
    dithered: &Dithered16,
    sample_rate: u32,
) -> Result<(), ExportError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for (l, r) in dithered.left.iter().zip(dithered.right.iter()) {
        writer.write_sample(*l)?;
        writer.write_sample(*r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// TPDF-dithered stereo 24-bit payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Dithered24 {
    pub left: Vec<i32>,
    pub right: Vec<i32>,
}

/// Quantize a float master to 24-bit with TPDF dither at the 24-bit LSB,
/// seeded from the session seed — the 24-bit twin of mastering's
/// `dither_tpdf_16`, kept here because 24-bit is an export-only depth.
///
/// At 24 bits the dither is ~144 dB down and inaudible; it is applied
/// anyway so the quantization error stays uncorrelated with the signal,
/// which is the whole reason the deliverable is dithered at all.
pub fn dither_tpdf_24(left: &[f32], right: &[f32], seed: u64) -> Dithered24 {
    // SplitMix64 is defined over every u64 state, so the seed goes in as it
    // is: OR-ing in a low bit would alias each even seed onto its odd
    // neighbour and hand two different sessions the same dither stream.
    let mut state = seed;
    let mut next_unit = move || -> f64 {
        // SplitMix64 — same shape as mastering's Rng, kept local so the
        // 24-bit path never perturbs the 16-bit stream's sequence.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z = z ^ (z >> 31);
        (z >> 11) as f64 / (1u64 << 53) as f64
    };
    let quantize = |x: f32, unit: &mut dyn FnMut() -> f64| -> i32 {
        let t = unit() + unit() - 1.0;
        let scaled = x as f64 * PCM24_SCALE as f64 + t;
        scaled.round().clamp(-8_388_608.0, 8_388_607.0) as i32
    };
    let mut out_l = Vec::with_capacity(left.len());
    let mut out_r = Vec::with_capacity(right.len());
    for (l, r) in left.iter().zip(right.iter()) {
        out_l.push(quantize(*l, &mut next_unit));
        out_r.push(quantize(*r, &mut next_unit));
    }
    Dithered24 { left: out_l, right: out_r }
}

/// CBR rates the press-kit presets offer, highest first.
pub const MP3_RATES: [u16; 3] = [320, 256, 192];

/// Sample rates an MP3 deliverable may use: the MPEG-1 Layer III set.
///
/// This is not fussiness. At MPEG-2/2.5 rates (24 kHz and below) Layer III
/// caps out at 160 kbps, and the encoder silently *snaps* a 320 kbps request
/// down to the nearest legal value — which would leave a file called
/// `…-320kbps.mp3` that is nothing of the sort. Rejecting the combination up
/// front is the only way the filename can stay true.
pub const MP3_SAMPLE_RATES: [u32; 3] = [32_000, 44_100, 48_000];

/// Encode a float master to a constant-bitrate MP3 in memory.
///
/// The float master is fed straight in: MP3 quantizes in its own domain, so
/// pre-dithering to 16 bits first would only add noise the encoder then has
/// to spend bits on.
///
/// **Determinism caveat.** The encoder reads `MP3_RESERVOIR`,
/// `MP3_RESV_GAIN` and `MP3_RESV_LOOKAHEAD` from the environment to steer
/// its bit-reservoir mode, so identical input encodes to identical bytes
/// only when those are unset — which they are in the app, in CI and in the
/// tests. Two further notes from the encoder's own documentation: reservoir
/// mode is on by default at or below 256 kbps and off at 320 (where the
/// author reports it produces valid-but-garbled frames), and it applies to
/// MPEG-1 CBR only. Setting `MP3_RESERVOIR=1` would therefore force a known
/// broken path at our default rate; nothing in this crate sets it.
pub fn encode_mp3_cbr(out: &RenderOutput, kbps: u16) -> Result<Vec<u8>, ExportError> {
    if !MP3_RATES.contains(&kbps) {
        return Err(ExportError::UnsupportedBitrate(kbps));
    }
    if !MP3_SAMPLE_RATES.contains(&out.sample_rate) {
        return Err(ExportError::UnsupportedMp3SampleRate(out.sample_rate));
    }
    let mut encoder = rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
        bitrate_kbps: kbps as u32,
        vbr_quality: None,
    });
    let mut bytes = Vec::new();
    // One second of frames at a time: bounded peak memory on a phone, and
    // well above the 1152-sample frame so every push emits packets.
    let chunk = out.sample_rate as usize;
    let mut interleaved = Vec::with_capacity(chunk * 2);
    let mut start = 0usize;
    while start < out.left.len() {
        let end = (start + chunk).min(out.left.len());
        interleaved.clear();
        for i in start..end {
            interleaved.push(out.left[i]);
            interleaved.push(out.right[i]);
        }
        encoder
            .push_pcm_f32(&interleaved, 2, out.sample_rate)
            .map_err(|e| ExportError::Mp3(format!("{e:?}")))?;
        drain(&mut encoder, &mut bytes);
        start = end;
    }
    encoder.finish();
    drain(&mut encoder, &mut bytes);
    if bytes.is_empty() {
        return Err(ExportError::Mp3("encoder produced no frames".to_string()));
    }
    Ok(bytes)
}

fn drain(encoder: &mut rusty_mp3::Mp3Encoder, into: &mut Vec<u8>) {
    while let Ok(packet) = encoder.next_packet() {
        into.extend_from_slice(&packet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sample_rate: u32, secs: f32) -> RenderOutput {
        let n = (sample_rate as f32 * secs) as usize;
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 / sample_rate as f64;
            let v = (std::f64::consts::TAU * 440.0 * t).sin() as f32 * 0.5;
            left.push(v);
            right.push(v * 0.8);
        }
        RenderOutput { left, right, sample_rate }
    }

    #[test]
    fn dither_24_is_deterministic_and_in_range() {
        let src = tone(48_000, 0.05);
        let a = dither_tpdf_24(&src.left, &src.right, 7);
        let b = dither_tpdf_24(&src.left, &src.right, 7);
        assert_eq!(a, b, "same seed must give the same payload");
        let c = dither_tpdf_24(&src.left, &src.right, 8);
        assert_ne!(a, c, "a different seed must move the dither");
        assert!(a.left.iter().all(|&s| (-8_388_608..=8_388_607).contains(&s)));
    }

    /// The dither must be small enough to be a rounding detail, not a signal
    /// change: 24-bit quantization error stays within a couple of LSBs.
    #[test]
    fn dither_24_tracks_the_source() {
        let src = tone(48_000, 0.05);
        let d = dither_tpdf_24(&src.left, &src.right, 3);
        for (s, q) in src.left.iter().zip(d.left.iter()) {
            let err = (*q as f64 - *s as f64 * PCM24_SCALE as f64).abs();
            assert!(err <= 2.0, "quantization error {err} LSB");
        }
    }

    #[test]
    fn hard_clipping_input_stays_inside_the_pcm_range() {
        let n = 512;
        let hot: Vec<f32> = (0..n).map(|i| if i % 2 == 0 { 4.0 } else { -4.0 }).collect();
        let d = dither_tpdf_24(&hot, &hot, 11);
        assert!(d.left.iter().all(|&s| (-8_388_608..=8_388_607).contains(&s)));
    }

    #[test]
    fn mp3_320_encodes_a_valid_stream() {
        let src = tone(48_000, 0.5);
        let bytes = encode_mp3_cbr(&src, 320).expect("encode");
        // Frame sync: 11 set bits open every MPEG audio frame. The stream
        // starts on the Xing/Info frame, which carries the same header.
        assert!(bytes.len() > 1000, "{} bytes is too short", bytes.len());
        assert_eq!(bytes[0], 0xFF, "no frame sync at byte 0");
        assert_eq!(bytes[1] & 0xE0, 0xE0, "no frame sync at byte 1");
    }

    #[test]
    fn mp3_is_deterministic() {
        let src = tone(48_000, 0.3);
        let a = encode_mp3_cbr(&src, 320).unwrap();
        let b = encode_mp3_cbr(&src, 320).unwrap();
        assert_eq!(a, b, "the same master must encode to the same bytes");
    }

    #[test]
    fn lower_rates_produce_smaller_files() {
        let src = tone(48_000, 0.3);
        let hi = encode_mp3_cbr(&src, 320).unwrap().len();
        let mid = encode_mp3_cbr(&src, 256).unwrap().len();
        let lo = encode_mp3_cbr(&src, 192).unwrap().len();
        assert!(hi > mid && mid > lo, "{hi} / {mid} / {lo}");
    }

    #[test]
    fn neighbouring_seeds_give_different_dither() {
        let src = tone(48_000, 0.02);
        let even = dither_tpdf_24(&src.left, &src.right, 4);
        let odd = dither_tpdf_24(&src.left, &src.right, 5);
        assert_ne!(even, odd, "seeds 4 and 5 must not share a dither stream");
    }

    #[test]
    fn rejects_an_mp3_sample_rate_mpeg1_cannot_carry() {
        let src = tone(24_000, 0.05);
        assert!(matches!(
            encode_mp3_cbr(&src, 320),
            Err(ExportError::UnsupportedMp3SampleRate(24_000))
        ));
    }

    #[test]
    fn rejects_a_rate_that_is_not_offered() {
        let src = tone(48_000, 0.05);
        assert!(matches!(
            encode_mp3_cbr(&src, 128),
            Err(ExportError::UnsupportedBitrate(128))
        ));
    }
}
