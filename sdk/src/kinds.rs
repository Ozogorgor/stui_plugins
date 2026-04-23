//! Typed kinds for search scoping and entry classification.
//!
//! `EntryKind` describes what a returned entry *is* (wire contract on
//! PluginEntry). `SearchScope` describes what a caller is *asking for*
//! (request parameter). Identical members today; kept separate so future
//! runtime-only scope values don't leak into `EntryKind`.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Artist, Album, Track,
    Movie, Series, Episode,
}

impl Default for EntryKind {
    fn default() -> Self { EntryKind::Track }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    Artist, Album, Track,
    Movie, Series, Episode,
}

impl SearchScope {
    pub fn matches(self, kind: EntryKind) -> bool {
        matches!((self, kind),
            (SearchScope::Artist, EntryKind::Artist) |
            (SearchScope::Album,  EntryKind::Album)  |
            (SearchScope::Track,  EntryKind::Track)  |
            (SearchScope::Movie,  EntryKind::Movie)  |
            (SearchScope::Series, EntryKind::Series) |
            (SearchScope::Episode,EntryKind::Episode)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn entry_kind_snake_case() {
        assert_eq!(serde_json::to_string(&EntryKind::Artist).unwrap(), "\"artist\"");
    }

    #[test] fn search_scope_round_trips() {
        for s in [SearchScope::Artist, SearchScope::Track, SearchScope::Movie] {
            let j = serde_json::to_string(&s).unwrap();
            let back: SearchScope = serde_json::from_str(&j).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test] fn scope_matches_kind() {
        assert!(SearchScope::Track.matches(EntryKind::Track));
        assert!(!SearchScope::Track.matches(EntryKind::Artist));
    }
}
