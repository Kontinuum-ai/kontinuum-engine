//! JSON contract between a host and [`crate::export_session`] (#102).
//!
//! Hosts speak JSON, not Rust types: the iOS and macOS shells hand the
//! bridge a session document and a spec, and get a report back. Keeping the
//! shape here (rather than in the bridge) means the contract is versioned
//! and tested next to the code that honors it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Cut, Deliverable, Encoding, ExportDate, ExportRequest, Master, DEFAULT_SAMPLE_RATE};

/// A deliverable preset, named the way the export sheet names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Preset {
    /// 32-bit float WAV at the session's own rate.
    Archival,
    /// 48 kHz / 24-bit WAV (or whatever `sampleRate` asks for).
    Lossless,
    /// Premium-mastered 16-bit WAV.
    PressKitWav,
    /// Premium-mastered MP3 at `mp3Kbps`.
    PressKitMp3,
}

/// What a host asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSpec {
    pub artist: String,
    pub title: String,
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub out_dir: PathBuf,
    /// Empty means the default four-file set.
    #[serde(default)]
    pub presets: Vec<Preset>,
    #[serde(default = "default_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_kbps")]
    pub mp3_kbps: u16,
    /// Append one unmastered 24-bit stem per track.
    #[serde(default)]
    pub stems: bool,
}

fn default_rate() -> u32 {
    DEFAULT_SAMPLE_RATE
}

fn default_kbps() -> u16 {
    320
}

impl ExportSpec {
    /// Resolve into the typed request. `track_count` comes from the session
    /// and only matters when `stems` is set.
    pub fn into_request(self, track_count: usize) -> ExportRequest {
        let rate = self.sample_rate;
        let presets = if self.presets.is_empty() {
            vec![Preset::Archival, Preset::Lossless, Preset::PressKitWav, Preset::PressKitMp3]
        } else {
            self.presets
        };
        let mut deliverables: Vec<Deliverable> = presets
            .iter()
            .map(|p| match p {
                Preset::Archival => Deliverable::archival(rate),
                Preset::Lossless => Deliverable::lossless(rate),
                Preset::PressKitWav => Deliverable::press_kit_wav(rate),
                Preset::PressKitMp3 => Deliverable::press_kit_mp3(rate, self.mp3_kbps),
            })
            .collect();
        if self.stems {
            deliverables.extend((0..track_count).map(|i| Deliverable::stem(i, rate)));
        }
        ExportRequest {
            artist: self.artist,
            title: self.title,
            date: ExportDate::new(self.year, self.month, self.day),
            deliverables,
            out_dir: self.out_dir,
        }
    }
}

/// One written file, as the host sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub path: PathBuf,
    /// `"fullMix"`, or `"stem:<track id>"`.
    pub cut: String,
    /// `"wavFloat32"`, `"wavPcm24"`, `"wavPcm16"`, `"mp3_320"`.
    pub encoding: String,
    pub sample_rate: u32,
    /// `"live"`, `"premium"`, `"none"`.
    pub master: String,
    pub bytes: u64,
    pub frames: usize,
    pub duration_secs: f64,
    /// Lowercase hex, so JavaScript-ish hosts cannot round-trip it through a
    /// float and lose the low bits.
    pub content_hash: String,
}

/// What a host gets back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReportJson {
    pub seed: u64,
    pub files: Vec<FileReport>,
}

impl ExportReportJson {
    pub fn from_report(report: &crate::ExportReport, session: &kontinuum_ir::Session) -> Self {
        ExportReportJson {
            seed: report.seed,
            files: report
                .files
                .iter()
                .map(|f| FileReport {
                    path: f.path.clone(),
                    cut: match &f.cut {
                        Cut::FullMix => "fullMix".to_string(),
                        Cut::Stem(i) => match session.tracks.get(*i) {
                            Some(t) => format!("stem:{}", t.id),
                            None => format!("stem:{i}"),
                        },
                    },
                    encoding: match f.encoding {
                        Encoding::WavFloat32 => "wavFloat32".to_string(),
                        Encoding::WavPcm24 => "wavPcm24".to_string(),
                        Encoding::WavPcm16 => "wavPcm16".to_string(),
                        Encoding::Mp3Cbr { kbps } => format!("mp3_{kbps}"),
                    },
                    sample_rate: f.sample_rate,
                    master: match f.master {
                        Master::Live => "live",
                        Master::Premium => "premium",
                        Master::None => "none",
                    }
                    .to_string(),
                    bytes: f.bytes,
                    frames: f.frames,
                    duration_secs: f.duration_secs(),
                    content_hash: format!("{:016x}", f.content_hash),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_spec_defaults_to_the_four_file_set() {
        let spec: ExportSpec = serde_json::from_str(
            r#"{"artist":"K","title":"T","year":2026,"month":9,"day":2,"outDir":"/tmp/x"}"#,
        )
        .expect("parse");
        assert_eq!(spec.sample_rate, DEFAULT_SAMPLE_RATE);
        assert_eq!(spec.mp3_kbps, 320);
        assert!(!spec.stems);
        let req = spec.into_request(4);
        assert_eq!(req.deliverables.len(), 4);
        assert_eq!(req.deliverables, Deliverable::default_set(DEFAULT_SAMPLE_RATE));
    }

    #[test]
    fn stems_are_appended_one_per_track() {
        let spec: ExportSpec = serde_json::from_str(
            r#"{"artist":"K","title":"T","year":2026,"month":9,"day":2,
                "outDir":"/tmp/x","presets":["archival"],"stems":true}"#,
        )
        .expect("parse");
        let req = spec.into_request(3);
        assert_eq!(req.deliverables.len(), 4);
        assert_eq!(req.deliverables[1].cut, Cut::Stem(0));
        assert_eq!(req.deliverables[3].cut, Cut::Stem(2));
        assert!(req.deliverables[1..].iter().all(|d| d.master == Master::None));
    }

    #[test]
    fn a_typo_in_the_spec_is_an_error_not_a_silent_default() {
        let bad = r#"{"artist":"K","title":"T","year":2026,"month":9,"day":2,
                      "outDir":"/tmp/x","sampleRat":44100}"#;
        assert!(serde_json::from_str::<ExportSpec>(bad).is_err());
    }

    #[test]
    fn content_hashes_serialize_as_fixed_width_hex() {
        let f = FileReport {
            path: "/tmp/a.wav".into(),
            cut: "fullMix".into(),
            encoding: "wavPcm24".into(),
            sample_rate: 48_000,
            master: "live".into(),
            bytes: 10,
            frames: 5,
            duration_secs: 0.1,
            content_hash: format!("{:016x}", 0x00ff_u64),
        };
        assert_eq!(f.content_hash, "00000000000000ff");
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"contentHash\":\"00000000000000ff\""), "{json}");
        assert!(json.contains("\"sampleRate\":48000"), "{json}");
    }
}
