//! nyaa-provider — anime stream resolver via nyaa.si.
//!
//! Nyaa is the canonical public anime tracker; release groups
//! (SubsPlease, Erai-raws, Judas, etc.) post here first, so it
//! frequently has releases hours before AnimeTosho mirrors them.
//!
//! ## API
//!
//!   GET {BASE_URL}/?page=rss&q={query}&c={category}&f={filter}
//!
//! Response is RSS 2.0 with a Nyaa-specific namespace
//! (`xmlns:nyaa="https://nyaa.si/xmlns/nyaa"`) carrying seeders,
//! leechers, infohash, size, category, and trusted/remake flags as
//! per-item extension elements.

use serde::Deserialize;
use quick_xml::de::from_str as xml_from_str;
use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, StreamProvider,
    FindStreamsRequest, FindStreamsResponse, Stream,
};

// ── Plugin struct ─────────────────────────────────────────────────────────────

pub struct NyaaProvider {
    manifest: PluginManifest,
}

impl Default for NyaaProvider {
    fn default() -> Self {
        Self {
            manifest: parse_manifest(include_str!("../plugin.toml"))
                .expect("plugin.toml failed to parse at compile time"),
        }
    }
}

impl Plugin for NyaaProvider {
    fn manifest(&self) -> &PluginManifest { &self.manifest }
}

impl CatalogPlugin for NyaaProvider {
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::err(
            "UNSUPPORTED",
            "nyaa is wired for streams only — use anilist/jackett for catalog browse",
        )
    }
}

// ── StreamProvider ────────────────────────────────────────────────────────────

impl StreamProvider for NyaaProvider {
    fn find_streams(&self, req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        let query = build_query(&req);
        if query.is_empty() {
            plugin_info!("nyaa: skipped — empty title");
            return PluginResult::ok(FindStreamsResponse { streams: vec![] });
        }

        let url = format!(
            "{}/?page=rss&q={}&c={}&f={}",
            cfg.base_url.trim_end_matches('/'),
            url_encode(&query),
            cfg.category,
            cfg.filter,
        );

        plugin_info!("nyaa: find_streams query — {}", query);

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };
        let rss: NyaaRss = match xml_from_str(&raw) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("PARSE_ERROR", &e.to_string()),
        };

        let provider_fallback = self.manifest.plugin.name.clone();
        let streams: Vec<Stream> = rss.channel.items
            .into_iter()
            .filter_map(|i| i.into_stream(&provider_fallback))
            .collect();

        plugin_info!("nyaa: find_streams returned {} candidates", streams.len());
        PluginResult::ok(FindStreamsResponse { streams })
    }
}

// ── Query construction ────────────────────────────────────────────────────────

/// Anime release titles use mixed numbering conventions:
///   "[SubsPlease] Show - 03 (1080p)"            ← episode-only
///   "[Erai-raws] Show S01E03 [1080p]"           ← S/E
///   "[Judas] Show Season 1 - Episode 3"         ← long form
/// Nyaa's search is permissive (substring + token), so feeding both
/// the title and the bare episode number yields broad coverage. We
/// avoid forcing the "S01E01" form because it would miss the
/// majority of releases that omit the season prefix.
fn build_query(req: &FindStreamsRequest) -> String {
    let title = req.title.trim();
    if title.is_empty() { return String::new(); }
    match req.episode {
        Some(e) => format!("{} {:02}", title, e),
        None    => title.to_string(),
    }
}

// ── Nyaa RSS shape ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct NyaaRss {
    channel: NyaaChannel,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct NyaaChannel {
    #[serde(rename = "item")]
    items: Vec<NyaaItem>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct NyaaItem {
    title: String,
    /// .torrent download URL (https://nyaa.si/download/<id>.torrent)
    link: Option<String>,
    /// Nyaa namespace extension fields. quick-xml strips the
    /// namespace prefix from element names by default, so we match on
    /// the bare local name (`seeders` / `infoHash` etc.) — using
    /// `nyaa:seeders` here would silently match nothing.
    #[serde(rename = "seeders")]
    seeders: Option<String>,
    #[serde(rename = "leechers")]
    leechers: Option<String>,
    #[serde(rename = "infoHash")]
    info_hash: Option<String>,
    #[serde(rename = "size")]
    size_str: Option<String>,
    #[serde(rename = "trusted")]
    trusted: Option<String>,
}

impl NyaaItem {
    fn into_stream(self, fallback_provider: &str) -> Option<Stream> {
        let url = if let Some(hash) = self.info_hash.as_deref().filter(|s| !s.is_empty()) {
            // Nyaa always provides an info-hash; synthesise a magnet
            // with the standard public tracker list. The `link`
            // (.torrent file) is a fallback only — magnets give the
            // client more control over peer discovery.
            let dn = url_encode(&self.title);
            synth_magnet(hash, &dn)
        } else if let Some(l) = self.link.as_deref().filter(|s| !s.is_empty()) {
            l.to_string()
        } else {
            return None;
        };

        let release_group = parse_release_group(&self.title);
        // "Trusted ✓" item — Nyaa's curator-approved flag. We don't
        // surface it specifically yet; could be wired into the
        // ranker's score later.
        let _trusted = self.trusted.as_deref() == Some("Yes");

        let seeders = self.seeders
            .as_deref()
            .and_then(|s| s.parse::<i32>().ok())
            .map(|s| s.max(0) as u32);
        let size_bytes = self.size_str.as_deref().and_then(parse_human_size);

        Some(Stream {
            url,
            title:    self.title.clone(),
            // Release-group tag is the natural per-stream label here
            // ("SubsPlease", "Erai-raws", "Judas"). Falls back to
            // "nyaa" only when the title is missing the leading
            // bracket (very rare).
            provider: release_group.unwrap_or_else(|| fallback_provider.to_string()),
            quality:  extract_quality(&self.title),
            codec:    extract_codec(&self.title),
            source:   extract_source(&self.title),
            hdr:      extract_hdr(&self.title),
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
    category: String,
    filter:   String,
}

impl Config {
    fn load() -> Result<Self, String> {
        Ok(Config {
            base_url: env_or("NYAA_BASE_URL", "https://nyaa.si"),
            category: env_or("NYAA_CATEGORY", "1_0"),
            filter:   env_or("NYAA_FILTER",   "0"),
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

/// Extract the leading bracketed release group tag.
fn parse_release_group(title: &str) -> Option<String> {
    let t = title.trim_start();
    if !t.starts_with('[') { return None; }
    let close = t.find(']')?;
    let inner = t[1..close].trim();
    if inner.is_empty() { None } else { Some(inner.to_string()) }
}

/// Parse a human-readable size string ("2.2 GiB", "650 MiB") to bytes.
/// Nyaa's RSS uses GiB/MiB/KiB (binary) — be careful not to confuse
/// with GB/MB/KB (decimal) elsewhere.
fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let mut split = s.split_whitespace();
    let num: f64 = split.next()?.parse().ok()?;
    let unit = split.next()?.to_uppercase();
    let mult: u64 = match unit.as_str() {
        "GIB" | "GB" => 1_u64 << 30,
        "MIB" | "MB" => 1_u64 << 20,
        "KIB" | "KB" => 1_u64 << 10,
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

stui_export_catalog_plugin!(NyaaProvider);
