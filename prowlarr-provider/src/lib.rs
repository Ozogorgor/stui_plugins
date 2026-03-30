//! prowlarr-provider — stui plugin that searches a local Prowlarr instance.
//!
//! ## How it works
//!
//! Prowlarr is an indexer manager.  It speaks the Newznab/Torznab protocol and
//! exposes a unified REST API that queries all your configured trackers in one
//! request.  We use the aggregate search endpoint:
//!
//!   GET {PROWLARR_URL}/api/v1/search
//!       ?query={q}
//!       &indexerIds=-1          (all indexers)
//!       &categories={cats}      (tab-appropriate Newznab category IDs)
//!       &type=search
//!     Header: X-Api-Key: {PROWLARR_API_KEY}
//!
//! The response is a JSON array of search results.  Each result has:
//!   title, size, seeders, leechers, indexer, protocol,
//!   downloadUrl  (direct .torrent download OR null for magnet-only indexers)
//!   infoHash     (40-char hex — used to build magnet URI if no downloadUrl)
//!   imdbId, tmdbId, tvdbId  (optional — present for many private trackers)
//!
//! ## Configuration
//!
//! Set these environment variables (or add to ~/.config/stui/config.toml):
//!   PROWLARR_URL      = http://localhost:9696
//!   PROWLARR_API_KEY  = (from Prowlarr → Settings → General)
//!
//! ## Build
//!
//! ```bash
//! rustup target add wasm32-wasip1
//! cargo build --release --target wasm32-wasip1
//! install -Dm644 target/wasm32-wasip1/release/prowlarr_provider.wasm \
//!     ~/.stui/plugins/prowlarr-provider/plugin.wasm
//! cp plugin.toml ~/.stui/plugins/prowlarr-provider/
//! ```

use serde::Deserialize;
use stui_plugin_sdk::prelude::*;

// ── Plugin struct ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ProwlarrProvider;

impl StuiPlugin for ProwlarrProvider {
    fn name(&self) -> &str { "prowlarr-provider" }
    fn version(&self) -> &str { "0.1.0" }
    fn plugin_type(&self) -> PluginType { PluginType::Provider }

    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        // Choose Newznab categories based on tab
        let categories = match req.tab.as_str() {
            "movies"  => "2000,2010,2020,2030",
            "series"  => "5000,5020,5040,5070,5080",
            "music"   => "3000,3010,3020,3040",
            _         => "2000,5000",
        };

        let query_enc = url_encode(&req.query);
        let url = format!(
            "{}/api/v1/search?query={}&indexerIds=-1&{}&type=search",
            cfg.base_url,
            query_enc,
            build_cat_params(categories),
        );

        plugin_info!("prowlarr: searching — {}", url);

        let raw = match http_get_with_key(&url, &cfg.api_key) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let results: Vec<ProwlarrResult> = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                plugin_error!("prowlarr: parse error: {e}");
                return PluginResult::err("PARSE_ERROR", &e.to_string());
            }
        };

        plugin_info!("prowlarr: {} results", results.len());

        let items: Vec<PluginEntry> = results
            .into_iter()
            .take(req.limit as usize)
            .map(|r| r.into_entry())
            .collect();

        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        // The entry ID is "{infoHash}|{downloadUrl}" — packed by into_entry().
        // We extract the download URL (or build a magnet) and return it to
        // the runtime, which hands it to aria2.
        let (info_hash, download_url) = parse_entry_id(&req.entry_id);

        let stream_url = if !download_url.is_empty() {
            // Direct .torrent download URL — aria2 will fetch + parse it
            download_url
        } else if !info_hash.is_empty() {
            // Build a minimal magnet URI from the hash
            format!("magnet:?xt=urn:btih:{}&dn=torrent", info_hash)
        } else {
            return PluginResult::err("RESOLVE_ERROR", "no downloadUrl or infoHash");
        };

        plugin_info!("prowlarr: resolve → {}", &stream_url[..stream_url.len().min(80)]);

        PluginResult::ok(ResolveResponse {
            stream_url,
            quality: None,  // quality comes from the title string (e.g. "1080p")
            subtitles: vec![],
        })
    }
}

// ── Prowlarr API types ────────────────────────────────────────────────────────

/// One result from GET /api/v1/search
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProwlarrResult {
    title:        String,
    #[serde(default)]
    size:         u64,            // bytes
    #[serde(default)]
    seeders:      i32,
    #[serde(default)]
    leechers:     i32,
    #[serde(default)]
    indexer:      String,
    #[serde(default)]
    protocol:     String,         // "torrent" | "usenet"
    #[serde(default)]
    download_url: String,         // direct .torrent URL (may be empty)
    #[serde(default)]
    info_url:     String,         // tracker page
    #[serde(default)]
    info_hash:    String,         // 40-char hex SHA1
    #[serde(default)]
    imdb_id:      Option<i64>,
    #[serde(default)]
    tmdb_id:      Option<i64>,
    // Category list is present but we don't need it for the entry
}

impl ProwlarrResult {
    fn into_entry(self) -> PluginEntry {
        // Quality hint from title
        let quality = extract_quality(&self.title);
        // Size as human-readable genre-like string (we borrow the genre slot)
        let size_str = humanize_bytes(self.size);
        let meta = format!(
            "{size_str}  ↑{} ↓{}  {indexer}",
            self.seeders, self.leechers,
            indexer = self.indexer,
        );

        // Pack infoHash and downloadUrl into the ID so resolve() can use them
        // without a second network call.
        let id = format!("{}|{}", self.info_hash, self.download_url);

        let imdb_id = self.imdb_id.map(|i| format!("tt{:07}", i));

        PluginEntry {
            id,
            title:       self.title,
            year:        None,         // Prowlarr doesn't always provide year
            genre:       Some(meta),   // seeders/size packed into genre slot
            rating:      quality,      // "1080p", "4K", etc.
            description: Some(format!(
                "Protocol: {}  InfoURL: {}",
                self.protocol, self.info_url
            )),
            poster_url:  None,
            imdb_id,
        }
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    base_url: String,
    api_key:  String,
}

impl Config {
    fn load() -> Result<Self, String> {
        // The host passes env vars from plugin.toml [env] section to the plugin
        // via the cache under the key "__env:VAR_NAME".
        let base_url = env_or("PROWLARR_URL", "http://localhost:9696");
        let api_key  = env_or("PROWLARR_API_KEY", "");

        if api_key.is_empty() {
            return Err(
                "PROWLARR_API_KEY is not set. \
                 Add it to ~/.config/stui/config.toml or set the env var.".into()
            );
        }

        Ok(Config { base_url, api_key })
    }
}

// ── HTTP helper with X-Api-Key header ─────────────────────────────────────────
//
// The SDK's http_get sends a plain GET.  Prowlarr requires the API key in
// the X-Api-Key header.  We encode it into the URL query string as well
// (Prowlarr accepts both).

fn http_get_with_key(url: &str, api_key: &str) -> Result<String, String> {
    // Prowlarr accepts the API key as a query parameter (apikey=) AND as a
    // header (X-Api-Key).  We append it to the query string because the current
    // SDK only provides a plain http_get.  A future SDK version will add header
    // support; for now this is functionally equivalent for local instances.
    let url_with_key = format!("{}&apikey={}", url, api_key);
    http_get(&url_with_key)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve environment variables through the host cache mechanism.
/// The runtime injects `__env:{VAR}` into the plugin cache from plugin.toml [env].
fn env_or(var: &str, default: &str) -> String {
    let cache_key = format!("__env:{}", var);
    cache_get(&cache_key).unwrap_or_else(|| default.to_string())
}

/// Build repeated `categories=X&categories=Y` query params from a comma list.
fn build_cat_params(cats: &str) -> String {
    cats.split(',')
        .map(|c| format!("categories={}", c.trim()))
        .collect::<Vec<_>>()
        .join("&")
}

/// Minimal percent-encoding for URL query values.
fn url_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9'
            | '-' | '_' | '.' | '~' => vec![c],
            ' ' => vec!['+'],
            c => {
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf).as_bytes();
                bytes.iter().flat_map(|&b| {
                    let hi = (b >> 4) as char;
                    let lo = (b & 0xf) as char;
                    let hex = |n: u8| -> char {
                        if n < 10 { (b'0' + n) as char } else { (b'a' + n - 10) as char }
                    };
                    vec!['%', hex(b >> 4), hex(b & 0xf)]
                }).collect::<Vec<_>>()
            }
        })
        .collect()
}

/// Split "infoHash|downloadUrl" packed ID.
fn parse_entry_id(id: &str) -> (String, String) {
    if let Some(pos) = id.find('|') {
        let hash = id[..pos].to_string();
        let url  = id[pos+1..].to_string();
        (hash, url)
    } else {
        (id.to_string(), String::new())
    }
}

/// Try to extract a quality string from a release title.
fn extract_quality(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    for tag in &["2160P", "4K", "UHD", "1080P", "720P", "480P", "BDREMUX", "BLURAY", "WEB-DL"] {
        if t.contains(tag) {
            return Some(tag.to_lowercase()
                .replace("p", "p")
                .replace("bdremux", "BD Remux")
                .replace("bluray", "Blu-ray")
                .replace("web-dl", "WEB-DL"));
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

stui_export_plugin!(ProwlarrProvider);
