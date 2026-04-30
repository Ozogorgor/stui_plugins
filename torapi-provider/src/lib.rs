//! torapi-provider — Russian tracker coverage via Lifailon/TorAPI.
//!
//! TorAPI (https://github.com/Lifailon/TorAPI) is an open-source Go
//! service that wraps four major Russian trackers behind a single
//! JSON API: RuTracker, Kinozal, RuTor and NoNameClub. The public
//! instance at `torapi.vercel.app` is free and unauthenticated; for
//! production use self-host with `docker run lifailon/torapi`.
//!
//! ## API
//!
//!   GET {BASE}/api/search/title/all?query={q}&limit={n}
//!
//! Response:
//!   { "RuTracker": [...], "Kinozal": [...], "RuTor": [...], "NoNameClub": [...] }
//!
//! Each provider's array contains items with `Name`, `Torrent` (.torrent
//! download URL), `Size` (human-readable), `Seeds`/`Peers` (string ints).
//! Only RuTor includes a `Hash` field inline — for the other providers
//! we'd need a per-result `/api/search/id/...` round-trip to extract
//! the magnet hash, which would balloon the latency past usefulness.
//! Instead we hand the bare `.torrent` URL to the runtime; aria2 fetches
//! it directly. RuTracker's `dl.php` is forum-login-walled, so those
//! results are dropped (logged) rather than fed to a guaranteed failure.

use serde::Deserialize;
use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, StreamProvider,
    FindStreamsRequest, FindStreamsResponse, Stream,
};

// ── Plugin struct ─────────────────────────────────────────────────────────────

pub struct TorApiProvider {
    manifest: PluginManifest,
}

impl Default for TorApiProvider {
    fn default() -> Self {
        Self {
            manifest: parse_manifest(include_str!("../plugin.toml"))
                .expect("plugin.toml failed to parse at compile time"),
        }
    }
}

impl Plugin for TorApiProvider {
    fn manifest(&self) -> &PluginManifest { &self.manifest }
}

impl CatalogPlugin for TorApiProvider {
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::err(
            "UNSUPPORTED",
            "torapi is wired for streams only — use anilist/jackett for catalog browse",
        )
    }
}

// ── StreamProvider ────────────────────────────────────────────────────────────

impl StreamProvider for TorApiProvider {
    fn find_streams(&self, req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        let query = build_query(&req);
        if query.is_empty() {
            plugin_info!("torapi: skipped — empty title");
            return PluginResult::ok(FindStreamsResponse { streams: vec![] });
        }

        let url = format!(
            "{}/api/search/title/all?query={}&limit={}",
            cfg.base_url.trim_end_matches('/'),
            url_encode(&query),
            cfg.limit,
        );

        plugin_info!("torapi: find_streams query — {}", query);

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };
        let resp: TorApiResponse = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("PARSE_ERROR", &e.to_string()),
        };

        // Fan-out the per-provider arrays into one flat Vec<Stream>,
        // tagging each result with the originating tracker so the
        // user can tell at a glance where a release came from.
        let mut streams: Vec<Stream> = Vec::new();
        // RuTracker results carry forum dl.php URLs that require a
        // logged-in session cookie — handing them to aria2 always
        // 401s. Filter them out at the source so the UI doesn't
        // surface streams that can never play. Once the SDK gains
        // header/cookie-aware HTTP, we can lift this restriction.
        let _ = resp.rutracker; // intentionally dropped (auth-walled)
        for item in resp.rutor      { if let Some(s) = item.into_stream("RuTor")      { streams.push(s); } }
        for item in resp.kinozal    { if let Some(s) = item.into_stream("Kinozal")    { streams.push(s); } }
        for item in resp.nonameclub { if let Some(s) = item.into_stream("NoNameClub") { streams.push(s); } }

        plugin_info!("torapi: find_streams returned {} candidates", streams.len());
        PluginResult::ok(FindStreamsResponse { streams })
    }
}

// ── Query construction ────────────────────────────────────────────────────────
//
// Russian trackers tag releases bilingually: a Matrix release usually
// has the original "The Matrix" alongside the Russian "Матрица". We
// search by the original title (which arrives via TMDB's
// `original_name` upstream — `req.title` carries it). For series we
// don't append S/E because Russian trackers use Cyrillic season/series
// markers ("1 сезон 5 серия") that wouldn't match a Latin "S01E05"
// query string.
fn build_query(req: &FindStreamsRequest) -> String {
    let title = req.title.trim();
    if title.is_empty() { return String::new(); }
    match req.year {
        Some(y) => format!("{} {}", title, y),
        None    => title.to_string(),
    }
}

// ── TorAPI JSON shape ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TorApiResponse {
    #[serde(rename = "RuTracker")]
    rutracker:  Vec<TorApiItem>,
    #[serde(rename = "Kinozal")]
    kinozal:    Vec<TorApiItem>,
    #[serde(rename = "RuTor")]
    rutor:      Vec<TorApiItem>,
    #[serde(rename = "NoNameClub")]
    nonameclub: Vec<TorApiItem>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, rename_all = "PascalCase")]
struct TorApiItem {
    name:    String,
    /// Only RuTor exposes the SHA-1 hash inline; the others omit it
    /// and would require a `/api/search/id/{provider}` round-trip.
    hash:    Option<String>,
    /// `.torrent` download URL. Always present, but RuTracker's
    /// version is auth-walled (filtered out at the dispatch site).
    torrent: Option<String>,
    /// Human-readable size string ("42.85 GB", "650 MB").
    size:    Option<String>,
    seeds:   Option<String>,
    peers:   Option<String>,
}

impl TorApiItem {
    fn into_stream(self, provider_label: &str) -> Option<Stream> {
        let url = if let Some(hash) = self.hash.as_deref().filter(|h| !h.is_empty()) {
            // Synthesise a magnet from the hash + standard tracker
            // list. Cheaper for the runtime than fetching a .torrent
            // file just to extract the same hash.
            let dn = url_encode(&self.name);
            synth_magnet(hash, &dn)
        } else if let Some(t) = self.torrent.as_deref().filter(|s| !s.is_empty()) {
            // Hand the .torrent URL straight to the runtime; aria2
            // resolves it. Works for Kinozal and NoNameClub on
            // unauthenticated endpoints.
            t.to_string()
        } else {
            return None;
        };

        let seeders   = self.seeds.as_deref().and_then(|s| s.parse::<i32>().ok()).map(|s| s.max(0) as u32);
        let size_bytes = self.size.as_deref().and_then(parse_human_size);

        Some(Stream {
            url,
            title:   self.name.clone(),
            provider: provider_label.to_string(),
            quality:  extract_quality(&self.name),
            codec:    extract_codec(&self.name),
            source:   extract_source(&self.name),
            hdr:      extract_hdr(&self.name),
            seeders,
            size_bytes,
            language: None,
            subtitles: vec![],
        })
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    base_url: String,
    limit:    String,
}

impl Config {
    fn load() -> Result<Self, String> {
        Ok(Config {
            base_url: env_or("TORAPI_BASE_URL", "https://torapi.vercel.app"),
            limit:    env_or("TORAPI_LIMIT",    "20"),
        })
    }
}

fn env_or(var: &str, default: &str) -> String {
    let key = format!("__env:{}", var);
    cache_get(&key).unwrap_or_else(|| default.to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn synth_magnet(info_hash: &str, dn_encoded: &str) -> String {
    const TRACKERS: &[&str] = &[
        "udp://tracker.opentrackr.org:1337/announce",
        "udp://open.demonii.com:1337/announce",
        "udp://open.tracker.cl:1337/announce",
        "udp://exodus.desync.com:6969/announce",
        "udp://tracker.torrent.eu.org:451/announce",
    ];
    let tr = TRACKERS
        .iter()
        .map(|t| format!("tr={}", url_encode(t)))
        .collect::<Vec<_>>()
        .join("&");
    format!("magnet:?xt=urn:btih:{}&dn={}&{}", info_hash, dn_encoded, tr)
}

/// Parse "42.85 GB", "650 MB", "1.4 TB" → bytes.
fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let mut split = s.split_whitespace();
    let num: f64 = split.next()?.parse().ok()?;
    let unit = split.next()?.to_uppercase();
    let mult: u64 = match unit.as_str() {
        "TB" | "TIB" => 1_u64 << 40,
        "GB" | "GIB" => 1_u64 << 30,
        "MB" | "MIB" => 1_u64 << 20,
        "KB" | "KIB" => 1_u64 << 10,
        "B"          => 1,
        _            => return None,
    };
    Some((num * mult as f64) as u64)
}

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

stui_export_catalog_plugin!(TorApiProvider);
