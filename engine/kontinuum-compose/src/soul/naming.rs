//! Naming guardrail (issue #55 "legal guardrail (naming, not content)"):
//! style is not copyrightable and every layer is our own data — but artist
//! names are trademarks/publicity rights. First-party shipped souls use
//! descriptive names ("Detroit 909 minimalism", "dusty microhouse,
//! Perlon-school"); user-created packs may be named freely locally, but the
//! **shared catalog rejects real artist names in titles** (community
//! convention: "inspired-by" descriptors). Verified artist accounts lift
//! this for their own name at the catalog level, not here.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamingError {
    /// The title names a real artist; share under a descriptor instead.
    RealArtist(String),
}

impl std::fmt::Display for NamingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamingError::RealArtist(name) => write!(
                f,
                "title `{name}` names a real artist; shared-catalog titles must be descriptive (\"inspired-by\" convention)"
            ),
        }
    }
}

impl std::error::Error for NamingError {}

/// Case- and whitespace-insensitive exact-title blocklist of iconic
/// electronic artists. Deliberately conservative: substring matches would
/// reject legitimate descriptive titles ("Mills & Machine Rooms" is fine,
/// "Jeff Mills" is not). The shared catalog extends this list; local packs
/// never run this check.
pub fn check_shareable_name(name: &str) -> Result<(), NamingError> {
    let key = normalize(name);
    if ARTIST_BLOCKLIST.iter().any(|a| normalize(a) == key) {
        return Err(NamingError::RealArtist(name.to_string()));
    }
    Ok(())
}

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Iconic electronic artists whose names must not become shared pack titles.
const ARTIST_BLOCKLIST: &[&str] = &[
    "Jeff Mills",
    "Richie Hawtin",
    "Plastikman",
    "Carl Cox",
    "Nina Kraviz",
    "Derrick May",
    "Kevin Saunderson",
    "Juan Atkins",
    "Robert Hood",
    "Dave Clarke",
    "Sven Vath",
    "Ricardo Villalobos",
    "Moodymann",
    "Theo Parrish",
    "Aphex Twin",
    "Burial",
    "Boards of Canada",
    "Basic Channel",
    "Moritz von Oswald",
    "Mark Ernestus",
    "Green Velvet",
    "Amelie Lens",
    "Charlotte de Witte",
    "Solomun",
    "Maceo Plex",
    "Jamie Jones",
    "Seth Troxler",
    "Ben Klock",
    "Marcel Dettmann",
    "Len Faki",
    "Helena Hauff",
    "Blawan",
    "Surgeon",
    "James Ruskin",
    "Paul Kalkbrenner",
    "Chris Liebing",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptive_names_pass() {
        for name in [
            "Detroit 909 minimalism",
            "dusty microhouse, Perlon-school",
            "Warehouse Dub Charts",
            "jeff mills inspired workshop", // substring is not the artist
        ] {
            assert_eq!(check_shareable_name(name), Ok(()), "{name} must pass");
        }
    }

    #[test]
    fn real_artist_names_are_rejected_regardless_of_case_or_spacing() {
        for name in ["Jeff Mills", "jeff   mills", "JEFF MILLS", "aphex twin"] {
            assert!(
                matches!(check_shareable_name(name), Err(NamingError::RealArtist(_))),
                "{name} must be rejected"
            );
        }
    }
}
