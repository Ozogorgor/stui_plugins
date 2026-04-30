//! animetosho-provider — anime stream resolver via animetosho.org.
//!
//! AnimeTosho is a longstanding anime tracker aggregator that mirrors
//! Nyaa, AniDex and NekoBT and exposes them through a clean JSON feed.
//! Magnet URIs are precomputed server-side, so we don't need to
//! synthesise them from infohashes — every result already has a
//! ready-to-stream `magnet_uri`.
//!
//! ## API
//!
//!   GET {BASE_URL}/feed/json?q={query}&qx=1
//!
//! `qx=1` enables AnimeTosho's "exclude crap" server filter, which
//! drops fakes / mislabelled / low-quality entries. Cheap quality win.
//!
//! Response: `Vec<TorrentEntry>` — array of objects with `title`,
//! `magnet_uri`, `torrent_url`, `info_hash`, `total_size`,
//! `nyaa_id`/`anidex_id`/`nekobt_id` cross-tracker references, and a
//! handful of metadata fields.

use serde::Deserialize;
use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, StreamProvider,
    FindStreamsRequest, FindStreamsResponse, Stream,
};

// ── Plugin struct ─────────────────────────────────────────────────────────────

pub struct AnimeToshoProvider {
    manifest: PluginManifest,
}

impl Default for AnimeToshoProvider {
    fn default() -> Self {
        Self {
            manifest: parse_manifest(include_str!("../plugin.toml"))
                .expect("plugin.toml failed to parse at compile time"),
        }
    }
}

impl Plugin for AnimeToshoProvider {
    fn manifest(&self) -> &PluginManifest { &self.manifest }
}

impl CatalogPlugin for AnimeToshoProvider {
    // AnimeTosho's search endpoint is text-based (no IMDB id) and would
    // make a fine catalog browse path, but the runtime has Jackett and
    // anilist for that — we focus on streams only.
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::err(
            "UNSUPPORTED",
            "animetosho is wired for streams only — use anilist/jackett for catalog browse",
        )
    }
}

// ── StreamProvider ────────────────────────────────────────────────────────────

impl StreamProvider for AnimeToshoProvider {
    fn find_streams(&self, req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        let query = build_query(&req);
        if query.is_empty() {
            plugin_info!("animetosho: skipped — empty title");
            return PluginResult::ok(FindStreamsResponse { streams: vec![] });
        }

        let url = format!(
            "{}/feed/json?q={}&qx={}",
            cfg.base_url.trim_end_matches('/'),
            url_encode(&query),
            cfg.qx,
        );

        plugin_info!("animetosho: find_streams query — {}", query);

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };
        let entries: Vec<ToshoEntry> = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("PARSE_ERROR", &e.to_string()),
        };

        let provider_fallback = self.manifest.plugin.name.clone();
        let streams: Vec<Stream> = entries
            .into_iter()
            .filter_map(|e| e.into_stream(&provider_fallback))
            .collect();

        plugin_info!("animetosho: find_streams returned {} candidates", streams.len());
        PluginResult::ok(FindStreamsResponse { streams })
    }
}

// ── Query construction ────────────────────────────────────────────────────────
//
// Anime release groups don't use S01E01 notation universally. Most
// modern groups (SubsPlease, Erai-raws) tag releases with just the
// episode number ("Show - 03"), while older / re-encoded BD batches
// may use "Show S01E03". Including just the title + episode number
// covers both styles when the search engine is permissive (which
// AnimeTosho is — it does substring + token matching).
fn build_query(req: &FindStreamsRequest) -> String {
    let title = req.title.trim();
    if title.is_empty() { return String::new(); }
    match (req.season, req.episode) {
        (Some(_), Some(e)) => format!("{} {:02}", title, e),
        _ => title.to_string(),
    }
}

// ── AnimeTosho JSON shape ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ToshoEntry {
    title: String,
    /// Torrent magnet URI (precomputed server-side).
    magnet_uri: Option<String>,
    /// .torrent download URL (fallback path).
    torrent_url: Option<String>,
    /// SHA1 info hash, hex.
    info_hash: Option<String>,
    /// Bytes.
    total_size: Option<u64>,
    /// AnimeTosho's quality flag — `1` = OK, `2` = needs re-encoding,
    /// `4` = dead torrent, `8` = manually disabled. Only `1` is safe
    /// to surface; we filter the rest at parse time.
    status: Option<u32>,
    /// Cross-tracker IDs — useful for picking a meaningful provider label.
    #[serde(default)]
    nyaa_id: Option<u64>,
    #[serde(default)]
    anidex_id: Option<u64>,
    #[serde(default)]
    nekobt_id: Option<u64>,
    #[serde(default)]
    seeders: Option<i32>,
}

impl ToshoEntry {
    fn into_stream(self, fallback_provider: &str) -> Option<Stream> {
        // Filter out dead/disabled rows — AnimeTosho marks these but
        // still returns them in the feed.
        if matches!(self.status, Some(4) | Some(8)) {
            return None;
        }

        let url = self.magnet_uri
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let hash = self.info_hash.as_deref().filter(|s| !s.is_empty())?;
                let dn = url_encode(&self.title);
                Some(synth_magnet(hash, &dn))
            })
            .or_else(|| self.torrent_url.as_deref().filter(|s| !s.is_empty()).map(str::to_string))?;

        let release_group = parse_release_group(&self.title);
        // Provider label: prefer the release-group tag in the title
        // (e.g. "Erai-raws", "SubsPlease") — that's what the user
        // recognises. Fall back to the cross-tracker source name when
        // the title has no leading group bracket. Only when none of
        // those land do we use the plugin name.
        let provider_label = release_group
            .or_else(|| {
                if self.nyaa_id.is_some()    { Some("Nyaa".to_string()) }
                else if self.anidex_id.is_some() { Some("AniDex".to_string()) }
                else if self.nekobt_id.is_some() { Some("NekoBT".to_string()) }
                else { None }
            })
            .unwrap_or_else(|| fallback_provider.to_string());

        let seeders = self.seeders.map(|s| s.max(0) as u32);

        Some(Stream {
            url,
            title:    self.title.clone(),
            provider: provider_label,
            quality:  extract_quality(&self.title),
            codec:    extract_codec(&self.title),
            source:   extract_source(&self.title),
            hdr:      extract_hdr(&self.title),
            seeders,
            size_bytes: self.total_size.filter(|&n| n > 0),
            language: None,
            subtitles: vec![],
        })
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    base_url: String,
    qx:       String,
}

impl Config {
    fn load() -> Result<Self, String> {
        Ok(Config {
            base_url: env_or("ANIMETOSHO_BASE_URL", "https://animetosho.org"),
            qx:       env_or("ANIMETOSHO_QX", "1"),
        })
    }
}

fn env_or(var: &str, default: &str) -> String {
    let key = format!("__env:{}", var);
    cache_get(&key).unwrap_or_else(|| default.to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Synthesize a magnet URI from a 40-char hex info-hash plus the standard
/// public tracker list. Same trackers used by Stremio/Torrentio so
/// magnet warm-up time is comparable across providers.
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

/// Extract the leading bracketed release group tag — e.g. "Erai-raws"
/// from "[Erai-raws] Show - 03 [1080p].mkv". Anime release titles use
/// this convention almost universally.
fn parse_release_group(title: &str) -> Option<String> {
    let t = title.trim_start();
    if !t.starts_with('[') { return None; }
    let close = t.find(']')?;
    let inner = t[1..close].trim();
    if inner.is_empty() { None } else { Some(inner.to_string()) }
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

stui_export_catalog_plugin!(AnimeToshoProvider);
