//! RateYourMusic metadata provider.
//!
//! RYM has no public API, so this plugin scrapes album pages for the
//! community rating. Two verbs are exposed:
//!
//! - `search`: query the search page and surface up to N results
//!   with title + (parsed) artist. Used by the catalog when the user
//!   types a music search.
//! - `enrich`: given a partial entry with title + artist_name, find
//!   the best-matching album page and pull the average rating off
//!   it. RYM's site uses a 0–5 scale; this plugin emits 0–10 by
//!   multiplying ×2 to match the convention other music sources
//!   (kitsu/anilist) use, so the catalog aggregator's per-source
//!   weights apply with `normalize: 1.0`.
//!
//! ## Rate Limiting
//!
//! RYM blocks aggressive scrapers behind Cloudflare. The plugin
//! manifest caps requests at 1 rps with a small burst; the runtime
//! supervisor enforces this. The enrich path makes 2 requests per
//! album (search + detail) so a 50-entry music grid takes ~100 s
//! cold. Subsequent boots reuse the runtime's sqlite HTTP cache.

use regex::Regex;

use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, CatalogPlugin, EnrichRequest, EnrichResponse, EntryKind,
    Plugin, PluginManifest, SearchRequest, SearchResponse, SearchScope,
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
            SearchScope::Album => "l", // RYM searchtype: l = release
            SearchScope::Artist => "a",
            _ => {
                return PluginResult::err(
                    "unsupported_scope",
                    "RYM only supports album/artist scopes",
                );
            }
        };

        let url = format!(
            "{}/search?searchtype={}&searchterm={}",
            API_BASE,
            search_scope,
            urlencode(&req.query),
        );

        let body = match http_get(&url) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("remote_error", &e),
        };

        let entry_kind = match req.scope {
            SearchScope::Artist => EntryKind::Artist,
            _ => EntryKind::Album,
        };
        let items = parse_search_results(&body, entry_kind);
        let total = items.len() as u32;
        PluginResult::Ok(SearchResponse { items, total })
    }

    fn enrich(&self, req: EnrichRequest) -> PluginResult<EnrichResponse> {
        let title = req.partial.title.trim();
        if title.is_empty() {
            return PluginResult::err("invalid_request", "rym enrich: empty title");
        }
        let artist = req
            .partial
            .artist_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // Compose "title artist" — RYM's relevance ranking handles the
        // join, and including the artist disambiguates same-titled
        // albums (e.g. there are dozens of "Demon Days").
        let query = match artist {
            Some(a) => format!("{title} {a}"),
            None => title.to_string(),
        };
        let search_url = format!(
            "{}/search?searchtype=l&searchterm={}",
            API_BASE,
            urlencode(&query),
        );

        let search_body = match http_get(&search_url) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("remote_error", &e),
        };

        // Pick the first /release/album/{artist-slug}/{album-slug}/
        // link. The search page lists results in relevance order, so
        // the top hit is usually the right album.
        let detail_path = match find_first_release_path(&search_body) {
            Some(p) => p,
            None => {
                return PluginResult::err(
                    "unknown_id",
                    &format!("rym: no release matched '{}'", query),
                );
            }
        };

        let detail_url = format!("{}{}", API_BASE, detail_path);
        let detail_body = match http_get(&detail_url) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("remote_error", &e),
        };

        // Parse the average rating off the album detail page. Try
        // multiple shapes — RYM's markup has changed historically and
        // may serve different variants depending on auth/region.
        let raw = match parse_album_rating(&detail_body) {
            Some(r) => r,
            None => {
                return PluginResult::err(
                    "parse_error",
                    "rym: rating element not found on album page (markup changed?)",
                );
            }
        };
        // RYM publishes ratings on 0–5; emit 0–10 to match the
        // pre-normalised convention used by other music plugins.
        let rating_0_10 = (raw * 2.0).clamp(0.0, 10.0);

        let (artist_slug, album_slug) = parse_release_slugs(&detail_path);
        let display_title = album_slug
            .as_deref()
            .map(slug_to_title)
            .unwrap_or_else(|| title.to_string());
        let display_artist = artist
            .map(|a| a.to_string())
            .or_else(|| artist_slug.as_deref().map(slug_to_title));

        let entry = PluginEntry {
            id: detail_path.trim_start_matches('/').to_string(),
            title: display_title,
            kind: EntryKind::Album,
            source: "rym".to_string(),
            artist_name: display_artist,
            rating: Some(rating_0_10),
            ..Default::default()
        };
        // High confidence when we have title + artist; lower when we
        // matched on title alone (could be the wrong album).
        let confidence = if artist.is_some() { 0.85 } else { 0.55 };
        PluginResult::Ok(EnrichResponse { entry, confidence })
    }
}

// ── Scraping helpers ──────────────────────────────────────────────────────────

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push('+'),
            _ => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    out
}

fn parse_search_results(html: &str, kind: EntryKind) -> Vec<PluginEntry> {
    // Match search-result links of the form
    //   <a href="/release/album/<artist-slug>/<album-slug>/" ...>Title</a>
    // (artist links use /artist/<artist-slug>/ instead).
    // We're flexible on the inner HTML between the tag end and the
    // visible title text — RYM wraps the title in either <span> or
    // bare text depending on context.
    let pattern = match kind {
        EntryKind::Artist => {
            r#"href="(/artist/([^/"]+)/)"[^>]*>(?:<[^>]+>)?([^<]{2,200})<"#
        }
        _ => r#"href="(/release/album/([^/]+)/([^/]+)/)"[^>]*>(?:<[^>]+>)?([^<]{2,200})<"#,
    };
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut entries = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cap in re.captures_iter(html).take(50) {
        let path = match cap.get(1) {
            Some(m) => m.as_str().to_string(),
            None => continue,
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        let title = match cap.iter().last().and_then(|m| m).map(|m| m.as_str().trim()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => continue,
        };
        let artist_name = if matches!(kind, EntryKind::Album) {
            cap.get(2).map(|m| slug_to_title(m.as_str()))
        } else {
            None
        };
        entries.push(PluginEntry {
            id: path.trim_start_matches('/').to_string(),
            title,
            kind,
            source: "rym".to_string(),
            artist_name,
            ..Default::default()
        });
        if entries.len() >= 25 {
            break;
        }
    }
    entries
}

/// Find the first `/release/album/<artist>/<album>/` link in a
/// search result page. Used by `enrich` to jump from query → detail
/// page in one HTTP round-trip's worth of parsing.
fn find_first_release_path(html: &str) -> Option<String> {
    let re = Regex::new(r#"href="(/release/album/[^/]+/[^/]+/)""#).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Pull the (artist_slug, album_slug) pair out of a release path.
fn parse_release_slugs(path: &str) -> (Option<String>, Option<String>) {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    // Expected: ["release", "album", artist_slug, album_slug]
    if segments.len() >= 4 && segments[0] == "release" {
        return (Some(segments[2].to_string()), Some(segments[3].to_string()));
    }
    (None, None)
}

/// "the-dark-side-of-the-moon" → "The Dark Side Of The Moon".
fn slug_to_title(slug: &str) -> String {
    slug.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse the average rating (0.0–5.0) off an RYM album detail page.
/// RYM has shipped a few different markup shapes over the years; we
/// try them in order of currency. The first regex hits the modern
/// markup with `class="avg_rating"`; the fallbacks cover the
/// `<meta itemprop="ratingValue">` schema-org tag and a CSS-class
/// variant. Returns None if none match — caller treats that as a
/// rate-limit / Cloudflare-wall situation.
fn parse_album_rating(html: &str) -> Option<f32> {
    const PATTERNS: &[&str] = &[
        r#"class="avg_rating"[^>]*>\s*([0-9]+(?:\.[0-9]+)?)\s*<"#,
        r#"itemprop="ratingValue"[^>]*content="([0-9]+(?:\.[0-9]+)?)""#,
        r#"<meta\s+itemprop="ratingValue"[^>]*content="([0-9]+(?:\.[0-9]+)?)""#,
        r#"data-rating-value="([0-9]+(?:\.[0-9]+)?)""#,
    ];
    for p in PATTERNS {
        if let Ok(re) = Regex::new(p) {
            if let Some(cap) = re.captures(html) {
                if let Some(m) = cap.get(1) {
                    if let Ok(n) = m.as_str().parse::<f32>() {
                        if n > 0.0 {
                            return Some(n);
                        }
                    }
                }
            }
        }
    }
    None
}

stui_export_catalog_plugin!(RymPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_to_title_handles_basic_cases() {
        assert_eq!(slug_to_title("the-dark-side-of-the-moon"), "The Dark Side Of The Moon");
        assert_eq!(slug_to_title("kid-a"), "Kid A");
        assert_eq!(slug_to_title(""), "");
    }

    #[test]
    fn parse_release_slugs_extracts_pair() {
        let (a, b) = parse_release_slugs("/release/album/pink-floyd/dark-side/");
        assert_eq!(a.as_deref(), Some("pink-floyd"));
        assert_eq!(b.as_deref(), Some("dark-side"));
    }

    #[test]
    fn parse_album_rating_modern_markup() {
        let html = r#"<div><span class="avg_rating">  4.21  </span></div>"#;
        assert_eq!(parse_album_rating(html), Some(4.21));
    }

    #[test]
    fn parse_album_rating_schema_org_meta() {
        let html = r#"<meta itemprop="ratingValue" content="3.85" />"#;
        assert_eq!(parse_album_rating(html), Some(3.85));
    }

    #[test]
    fn parse_album_rating_returns_none_when_absent() {
        let html = "<html><body>No rating here</body></html>";
        assert_eq!(parse_album_rating(html), None);
    }

    #[test]
    fn find_first_release_path_picks_first() {
        let html = r#"
            <a href="/artist/foo/">Foo</a>
            <a href="/release/album/foo/bar/">First</a>
            <a href="/release/album/baz/qux/">Second</a>
        "#;
        assert_eq!(find_first_release_path(html), Some("/release/album/foo/bar/".into()));
    }

    #[test]
    fn urlencode_handles_unicode_and_spaces() {
        assert_eq!(urlencode("hello world"), "hello+world");
        assert_eq!(urlencode("naïve"), "na%C3%AFve");
    }
}
