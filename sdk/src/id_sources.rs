//! Canonical id-source constants for plugin manifests and lookup requests.
//!
//! Closed set; adding a new id source requires an SDK version bump. Runtime
//! rejects unknown id-sources at manifest load.

pub const TMDB: &str        = "tmdb";
pub const IMDB: &str        = "imdb";
pub const TVDB: &str        = "tvdb";
pub const MUSICBRAINZ: &str = "musicbrainz";
pub const DISCOGS: &str     = "discogs";
pub const ANILIST: &str     = "anilist";
pub const KITSU: &str       = "kitsu";
pub const MYANIMELIST: &str = "myanimelist";

/// Whether a given string is a canonical id-source.
pub fn is_canonical(source: &str) -> bool {
    matches!(
        source,
        TMDB | IMDB | TVDB | MUSICBRAINZ | DISCOGS | ANILIST | KITSU | MYANIMELIST
    )
}

/// All canonical id-sources as a slice (useful for iteration, tests).
pub const ALL: &[&str] = &[
    TMDB, IMDB, TVDB, MUSICBRAINZ, DISCOGS, ANILIST, KITSU, MYANIMELIST,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_values_stable() {
        assert_eq!(TMDB, "tmdb");
        assert_eq!(MUSICBRAINZ, "musicbrainz");
    }

    #[test]
    fn is_canonical_rejects_unknown() {
        assert!(is_canonical("tmdb"));
        assert!(!is_canonical("unknown"));
        assert!(!is_canonical(""));
    }

    #[test]
    fn all_contains_every_constant() {
        assert_eq!(ALL.len(), 8);
        for s in ALL { assert!(is_canonical(s)); }
    }
}
