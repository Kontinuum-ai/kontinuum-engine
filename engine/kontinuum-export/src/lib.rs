//! `kontinuum-export` — deliverable masters from a session (issue #102).
//!
//! Kontinuum renders music nobody could keep. This crate is the way out:
//! a session document in, a directory of industry-named files out.
//!
//! # What "the master" is here
//!
//! There is no source recording to be faithful to — the session *is* the
//! source, and it is deterministic on its seed. So the deliverables anchor
//! to the render graph rather than to a tape:
//!
//! | Preset | Chain | File |
//! |---|---|---|
//! | Archival | live mastering chain, graph's own rate | 32-bit float WAV |
//! | Lossless | live mastering chain | 48 kHz / 24-bit WAV |
//! | Press kit | offline premium chain (linear-phase tilt, ×8 limiting, LUFS normalize) | 16-bit WAV + MP3 320 |
//!
//! The archival file is the graph's own samples: no quantization, no
//! dither, nothing lost, nothing to reconstruct. Rate is not resampled —
//! the engine simply *builds* at the rate you ask for, so a 44.1 kHz
//! deliverable is a 44.1 kHz render, not a resampled 48 kHz one. There is
//! no resampler in the workspace and this crate does not add one.
//!
//! # Stems
//!
//! [`Cut::Stem`] renders the session with every other track muted and the
//! muted tracks' sends zeroed. Stems come out **unmastered** — a per-stem
//! master limiter would pump against a mix that is not there.
//!
//! They are **not additive**, and deliberately so. A muted track still
//! renders into the AutoMixer, so it keeps feeding the shared state that
//! makes a stem sit where it sits: the #76 kick duck, and the gain-staging
//! anchor every track's servo is levelled against. A stem is the track *as
//! it is in the record*, not the track alone in a room — which is what you
//! want to hand someone, and which means summing the stems does not
//! reconstruct the mix. The full contract is on
//! [`kontinuum_offline::RenderOptions::muted_tracks`].
//!
//! A **premium** stem is its own contract (#121). In a float deliverable it
//! ships at mix gain — tilt EQ and the full mix's shared drive, the ×8
//! limiter bypassed, samples may exceed 0 dBFS — because the limiter
//! engages by crest factor and would re-spread the gains the shared drive
//! evened out. A 16/24-bit deliverable cannot hold those samples, so its
//! premium stems keep the full chain and are independently legal masters.

use std::path::{Path, PathBuf};


use kontinuum_ir::Session;
use kontinuum_mastering::targets::MasteringTargets;
use kontinuum_offline::{
    premium_master, premium_master_peaks_bypassed, premium_master_with_drive, render_session_with,
    PremiumDrive, RenderError, RenderOptions, RenderOutput,
};

pub mod encode;
pub mod naming;
pub mod spec;

pub use encode::{Encoding, MP3_RATES};
pub use naming::ExportDate;
pub use spec::{ExportReportJson, ExportSpec, FileReport, Preset};

/// Everything that can go wrong producing a deliverable.
#[derive(Debug)]
pub enum ExportError {
    Render(RenderError),
    Io(std::io::Error),
    Wav(hound::Error),
    Mp3(String),
    /// A CBR rate outside [`MP3_RATES`].
    UnsupportedBitrate(u16),
    /// The request named no deliverables at all.
    NothingRequested,
    /// A stem was requested for a track the session does not have.
    NoSuchTrack(usize),
    /// An MP3 was requested at a sample rate MPEG-1 Layer III cannot carry.
    UnsupportedMp3SampleRate(u32),
    /// Two deliverables resolve to the same filename; writing both would
    /// silently leave only the second.
    CollidingNames(String),
    /// A sample rate outside [`SAMPLE_RATE_RANGE`].
    UnsupportedSampleRate(u32),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::Render(e) => write!(f, "render failed: {e}"),
            ExportError::Io(e) => write!(f, "io error: {e}"),
            ExportError::Wav(e) => write!(f, "wav encode failed: {e}"),
            ExportError::Mp3(e) => write!(f, "mp3 encode failed: {e}"),
            ExportError::UnsupportedBitrate(k) => {
                write!(f, "{k} kbps is not an offered rate (320, 256, 192)")
            }
            ExportError::NothingRequested => write!(f, "export requested no deliverables"),
            ExportError::NoSuchTrack(i) => write!(f, "no track at index {i}"),
            ExportError::UnsupportedMp3SampleRate(sr) => write!(
                f,
                "{sr} Hz cannot carry an MP3 at these rates (use 32000, 44100 or 48000)"
            ),
            ExportError::CollidingNames(name) => {
                write!(f, "two deliverables both resolve to \"{name}\"")
            }
            ExportError::UnsupportedSampleRate(sr) => write!(
                f,
                "{sr} Hz is outside the supported {}..={} Hz range",
                SAMPLE_RATE_RANGE.start(),
                SAMPLE_RATE_RANGE.end()
            ),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Render(e) => Some(e),
            ExportError::Io(e) => Some(e),
            ExportError::Wav(e) => Some(e),
            _ => None,
        }
    }
}

impl From<RenderError> for ExportError {
    fn from(e: RenderError) -> Self {
        ExportError::Render(e)
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        ExportError::Io(e)
    }
}

impl From<hound::Error> for ExportError {
    fn from(e: hound::Error) -> Self {
        ExportError::Wav(e)
    }
}

/// Which mastering chain a deliverable is rendered through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Master {
    /// The graph's real-time chain (#98) — bit-for-bit the master the
    /// listener heard, because the engine is deterministic on the seed.
    Live,
    /// The offline premium chain (#28): linear-phase tilt EQ, ×8
    /// oversampled true-peak limiting, loudness normalization to the
    /// targets file. Latency is free offline, so quality wins.
    Premium,
    /// No master bus processing — what stems want.
    None,
}

/// Which cut of the session a deliverable carries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Cut {
    /// Every track, as mixed.
    FullMix,
    /// One track alone, by session track index.
    Stem(usize),
}

impl Cut {
    /// The `(…)` component of the AES filename.
    fn label(&self, session: &Session) -> String {
        match self {
            Cut::FullMix => "Full Mix".to_string(),
            Cut::Stem(i) => match session.tracks.get(*i) {
                Some(t) => format!("Stem {}", t.id),
                None => format!("Stem {i}"),
            },
        }
    }
}

/// One file to produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deliverable {
    pub cut: Cut,
    pub encoding: Encoding,
    pub sample_rate: u32,
    pub master: Master,
}

/// Sample rate every preset renders at unless the caller overrides it. The
/// engine builds at whatever rate it is handed; 48 kHz is the iOS device
/// rate and the minimum lossless deliverable spec.
pub const DEFAULT_SAMPLE_RATE: u32 = kontinuum_offline::DEFAULT_SAMPLE_RATE;

/// Sample rates an export will render at.
///
/// The renderer sizes its buffers from the rate, so an absurd one asks for
/// an allocation that aborts the process instead of returning an error —
/// and this crate sits behind an FFI boundary that takes its parameters as
/// JSON. The range spans every rate a deliverable plausibly wants, from
/// telephone-band up to 192 kHz.
pub const SAMPLE_RATE_RANGE: std::ops::RangeInclusive<u32> = 8_000..=192_000;

impl Deliverable {
    /// Archival master: the graph's own float samples at `sample_rate`.
    pub fn archival(sample_rate: u32) -> Self {
        Deliverable {
            cut: Cut::FullMix,
            encoding: Encoding::WavFloat32,
            sample_rate,
            master: Master::Live,
        }
    }

    /// Minimum lossless deliverable: 24-bit WAV.
    pub fn lossless(sample_rate: u32) -> Self {
        Deliverable {
            cut: Cut::FullMix,
            encoding: Encoding::WavPcm24,
            sample_rate,
            master: Master::Live,
        }
    }

    /// Press-kit lossless: premium-mastered, dithered to 16-bit.
    pub fn press_kit_wav(sample_rate: u32) -> Self {
        Deliverable {
            cut: Cut::FullMix,
            encoding: Encoding::WavPcm16,
            sample_rate,
            master: Master::Premium,
        }
    }

    /// Press-kit MP3 at a CBR rate from [`MP3_RATES`].
    pub fn press_kit_mp3(sample_rate: u32, kbps: u16) -> Self {
        Deliverable {
            cut: Cut::FullMix,
            encoding: Encoding::Mp3Cbr { kbps },
            sample_rate,
            master: Master::Premium,
        }
    }

    /// One unmastered 24-bit stem.
    pub fn stem(track: usize, sample_rate: u32) -> Self {
        Deliverable {
            cut: Cut::Stem(track),
            encoding: Encoding::WavPcm24,
            sample_rate,
            master: Master::None,
        }
    }

    /// The v1 default set: archival, lossless, and the two press-kit files.
    pub fn default_set(sample_rate: u32) -> Vec<Deliverable> {
        vec![
            Deliverable::archival(sample_rate),
            Deliverable::lossless(sample_rate),
            Deliverable::press_kit_wav(sample_rate),
            Deliverable::press_kit_mp3(sample_rate, 320),
        ]
    }
}

/// A whole export job.
#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub artist: String,
    pub title: String,
    pub date: ExportDate,
    pub deliverables: Vec<Deliverable>,
    /// Directory to write into; created if it does not exist.
    pub out_dir: PathBuf,
}

impl ExportRequest {
    /// The v1 default: four full-mix files at [`DEFAULT_SAMPLE_RATE`].
    pub fn new(artist: &str, title: &str, date: ExportDate, out_dir: impl Into<PathBuf>) -> Self {
        ExportRequest {
            artist: artist.to_string(),
            title: title.to_string(),
            date,
            deliverables: Deliverable::default_set(DEFAULT_SAMPLE_RATE),
            out_dir: out_dir.into(),
        }
    }

    /// Append one 24-bit stem per session track.
    pub fn with_stems(mut self, session: &Session, sample_rate: u32) -> Self {
        for i in 0..session.tracks.len() {
            self.deliverables.push(Deliverable::stem(i, sample_rate));
        }
        self
    }
}

/// One file that was written.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportedFile {
    pub path: PathBuf,
    pub cut: Cut,
    pub encoding: Encoding,
    pub sample_rate: u32,
    pub master: Master,
    /// Size on disk.
    pub bytes: u64,
    /// Sample frames of program (excludes container overhead).
    pub frames: usize,
    /// FNV-1a 64 of the encoded file — the deliverable's fingerprint, and
    /// what a golden test pins.
    pub content_hash: u64,
}

impl ExportedFile {
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate as f64
    }
}

/// Everything an export produced.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportReport {
    pub files: Vec<ExportedFile>,
    /// Session seed — with the session document, the handle that re-renders
    /// any of these files bit-for-bit.
    pub seed: u64,
}

/// Render and write every deliverable in `request`.
///
/// Renders are shared across deliverables that need the same one, so the
/// default four-file set costs two renders (live-mastered and premium), not
/// four. Files are written in request order; on the first failure, the files
/// already written stay on disk and the error is returned.
pub fn export_session(
    session: &Session,
    request: &ExportRequest,
    targets: &MasteringTargets,
) -> Result<ExportReport, ExportError> {
    if request.deliverables.is_empty() {
        return Err(ExportError::NothingRequested);
    }
    // Everything that can be known before a single sample is rendered is
    // checked here, so a request that cannot be served in full does not
    // leave a half-written directory behind.
    // (name, case-folded name) — the fold is only for collision detection;
    // the file is written with the name as built.
    let mut names: Vec<(String, String)> = Vec::with_capacity(request.deliverables.len());
    for d in &request.deliverables {
        if !SAMPLE_RATE_RANGE.contains(&d.sample_rate) {
            return Err(ExportError::UnsupportedSampleRate(d.sample_rate));
        }
        if let Cut::Stem(i) = d.cut {
            if i >= session.tracks.len() {
                return Err(ExportError::NoSuchTrack(i));
            }
        }
        if let Encoding::Mp3Cbr { kbps } = d.encoding {
            if !MP3_RATES.contains(&kbps) {
                return Err(ExportError::UnsupportedBitrate(kbps));
            }
            if !encode::MP3_SAMPLE_RATES.contains(&d.sample_rate) {
                return Err(ExportError::UnsupportedMp3SampleRate(d.sample_rate));
            }
        }
        // Sanitizing collapses characters, so two distinct track ids can land
        // on one filename ("a/b" and "a b" both become "a b"). Writing both
        // would leave only the last, with a report claiming two files.
        // Compared case-insensitively because the filesystems this ships on
        // are: APFS and HFS+ are case-insensitive by default and NTFS always
        // is, so stems "Kick" and "kick" are one file on the disk that
        // actually receives them.
        let name = deliverable_name(session, request, d);
        let folded = name.to_lowercase();
        if names.iter().any(|(_, f)| *f == folded) {
            return Err(ExportError::CollidingNames(name));
        }
        names.push((name, folded));
    }
    std::fs::create_dir_all(&request.out_dir)?;

    // A render is held only while a later deliverable still needs it. Stems
    // are each written once, and a full-length session is hundreds of
    // megabytes of f32 per cut, so keeping them all would be the difference
    // between an export and a jetsam kill on a phone.
    let keys: Vec<RenderKey> = request
        .deliverables
        .iter()
        .map(|d| (d.cut.clone(), d.sample_rate, d.master, premium_stem_limits(d)))
        .collect();
    let mut cache: Vec<(RenderKey, RenderOutput)> = Vec::new();
    // Premium stems are gain-referenced to the premium FULL MIX (see
    // `PremiumDrive`): solving each stem's loudness on its own aims every
    // one of them at the same target, which is the mix balance being
    // discarded. Solved lazily, once per sample rate, only if a premium
    // stem is actually requested.
    let mut mix_drive: Vec<(u32, f64)> = Vec::new();
    let mut files = Vec::with_capacity(request.deliverables.len());
    for (i, (d, (name, _))) in request.deliverables.iter().zip(names).enumerate() {
        let key = &keys[i];
        if !cache.iter().any(|(k, _)| k == key) {
            let rendered = render_cut(
                session,
                &d.cut,
                d.sample_rate,
                d.master,
                targets,
                premium_stem_limits(d),
                &mut mix_drive,
            )?;
            cache.push((key.clone(), rendered));
        }
        let render = cache.iter().find(|(k, _)| k == key).map(|(_, r)| r).expect("just cached");
        files.push(write_deliverable(request, d, &name, session.seed, render)?);
        if !keys[i + 1..].contains(key) {
            cache.retain(|(k, _)| k != key);
        }
    }
    Ok(ExportReport { files, seed: session.seed })
}

/// What a cached render is keyed by: the cut, the rate it was built at,
/// which mastering chain it went through, and whether the premium chain's
/// peak-control stage runs (#121 — only a premium stem's answer depends on
/// the deliverable's encoding; every other combination is constant `true`,
/// so renders still share across encodings elsewhere).
type RenderKey = (Cut, u32, Master, bool);

/// Whether a deliverable's premium chain runs its peak-control stage (the
/// ×8 limiter and the ceiling trim).
///
/// Only a **premium stem** even asks (#121): the limiter engages by crest
/// factor, so with one shared drive in, a sparse percussive stem gives
/// several dB back that a dense sustained one keeps — ~8 dB of spread out
/// on the fixture, and the mix balance the shared drive preserves is
/// destroyed anyway. A float deliverable can carry the over-full-scale
/// samples the bypass leaves behind, so its stem ships at mix gain; an int
/// deliverable cannot exceed 0 dBFS, so its stems stay independently legal
/// masters with the full chain. MP3 quantizes in its own domain — it keeps
/// the chain too.
fn premium_stem_limits(d: &Deliverable) -> bool {
    match (&d.master, &d.cut) {
        (Master::Premium, Cut::Stem(_)) => !d.encoding.exceeds_full_scale(),
        _ => true,
    }
}

/// The AES filename for one deliverable.
fn deliverable_name(session: &Session, request: &ExportRequest, d: &Deliverable) -> String {
    naming::deliverable_name(
        &request.artist,
        &request.title,
        &d.cut.label(session),
        d.sample_rate,
        &d.encoding.spec_tag(),
        request.date,
        d.encoding.extension(),
    )
}

/// Render one (cut, rate, master) combination. `peak_control` decides the
/// premium stem's peak-control stage — see [`premium_stem_limits`].
fn render_cut(
    session: &Session,
    cut: &Cut,
    sample_rate: u32,
    master: Master,
    targets: &MasteringTargets,
    peak_control: bool,
    mix_drive: &mut Vec<(u32, f64)>,
) -> Result<RenderOutput, ExportError> {
    let options = match cut {
        Cut::FullMix => RenderOptions { mastering: master == Master::Live, muted_tracks: Vec::new() },
        Cut::Stem(i) => {
            let mut o = RenderOptions::stem(session.tracks.len(), *i);
            o.mastering = master == Master::Live;
            o
        }
    };
    // Every path renders the cut the caller asked for. The premium chain
    // then masters *that* render — it must not go back to the session and
    // re-render the full mix, or a premium stem would silently be the whole
    // mix under a stem filename. `options.mastering` is false here, which is
    // what the premium chain needs: it does its own mastering.
    let rendered = render_session_with(session, sample_rate, &options)?;
    match (master, cut) {
        // A full-mix premium delivery solves its own drive — and that is
        // exactly the drive the stems need, so seed the cache with it. The
        // options above are `{ mastering: false, muted_tracks: [] }` here,
        // i.e. `RenderOptions::unmastered()`, so this is provably the same
        // value `full_mix_drive` would render the mix again to compute.
        (Master::Premium, Cut::FullMix) => {
            let mastered = premium_master(rendered, session.seed, targets);
            if !mix_drive.iter().any(|(sr, _)| *sr == sample_rate) {
                mix_drive.push((sample_rate, mastered.drive_db));
            }
            Ok(mastered.master)
        }
        // A premium stem reuses the full mix's drive, so the stem set keeps
        // the balance it has in the mix. A float deliverable also skips the
        // per-stem limiter and ceiling trim — peak control engages by crest
        // factor and would re-spread the gains (#121); an int deliverable
        // keeps the full chain because it cannot hold over-full-scale
        // samples.
        (Master::Premium, Cut::Stem(_)) => {
            let drive = full_mix_drive(session, sample_rate, targets, mix_drive)?;
            let mastered = if peak_control {
                premium_master_with_drive(
                    rendered,
                    session.seed,
                    targets,
                    PremiumDrive::Fixed(drive),
                )
            } else {
                premium_master_peaks_bypassed(rendered, session.seed, targets, drive)
            };
            Ok(mastered.master)
        }
        (Master::Live | Master::None, _) => Ok(rendered),
    }
}

/// The premium chain's loudness drive for this session's full mix at
/// `sample_rate`, solved once and memoised in `cache`.
fn full_mix_drive(
    session: &Session,
    sample_rate: u32,
    targets: &MasteringTargets,
    cache: &mut Vec<(u32, f64)>,
) -> Result<f64, ExportError> {
    if let Some((_, db)) = cache.iter().find(|(sr, _)| *sr == sample_rate) {
        return Ok(*db);
    }
    let mix = render_session_with(session, sample_rate, &RenderOptions::unmastered())?;
    let db = premium_master(mix, session.seed, targets).drive_db;
    cache.push((sample_rate, db));
    Ok(db)
}

fn write_deliverable(
    request: &ExportRequest,
    deliverable: &Deliverable,
    name: &str,
    seed: u64,
    render: &RenderOutput,
) -> Result<ExportedFile, ExportError> {
    let path = request.out_dir.join(name);
    match deliverable.encoding {
        Encoding::WavFloat32 => encode::write_wav_f32(&path, render)?,
        Encoding::WavPcm24 => {
            let d = encode::dither_tpdf_24(&render.left, &render.right, seed);
            encode::write_wav_pcm24(&path, &d.left, &d.right, render.sample_rate)?;
        }
        Encoding::WavPcm16 => {
            let d = kontinuum_mastering::offline::dither_tpdf_16(&render.left, &render.right, seed);
            encode::write_wav_pcm16(&path, &d, render.sample_rate)?;
        }
        Encoding::Mp3Cbr { kbps } => {
            let bytes = encode::encode_mp3_cbr(render, kbps)?;
            std::fs::write(&path, &bytes)?;
        }
    }
    fingerprint(&path, deliverable, render)
}

fn fingerprint(
    path: &Path,
    deliverable: &Deliverable,
    render: &RenderOutput,
) -> Result<ExportedFile, ExportError> {
    let (bytes, content_hash) = hash_file(path)?;
    Ok(ExportedFile {
        path: path.to_path_buf(),
        cut: deliverable.cut.clone(),
        encoding: deliverable.encoding,
        sample_rate: render.sample_rate,
        master: deliverable.master,
        bytes,
        frames: render.left.len(),
        content_hash,
    })
}

/// Read buffer for [`hash_file`]. The renderer already holds the whole
/// program in memory, so the fingerprint pass must not add a second copy of
/// the file on top of it: a session at the 2048-bar ceiling is a multi-
/// hundred-megabyte WAV, and a phone will not forgive reading that twice.
const HASH_CHUNK: usize = 64 * 1024;

/// `(size, FNV-1a 64)` of a file, read in chunks. FNV-1a is a plain byte
/// fold, so hashing chunk by chunk gives exactly the value
/// [`fnv1a64`] would return for the whole buffer — asserted in the tests.
fn hash_file(path: &Path) -> Result<(u64, u64), ExportError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; HASH_CHUNK];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut size: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        for b in &buf[..n] {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    Ok((size, hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_core::fnv1a64;

    /// The chunked fold must agree with the one-shot hash the rest of the
    /// engine uses for golden renders, at and around the chunk boundary.
    #[test]
    fn chunked_hashing_matches_the_one_shot_hash() {
        let dir = std::env::temp_dir().join(format!("kontinuum-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        for len in [0usize, 1, HASH_CHUNK - 1, HASH_CHUNK, HASH_CHUNK + 1, HASH_CHUNK * 2 + 7] {
            let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let path = dir.join(format!("{len}.bin"));
            std::fs::write(&path, &bytes).expect("write");
            let (size, hash) = hash_file(&path).expect("hash");
            assert_eq!(size, len as u64, "size for {len} bytes");
            assert_eq!(hash, fnv1a64(&bytes), "hash for {len} bytes");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
