//! RateYourMusic metadata provider.
//!
//! Scrapes album ratings and search results from rateyourmusic.com.
//!
//! ## Rate Limiting
//!
//! This plugin is polite: max 1 request/second per rate limit config.
//! RYM is known to block aggressive scrapers.

use regex::Regex;

use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, EntryKind, SearchScope,
    SearchRequest, SearchResponse,
};

const API_BASE: &str = "https://rateyourmusic.com";

// ── Plugin ────────────────────────────────────────────────────────────────────

pub struct RymPlugin {
    manifest: PluginManifest,
}

impl Default for RymPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl RymPlugin {
    pub fn new() -> Self {
        let manifest: PluginManifest = parse_manifest(include_str!("../plugin.toml"))
            .expect("plugin.toml failed to parse at compile time");
        Self { manifest }
    }
}

impl Plugin for RymPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
}

impl CatalogPlugin for RymPlugin {
    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let search_scope = match req.scope {
            SearchScope::Album => "album",
            SearchScope::Artist => "artist",
            _ => {
                return PluginResult::err(
                    "unsupported_scope",
                    "RYM only supports album/artist scopes",
                );
            }
        };

        let url = format!(
            "{}/{}-search?searchterm={}",
            API_BASE,
            search_scope,
            urlencoding_encode(&req.query),
        );

        let body = match http_get(&url) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("remote_error", &e),
        };

        let items = parse_search_results(&body, search_scope);
        let total = items.len() as u32;

        PluginResult::Ok(SearchResponse {
            items,
            total,
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn urlencoding_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 3);
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => encoded.push(c),
            ' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{:02X}", c as u8)),
        }
    }
    encoded
}

fn parse_search_results(html: &str, search_type: &str) -> Vec<PluginEntry> {
    let mut entries = Vec::new();

    // Find links matching /release/album-name/ or /artist/name/
    // Example: <a href="/release/album/pink-floyd/the-dark-side-of-the-moon/">
    let re = Regex::new(r#"href="/[^/]+/([^/]+)/"[^>]*><span[^>]*>([^<]+)<"#).ok();
    let re = match re {
        Some(r) => r,
        None => return entries,
    };

    for cap in re.captures_iter(html).take(25) {
        let slug = match cap.get(1) {
            Some(m) => m.as_str(),
            None => continue,
        };
        let title = match cap.get(2) {
            Some(m) => m.as_str(),
            None => continue,
        };

        let id = format!("{}/{}", search_type, slug);

        entries.push(PluginEntry {
            id,
            title: title.to_string(),
            kind: match search_type {
                "artist" => EntryKind::Artist,
                _ => EntryKind::Album,
            },
            source: "rym".to_string(),
            ..Default::default()
        });
    }

    entries
}

stui_export_catalog_plugin!(RymPlugin);