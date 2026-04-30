//! torrentio-provider — stream provider via the Torrentio public API.
//!
//! Torrentio is a Stremio addon that aggregates a curated list of
//! public trackers (RARBG, 1337x, ThePirateBay, EZTV, YTS, NyaaSi, …)
//! behind a single fast JSON endpoint. The official instance typically
//! responds in 200-500 ms — orders of magnitude faster than running a
//! Jackett/Prowlarr Torznab fan-out across the same set, because
//! Torrentio caches per-IMDB-id results aggressively.
//!
//! ## API
//!
//!   GET {BASE_URL}/{CONFIG}/stream/{type}/{id}.json
//!
//! Where:
//!   - `{CONFIG}` is `providers=…` (pipe-separated) plus optional
//!     `realdebrid=TOKEN` / `alldebrid=TOKEN` etc.
//!   - `{type}` is `movie` or `series`
//!   - `{id}` is `tt0816692` for movies, `tt0944947:1:1` for episodes
//!
//! Response shape:
//!   { "streams": [ {
//!       "name":     "Torrentio\nRARBG",
//!       "title":    "The Matrix 1999 1080p BluRay x264-AMIABLE\n👤 1234 💾 9.5 GB ⚙️ RARBG",
//!       "infoHash": "abc…",
//!       "fileIdx":  0,
//!       "behaviorHints": { "bingeGroup": "…" }
//!   } ] }
//!
//! Most metadata (seeders, size, source, quality) is encoded in the
//! `title` field with emoji separators. The first line of the title is
//! the release name; subsequent lines hold structured stats. The
//! second line of `name` carries the originating tracker — we use that
//! as the user-visible `Stream.provider` label.

use serde::Deserialize;
use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, StreamProvider,
    EntryKind,
    FindStreamsRequest, FindStreamsResponse, Stream,
};

// ── Plugin struct ─────────────────────────────────────────────────────────────

pub struct TorrentioProvider {
    manifest: PluginManifest,
}

impl Default for TorrentioProvider {
    fn default() -> Self {
        Self {
            manifest: parse_manifest(include_str!("../plugin.toml"))
                .expect("plugin.toml failed to parse at compile time"),
        }
    }
}

impl Plugin for TorrentioProvider {
    fn manifest(&self) -> &PluginManifest { &self.manifest }
}

// Catalog browsing isn't a Torrentio feature — it's IMDB-id-keyed only.
// Surface UNSUPPORTED so the runtime won't dispatch a text search to us.
impl CatalogPlugin for TorrentioProvider {
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::err(
            "UNSUPPORTED",
            "torrentio is IMDB-keyed only — no text search",
        )
    }
}

// ── StreamProvider ────────────────────────────────────────────────────────────

impl StreamProvider for TorrentioProvider {
    fn find_streams(&self, req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        // Torrentio is keyed by IMDB id. The runtime resolves
        // title→IMDB upstream (TMDB lookup, anime cross-tier, etc.)
        // and populates `imdb_id` (or `external_ids["imdb"]`) on the
        // request. Without one, return cleanly empty rather than
        // making a doomed call.
        let imdb_id = if let Some(id) = req.imdb_id.as_deref().filter(|s| !s.is_empty()) {
            id.to_string()
        } else if let Some(id) = req.external_ids.get("imdb").filter(|s| !s.is_empty()) {
            id.clone()
        } else {
            plugin_info!("torrentio: skipped — no imdb_id (title={})", req.title);
            return PluginResult::ok(FindStreamsResponse { streams: vec![] });
        };

        // Torrentio addresses series at `tt…:season:episode`. Without
        // S/E we'd be querying a non-existent endpoint, so bail early
        // for series whose episode is unknown.
        let (stream_type, stream_id) = match (req.kind, req.season, req.episode) {
            (EntryKind::Movie, _, _) => ("movie", imdb_id.clone()),
            (EntryKind::Series, Some(s), Some(e)) | (EntryKind::Episode, Some(s), Some(e)) => {
                ("series", format!("{}:{}:{}", imdb_id, s, e))
            }
            (EntryKind::Series, _, _) | (EntryKind::Episode, _, _) => {
                plugin_info!("torrentio: series without season/episode — skipped");
                return PluginResult::ok(FindStreamsResponse { streams: vec![] });
            }
            _ => return PluginResult::ok(FindStreamsResponse { streams: vec![] }),
        };

        let base = cfg.base_url.trim_end_matches('/');
        let url = if cfg.config_segment.is_empty() {
            format!("{}/stream/{}/{}.json", base, stream_type, stream_id)
        } else {
            format!(
                "{}/{}/stream/{}/{}.json",
                base, cfg.config_segment, stream_type, stream_id,
            )
        };

        plugin_info!(
            "torrentio: find_streams query — {} ({})",
            req.title, stream_id,
        );

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };
        let envelope: TorrentioResponse = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("PARSE_ERROR", &e.to_string()),
        };

        let provider_fallback = self.manifest.plugin.name.clone();
        let streams: Vec<Stream> = envelope.streams
            .into_iter()
            .filter_map(|s| s.into_stream(&provider_fallback))
            .collect();

        plugin_info!("torrentio: find_streams returned {} candidates", streams.len());
        PluginResult::ok(FindStreamsResponse { streams })
    }
}

// ── Torrentio API types ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TorrentioResponse {
    streams: Vec<TorrentioStream>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TorrentioStream {
    /// Two-line label: "Torrentio\n<TrackerName>".
    name: String,
    /// Multi-line release info; the first line is the release name,
    /// subsequent lines hold stats encoded with emoji separators
    /// (👤 seeders, 💾 size, ⚙️ source/origin).
    title: String,
    /// 40-char hex SHA1. Always populated for torrent streams.
    #[serde(rename = "infoHash")]
    info_hash: Option<String>,
    /// Direct URL — populated only when a debrid service is configured
    /// (Real-Debrid etc. resolve the magnet into an HTTP CDN URL).
    url: Option<String>,
}

impl TorrentioStream {
    fn into_stream(self, fallback_provider: &str) -> Option<Stream> {
        // Prefer an explicit url (debrid path) over a synthesised magnet —
        // debrid URLs are direct HTTP and play far faster than waiting
        // for a torrent swarm to ramp up.
        let url = if let Some(u) = self.url.as_deref().filter(|u| !u.is_empty()) {
            u.to_string()
        } else if let Some(hash) = self.info_hash.as_deref().filter(|h| !h.is_empty()) {
            // Standard public trackers — same set Stremio's official
            // addon uses. Without these the magnet resolves much
            // slower (peer discovery via DHT only).
            const TRACKERS: &[&str] = &[
                "udp://tracker.opentrackr.org:1337/announce",
                "udp://open.demonii.com:1337/announce",
                "udp://open.tracker.cl:1337/announce",
                "udp://exodus.desync.com:6969/announce",
                "udp://tracker.torrent.eu.org:451/announce",
            ];
            let tracker_str = TRACKERS
                .iter()
                .map(|t| format!("tr={}", url_encode(t)))
                .collect::<Vec<_>>()
                .join("&");
            let dn = url_encode(&first_line(&self.title));
            format!("magnet:?xt=urn:btih:{}&dn={}&{}", hash, dn, tracker_str)
        } else {
            return None;
        };

        let release_name = first_line(&self.title);
        let seeders      = parse_seeders(&self.title);
        let size_bytes   = parse_size(&self.title);

        // The originating tracker is encoded as `⚙️ <Name>` somewhere
        // in the title (Torrentio formats every stream that way). The
        // second line of `name` is the *quality tier* tag ("4k",
        // "4k DV | HDR", …), not the indexer — easy to mistake.
        let provider_label = parse_indexer(&self.title)
            .unwrap_or_else(|| fallback_provider.to_string());

        Some(Stream {
            url,
            title:    release_name.clone(),
            provider: provider_label,
            quality:  extract_quality(&release_name),
            codec:    extract_codec(&release_name),
            source:   extract_source(&release_name),
            hdr:      extract_hdr(&release_name),
            seeders,
            size_bytes,
            language: None,
            subtitles: vec![],
        })
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    base_url:       String,
    config_segment: String,
}

impl Config {
    fn load() -> Result<Self, String> {
        let base_url  = env_or("TORRENTIO_BASE_URL", "https://torrentio.strem.fun");
        let providers = env_or("TORRENTIO_PROVIDERS", "");
        let debrid    = env_or("TORRENTIO_DEBRID", "");

        // Build the optional `/{config}/` URL segment. When neither a
        // provider override nor a debrid token is set, omit it entirely
        // — Torrentio then uses its own server-side defaults, which is
        // strictly better than passing an outdated provider list (e.g.
        // "rarbg" — that tracker shut down in 2023; including it filters
        // out every other source for backwards-compat reasons).
        let mut parts: Vec<String> = Vec::new();
        if !providers.is_empty() {
            parts.push(format!("providers={}", providers));
        }
        if !debrid.is_empty() {
            parts.push(debrid);
        }
        Ok(Config {
            base_url,
            config_segment: parts.join("|"),
        })
    }
}

fn env_or(var: &str, default: &str) -> String {
    let key = format!("__env:{}", var);
    cache_get(&key).unwrap_or_else(|| default.to_string())
}

// ── Title parsers ─────────────────────────────────────────────────────────────

fn first_line(s: &str) -> String {
    s.split('\n').next().unwrap_or(s).trim().to_string()
}

/// Extract the originating tracker name from a Torrentio title.
/// The title always contains `⚙️ <Name>` (e.g. "⚙️ ThePirateBay") —
/// the name runs until the next emoji separator or end of line.
fn parse_indexer(s: &str) -> Option<String> {
    let idx = s.find('⚙')?;
    let tail = &s[idx + '⚙'.len_utf8()..];
    // Skip the variation-selector / FE0F suffix and any whitespace.
    let trimmed: String = tail
        .chars()
        .skip_while(|c| c.is_whitespace() || !c.is_ascii_alphanumeric())
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(*c, '.' | '-' | '_'))
        .collect();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Pull seeder count from a Torrentio title. Looks for the 👤 marker
/// first (Torrentio's standard format), then falls back to "Seeds: N".
fn parse_seeders(s: &str) -> Option<u32> {
    if let Some(idx) = s.find('👤') {
        let tail = &s[idx + '👤'.len_utf8()..];
        let digits: String = tail.chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit() || *c == ',')
            .filter(|c| *c != ',')
            .collect();
        if let Ok(n) = digits.parse::<u32>() {
            return Some(n);
        }
    }
    let lower = s.to_lowercase();
    for marker in ["seeds:", "seed:", "seeders:", "seeder:"] {
        if let Some(idx) = lower.find(marker) {
            let tail = &s[idx + marker.len()..];
            let digits: String = tail.chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = digits.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Pull size in bytes from a Torrentio title. Looks for "💾 N.N GB"
/// first (Torrentio's standard format), then falls back to any
/// "N.N (GB|MB|KB)" elsewhere in the string.
fn parse_size(s: &str) -> Option<u64> {
    fn unit_multiplier(unit: &str) -> Option<u64> {
        match unit.to_uppercase().as_str() {
            "GB" | "GIB" => Some(1_u64 << 30),
            "MB" | "MIB" => Some(1_u64 << 20),
            "KB" | "KIB" => Some(1_u64 << 10),
            _ => None,
        }
    }

    // Try after the 💾 marker first
    if let Some(idx) = s.find('💾') {
        let tail = &s[idx + '💾'.len_utf8()..];
        if let Some(parsed) = parse_size_token(tail, unit_multiplier) {
            return Some(parsed);
        }
    }
    // Fallback: walk the string looking for any "N.N UNIT"
    parse_size_token(s, unit_multiplier)
}

fn parse_size_token(s: &str, unit_multiplier: fn(&str) -> Option<u64>) -> Option<u64> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') { i += 1; }
            let num: Option<f64> = chars[start..i].iter().collect::<String>().parse().ok();
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() { j += 1; }
            let unit: String = chars[j..].iter()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            if let (Some(num), Some(mult)) = (num, unit_multiplier(&unit)) {
                return Some((num * mult as f64) as u64);
            }
        }
        i += 1;
    }
    None
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

stui_export_catalog_plugin!(TorrentioProvider);
