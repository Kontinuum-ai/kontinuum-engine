//! AES-convention deliverable filenames (#102).
//!
//! Shape: `{artist} - {title} ({mix}) {rate}-{depth} {YYYYMMDD}.{ext}`, e.g.
//! `Kontinuum - Night Shift (Full Mix) 48k-24bit 20260902.wav`. The point of
//! the convention (AES TD1002.2.15) is that the file says what it *is*, so
//! `final_v3_FINAL(2).wav` never has to exist: two renders of the same
//! session at the same spec on the same day collide by design — they are the
//! same deliverable, because the engine is deterministic on the session seed.

/// Calendar stamp for a deliverable name. Supplied by the caller (the host
/// owns the clock) so a render is reproducible from its inputs alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl ExportDate {
    /// Clamped to a printable calendar stamp; no calendar validation beyond
    /// range (a bad date from a host should not fail an export).
    pub fn new(year: u16, month: u8, day: u8) -> Self {
        ExportDate { year: year.min(9999), month: month.clamp(1, 12), day: day.clamp(1, 31) }
    }

    fn stamp(&self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }
}

/// Characters no common filesystem (APFS, HFS+, exFAT, NTFS) will take, plus
/// the ones that make a name ambiguous once it is pasted into a shell or a
/// submission form.
const ILLEGAL: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'];

/// Longest single name component we emit, in bytes.
const MAX_COMPONENT: usize = 96;

/// Longest whole filename we emit, in bytes. APFS, HFS+, exFAT and NTFS all
/// cap a path component at 255 bytes, and capping the three free-text fields
/// individually is not enough — artist + title + mix at 96 bytes each blows
/// past 255 together. The budget leaves room for the " 2.wav" style suffixes
/// share sheets and mail clients append to avoid their own collisions.
const MAX_FILENAME: usize = 200;

/// Squeeze one user-supplied field into a filename component: illegal and
/// control characters out, whitespace collapsed, leading/trailing dots and
/// spaces trimmed (a trailing dot is invalid on some hosts, a leading one
/// hides the file), truncated on a char boundary.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_space = false;
    for ch in raw.chars() {
        let ch = if ILLEGAL.contains(&ch) || ch.is_control() { ' ' } else { ch };
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    while out.len() > MAX_COMPONENT {
        out.pop();
    }
    let trimmed = out.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Human sample-rate tag: `48k`, `44.1k`, `96k`.
pub fn rate_tag(sample_rate: u32) -> String {
    let khz = sample_rate as f64 / 1000.0;
    if (khz - khz.round()).abs() < 1e-9 {
        format!("{}k", khz.round() as u64)
    } else {
        format!("{khz}k")
    }
}

/// Assemble the deliverable filename. `mix` names which cut this is ("Full
/// Mix", "Stem Bass"), `spec` the format tag ("24bit", "320kbps").
///
/// If the three free-text fields would push the name past [`MAX_FILENAME`],
/// the *title* is shortened first and then the *artist* — never the mix, the
/// rate/spec tag or the date, which are what make the name a deliverable
/// description rather than a label.
pub fn deliverable_name(
    artist: &str,
    title: &str,
    mix: &str,
    sample_rate: u32,
    spec: &str,
    date: ExportDate,
    extension: &str,
) -> String {
    let mix = sanitize(mix);
    let rate = rate_tag(sample_rate);
    let stamp = date.stamp();
    let assemble = |artist: &str, title: &str| {
        format!("{artist} - {title} ({mix}) {rate}-{spec} {stamp}.{extension}")
    };
    let mut artist = sanitize(artist);
    let mut title = sanitize(title);
    // Trim the title down to one character before touching the artist, then
    // trim the artist the same way. Both stay non-empty, so the shape of the
    // name survives even an absurd input.
    while assemble(&artist, &title).len() > MAX_FILENAME && title.chars().count() > 1 {
        title.pop();
    }
    while assemble(&artist, &title).len() > MAX_FILENAME && artist.chars().count() > 1 {
        artist.pop();
    }
    assemble(artist.trim_end(), title.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_the_aes_shape() {
        let name = deliverable_name(
            "Kontinuum",
            "Night Shift",
            "Full Mix",
            48_000,
            "24bit",
            ExportDate::new(2026, 9, 2),
            "wav",
        );
        assert_eq!(name, "Kontinuum - Night Shift (Full Mix) 48k-24bit 20260902.wav");
    }

    #[test]
    fn strips_path_separators_and_collapses_whitespace() {
        // Leading dots are trimmed too, so a traversal string cannot survive
        // as a relative path component.
        assert_eq!(sanitize("../../etc/passwd"), "etc passwd");
        assert_eq!(sanitize("  many   spaces \t here "), "many spaces here");
        assert_eq!(sanitize("tab\there"), "tab here");
    }

    #[test]
    fn never_yields_an_empty_or_hidden_component() {
        assert_eq!(sanitize(""), "Untitled");
        assert_eq!(sanitize("   "), "Untitled");
        assert_eq!(sanitize("..."), "Untitled");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize("trailing."), "trailing");
    }

    #[test]
    fn truncates_on_a_char_boundary() {
        let long = "é".repeat(200);
        let out = sanitize(&long);
        assert!(out.len() <= MAX_COMPONENT, "{} bytes", out.len());
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_long_artist_and_title_still_fit_a_filesystem_component() {
        let name = deliverable_name(
            &"Artist".repeat(40),
            &"Title".repeat(40),
            "Stem kick",
            48_000,
            "320kbps",
            ExportDate::new(2026, 9, 2),
            "mp3",
        );
        assert!(name.len() <= MAX_FILENAME, "{} bytes: {name}", name.len());
        // The machine-readable tail is never what gets sacrificed.
        assert!(name.ends_with(" (Stem kick) 48k-320kbps 20260902.mp3"), "{name}");
    }

    #[test]
    fn rate_tags_read_the_way_engineers_write_them() {
        assert_eq!(rate_tag(48_000), "48k");
        assert_eq!(rate_tag(96_000), "96k");
        assert_eq!(rate_tag(44_100), "44.1k");
    }
}
