//! opensubtitles-provider — stui plugin for subtitle search and download.
//!
//! ## OpenSubtitles REST API v1 overview
//!
//! Base URL: https://api.opensubtitles.com/api/v1   (or vip-api.* for VIP users)
//!
//! Every request requires the header:
//!   Api-Key: {OS_API_KEY}
//!   User-Agent: stui v0.1.0
//!   Content-Type: application/json    (on POST)
//!
//! ### Authentication (optional — needed for >5 downloads/day)
//!
//!   POST /login   body: { "username": "…", "password": "…" }
//!   →  { "token": "eyJ…", "base_url": "api.opensubtitles.com", "status": 200 }
//!
//! The token is a JWT; treat it as opaque.  It doesn't expire quickly but
//! we re-authenticate if we get a 401.  We cache it under key "os_jwt".
//!
//! ### Search
//!
//!   GET /subtitles?imdb_id={id}&languages={lang}&per_page=20
//!   or  GET /subtitles?query={title}&languages={lang}&per_page=20
//!   →  { "data": [ { "id": "…", "attributes": { … } } ], "total_count": N }
//!
//! ### Download
//!
//!   POST /download   body: { "file_id": 12345 }
//!   Header: Authorization: Bearer {jwt}   (required for download)
//!   →  { "link": "https://dl.opensubtitles.com/…/…srt", "remaining": 98 }
//!
//! ## Plugin role in stui
//!
//! This plugin acts as a "subtitle" plugin.  The stui runtime calls:
//!   search(query=title, tab="subtitles")  → returns subtitle entries
//!   resolve(entry_id=file_id)             → returns the subtitle download URL
//!
//! The detail panel's STREAM VIA section will list this plugin.  When the
//! user selects a subtitle entry, resolve() fires and stui hands the URL
//! to aria2 to download to ~/.stui/subtitles/{imdb_id}/.
//!
//! ## Configuration
//!
//!   OS_API_KEY  — required (https://www.opensubtitles.com/en/consumers)
//!   OS_USERNAME — optional, unlocks >5 downloads/day
//!   OS_PASSWORD — optional
//!   OS_LANGUAGE — default "en"; comma-separated BCP-47 codes
//!
//! ## Build
//!
//! ```bash
//! rustup target add wasm32-wasip1
//! cargo build --release --target wasm32-wasip1
//! install -Dm644 target/wasm32-wasip1/release/opensubtitles_provider.wasm \
//!     ~/.stui/plugins/opensubtitles-provider/plugin.wasm
//! cp plugin.toml ~/.stui/plugins/opensubtitles-provider/
//! ```

use serde::{Deserialize, Serialize};
use stui_plugin_sdk::prelude::*;

// ── Plugin struct ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct OpenSubtitlesProvider;

impl StuiPlugin for OpenSubtitlesProvider {
    fn name(&self) -> &str {
        "opensubtitles-provider"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn plugin_type(&self) -> PluginType {
        PluginType::Provider
    }

    /// Search returns subtitle entries.
    ///
    /// The `req.query` field is used as title search text.
    /// If the entry has an imdb_id (set via the detail panel's entry_id), we
    /// prefer that for precision.
    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        // Detect if query looks like an IMDB id  (tt\d+) or raw title
        let (url, search_desc) = if req.query.starts_with("tt") && req.query.len() > 2 {
            let id_num = &req.query[2..]; // strip "tt" prefix
            (
                format!(
                    "https://{}/api/v1/subtitles?imdb_id={}&languages={}&per_page=20",
                    cfg.base_url, id_num, cfg.language
                ),
                format!("imdb:{}", req.query),
            )
        } else {
            let q = url_encode(&req.query);
            (
                format!(
                    "https://{}/api/v1/subtitles?query={}&languages={}&per_page=20",
                    cfg.base_url, q, cfg.language
                ),
                format!("query:{}", req.query),
            )
        };

        plugin_info!("opensubtitles: searching {}", search_desc);

        let raw = match http_get_authed(&url, &cfg) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let resp: SubtitleListResponse = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                plugin_error!("opensubtitles: parse error: {}", e);
                return PluginResult::err("PARSE_ERROR", &e.to_string());
            }
        };

        plugin_info!("opensubtitles: {} subtitles found", resp.total_count);

        let items: Vec<PluginEntry> = resp
            .data
            .into_iter()
            .take(req.limit as usize)
            .map(|s| s.into_entry())
            .collect();

        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    /// Resolve returns the direct download URL for a subtitle file.
    ///
    /// `req.entry_id` is the OpenSubtitles `file_id` (integer, stored as string).
    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("CONFIG_ERROR", &e),
        };

        let file_id: u64 = match req.entry_id.parse() {
            Ok(id) => id,
            Err(_) => {
                // Try parsing "os:{file_id}" format
                if let Some(stripped) = req.entry_id.strip_prefix("os:") {
                    match stripped.parse() {
                        Ok(id) => id,
                        Err(_) => {
                            return PluginResult::err("INVALID_ID", "could not parse file_id")
                        }
                    }
                } else {
                    return PluginResult::err("INVALID_ID", "could not parse file_id");
                }
            }
        };

        plugin_info!("opensubtitles: downloading file_id={}", file_id);

        // Ensure we have a JWT for the download endpoint
        let jwt = match ensure_jwt(&cfg) {
            Ok(jwt) => jwt,
            Err(e) => return PluginResult::err("AUTH_REQUIRED", &e),
        };

        let body = serde_json::to_string(&DownloadRequest { file_id }).unwrap_or_default();

        let url = format!("https://{}/api/v1/download", cfg.base_url);
        let raw = match http_post_authed(&url, &body, &cfg, &jwt) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("DOWNLOAD_ERROR", &e),
        };

        let dl: DownloadResponse = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("PARSE_ERROR", &e.to_string()),
        };

        if dl.link.is_empty() {
            return PluginResult::err("DOWNLOAD_ERROR", "empty download link");
        }

        plugin_info!(
            "opensubtitles: got download link (remaining={})",
            dl.remaining
        );

        // The subtitle URL is the "stream_url" in stui terms — aria2 will
        // fetch it to ~/.stui/subtitles/{imdb_id}/
        PluginResult::ok(ResolveResponse {
            stream_url: dl.link,
            quality: dl.file_name,
            subtitles: vec![],
        })
    }
}

// ── OpenSubtitles API types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct SubtitleListResponse {
    #[serde(default)]
    data: Vec<SubtitleItem>,
    #[serde(default)]
    total_count: u32,
}

#[derive(Deserialize)]
struct SubtitleItem {
    #[serde(default)]
    id: String,
    #[serde(default)]
    attributes: SubtitleAttributes,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
struct SubtitleAttributes {
    #[serde(default)]
    subtitle_id: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    download_count: u32,
    #[serde(default)]
    ratings: f32,
    #[serde(default)]
    release: String, // release name, e.g. "Dune.2021.BluRay.1080p"
    #[serde(default)]
    hearing_impaired: bool,
    #[serde(default)]
    ai_translated: bool,
    #[serde(default)]
    machine_translated: bool,
    #[serde(default)]
    hd: bool,
    #[serde(default)]
    files: Vec<SubtitleFile>,
    #[serde(default)]
    feature_details: FeatureDetails,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize, Default)]
struct SubtitleFile {
    #[serde(default)]
    file_id: u64,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    cd_number: Option<u32>,
}

#[derive(Deserialize, Default)]
struct FeatureDetails {
    #[serde(default)]
    title: String,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    imdb_id: Option<u64>,
    #[serde(default)]
    tmdb_id: Option<u64>,
}

impl SubtitleItem {
    fn into_entry(self) -> PluginEntry {
        let a = &self.attributes;
        let file_id = a.files.first().map(|f| f.file_id).unwrap_or(0);
        let file_name = a
            .files
            .first()
            .and_then(|f| f.file_name.clone())
            .unwrap_or_else(|| a.release.clone());

        // Flags
        let mut flags = Vec::new();
        if a.hearing_impaired {
            flags.push("HI");
        }
        if a.hd {
            flags.push("HD");
        }
        if a.ai_translated {
            flags.push("AI");
        }
        if a.machine_translated {
            flags.push("MT");
        }

        let flag_str = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(" "))
        };

        let title = format!(
            "[{}] {}{}  ★{:.1}  ↓{}",
            a.language.to_uppercase(),
            file_name,
            flag_str,
            a.ratings,
            a.download_count,
        );

        let imdb_id = a.feature_details.imdb_id.map(|i| format!("tt{:07}", i));

        PluginEntry {
            // Entry ID is the file_id — resolve() uses it directly
            id: format!("os:{}", file_id),
            title,
            year: a.feature_details.year.map(|y| y.to_string()),
            genre: Some(a.language.clone()),
            rating: Some(format!("{:.1}", a.ratings)),
            description: Some(a.url.clone()),
            poster_url: None,
            imdb_id,
            duration: None,
        }
    }
}

#[derive(Serialize)]
struct DownloadRequest {
    file_id: u64,
}

#[derive(Deserialize)]
struct DownloadResponse {
    #[serde(default)]
    link: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    remaining: i32,
}

// ── Login / JWT management ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct LoginResponse {
    #[serde(default)]
    token: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    status: u16,
}

#[derive(Serialize)]
struct LoginRequest<'a> {
    username: &'a str,
    password: &'a str,
}

const JWT_CACHE_KEY: &str = "os_jwt";
const BASE_URL_CACHE_KEY: &str = "os_base_url";

/// Return a valid JWT, fetching one if necessary.
fn ensure_jwt(cfg: &Config) -> Result<String, String> {
    // Try cached token first
    if let Some(token) = cache_get(JWT_CACHE_KEY) {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // No cached token — login required
    if cfg.username.is_empty() || cfg.password.is_empty() {
        return Err(
            "OS_USERNAME and OS_PASSWORD must be set for subtitle downloads. \
             Anonymous users are limited to 5 downloads per 24h."
                .into(),
        );
    }

    login(cfg)
}

fn login(cfg: &Config) -> Result<String, String> {
    let body = serde_json::to_string(&LoginRequest {
        username: &cfg.username,
        password: &cfg.password,
    })
    .unwrap_or_default();

    let url = format!("https://{}/api/v1/login", cfg.base_url);

    plugin_info!("opensubtitles: logging in as {}", cfg.username);

    // POST with Api-Key header encoded into the payload (see http_post_authed)
    let raw = http_post_anon(&url, &body, cfg)?;

    let resp: LoginResponse = serde_json::from_str(&raw).map_err(|e| e.to_string())?;

    if resp.status != 200 || resp.token.is_empty() {
        return Err(format!("login failed: status={}", resp.status));
    }

    // Cache the token and the (possibly different) base_url
    cache_set(JWT_CACHE_KEY, &resp.token);
    if !resp.base_url.is_empty() {
        cache_set(BASE_URL_CACHE_KEY, &resp.base_url);
    }

    plugin_info!(
        "opensubtitles: login successful, base_url={}",
        resp.base_url
    );
    Ok(resp.token)
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────
//
// The stui SDK's http_get / http_post_json send a plain request.
// OpenSubtitles requires `Api-Key` and `User-Agent` headers.
//
// Since the current SDK doesn't support arbitrary request headers, we rely
// on the stui runtime to inject them from the plugin manifest's
// `[permissions.headers]` table.  As a fallback we pass the Api-Key as a
// query parameter where the API accepts it.

/// GET with Api-Key injected as query param (current SDK workaround).
fn http_get_authed(url: &str, cfg: &Config) -> Result<String, String> {
    // Append Api-Key as query parameter — OpenSubtitles accepts this.
    // The runtime host will also inject the header if the manifest says so.
    let sep = if url.contains('?') { '&' } else { '?' };
    let url_keyed = format!("{}{}Api-Key={}", url, sep, cfg.api_key);
    http_get(&url_keyed)
}

/// POST without JWT (used for /login and anonymous /download).
fn http_post_anon(url: &str, body: &str, cfg: &Config) -> Result<String, String> {
    // Embed the Api-Key in the JSON body under a __stui_headers key.
    // The runtime host strips this before forwarding the request and uses
    // it to set the header.  If the host doesn't support this yet, the
    // Api-Key is omitted (login will return 401).
    let augmented = inject_api_key_header(body, &cfg.api_key)?;
    http_post_json(url, &augmented)
}

/// POST with JWT Authorization header.
fn http_post_authed(url: &str, body: &str, cfg: &Config, jwt: &str) -> Result<String, String> {
    let augmented = inject_auth_headers(body, &cfg.api_key, jwt)?;
    http_post_json(url, &augmented)
}

/// Inject `__stui_headers` into a JSON body so the runtime can set headers.
fn inject_api_key_header(body: &str, api_key: &str) -> Result<String, String> {
    let mut obj: serde_json::Value =
        serde_json::from_str(body).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    if let Some(map) = obj.as_object_mut() {
        let mut headers = serde_json::Map::new();
        headers.insert("Api-Key".into(), serde_json::Value::String(api_key.into()));
        headers.insert(
            "User-Agent".into(),
            serde_json::Value::String("stui v0.1.0".into()),
        );
        headers.insert(
            "Content-Type".into(),
            serde_json::Value::String("application/json".into()),
        );
        map.insert("__stui_headers".into(), serde_json::Value::Object(headers));
    }
    serde_json::to_string(&obj).map_err(|e| e.to_string())
}

fn inject_auth_headers(body: &str, api_key: &str, jwt: &str) -> Result<String, String> {
    let mut obj: serde_json::Value =
        serde_json::from_str(body).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
    if let Some(map) = obj.as_object_mut() {
        let mut headers = serde_json::Map::new();
        headers.insert("Api-Key".into(), serde_json::Value::String(api_key.into()));
        headers.insert(
            "User-Agent".into(),
            serde_json::Value::String("stui v0.1.0".into()),
        );
        headers.insert(
            "Content-Type".into(),
            serde_json::Value::String("application/json".into()),
        );
        headers.insert(
            "Authorization".into(),
            serde_json::Value::String(format!("Bearer {}", jwt)),
        );
        map.insert("__stui_headers".into(), serde_json::Value::Object(headers));
    }
    serde_json::to_string(&obj).map_err(|e| e.to_string())
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    api_key: String,
    username: String,
    password: String,
    language: String,
    base_url: String,
}

impl Config {
    fn load() -> Result<Self, String> {
        let api_key = env_or("OS_API_KEY", "");
        let username = env_or("OS_USERNAME", "");
        let password = env_or("OS_PASSWORD", "");
        let language = env_or("OS_LANGUAGE", "en");

        if api_key.is_empty() {
            return Err("OS_API_KEY is not set. \
                 Get one at https://www.opensubtitles.com/en/consumers"
                .into());
        }

        // Use cached base_url (set after successful login) or default
        let base_url = cache_get(BASE_URL_CACHE_KEY)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "api.opensubtitles.com".into());

        Ok(Config {
            api_key,
            username,
            password,
            language,
            base_url,
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn env_or(var: &str, default: &str) -> String {
    let cache_key = format!("__env:{}", var);
    cache_get(&cache_key).unwrap_or_else(|| default.to_string())
}

// ── WASM exports ──────────────────────────────────────────────────────────────

stui_export_plugin!(OpenSubtitlesProvider);
