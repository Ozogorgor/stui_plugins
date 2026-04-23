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
use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{
    parse_manifest, PluginManifest,
    Plugin, CatalogPlugin,
    EntryKind, SearchScope,
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

        // Map the new SearchScope enum to Jackett's Newznab categories.
        // Music scopes fan out to 3000-range categories even though our
        // manifest advertises only movie/series kinds — if the runtime
        // ever dispatches a music scope here (manual test, future support),
        // do the right thing instead of silently returning movies.
        let categories = match req.scope {
            SearchScope::Movie => "2000,2010,2020,2030",
            SearchScope::Series | SearchScope::Episode => "5000,5020,5040,5070,5080",
            SearchScope::Track | SearchScope::Artist | SearchScope::Album => "3000,3010,3020,3040",
            // _ unreachable — SearchScope only has 6 variants, covered.
        };

        let query_enc = url_encode(&req.query);
        let url = format!(
            "{}/api/v2.0/indexers/all/results?apikey={}&Query.SearchTerm={}&{}",
            cfg.base_url, cfg.api_key, query_enc, build_cat_params(categories),
        );

        plugin_info!("jackett: searching — {}", url);

        let raw = match http_get(&url) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let envelope: JackettEnvelope = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                plugin_error!("jackett: parse error: {e}");
                return PluginResult::err("PARSE_ERROR", &e.to_string());
            }
        };

        plugin_info!("jackett: {} results", envelope.results.len());

        // Must align with the category match above — same SearchScope variants.
        let kind = match req.scope {
            SearchScope::Series | SearchScope::Episode => EntryKind::Series,
            SearchScope::Track | SearchScope::Artist | SearchScope::Album => EntryKind::Track,
            _ => EntryKind::Movie,
        };

        let items: Vec<PluginEntry> = envelope
            .results
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

// `StuiPlugin` is deprecated in favor of `Plugin + CatalogPlugin`, but
// `stui_export_plugin!` still requires it for the `stui_resolve` ABI
// export. This block goes away when the subtitle/stream ABIs land and
// the macro drops its `$plugin_ty: StuiPlugin` bound.
#[allow(deprecated)]
impl StuiPlugin for JackettProvider {
    fn name(&self) -> &str { &self.manifest.plugin.name }
    fn version(&self) -> &str { &self.manifest.plugin.version }
    fn plugin_type(&self) -> PluginType { PluginType::Provider }

    // Never dispatched — stui_search routes through CatalogPlugin::search
    // via the stui_export_plugin! macro. Kept as a trait stub so the
    // macro's bound `$plugin_ty: StuiPlugin` is satisfied.
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::err("LEGACY_UNUSED", "search dispatches via CatalogPlugin")
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        // The entry ID is "{info_hash}|{magnet_uri}|{link}" packed by into_entry().
        let (info_hash, magnet_uri, link) = parse_entry_id(&req.entry_id);

        let stream_url = if !magnet_uri.is_empty() {
            magnet_uri
        } else if !link.is_empty() {
            link
        } else if !info_hash.is_empty() {
            format!("magnet:?xt=urn:btih:{}&dn=torrent", info_hash)
        } else {
            return PluginResult::err("RESOLVE_ERROR", "no MagnetUri, Link, or InfoHash");
        };

        let truncated: String = stream_url.chars().take(80).collect();
        plugin_info!("jackett: resolve → {}", truncated);

        PluginResult::ok(ResolveResponse {
            stream_url,
            // Quality is already embedded in PluginEntry.description at
            // search time (extracted from the release title); the resolver
            // doesn't re-derive it.
            quality: None,
            subtitles: vec![],
        })
    }
}

// ── Jackett API types ─────────────────────────────────────────────────────────

/// Top-level response wrapper from GET /api/v2.0/indexers/all/results
#[derive(Debug, Deserialize)]
struct JackettEnvelope {
    #[serde(rename = "Results")]
    results: Vec<JackettResult>,
}

/// One result from the Jackett aggregate search
#[derive(Debug, Deserialize)]
struct JackettResult {
    #[serde(rename = "Title", default)]
    title: String,
    #[serde(rename = "Size", default)]
    size: u64, // bytes
    #[serde(rename = "Seeders", default)]
    seeders: i32,
    #[serde(rename = "Peers", default)]
    peers: i32, // total peers (seeders + leechers)
    #[serde(rename = "Tracker", default)]
    tracker: String,
    #[serde(rename = "Link", default)]
    link: String, // direct .torrent download URL (may be empty)
    #[serde(rename = "MagnetUri", default)]
    magnet_uri: String, // full magnet link (may be empty)
    #[serde(rename = "InfoHash", default)]
    info_hash: String, // 40-char hex SHA1 (may be empty)
    #[serde(rename = "Imdb", default)]
    imdb: Option<i64>, // IMDB numeric ID (0 or null if absent)
}

impl JackettResult {
    fn into_entry(self, kind: EntryKind) -> PluginEntry {
        let quality = extract_quality(&self.title);
        let size_str = humanize_bytes(self.size);
        let leechers = (self.peers - self.seeders).max(0);
        let meta = format!(
            "{size_str}  ↑{} ↓{}  {}",
            self.seeders, leechers, self.tracker,
        );

        // Pack the three resolution handles into the ID so resolve() needs
        // no second network call. Delimiters: '|' separates hash, magnet,
        // and link. Fields may be empty strings.
        let id = format!("{}|{}|{}", self.info_hash, self.magnet_uri, self.link);

        let imdb_id = self.imdb.filter(|&i| i > 0).map(|i| format!("tt{:07}", i));

        // Put the non-numeric quality tag into description alongside the
        // size/seeders/tracker meta — PluginEntry.rating is f32 and
        // "1080p"/"4K" aren't ratings. Quality first so the row remains scannable.
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
            // to None/empty — jackett has no metadata beyond title + size.
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

/// Build repeated `Category[]=X&Category[]=Y` query params from a comma list.
fn build_cat_params(cats: &str) -> String {
    cats.split(',')
        .map(|c| format!("Category[]={}", c.trim()))
        .collect::<Vec<_>>()
        .join("&")
}

/// Split "{info_hash}|{magnet_uri}|{link}" packed ID.
fn parse_entry_id(id: &str) -> (String, String, String) {
    let mut parts = id.splitn(3, '|');
    let hash = parts.next().unwrap_or("").to_string();
    let magnet = parts.next().unwrap_or("").to_string();
    let link = parts.next().unwrap_or("").to_string();
    (hash, magnet, link)
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

stui_export_plugin!(JackettProvider);
