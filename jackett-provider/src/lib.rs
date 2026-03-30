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

// ── Plugin struct ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct JackettProvider;

impl StuiPlugin for JackettProvider {
    fn name(&self) -> &str { "jackett-provider" }
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
            "{}/api/v2.0/indexers/all/results?apikey={}&Query.SearchTerm={}&{}",
            cfg.base_url,
            cfg.api_key,
            query_enc,
            build_cat_params(categories),
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

        let items: Vec<PluginEntry> = envelope.results
            .into_iter()
            .take(req.limit as usize)
            .map(|r| r.into_entry())
            .collect();

        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        // The entry ID is "{info_hash}|{magnet_uri}|{link}" packed by into_entry().
        // Prefer MagnetUri, then Link (.torrent download), then build magnet from hash.
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

        plugin_info!("jackett: resolve → {}", &stream_url[..stream_url.len().min(80)]);

        PluginResult::ok(ResolveResponse {
            stream_url,
            quality: None,  // quality comes from the title string (e.g. "1080p")
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
    title:      String,
    #[serde(rename = "Size", default)]
    size:       u64,            // bytes
    #[serde(rename = "Seeders", default)]
    seeders:    i32,
    #[serde(rename = "Peers", default)]
    peers:      i32,            // total peers (seeders + leechers)
    #[serde(rename = "Tracker", default)]
    tracker:    String,
    #[serde(rename = "Link", default)]
    link:       String,         // direct .torrent download URL (may be empty)
    #[serde(rename = "MagnetUri", default)]
    magnet_uri: String,         // full magnet link (may be empty)
    #[serde(rename = "InfoHash", default)]
    info_hash:  String,         // 40-char hex SHA1 (may be empty)
    #[serde(rename = "Imdb", default)]
    imdb:       Option<i64>,    // IMDB numeric ID (0 or null if absent)
}

impl JackettResult {
    fn into_entry(self) -> PluginEntry {
        let quality  = extract_quality(&self.title);
        let size_str = humanize_bytes(self.size);
        let leechers = (self.peers - self.seeders).max(0);
        let meta = format!(
            "{size_str}  ↑{} ↓{}  {}",
            self.seeders, leechers, self.tracker,
        );

        // Pack the three resolution handles into the ID so resolve() needs no
        // second network call.  Delimiters: first '|' separates hash, second
        // '|' separates magnet from link.  Fields may be empty strings.
        let id = format!("{}|{}|{}", self.info_hash, self.magnet_uri, self.link);

        let imdb_id = self.imdb
            .filter(|&i| i > 0)
            .map(|i| format!("tt{:07}", i));

        PluginEntry {
            id,
            title:       self.title,
            year:        None,
            genre:       Some(meta),   // seeders/size/tracker packed into genre slot
            rating:      quality,      // "1080p", "4K", etc.
            description: None,
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
        let base_url = env_or("JACKETT_URL", "http://localhost:9117");
        let api_key  = env_or("JACKETT_API_KEY", "");

        if api_key.is_empty() {
            return Err(
                "JACKETT_API_KEY is not set. \
                 Add it to ~/.config/stui/config.toml or set the env var.".into()
            );
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
                    let hex = |n: u8| -> char {
                        if n < 10 { (b'0' + n) as char } else { (b'a' + n - 10) as char }
                    };
                    vec!['%', hex(b >> 4), hex(b & 0xf)]
                }).collect::<Vec<_>>()
            }
        })
        .collect()
}

/// Split "{info_hash}|{magnet_uri}|{link}" packed ID.
fn parse_entry_id(id: &str) -> (String, String, String) {
    let mut parts = id.splitn(3, '|');
    let hash   = parts.next().unwrap_or("").to_string();
    let magnet = parts.next().unwrap_or("").to_string();
    let link   = parts.next().unwrap_or("").to_string();
    (hash, magnet, link)
}

/// Try to extract a quality string from a release title.
fn extract_quality(title: &str) -> Option<String> {
    let t = title.to_uppercase();
    for tag in &["2160P", "4K", "UHD", "1080P", "720P", "480P", "BDREMUX", "BLURAY", "WEB-DL"] {
        if t.contains(tag) {
            return Some(tag.to_lowercase()
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

stui_export_plugin!(JackettProvider);
