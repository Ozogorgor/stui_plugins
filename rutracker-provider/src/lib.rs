//! rutracker-provider — direct RuTracker scraper with session-cookie auth.
//!
//! RuTracker is the largest Russian general tracker — deepest catalog
//! for movies, TV, music, audiobooks, software. Talking to it directly
//! (rather than through TorAPI's auth-walled `dl.php` proxy or
//! Jackett's slow Torznab fan-out) gives us magnet links in 4-6 s.
//!
//! ## Auth
//!
//! v1 uses a manually-supplied `bb_session` cookie value (config field).
//! Automated login is deferred until the SDK can surface response
//! `Set-Cookie` headers — at present `http_post` only exposes the body.
//! The user pastes the cookie value from a logged-in browser; the
//! plugin sends it verbatim as `Cookie: bb_session=<value>` on every
//! request via `http_get_with_headers`.
//!
//! ## Search → magnet extraction
//!
//! RuTracker's search page (`/forum/tracker.php?nm=…`) lists topic rows
//! with title / size / seeders but NO magnet. Magnets live on each
//! topic's own page (`/forum/viewtopic.php?t=…`). For the top-N rows
//! the plugin fetches the topic page sequentially and extracts the
//! magnet from the response HTML. `N` defaults to 8 — bumping it
//! linearly grows latency.

use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, StreamProvider,
    EntryKind,
    FindStreamsRequest, FindStreamsResponse, Stream,
};

// ── Plugin struct ─────────────────────────────────────────────────────────────

pub struct RuTrackerProvider {
    manifest: PluginManifest,
}

impl Default for RuTrackerProvider {
    fn default() -> Self {
        Self {
            manifest: parse_manifest(include_str!("../plugin.toml"))
                .expect("plugin.toml failed to parse at compile time"),
        }
    }
}

impl Plugin for RuTrackerProvider {
    fn manifest(&self) -> &PluginManifest { &self.manifest }
}

impl CatalogPlugin for RuTrackerProvider {
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::err(
            "UNSUPPORTED",
            "rutracker is wired for streams only — auth-required text search is heavy",
        )
    }
}

// ── StreamProvider ────────────────────────────────────────────────────────────

impl StreamProvider for RuTrackerProvider {
    fn find_streams(&self, req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        let query = build_query(&req);
        if query.is_empty() {
            plugin_info!("rutracker: skipped — empty title");
            return PluginResult::ok(FindStreamsResponse { streams: vec![] });
        }

        let cookie = format!("bb_session={}", cfg.bb_session);

        let search_url = format!(
            "{}/forum/tracker.php?nm={}",
            cfg.base_url.trim_end_matches('/'),
            url_encode(&query),
        );
        plugin_info!("rutracker: search — {}", query);

        let html = match http_get_with_headers(&search_url, &[("Cookie", cookie.as_str())]) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        // Login expired / invalid cookie → RuTracker redirects to login
        // page. Detect by looking for the login form marker; surface a
        // CONFIG_ERROR so the user knows to refresh the cookie.
        if html.contains("name=\"login_username\"") || html.contains("login.php") && !html.contains("class=\"hl-tr\"") {
            return PluginResult::err(
                "AUTH_ERROR",
                "rutracker session expired — re-paste the bb_session cookie from a logged-in browser",
            );
        }

        let rows = parse_search_html(&html);
        plugin_info!("rutracker: {} search rows (max {})", rows.len(), cfg.max_results);

        // Sequentially fetch topic pages for the top-N candidates and
        // extract their magnet links. Sequential because each `http_get`
        // is a synchronous host call — no parallelism within one
        // plugin invocation.
        let mut streams: Vec<Stream> = Vec::with_capacity(cfg.max_results);
        for row in rows.into_iter().take(cfg.max_results) {
            let topic_url = format!(
                "{}/forum/viewtopic.php?t={}",
                cfg.base_url.trim_end_matches('/'),
                row.topic_id,
            );
            let topic_html = match http_get_with_headers(&topic_url, &[("Cookie", cookie.as_str())]) {
                Ok(b) => b,
                Err(_) => continue, // one bad topic shouldn't sink the whole search
            };
            let magnet = match extract_magnet(&topic_html) {
                Some(m) => m,
                None => continue,
            };

            streams.push(Stream {
                url: magnet,
                title: row.title.clone(),
                provider: "RuTracker".to_string(),
                quality: extract_quality(&row.title),
                codec:   extract_codec(&row.title),
                source:  extract_source(&row.title),
                hdr:     extract_hdr(&row.title),
                seeders: row.seeders,
                size_bytes: row.size_bytes,
                language: None,
                subtitles: vec![],
            });
        }

        plugin_info!("rutracker: returning {} streams", streams.len());
        PluginResult::ok(FindStreamsResponse { streams })
    }
}

// ── Query construction ────────────────────────────────────────────────────────
//
// RuTracker's search input is free text — append year for movies to
// disambiguate. Episodes are tricky on Russian trackers (Cyrillic
// "сезон"/"серия" markers), so for series we just pass the bare title
// and let the result-side title parsing pick up SxxEyy or full-season
// releases.
fn build_query(req: &FindStreamsRequest) -> String {
    let title = req.title.trim();
    if title.is_empty() { return String::new(); }
    match (req.kind, req.year) {
        (EntryKind::Movie, Some(y)) => format!("{} {}", title, y),
        _                           => title.to_string(),
    }
}

// ── HTML scraping ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct SearchRow {
    topic_id:   String,
    title:      String,
    seeders:    Option<u32>,
    size_bytes: Option<u64>,
}

/// Parse RuTracker's search-results page.
///
/// Each row of interest is `<tr id="trs-tr-N" class="...hl-tr...">`.
/// We extract topic_id from the id attribute, the title from the
/// `<a class="med tLink ...">` inside, the size from a `data-ts_text`
/// numeric byte attribute, and the seeders from the `b.seedmed` /
/// `td[data-ts_text]` cells. RuTracker re-orders cells over time —
/// the parsing here is intentionally tolerant of layout drift.
fn parse_search_html(html: &str) -> Vec<SearchRow> {
    let mut rows = Vec::new();
    let mut cursor = 0;
    while let Some(idx) = html[cursor..].find("<tr id=\"trs-tr-") {
        let start = cursor + idx;
        // topic id sits between `trs-tr-` and the next `"`
        let id_start = start + r#"<tr id="trs-tr-"#.len();
        let id_end = match html[id_start..].find('"') {
            Some(p) => id_start + p,
            None => break,
        };
        let topic_id = html[id_start..id_end].to_string();

        // Find end of this <tr>...</tr> so we don't bleed into the next row
        let row_end = match html[id_end..].find("</tr>") {
            Some(p) => id_end + p,
            None => break,
        };
        let row = &html[id_end..row_end];

        // Title: <a ... class="..tLink..">TITLE</a>
        let title = extract_after(row, "tLink", '>')
            .and_then(|s| extract_until(s, "</a>"))
            .map(html_decode)
            .unwrap_or_default();

        // Seeders: cell with class "seedmed" wraps a <b>NUMBER</b>
        let seeders = extract_after(row, "seedmed", '>')
            .and_then(|s| extract_after(s, "<b>", '>'))
            .and_then(|s| extract_until(s, "</b>"))
            .and_then(|s| s.trim().parse::<u32>().ok());

        // Size in bytes: the row exposes a numeric column with
        // `data-ts_text="N"` for sortable size; capture it when found.
        let size_bytes = extract_after(row, "data-ts_text=\"", '"')
            .and_then(|s| extract_until(s, "\""))
            .and_then(|s| s.parse::<u64>().ok());

        if !title.is_empty() && !topic_id.is_empty() {
            rows.push(SearchRow {
                topic_id,
                title,
                seeders,
                size_bytes,
            });
        }

        cursor = row_end + "</tr>".len();
    }
    rows
}

/// Extract a magnet URI from a topic-page HTML response.
///
/// RuTracker injects `<a class="magnet-link" href="magnet:?xt=urn:btih:…">`
/// near the top of every topic. Returns the bare magnet string.
fn extract_magnet(html: &str) -> Option<String> {
    let needle = "href=\"magnet:";
    let start = html.find(needle)?;
    let after = &html[start + r#"href=""#.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Find `marker` in `s`, then advance past the next occurrence of `terminator`.
/// Used to skip past attribute matches before extracting content.
fn extract_after<'a>(s: &'a str, marker: &str, terminator: char) -> Option<&'a str> {
    let i = s.find(marker)?;
    let after = &s[i + marker.len()..];
    let j = after.find(terminator)?;
    Some(&after[j + terminator.len_utf8()..])
}

fn extract_until<'a>(s: &'a str, terminator: &str) -> Option<&'a str> {
    let i = s.find(terminator)?;
    Some(&s[..i])
}

/// Decode the small set of HTML entities RuTracker actually uses.
/// Anything more elaborate is rare enough we leave the raw text.
fn html_decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&nbsp;", " ")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .trim()
        .to_string()
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    base_url:    String,
    bb_session:  String,
    max_results: usize,
}

impl Config {
    fn load() -> Result<Self, String> {
        let base_url   = env_or("RUTRACKER_BASE_URL", "https://rutracker.org");
        let bb_session = env_or("RUTRACKER_BB_SESSION", "");
        let max_str    = env_or("RUTRACKER_MAX_RESULTS", "8");

        if bb_session.is_empty() {
            return Err(
                "RUTRACKER_BB_SESSION is empty — paste the bb_session cookie value \
                 from a logged-in browser into the plugin config".into()
            );
        }

        let max_results = max_str.parse::<usize>().unwrap_or(8).max(1).min(50);

        Ok(Config { base_url, bb_session, max_results })
    }
}

fn env_or(var: &str, default: &str) -> String {
    let key = format!("__env:{}", var);
    cache_get(&key).unwrap_or_else(|| default.to_string())
}

// ── Title metadata extractors (mirror jackett-provider's helpers) ────────────

fn extract_quality(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    for tag in &["2160P", "4K", "UHD", "1080P", "720P", "480P", "BDREMUX", "BLURAY", "WEB-DL"] {
        if t.contains(tag) {
            return Some(
                tag.to_lowercase()
                    .replace("bdremux", "BD Remux")
                    .replace("bluray", "Blu-ray")
                    .replace("web-dl", "WEB-DL"),
            );
        }
    }
    None
}

fn extract_codec(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    if t.contains("X265") || t.contains("H.265") || t.contains("HEVC") { return Some("h265".into()); }
    if t.contains("AV1") { return Some("av1".into()); }
    if t.contains("X264") || t.contains("H.264") || t.contains("AVC") { return Some("h264".into()); }
    None
}

fn extract_source(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    for (tag, label) in [
        ("BLURAY", "BluRay"), ("BDREMUX", "BDRemux"),
        ("WEB-DL", "WEB-DL"), ("WEBDL", "WEB-DL"), ("WEBRIP", "WEBRip"),
        ("HDTV", "HDTV"), ("DVDRIP", "DVDRip"),
        ("HDCAM", "CAM"), ("CAM", "CAM"), ("TS", "TS"),
    ] { if t.contains(tag) { return Some(label.into()); } }
    None
}

fn extract_hdr(title: &str) -> bool {
    let t = title.to_uppercase();
    t.contains("HDR10+") || t.contains("HDR10") || t.contains("HDR")
        || t.contains("DOLBY VISION") || t.contains("DV ") || t.contains(" DV.")
}

stui_export_catalog_plugin!(RuTrackerProvider);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_magnet_from_topic_html() {
        let html = r#"<div><a class="magnet-link" href="magnet:?xt=urn:btih:abc123&dn=Foo">link</a></div>"#;
        assert_eq!(
            extract_magnet(html).as_deref(),
            Some("magnet:?xt=urn:btih:abc123&dn=Foo"),
        );
    }

    #[test]
    fn parse_search_html_extracts_topic_id_title_seeders() {
        let html = r#"
            <tr id="trs-tr-12345" class="hl-tr">
                <td><a class="med tLink some-class">My Show S01E01 1080p</a></td>
                <td data-ts_text="1234567890">1.1 GB</td>
                <td class="seedmed"><b>42</b></td>
            </tr>
        "#;
        let rows = parse_search_html(html);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].topic_id, "12345");
        assert_eq!(rows[0].title, "My Show S01E01 1080p");
        assert_eq!(rows[0].seeders, Some(42));
        assert_eq!(rows[0].size_bytes, Some(1234567890));
    }
}
