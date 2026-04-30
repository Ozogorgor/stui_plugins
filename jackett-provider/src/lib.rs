//! jackett-provider — stui plugin that searches a local Jackett instance.
//!
//! ## How it works
//!
//! Jackett is an indexer proxy.  It exposes a unified REST API that queries
//! all your configured trackers via a single aggregate endpoint:
//!
//!   GET {JACKETT_URL}/api/v2.0/indexers/all/results
//!       ?apikey={JACKETT_API_KEY}
//!       &Query.SearchTerm={q}
//!       &Category[]={cat1}
//!       &Category[]={cat2}
//!
//! The response is a JSON object:
//!   { "Results": [...], "Indexers": [...] }
//!
//! Each result has:
//!   Title, Size, Seeders, Peers, Tracker,
//!   Link        (direct .torrent download URL, may be empty)
//!   MagnetUri   (magnet link, may be empty)
//!   InfoHash    (40-char hex SHA1, may be empty)
//!   Imdb        (integer IMDB ID, 0 if absent)
//!
//! ## Configuration
//!
//! Set these environment variables (or add to ~/.config/stui/config.toml):
//!   JACKETT_URL      = http://localhost:9117
//!   JACKETT_API_KEY  = (from Jackett → Dashboard → API Key)
//!
//! ## Build
//!
//! ```bash
//! rustup target add wasm32-wasip1
//! cargo build --release --target wasm32-wasip1
//! install -Dm644 target/wasm32-wasip1/release/jackett_provider.wasm \
//!     ~/.stui/plugins/jackett-provider/plugin.wasm
//! cp plugin.toml ~/.stui/plugins/jackett-provider/
//! ```

use serde::Deserialize;
use quick_xml::de::from_str as xml_from_str;
use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin, StreamProvider,
    EntryKind, SearchScope,
    FindStreamsRequest, FindStreamsResponse, Stream,
};

// ── Plugin struct ─────────────────────────────────────────────────────────────

pub struct JackettProvider {
    manifest: PluginManifest,
}

impl Default for JackettProvider {
    fn default() -> Self {
        Self {
            manifest: parse_manifest(include_str!("../plugin.toml"))
                .expect("plugin.toml failed to parse at compile time"),
        }
    }
}

impl Plugin for JackettProvider {
    fn manifest(&self) -> &PluginManifest { &self.manifest }
    // init/shutdown use default no-op impls from the trait
}

impl CatalogPlugin for JackettProvider {
    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        // Map the new SearchScope enum to Newznab/Torznab categories
        // and the matching Torznab `t=` operation. tvsearch / movie /
        // music are smarter than the generic `search` because each
        // indexer can refine the query (e.g. tvsearch honours season +
        // episode params on a per-tracker basis).
        let (t_op, categories) = match req.scope {
            SearchScope::Movie => ("movie",     "2000,2010,2020,2030"),
            SearchScope::Series | SearchScope::Episode => ("tvsearch", "5000,5020,5040,5070,5080"),
            SearchScope::Track | SearchScope::Artist | SearchScope::Album => ("music", "3000,3010,3020,3040"),
        };

        let url = build_torznab_url(&cfg.base_url, &cfg.api_key, t_op, &req.query, None, None, categories);
        plugin_info!("jackett: searching — {}", url);

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let rss: TorznabRss = match xml_from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                plugin_error!("jackett: parse error: {e}");
                return PluginResult::err("PARSE_ERROR", &e.to_string());
            }
        };

        plugin_info!("jackett: {} results", rss.channel.items.len());

        // Must align with the category match above — same SearchScope variants.
        let kind = match req.scope {
            SearchScope::Series | SearchScope::Episode => EntryKind::Series,
            SearchScope::Track | SearchScope::Artist | SearchScope::Album => EntryKind::Track,
            _ => EntryKind::Movie,
        };

        let items: Vec<PluginEntry> = rss.channel.items
            .into_iter()
            .take(req.limit as usize)
            .map(|r| r.into_entry(kind))
            .collect();

        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    // lookup / enrich / get_artwork / get_credits / related use the default
    // NOT_IMPLEMENTED returns from the trait — jackett is a torrent search
    // plugin, not a metadata source.
}

// ── StreamProvider impl — episode/movie-anchored stream search ──────────────
//
// `find_streams` is the verb the runtime uses to populate the
// per-episode streams column on the detail card. Unlike `search`
// (which returns torrents as catalog rows for browsing), `find_streams`
// is anchored to a specific media reference: title + year for movies,
// title + S/E for series episodes. Each Jackett result becomes a
// rich `Stream` carrying URL + quality + codec + source + hdr +
// seeders + size_bytes — enough metadata for the runtime ranker to
// score without going back to the plugin.
impl StreamProvider for JackettProvider {
    fn find_streams(&self, req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        // Pick the Torznab operation + categories from the kind. For
        // tvsearch we pass `season=` and `ep=` so trackers can refine
        // the search on their side; for movies we pass the title +
        // year via the bare query.
        let (t_op, categories) = match req.kind {
            EntryKind::Movie => ("movie",     "2000,2010,2020,2030"),
            EntryKind::Series | EntryKind::Episode => ("tvsearch", "5000,5020,5040,5070,5080"),
            _ => ("search", "2000,5000"), // mixed fallback
        };
        // `q` carries title (+ year for movies). Season/episode go in
        // dedicated params for tvsearch — that's what indexers expect.
        let q = match req.kind {
            EntryKind::Movie => match req.year {
                Some(y) => format!("{} {}", req.title, y),
                None    => req.title.clone(),
            },
            _ => req.title.clone(),
        };

        let url = build_torznab_url(
            &cfg.base_url, &cfg.api_key, t_op, &q,
            req.season, req.episode, categories,
        );

        plugin_info!("jackett: find_streams query — {} (t={})", q, t_op);

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };
        let rss: TorznabRss = match xml_from_str(&raw) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("PARSE_ERROR", &e.to_string()),
        };

        let provider_name = self.manifest.plugin.name.clone();
        let streams: Vec<Stream> = rss.channel.items
            .into_iter()
            .filter_map(|r| r.into_stream(&provider_name))
            .collect();

        plugin_info!("jackett: find_streams returned {} candidates", streams.len());
        PluginResult::ok(FindStreamsResponse { streams })
    }
}

// ── Jackett API types (Torznab/RSS) ──────────────────────────────────────────
//
// We talk to Jackett over its Torznab endpoint
// (`/api/v2.0/indexers/all/results/torznab/api`) which returns RSS XML
// with the torznab namespace extension. The non-Torznab JSON endpoint
// at `/api/v2.0/indexers/all/results` ignores `Query.SearchTerm` for
// the aggregate path on at least some Jackett builds — it returns each
// indexer's "latest items" feed unioned together, regardless of query.
// Torznab does proper server-side filtering, at the cost of being
// slower (it actually fans out the search to every configured tracker).

#[derive(Debug, Deserialize)]
struct TorznabRss {
    channel: TorznabChannel,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TorznabChannel {
    #[serde(rename = "item")]
    items: Vec<TorznabItem>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TorznabItem {
    title: String,
    /// Jackett-mediated .torrent download URL.
    link: Option<String>,
    /// Bytes; sometimes also surfaced as a torznab:attr (we fall back).
    size: Option<u64>,
    /// The originating indexer (e.g. "1337x", "RuTracker", "AniLibria").
    /// Becomes our user-visible Stream.provider label.
    jackettindexer: Option<JackettIndexer>,
    /// Newznab/Torznab extension attributes. Keyed by `name`. Common
    /// names: seeders, peers, infohash, magneturl, imdbid, size, year.
    ///
    /// quick-xml strips the namespace prefix from element names by
    /// default, so the wire `<torznab:attr>` element matches under
    /// the bare local name `attr` here. Renaming to `torznab:attr`
    /// silently matches NOTHING (0 entries), which previously left
    /// every Jackett result without seeders / magnet / info-hash.
    #[serde(rename = "attr")]
    attrs: Vec<TorznabAttr>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct JackettIndexer {
    #[serde(rename = "$text")]
    name: String,
    #[serde(rename = "@id")]
    #[allow(dead_code)] // kept for forward-compat (per-indexer dispatch)
    id: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TorznabAttr {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@value")]
    value: String,
}

impl TorznabItem {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.name == name).map(|a| a.value.as_str())
    }
    fn attr_u64(&self, name: &str) -> Option<u64> {
        self.attr(name).and_then(|s| s.parse().ok())
    }
    fn attr_i32(&self, name: &str) -> Option<i32> {
        self.attr(name).and_then(|s| s.parse().ok())
    }
    /// Convert one Torznab item into a rich `Stream` for the
    /// `find_streams` flow. Returns `None` when no usable URL handle
    /// is present — the runtime expects every `Stream.url` to be
    /// playable.
    fn into_stream(self, fallback_provider: &str) -> Option<Stream> {
        let magnet    = self.attr("magneturl").map(str::to_string);
        let info_hash = self.attr("infohash").map(str::to_string);

        let url = if let Some(m) = magnet.as_deref().filter(|s| !s.is_empty()) {
            m.to_string()
        } else if let Some(h) = info_hash.as_deref().filter(|s| !s.is_empty()) {
            // Synthesise a magnet from just the info-hash. `dn=` lets
            // clients show a name before metadata fetches; we don't
            // have a tracker list here so the magnet is bare.
            format!("magnet:?xt=urn:btih:{}&dn={}", h, url_encode(&self.title))
        } else if let Some(l) = self.link.as_deref().filter(|s| !s.is_empty()) {
            // .torrent download URL fallback (Jackett-mediated). The
            // runtime hands these to aria2 which fetches and seeds.
            l.to_string()
        } else {
            return None;
        };

        let quality = extract_quality(&self.title);
        let codec   = extract_codec(&self.title);
        let source  = extract_source(&self.title);
        let hdr     = extract_hdr(&self.title);
        let seeders = self.attr_i32("seeders").map(|s| s.max(0) as u32);
        let size_bytes = self.size.or_else(|| self.attr_u64("size")).filter(|&n| n > 0);
        // Surface the originating indexer (e.g. "RuTracker", "1337x")
        // rather than the bare plugin name — Jackett aggregates many
        // trackers and the user wants per-source visibility.
        let provider_label = self.jackettindexer
            .as_ref()
            .map(|j| j.name.as_str())
            .filter(|n| !n.is_empty())
            .unwrap_or(fallback_provider)
            .to_string();

        Some(Stream {
            url,
            title: self.title.clone(),
            provider: provider_label,
            quality,
            codec,
            source,
            hdr,
            seeders,
            size_bytes,
            language: None,
            subtitles: vec![],
        })
    }

    fn into_entry(self, kind: EntryKind) -> PluginEntry {
        let quality   = extract_quality(&self.title);
        let size      = self.size.or_else(|| self.attr_u64("size")).unwrap_or(0);
        let size_str  = humanize_bytes(size);
        let seeders   = self.attr_i32("seeders").unwrap_or(0);
        let peers     = self.attr_i32("peers").unwrap_or(0);
        let leechers  = (peers - seeders).max(0);
        let indexer   = self.jackettindexer.as_ref().map(|j| j.name.as_str()).unwrap_or("");
        let meta = format!("{size_str}  ↑{} ↓{}  {}", seeders, leechers, indexer);

        // Pack the three resolution handles into the ID so resolve()
        // needs no second network call. Delimiters: '|' separates
        // hash, magnet, and link. Fields may be empty strings.
        let id = format!(
            "{}|{}|{}",
            self.attr("infohash").unwrap_or(""),
            self.attr("magneturl").unwrap_or(""),
            self.link.as_deref().unwrap_or(""),
        );

        // Torznab encodes IMDB id as `imdbid`, sometimes with a "tt" prefix
        // sometimes without. Normalise to the canonical "ttNNNNNNN" form.
        let imdb_id = self.attr("imdbid").or_else(|| self.attr("imdb"))
            .filter(|s| !s.is_empty())
            .map(|raw| if raw.starts_with("tt") { raw.to_string() } else { format!("tt{raw}") });

        let description = match quality {
            Some(q) => Some(format!("{q} · {meta}")),
            None => Some(meta),
        };

        PluginEntry {
            id,
            kind,
            title: self.title,
            description,
            imdb_id,
            // All other Option fields and new ones (artist_name, album_name,
            // track_number, season, episode, original_language, genre,
            // rating, year, poster_url, duration, external_ids) default
            // to None/empty — Jackett has no metadata beyond title + size.
            ..Default::default()
        }
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    base_url: String,
    api_key: String,
}

impl Config {
    fn load() -> Result<Self, String> {
        // The host passes env vars from plugin.toml [env] section to the plugin
        // via the cache under the key "__env:VAR_NAME".
        let base_url = env_or("JACKETT_URL", "http://localhost:9117");
        let api_key = env_or("JACKETT_API_KEY", "");

        if api_key.is_empty() {
            return Err("JACKETT_API_KEY is not set. \
                 Add it to ~/.config/stui/config.toml or set the env var."
                .into());
        }

        Ok(Config { base_url, api_key })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve environment variables through the host cache mechanism.
/// The runtime injects `__env:{VAR}` into the plugin cache from plugin.toml [env].
fn env_or(var: &str, default: &str) -> String {
    let cache_key = format!("__env:{}", var);
    cache_get(&cache_key).unwrap_or_else(|| default.to_string())
}

/// Build a Jackett Torznab URL.
///
/// Endpoint: `/api/v2.0/indexers/all/results/torznab/api`. This is the
/// only Jackett endpoint that does proper server-side filtering on the
/// query — the JSON-style `/results?Query.SearchTerm=…` aggregate route
/// returns each indexer's "latest items" feed unioned together,
/// regardless of query, on at least some Jackett builds.
///
/// `t` selects the operation (`tvsearch`, `movie`, `music`, `search`).
/// `season` and `ep` are numeric refinements honoured by tvsearch.
/// `cats` is a comma-separated Newznab category list.
fn build_torznab_url(
    base_url: &str,
    api_key:  &str,
    t:        &str,
    q:        &str,
    season:   Option<u32>,
    ep:       Option<u32>,
    cats:     &str,
) -> String {
    let mut url = format!(
        "{}/api/v2.0/indexers/all/results/torznab/api?apikey={}&t={}&q={}&cat={}",
        base_url, api_key, t, url_encode(q), cats,
    );
    if let Some(s) = season { url.push_str(&format!("&season={}", s)); }
    if let Some(e) = ep     { url.push_str(&format!("&ep={}", e)); }
    url
}

/// Detect the encoding codec from a release title. Default to None when
/// unrecognised so the runtime ranker doesn't get a misleading value.
fn extract_codec(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    if t.contains("X265") || t.contains("H.265") || t.contains("HEVC") {
        return Some("h265".into());
    }
    if t.contains("AV1") {
        return Some("av1".into());
    }
    if t.contains("X264") || t.contains("H.264") || t.contains("AVC") {
        return Some("h264".into());
    }
    None
}

/// Detect the source class. Maps common scene tags to canonical names.
fn extract_source(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    for (tag, label) in [
        ("BLURAY", "BluRay"),
        ("BDREMUX", "BDRemux"),
        ("WEB-DL", "WEB-DL"),
        ("WEBDL", "WEB-DL"),
        ("WEBRIP", "WEBRip"),
        ("HDTV", "HDTV"),
        ("DVDRIP", "DVDRip"),
        ("CAM", "CAM"),
        ("HDCAM", "CAM"),
        ("TS", "TS"),
    ] {
        if t.contains(tag) {
            return Some(label.into());
        }
    }
    None
}

/// True when the release title advertises any HDR format.
fn extract_hdr(title: &str) -> bool {
    let t = title.to_uppercase();
    t.contains("HDR10+")
        || t.contains("HDR10")
        || t.contains("HDR")
        || t.contains("DOLBY VISION")
        || t.contains("DV ")
        || t.contains(" DV.")
}

/// Try to extract a quality string from a release title.
fn extract_quality(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    for tag in &[
        "2160P", "4K", "UHD", "1080P", "720P", "480P", "BDREMUX", "BLURAY", "WEB-DL",
    ] {
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

/// Human-readable byte size.
fn humanize_bytes(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    const KIB: u64 = 1 << 10;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.0} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ── WASM exports ──────────────────────────────────────────────────────────────

stui_export_catalog_plugin!(JackettProvider);
