//! # stui-plugin-sdk
//!
//! The Rust SDK for building stui plugins.
//!
//! ## Quick Start
//!
//! ```ignore
//! use stui_plugin_sdk::*;
//!
//! struct MyPlugin { manifest: PluginManifest }
//!
//! impl Plugin for MyPlugin {
//!     fn manifest(&self) -> &PluginManifest { &self.manifest }
//! }
//!
//! impl CatalogPlugin for MyPlugin {
//!     fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
//!         // ... your search logic ...
//!         Ok(SearchResponse { items: vec![], total: 0 })
//!     }
//!     // lookup / enrich / get_artwork / get_credits / related default to NOT_IMPLEMENTED
//! }
//!
//! stui_export_catalog_plugin!(MyPlugin);
//! ```
//!
//! For non-metadata plugin kinds (streams, subtitles, torrents) use
//! [`stui_export_plugin!`] with the legacy `StuiPlugin` trait — it remains
//! supported during the media-source plugin refactor.
//!
//! ## Compile to WASM
//!
//! ```bash
//! rustup target add wasm32-wasip1
//! cargo build --target wasm32-wasip1 --release
//! # Output: target/wasm32-wasip1/release/my_provider.wasm
//! ```

// ── Modules ─────────────────────────────────────────────────────────────────

pub mod kinds;
pub mod id_sources;
pub mod manifest;
pub mod capabilities;

pub use manifest::{
    PluginManifest, PluginMeta, AuthorMeta,
    Capabilities, CatalogCapability,
    VerbConfig, LookupConfig, ArtworkConfig,
    NetworkPermission, Permissions,
    RateLimit, PluginConfigField,
    ManifestValidationError,
};

/// Parse a plugin's canonical `plugin.toml` text into a [`PluginManifest`].
///
/// Plugins typically call this with `include_str!("../plugin.toml")` inside
/// their `new()` constructor so the manifest is embedded at compile time.
/// Using this helper lets plugins drop their direct `toml` crate dependency
/// — the SDK owns the only `toml::from_str` call site for the canonical
/// manifest schema.
///
/// ```no_run
/// use stui_plugin_sdk::parse_manifest;
/// let manifest = parse_manifest(include_str!("../plugin.toml"))
///     .expect("plugin.toml failed to parse at compile time");
/// ```
pub fn parse_manifest(text: &str) -> Result<PluginManifest, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}
pub use capabilities::{
    InitContext, InitRequest, InitResultEnvelope,
    PluginLogger, DefaultPluginLogger, PluginInitError,
    LookupRequest, LookupResponse,
    EnrichRequest, EnrichResponse,
    ArtworkRequest, ArtworkResponse, ArtworkSize, ArtworkVariant,
    CreditsRequest, CreditsResponse,
    CastMember, CastRole, CrewMember, CrewRole,
    RelatedRequest, RelatedResponse, RelationKind,
    EpisodesRequest, EpisodesResponse, EpisodeWire,
    err_not_implemented, normalize_crew_role,
    validate_manifest,
};

pub mod error_codes {
    //! Stable error-code string constants used in `PluginError::code`.
    //! The runtime matches on these strings, so changing a value is a
    //! wire-breaking change. Canonical form is snake_case to match
    //! the rest of the ABI.

    pub const UNSUPPORTED_SCOPE: &str = "unsupported_scope";
    pub const INVALID_REQUEST:   &str = "invalid_request";
    pub const NOT_IMPLEMENTED:   &str = "not_implemented";
    pub const UNKNOWN_ID:        &str = "unknown_id";
    pub const RATE_LIMITED:      &str = "rate_limited";
    pub const TRANSIENT:         &str = "transient";
    pub const REMOTE_ERROR:      &str = "remote_error";
    pub const PARSE_ERROR:       &str = "parse_error";
}

// ── ABI types (re-exported for plugin authors) ────────────────────────────────

pub const STUI_ABI_VERSION: i32 = 1;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
pub use kinds::{EntryKind, SearchScope};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub scope: SearchScope,
    pub page: u32,
    pub limit: u32,
    #[serde(default)]
    pub per_scope_limit: Option<u32>,
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveRequest {
    pub entry_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub items: Vec<PluginEntry>,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginEntry {
    pub id: String,
    pub kind: EntryKind,
    pub title: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub external_ids: HashMap<String, String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    /// For series entries: the total number of seasons. The episode
    /// browser uses this to populate its season list — without it, the
    /// browser falls back to a single-season default and over-shooting
    /// hits 404s on providers that strict-validate the season number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_count: Option<u32>,
    /// Per-season provider-native ids, parallel to seasons 1..=N. Used
    /// by providers (e.g. AniList) where each season is a SEPARATE
    /// catalog entry rather than a season-numbered slice of one entry.
    /// The TUI maps season N → `season_ids[N-1]` and sends `season=1`
    /// to the plugin (since each id is self-contained).
    ///
    /// Leave empty for TMDB-style providers — the same id serves every
    /// season, with the season number passed through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub season_ids: Vec<String>,

    /// ISO 639-1 code of the entry's original spoken/produced language
    /// (e.g. `"en"`, `"ja"`, `"ko"`). Used by the runtime's post-merge
    /// anime-mix classifier: `genre contains "Animation" AND language == "ja"`
    /// identifies Japanese animation from mainstream providers (TMDB etc).
    /// Anime-dedicated providers (kitsu, anilist) are classified by provider
    /// alone — populating this field is still helpful for future genre/lang
    /// filters but not required for the anime quota to work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_language: Option<String>,

    /// Per-source rating breakdown. Plugins that aggregate multiple
    /// rating providers in a single response (e.g. OMDb returns
    /// IMDb + Rotten Tomatoes + Metacritic in one payload) populate
    /// this map so the runtime's catalog aggregator can compose a
    /// weighted composite score with full provenance. Keys should
    /// match the `key` field of the aggregator's RatingWeight
    /// profiles — e.g. `imdb`, `tomatometer`, `audience_score`,
    /// `metacritic`. Values are stored on whatever native scale the
    /// upstream uses; the aggregator's per-key `normalize` field
    /// scales them to 0-10 at composite time.
    ///
    /// The single `rating` field above remains the plugin's
    /// authoritative "headline" score (typically the best-known or
    /// most-trusted source for that provider) and is what shows on
    /// the card when no per-source breakdown is needed.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub ratings: HashMap<String, f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveResponse {
    pub stream_url: String,
    pub quality: Option<String>,
    pub subtitles: Vec<SubtitleTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub language: String,
    pub url: String,
    pub format: String,
}

// ── StreamProvider verb types ────────────────────────────────────────────────
//
// New in this revision: the StreamProvider capability lets plugins return
// MANY streams per query (vs. the legacy `resolve` verb which returns one
// stream). Torrent indexers (Jackett, Prowlarr) and HTTP-search providers
// (Stremio addons, Torrentio, etc.) need this shape — one search returns
// dozens of release candidates differing in quality / source / seeders.
//
// The trait + ABI are additive — legacy `StuiPlugin::resolve` stays
// supported for plugins that produce a single stream. New stream plugins
// implement `StreamProvider::find_streams` and the runtime prefers it
// when both are advertised.

/// Request asking the plugin to return all stream candidates matching the
/// supplied media reference. Plugins use whatever combination of fields
/// makes sense for their backend — torrent indexers tend to query by
/// `title + year + season + episode`, Stremio addons key off `imdb_id`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FindStreamsRequest {
    /// Title for free-text search backends (jackett/prowlarr/torznab).
    pub title: String,
    /// Year disambiguator. Optional — torrent indexers use it to filter
    /// out reissues / different titles with the same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// What kind of media is being requested. Lets a plugin reject
    /// unsupported kinds (e.g. an audiobook indexer rejecting Movie).
    #[serde(default)]
    pub kind: EntryKind,
    /// Series-only: 1-based season number when querying for an episode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    /// Series-only: 1-based episode number within the season.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<u32>,
    /// External ids carried over from the catalog entry — IMDb is the
    /// most-supported anchor for torrent indexers; some plugins also
    /// recognise tmdb / tvdb / mal.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub external_ids: HashMap<String, String>,
    /// Pre-extracted IMDb id (`tt0xxxxxxx`). Convenience mirror of
    /// `external_ids["imdb"]` so common consumers don't have to look up
    /// the map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    /// Pre-extracted TMDB id (numeric, stringified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindStreamsResponse {
    pub streams: Vec<Stream>,
}

/// One stream candidate returned by a StreamProvider plugin. Designed to
/// carry enough metadata for the runtime's quality/health/policy ranker
/// to score and re-order without going back to the plugin.
///
/// `url` is the playable / fetchable URL — `magnet:?xt=urn:btih:…` for
/// torrents, `https://…` for direct streams. The runtime decides what
/// to do with it based on the URL scheme (`magnet:` → aria2, `https:` →
/// mpv direct, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stream {
    /// Playable URL. Magnet URI for torrents, HTTPS URL for direct streams.
    pub url: String,
    /// Human-readable label for the stream. Convention: release name for
    /// torrents (`The Show S01E02 1080p WEB-DL DDP5.1 H.264-GROUP`),
    /// quality label for direct streams (`1080p`).
    pub title: String,
    /// Provider name for grouping/dedup in the UI. Echo the plugin's
    /// `name` from manifest (e.g. `"jackett"`, `"prowlarr"`).
    pub provider: String,

    // ── Quality metadata (all optional — plugins fill what they can) ──

    /// Resolution / quality bucket as a label. `"4K"`, `"2160p"`,
    /// `"1080p"`, `"720p"`, etc. Drives the ranker's quality score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Encoding codec. `"h264"`, `"h265"`, `"av1"`. Used by the
    /// container-compat checks downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// Source class. `"WEB-DL"`, `"BluRay"`, `"HDTV"`, `"CAM"`. Used by
    /// the ranker's source-quality weighting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// HDR-format presence flag. Plugins that detect Dolby Vision /
    /// HDR10 / HDR10+ in release names set this true.
    #[serde(default)]
    pub hdr: bool,

    // ── Torrent-specific (None for direct streams) ────────────────────

    /// Seeder count from the torrent indexer. The ranker treats high
    /// seeders as a quality signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u32>,
    /// Total payload size in bytes. Used by user policy rules
    /// (`prefer_smaller`, `max_size_gb`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,

    // ── Audio / subtitle metadata ────────────────────────────────────

    /// ISO-639-1 audio language code. Used for "match my preferred
    /// audio language" filtering in the ranker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Embedded subtitle tracks the plugin knows about. External
    /// subtitle providers (opensubtitles, kitsunekko) return their
    /// candidates separately via the SubtitleProvider capability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<SubtitleTrack>,
}

/// StreamProvider capability. Plugins opt into this trait when their
/// manifest declares `[capabilities] streams = true`. The single
/// `find_streams` verb returns every stream candidate the plugin
/// knows about for the given media reference; the runtime aggregates
/// across providers, ranks via the user's policy, and returns to the
/// TUI.
///
/// `find_streams` has a default impl returning NotImplemented so a
/// plugin can opt in via `impl StreamProvider for MyPlugin {}` (no
/// body) and inherit the stub. That keeps the export macros simple —
/// they always emit `stui_find_streams` and never have to detect
/// per-plugin trait impls.
pub trait StreamProvider: Plugin {
    fn find_streams(&self, _req: FindStreamsRequest) -> PluginResult<FindStreamsResponse> {
        err_not_implemented()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PluginResult<T> {
    Ok(T),
    Err(PluginError),
}

impl<T> PluginResult<T> {
    pub fn ok(value: T) -> Self {
        Self::Ok(value)
    }
    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Err(PluginError {
            code: code.into(),
            message: message.into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginType {
    Provider,
    Resolver,
    Metadata,
    Auth,
    Subtitle,
    Indexer,
}

impl PluginType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Provider => "provider",
            Self::Resolver => "resolver",
            Self::Metadata => "metadata",
            Self::Auth => "auth",
            Self::Subtitle => "subtitle",
            Self::Indexer => "indexer",
        }
    }
}

// ── StuiPlugin trait ─────────────────────────────────────────────────────────

/// A legacy trait for non-metadata plugins (streams, subtitles, torrents).
///
/// New plugins should implement [`Plugin`] + [`CatalogPlugin`] instead.
#[deprecated(
    since = "0.2.0",
    note = "Use `Plugin` + `CatalogPlugin` for metadata plugins. Non-metadata use cases (streams, subtitles, torrents) will migrate to dedicated traits in a future refactor; `StuiPlugin` remains supported during that transition."
)]
pub trait StuiPlugin {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn plugin_type(&self) -> PluginType;

    /// Search for content matching `req.query` within the given `req.scope`.
    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse>;

    /// Resolve an entry ID into a playable stream URL.
    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse>;
}

// ── Plugin + CatalogPlugin traits ────────────────────────────────────────────

/// Root trait every plugin implements — identity + lifecycle.
///
/// Only [`Plugin::manifest`] is required; [`Plugin::init`] and
/// [`Plugin::shutdown`] have default no-op implementations.
pub trait Plugin {
    fn manifest(&self) -> &PluginManifest;
    fn init(&mut self, _ctx: &InitContext) -> Result<(), PluginInitError> { Ok(()) }
    fn shutdown(&mut self) -> Result<(), PluginError> { Ok(()) }
}

/// Metadata catalog capability. Plugins opt into this trait when they expose
/// `[capabilities.catalog]` in their manifest. All verbs except `search` are
/// optional; default impls return `NOT_IMPLEMENTED`.
pub trait CatalogPlugin: Plugin {
    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse>;

    fn lookup(&self, _req: LookupRequest) -> PluginResult<LookupResponse>
        { err_not_implemented() }
    fn enrich(&self, _req: EnrichRequest) -> PluginResult<EnrichResponse>
        { err_not_implemented() }
    fn get_artwork(&self, _req: ArtworkRequest) -> PluginResult<ArtworkResponse>
        { err_not_implemented() }
    fn get_credits(&self, _req: CreditsRequest) -> PluginResult<CreditsResponse>
        { err_not_implemented() }
    fn related(&self, _req: RelatedRequest) -> PluginResult<RelatedResponse>
        { err_not_implemented() }
    fn episodes(&self, _req: EpisodesRequest) -> PluginResult<EpisodesResponse>
        { err_not_implemented() }
}

// ── Host function imports (called by plugin at runtime) ───────────────────────

/// Host imports exposed to plugins. All functions are registered by the
/// runtime under the dedicated `stui` WASM import module so they don't
/// collide with WASI's `env` namespace.
///
/// Use the `log!` / `info!` / `warn!` macros instead of calling `stui_log`
/// directly.
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "stui")]
extern "C" {
    pub fn stui_log(level: i32, ptr: *const u8, len: i32);
    pub fn stui_http_get(url_ptr: *const u8, url_len: i32) -> i64;
    pub fn stui_cache_get(key_ptr: *const u8, key_len: i32) -> i64;
    pub fn stui_cache_set(key_ptr: *const u8, key_len: i32, val_ptr: *const u8, val_len: i32);
    pub fn stui_auth_allocate_port() -> i32;
    pub fn stui_auth_open_and_wait(url_ptr: *const u8, url_len: i32, timeout_ms: i32) -> i64;
    pub fn stui_exec(cmd_ptr: *const u8, cmd_len: i32, timeout_ms: i32) -> i64;
}

/// Log a message at the given level through the host logger.
pub fn host_log(level: i32, msg: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        stui_log(level, msg.as_ptr(), msg.len() as i32);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("[stui-plugin level={}] {}", level, msg);
    }
}

/// Convenience macros for logging from plugins.
#[macro_export]
macro_rules! plugin_info  { ($($t:tt)*) => { $crate::host_log(2, &format!($($t)*)) }; }
#[macro_export]
macro_rules! plugin_warn  { ($($t:tt)*) => { $crate::host_log(3, &format!($($t)*)) }; }
#[macro_export]
macro_rules! plugin_error { ($($t:tt)*) => { $crate::host_log(4, &format!($($t)*)) }; }
#[macro_export]
macro_rules! plugin_debug { ($($t:tt)*) => { $crate::host_log(1, &format!($($t)*)) }; }

/// Strip sensitive query parameters from a URL so it's safe to log.
///
/// Replaces the *value* of any query parameter whose key matches one of
/// the well-known auth names (`api_key`, `apikey`, `key`, `token`,
/// `access_token`, `secret`) with `***`. All other query params are
/// preserved. Fragments and paths are untouched.
///
/// Use this in `plugin_info!` / `plugin_debug!` calls that would
/// otherwise embed the full authenticated URL — even info-level logs
/// can end up in crash reports, user bug submissions, or terminal
/// scrollback.
///
/// ```
/// use stui_plugin_sdk::log_url;
/// let safe = log_url("https://api.example.com/x?query=matrix&api_key=deadbeef");
/// assert_eq!(safe, "https://api.example.com/x?query=matrix&api_key=***");
/// ```
pub fn log_url(url: &str) -> String {
    const SENSITIVE: &[&str] = &[
        "api_key", "apikey", "key", "token", "access_token", "secret",
    ];
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    // Split off fragment so we reattach it at the end unchanged.
    let (query, fragment) = match query.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None         => (query, None),
    };
    let scrubbed: Vec<String> = query
        .split('&')
        .map(|kv| match kv.split_once('=') {
            Some((k, _)) if SENSITIVE.iter().any(|s| k.eq_ignore_ascii_case(s)) => {
                format!("{k}=***")
            }
            _ => kv.to_string(),
        })
        .collect();
    let joined = scrubbed.join("&");
    match fragment {
        Some(f) => format!("{base}?{joined}#{f}"),
        None    => format!("{base}?{joined}"),
    }
}

#[cfg(test)]
mod log_url_tests {
    use super::log_url;

    #[test]
    fn strips_api_key() {
        assert_eq!(
            log_url("https://api.tmdb.org/3/search/movie?query=matrix&api_key=deadbeef"),
            "https://api.tmdb.org/3/search/movie?query=matrix&api_key=***",
        );
    }

    #[test]
    fn preserves_innocuous_params() {
        assert_eq!(
            log_url("https://x.example/y?page=2&limit=5"),
            "https://x.example/y?page=2&limit=5",
        );
    }

    #[test]
    fn handles_multiple_sensitive_keys() {
        let out = log_url("https://a?apikey=A&token=B&other=ok&access_token=C");
        assert!(out.contains("apikey=***"));
        assert!(out.contains("token=***"));
        assert!(out.contains("access_token=***"));
        assert!(out.contains("other=ok"));
    }

    #[test]
    fn case_insensitive_key_match() {
        assert_eq!(
            log_url("https://a?API_KEY=X"),
            "https://a?API_KEY=***",
        );
    }

    #[test]
    fn preserves_fragment() {
        assert_eq!(
            log_url("https://a/b?api_key=X#section"),
            "https://a/b?api_key=***#section",
        );
    }

    #[test]
    fn no_query_is_passthrough() {
        assert_eq!(log_url("https://a/b"), "https://a/b");
    }
}

/// Strip HTML markup and source-attribution suffixes from a provider
/// description.
///
/// Targets the AniList / Kitsu shape:
///
/// ```text
/// <i>Note: Mahō Shōjo no Sekai…</i><br><br>
/// In a world where magical girls...<br>
/// (Source: Crunchyroll)
/// ```
///
/// The pipeline:
/// 1. `<br>` / `<br/>` / `<br />` (any case) → `\n`.
/// 2. Strip every other HTML tag (greedy `<…>`).
/// 3. Decode the small set of common HTML entities (`&amp;`, `&lt;`,
///    `&gt;`, `&quot;`, `&#39;`, `&apos;`, `&nbsp;`).
/// 4. Remove a trailing `(Source: …)` clause when it sits in the final
///    ~200 chars and is single-line — that's how AniList writes attributions.
/// 5. Collapse runs of three+ blank lines to two and trim outer whitespace.
///
/// Idempotent on already-clean text, so providers can apply it
/// unconditionally on every description without checking first.
pub fn clean_description(s: &str) -> String {
    let mut out = s.to_string();

    // <br> variants → newline.
    for tag in &["<br>", "<br/>", "<br />", "<BR>", "<BR/>", "<BR />"] {
        out = out.replace(tag, "\n");
    }

    // Strip remaining HTML tags. Greedy `<…>` consumer; doesn't validate
    // the tag, just discards everything between angle brackets so stray
    // `<` in the text body is preserved intact.
    let stripped: String = {
        let mut buf = String::with_capacity(out.len());
        let mut in_tag = false;
        for ch in out.chars() {
            match ch {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => buf.push(ch),
                _ => {}
            }
        }
        buf
    };
    out = stripped;

    // HTML entities (small fixed set — full decoding would pull a
    // dependency we don't want in plugin wasms).
    out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Strip trailing "(Source: …)" attribution.
    if let Some(idx) = find_trailing_source_attribution(&out) {
        out.truncate(idx);
    }

    // Collapse 3+ consecutive newlines down to 2.
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }

    out.trim().to_string()
}

/// Find the byte index of a trailing `(Source: …)` clause.  Case-insensitive
/// match on `(source` followed by any non-newline up to `)`. Returns
/// `None` when the attribution is absent or appears too far from the end
/// to plausibly be the suffix.
fn find_trailing_source_attribution(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let needle = b"(source";
    if bytes.len() < needle.len() {
        return None;
    }
    // Walk right-to-left so we land on the LAST occurrence — most
    // descriptions only have one, but a wiki-style citation could
    // include earlier "(Source:" mentions in passing.
    for i in (0..=bytes.len() - needle.len()).rev() {
        let matches = needle
            .iter()
            .enumerate()
            .all(|(j, &n)| bytes[i + j].to_ascii_lowercase() == n);
        if !matches {
            continue;
        }
        let rest = &s[i..];
        // Heuristics: the attribution is short (≤200 chars) and never
        // wraps to a new line.  Anything else is likely a legitimate
        // mention of the word "source" within the description body.
        if rest.len() <= 200 && !rest.contains('\n') {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod clean_description_tests {
    use super::clean_description;

    #[test]
    fn br_becomes_newline() {
        let s = clean_description("Line one<br>Line two<br/>Line three");
        assert_eq!(s, "Line one\nLine two\nLine three");
    }

    #[test]
    fn strips_arbitrary_html_tags() {
        let s = clean_description("Hello <i>italic</i> and <b>bold</b>.");
        assert_eq!(s, "Hello italic and bold.");
    }

    #[test]
    fn strips_trailing_source_attribution() {
        let s = clean_description("A long synopsis here. (Source: Wikipedia)");
        assert_eq!(s, "A long synopsis here.");
    }

    #[test]
    fn keeps_word_source_when_not_attribution() {
        let s = clean_description("The protagonist is the source of all chaos.");
        assert_eq!(s, "The protagonist is the source of all chaos.");
    }

    #[test]
    fn decodes_entities() {
        let s = clean_description("Tom &amp; Jerry &quot;chase&quot; the cat&#39;s tail");
        assert_eq!(s, "Tom & Jerry \"chase\" the cat's tail");
    }

    #[test]
    fn collapses_excess_blank_lines() {
        let s = clean_description("Para 1.<br><br><br><br>Para 2.");
        assert_eq!(s, "Para 1.\n\nPara 2.");
    }

    #[test]
    fn idempotent_on_clean_text() {
        let already_clean = "A perfectly normal description.";
        assert_eq!(clean_description(already_clean), already_clean);
    }

    #[test]
    fn handles_empty_input() {
        assert_eq!(clean_description(""), "");
    }
}

/// Make an HTTP GET request through the sandboxed host.
/// Returns the response body as a String, or an error message.
pub fn http_get(url: &str) -> Result<String, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let packed = unsafe { stui_http_get(url.as_ptr(), url.len() as i32) };
        if packed == 0 {
            return Err("http_get returned null".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| e.to_string())?;
        let resp: crate::HttpResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if resp.status >= 200 && resp.status < 300 {
            Ok(resp.body)
        } else {
            Err(format!("HTTP {}: {}", resp.status, resp.body))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Host-side tests route through `sdk::testing::MockHost` fixtures
        // when any are registered. Unrecognised URLs fall through to the
        // "no fixture" error so forgotten registrations are loud.
        if let Some(body) = crate::testing::try_fixture(url) {
            return Ok(body);
        }
        Err(format!(
            "http_get only available in WASM context (url: {url})"
        ))
    }
}

// ── Host-side test harness ────────────────────────────────────────────────────

/// Host-side test utilities — fixture registration for the SDK's HTTP
/// helpers so plugin authors can unit-test verb dispatch without a live
/// upstream API.
///
/// ```
/// use stui_plugin_sdk::{http_get, testing::MockHost};
///
/// let _host = MockHost::new()
///     .with_fixture_response(
///         "https://api.example.com/x?query=inception",
///         r#"{"results":[{"id":1,"title":"Inception"}]}"#,
///     );
/// let body = http_get("https://api.example.com/x?query=inception").unwrap();
/// assert!(body.contains("\"Inception\""));
/// ```
///
/// Fixtures are stored in a thread-local, so tests in the same thread
/// share state — drop or `.reset()` between cases if needed. The
/// `MockHost` value itself is a thin handle; holding or dropping it does
/// NOT clear fixtures (so fluent builders in test helpers work as
/// expected).
pub mod testing {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static FIXTURES: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }

    /// Handle for registering canned HTTP responses. See module doc.
    pub struct MockHost;

    impl Default for MockHost {
        fn default() -> Self { Self::new() }
    }

    impl MockHost {
        /// Build a fresh handle; existing fixtures in the thread-local are
        /// preserved. Call [`MockHost::reset`] first if you want a clean
        /// slate.
        pub fn new() -> Self { MockHost }

        /// Register a canned JSON response for a given URL (exact match).
        /// Returns `self` so calls can be chained.
        pub fn with_fixture_response(
            self,
            url: impl Into<String>,
            body: impl Into<String>,
        ) -> Self {
            FIXTURES.with(|m| {
                m.borrow_mut().insert(url.into(), body.into());
            });
            self
        }

        /// Clear every registered fixture on the current thread. Intended
        /// for test tear-down; omit if your tests run on fresh threads or
        /// only register once per case.
        pub fn reset() {
            FIXTURES.with(|m| m.borrow_mut().clear());
        }
    }

    /// Internal hook: [`crate::http_get`] on non-WASM targets consults
    /// this to resolve fixtures before returning its "no live host" error.
    pub(crate) fn try_fixture(url: &str) -> Option<String> {
        FIXTURES.with(|m| m.borrow().get(url).cloned())
    }
}

#[cfg(test)]
mod mockhost_tests {
    use super::http_get;
    use super::testing::MockHost;

    fn reset() { MockHost::reset(); }

    #[test]
    fn fixture_satisfies_http_get() {
        reset();
        let _ = MockHost::new().with_fixture_response("https://a/x", r#"{"k":"v"}"#);
        assert_eq!(http_get("https://a/x").unwrap(), r#"{"k":"v"}"#);
    }

    #[test]
    fn unregistered_url_still_errors() {
        reset();
        let err = http_get("https://never-registered.example").unwrap_err();
        assert!(err.contains("only available in WASM"));
    }

    #[test]
    fn multiple_fixtures_chain() {
        reset();
        let _ = MockHost::new()
            .with_fixture_response("https://a", "A")
            .with_fixture_response("https://b", "B");
        assert_eq!(http_get("https://a").unwrap(), "A");
        assert_eq!(http_get("https://b").unwrap(), "B");
    }

    #[test]
    fn reset_clears_everything() {
        reset();
        let _ = MockHost::new().with_fixture_response("https://x", "body");
        assert!(http_get("https://x").is_ok());
        MockHost::reset();
        assert!(http_get("https://x").is_err());
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)] // fields only read inside #[cfg(target_arch = "wasm32")] blocks
struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Make an HTTP GET request with custom headers through the sandboxed host.
///
/// Mirrors `http_get` but lets the plugin attach arbitrary headers — most
/// commonly `Cookie:` for session-authenticated trackers (RuTracker,
/// Zamunda, BT.etree) and `User-Agent:` overrides for backends that
/// reject the default agent.
///
/// Headers are passed as `&[(name, value)]`. The host applies them
/// verbatim; no validation beyond what reqwest does.
///
/// Returns the response body as a String on 2xx, or an Err with the status+body.
pub fn http_get_with_headers(url: &str, headers: &[(&str, &str)]) -> Result<String, String> {
    // Encode {"url": "...", "__stui_headers": {"k": "v", ...}} for the
    // host. The host strips __stui_headers and applies them as real
    // request headers.
    let mut headers_json = String::from("{");
    for (i, (k, v)) in headers.iter().enumerate() {
        if i > 0 { headers_json.push(','); }
        headers_json.push_str(&serde_json::to_string(k).unwrap_or_default());
        headers_json.push(':');
        headers_json.push_str(&serde_json::to_string(v).unwrap_or_default());
    }
    headers_json.push('}');

    let payload = format!(
        "{{\"url\":{},\"__stui_headers\":{}}}",
        serde_json::to_string(url).unwrap_or_default(),
        headers_json,
    );
    #[cfg(target_arch = "wasm32")]
    {
        #[link(wasm_import_module = "stui")]
        extern "C" {
            fn stui_http_get_with_headers(ptr: *const u8, len: i32) -> i64;
        }
        let packed = unsafe { stui_http_get_with_headers(payload.as_ptr(), payload.len() as i32) };
        if packed == 0 {
            return Err("http_get_with_headers returned null".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| e.to_string())?;
        let resp: HttpResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if resp.status >= 200 && resp.status < 300 {
            Ok(resp.body)
        } else {
            Err(format!("HTTP {}: {}", resp.status, resp.body))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = payload;
        Err(format!(
            "http_get_with_headers only available in WASM context (url: {url})"
        ))
    }
}

/// Make an HTTP POST request with a JSON body through the sandboxed host.
///
/// The host function `stui_http_post` takes the URL and the JSON payload.
/// Internally the host adds any required CORS/auth headers from the plugin
/// manifest's `network_permissions` list.
///
/// Returns the response body as a String on 2xx, or an Err with the status+body.
pub fn http_post_json(url: &str, body: &str) -> Result<String, String> {
    // Encode request as a single JSON object the host can parse.
    // Format: {"url":"...","body":"..."}
    let payload = format!(
        "{{\"url\":{},\"body\":{}}}",
        serde_json::to_string(url).unwrap_or_default(),
        serde_json::to_string(body).unwrap_or_default(),
    );
    #[cfg(target_arch = "wasm32")]
    {
        #[link(wasm_import_module = "stui")]
        extern "C" {
            fn stui_http_post(ptr: *const u8, len: i32) -> i64;
        }
        let packed = unsafe { stui_http_post(payload.as_ptr(), payload.len() as i32) };
        if packed == 0 {
            return Err("http_post returned null".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| e.to_string())?;
        let resp: HttpResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if resp.status >= 200 && resp.status < 300 {
            Ok(resp.body)
        } else {
            Err(format!("HTTP {}: {}", resp.status, resp.body))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = payload;
        Err(format!(
            "http_post only available in WASM context (url: {url})"
        ))
    }
}

/// Retrieve a value from the host-managed key-value cache.
/// Returns None if the key is missing or expired.
pub fn cache_get(key: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let packed = unsafe { stui_cache_get(key.as_ptr(), key.len() as i32) };
        if packed == 0 {
            return None;
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        let s = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }.ok()?;
        Some(s.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        None
    }
}

/// Store a value in the host-managed key-value cache.
/// The cache is persistent across plugin calls within a session.
pub fn cache_set(key: &str, value: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        stui_cache_set(
            key.as_ptr(),
            key.len() as i32,
            value.as_ptr(),
            value.len() as i32,
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!(
            "[stui-plugin cache_set] key={key} value_len={}",
            value.len()
        );
    }
}

/// Percent-encode a string for use in URLs (RFC 3986).
/// Spaces are encoded as %20 (not +).
pub fn url_encode(s: &str) -> String {
    let mut encoded = String::new();
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => encoded.push(c),
            ' ' => encoded.push_str("%20"),
            _ => {
                for byte in c.to_string().as_bytes() {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    encoded
}

// ── OAuth helpers ──────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct OAuthCallback {
    pub code: String,
    pub state: Option<String>,
}

/// Parse the JSON blob returned by `stui_auth_open_and_wait`.
///
/// `code: Some` → `Ok(OAuthCallback)`.
/// `error: Some("timed_out")` → `Err("timed_out")`.
/// `error: Some("denied")` → `Err("denied: <message>")`.
/// `error: Some(other)` → `Err("denied: <other>")`.
/// Both absent (malformed) → `Err("timed_out")` as safe fallback.
pub fn parse_auth_json(json: &str) -> Result<OAuthCallback, String> {
    let val: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("timed_out (parse error: {e})"))?;
    if let Some(code) = val["code"].as_str().filter(|s| !s.is_empty()) {
        return Ok(OAuthCallback {
            code: code.to_string(),
            state: val["state"].as_str().map(|s| s.to_string()),
        });
    }
    match val["error"].as_str() {
        Some("timed_out") => Err("timed_out".into()),
        Some("denied") => {
            let msg = val["message"].as_str().unwrap_or("unknown");
            Err(format!("denied: {msg}"))
        }
        Some(e) => Err(format!("denied: {e}")),
        None => Err("timed_out".into()),
    }
}

pub fn auth_allocate_port() -> Result<u16, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let port = unsafe { stui_auth_allocate_port() };
        if port < 0 {
            return Err("port_allocation_failed".into());
        }
        Ok(port as u16)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Err("auth_allocate_port only available in WASM context".into())
    }
}

pub fn auth_open_and_wait(url: &str, timeout_ms: u32) -> Result<OAuthCallback, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let t_ms = timeout_ms.min(i32::MAX as u32) as i32;
        let packed = unsafe { stui_auth_open_and_wait(url.as_ptr(), url.len() as i32, t_ms) };
        if packed == 0 {
            return Err("timed_out".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        // Memory is NOT freed — matches established sdk pattern (http_get, cache_get)
        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| format!("timed_out (utf8 error: {e})"))?;
        parse_auth_json(json)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (url, timeout_ms);
        Err("auth_open_and_wait only available in WASM context".into())
    }
}

pub fn http_post_form(url: &str, body: &str) -> Result<String, String> {
    let payload = format!(
        "{{\"url\":{url_json},\"body\":{body_json},\"__stui_headers\":{{\"Content-Type\":\"application/x-www-form-urlencoded\"}}}}",
        url_json  = serde_json::to_string(url).unwrap_or_default(),
        body_json = serde_json::to_string(body).unwrap_or_default(),
    );
    #[cfg(target_arch = "wasm32")]
    {
        #[link(wasm_import_module = "stui")]
        extern "C" {
            fn stui_http_post(ptr: *const u8, len: i32) -> i64;
        }
        let packed = unsafe { stui_http_post(payload.as_ptr(), payload.len() as i32) };
        if packed == 0 {
            return Err("http_post_form returned null".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| e.to_string())?;
        let resp: HttpResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if resp.status >= 200 && resp.status < 300 {
            Ok(resp.body)
        } else {
            Err(format!("HTTP {}: {}", resp.status, resp.body))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = payload;
        Err(format!(
            "http_post_form only available in WASM context (url: {url})"
        ))
    }
}

/// Execute an external command and return its stdout.
///
/// The `cmd` should be a JSON object with the command and arguments:
/// ```json
/// {"cmd": "yt-dlp", "args": ["--flat-playlist", "-j", "https://soundcloud.com/search?q=test"]}
/// ```
///
/// Returns stdout on success, or an error message on failure.
/// Timeout is in milliseconds.
pub fn exec(cmd: &str, args: &[&str], timeout_ms: u32) -> Result<String, String> {
    let payload = format!(
        "{{\"cmd\":{},\"args\":{},\"timeout_ms\":{}}}",
        serde_json::to_string(cmd).unwrap_or_default(),
        serde_json::to_string(&args).unwrap_or_default(),
        timeout_ms
    );
    #[cfg(target_arch = "wasm32")]
    {
        #[link(wasm_import_module = "stui")]
        extern "C" {
            fn stui_exec(ptr: *const u8, len: i32, timeout_ms: i32) -> i64;
        }
        let packed =
            unsafe { stui_exec(payload.as_ptr(), payload.len() as i32, timeout_ms as i32) };
        if packed == 0 {
            return Err("stui_exec returned null".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;
        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| e.to_string())?;
        let resp: ExecResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if resp.status == 0 {
            Ok(resp.stdout)
        } else {
            Err(resp.stderr)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (cmd, args, timeout_ms, payload);
        Err("exec only available in WASM context".into())
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)] // fields only read inside #[cfg(target_arch = "wasm32")] blocks
struct ExecResponse {
    status: i32,
    stdout: String,
    stderr: String,
}

// ── WASM-only plugin instance cell ────────────────────────────────────────────

/// Single-threaded plugin instance storage used by `stui_export_plugin!` /
/// `stui_export_catalog_plugin!` on `wasm32-wasip1`.
///
/// The export macros previously wrapped the plugin in
/// `OnceLock<std::sync::Mutex<T>>` purely to satisfy the `Sync` bound on
/// statics. On `wasm32-wasip1` that path is actively harmful: the
/// `no_threads.rs` mutex impl asserts on recursive lock attempts, and
/// because wasm panics never run drop handlers, *any* verb panic leaves
/// the mutex permanently locked — every subsequent verb call then traps
/// with the misleading "cannot recursively acquire mutex" message,
/// hiding the real underlying panic.
///
/// This cell drops the mutex entirely on wasm and lets verb dispatch hand
/// out `&T` / `&mut T` directly. The `unsafe impl Sync` is sound because
/// WASI plugins run single-threaded and the host supervisor serializes
/// every verb call, so no two references can coexist.
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub struct WasmPluginCell<T>(::core::cell::UnsafeCell<Option<T>>);

#[cfg(target_arch = "wasm32")]
unsafe impl<T> ::core::marker::Sync for WasmPluginCell<T> {}

#[cfg(target_arch = "wasm32")]
impl<T: Default> WasmPluginCell<T> {
    pub const fn new() -> Self {
        Self(::core::cell::UnsafeCell::new(None))
    }

    /// Get an exclusive reference, lazily creating `T::default()` on first call.
    ///
    /// # Safety
    /// Caller must guarantee no other reference into the cell exists at
    /// the same time. The export macros call this only from wasm ABI
    /// entry points, which the host supervisor serializes — single-threaded
    /// WASI satisfies the requirement.
    pub unsafe fn borrow_mut(&self) -> &mut T {
        let opt = unsafe { &mut *self.0.get() };
        opt.get_or_insert_with(T::default)
    }
}

// ── ABI glue macro ────────────────────────────────────────────────────────────

/// Internal; always invoked by `stui_export_plugin!` / `stui_export_catalog_plugin!` — do not call directly.
///
/// Expands to a `#[no_mangle] pub extern "C" fn $fn_name(ptr: i32, len: i32) -> i64`
/// that deserialises `$req_ty` from the input buffer, dispatches via
/// `<$plugin_ty as $crate::CatalogPlugin>::$method`, and serialises the result.
/// `$resp_ty` is the `Ok`-variant type, used in the parse-error path so that
/// `PluginResult::<$resp_ty>::err(...)` resolves without ambiguity.
///
/// `$getter` must return a shared reference (`&$plugin_ty`) — all verb calls
/// take `&self`, so the singleton is borrowed immutably here.
#[doc(hidden)]
#[macro_export]
macro_rules! __catalog_abi_fn {
    (
        plugin   = $plugin_ty:ty,
        getter   = $getter:expr,
        fn_name  = $fn_name:ident,
        method   = $method:ident,
        req_ty   = $req_ty:ty,
        resp_ty  = $resp_ty:ty,
    ) => {
        #[no_mangle]
        pub extern "C" fn $fn_name(ptr: i32, len: i32) -> i64 {
            let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let req: $req_ty = match serde_json::from_slice(input) {
                Ok(r) => r,
                Err(e) => {
                    return $crate::__write_result(
                        &$crate::PluginResult::<$resp_ty>::err(
                            $crate::error_codes::PARSE_ERROR,
                            e.to_string(),
                        ),
                    );
                }
            };
            let borrow = $getter();
            let result = <$plugin_ty as $crate::CatalogPlugin>::$method(&*borrow, req);
            $crate::__write_result(&result)
        }
    };
}

/// Legacy — use [`stui_export_catalog_plugin!`] for metadata (CatalogPlugin) plugins.
///
/// Registers a legacy `StuiPlugin` plugin and generates all required WASM ABI exports,
/// including `stui_resolve` which requires the deprecated [`StuiPlugin`] trait. Use
/// this macro only for non-metadata plugin kinds (streams, subtitles, torrents) during
/// the transition period.
///
/// # Example
/// ```rust
/// stui_export_plugin!(MyProvider);
/// ```
///
/// This generates:
/// - `stui_abi_version() -> i32`
/// - `stui_alloc(len: i32) -> i32`
/// - `stui_free(ptr: i32, len: i32)`
/// - `stui_search(ptr: i32, len: i32) -> i64`
/// - `stui_lookup(ptr: i32, len: i32) -> i64`
/// - `stui_enrich(ptr: i32, len: i32) -> i64`
/// - `stui_get_artwork(ptr: i32, len: i32) -> i64`
/// - `stui_get_credits(ptr: i32, len: i32) -> i64`
/// - `stui_related(ptr: i32, len: i32) -> i64`
/// - `stui_resolve(ptr: i32, len: i32) -> i64`  *(legacy StuiPlugin path)*
#[macro_export]
macro_rules! stui_export_plugin {
    ($plugin_ty:ty) => {
        // Plugin instance storage. Two paths:
        //
        // * `wasm32-wasip1` — uses `WasmPluginCell` (no lock). See its docs
        //   for the rationale; tl;dr: the `Mutex<T>` we used to wrap this
        //   was both pointless (single-threaded runtime) and dangerous
        //   (any verb panic left the mutex permanently locked because wasm
        //   panics never run drop handlers).
        //
        // * Host targets — keeps `OnceLock<Mutex<T>>` so the existing
        //   host-side test infrastructure compiles and the static
        //   continues to satisfy the `Sync` bound.
        #[cfg(target_arch = "wasm32")]
        static PLUGIN_INSTANCE: $crate::WasmPluginCell<$plugin_ty> =
            $crate::WasmPluginCell::new();

        #[cfg(not(target_arch = "wasm32"))]
        static PLUGIN_INSTANCE: std::sync::OnceLock<std::sync::Mutex<$plugin_ty>> =
            std::sync::OnceLock::new();

        #[cfg(target_arch = "wasm32")]
        fn get_plugin() -> &'static $plugin_ty {
            // SAFETY: WASI is single-threaded and the host supervisor
            // serializes every verb call into the same instance, so no
            // two references into PLUGIN_INSTANCE can coexist.
            unsafe { PLUGIN_INSTANCE.borrow_mut() }
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn get_plugin() -> std::sync::MutexGuard<'static, $plugin_ty> {
            PLUGIN_INSTANCE
                .get_or_init(|| std::sync::Mutex::new(<$plugin_ty>::default()))
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        }

        /// ABI version — host checks this before calling any other function.
        #[no_mangle]
        pub extern "C" fn stui_abi_version() -> i32 {
            $crate::STUI_ABI_VERSION
        }

        /// Memory allocation — host uses this to write request JSON.
        #[no_mangle]
        pub extern "C" fn stui_alloc(len: i32) -> i32 {
            let mut buf = Vec::<u8>::with_capacity(len as usize);
            let ptr = buf.as_mut_ptr() as i32;
            std::mem::forget(buf);
            ptr
        }

        /// Memory free — host calls this after reading response JSON.
        #[no_mangle]
        pub extern "C" fn stui_free(ptr: i32, len: i32) {
            unsafe {
                let _ = Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize);
            }
        }

        // ── CatalogPlugin verb exports ────────────────────────────────────────

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_search,
            method   = search,
            req_ty   = $crate::SearchRequest,
            resp_ty  = $crate::SearchResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_lookup,
            method   = lookup,
            req_ty   = $crate::LookupRequest,
            resp_ty  = $crate::LookupResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_enrich,
            method   = enrich,
            req_ty   = $crate::EnrichRequest,
            resp_ty  = $crate::EnrichResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_get_artwork,
            method   = get_artwork,
            req_ty   = $crate::ArtworkRequest,
            resp_ty  = $crate::ArtworkResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_get_credits,
            method   = get_credits,
            req_ty   = $crate::CreditsRequest,
            resp_ty  = $crate::CreditsResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_related,
            method   = related,
            req_ty   = $crate::RelatedRequest,
            resp_ty  = $crate::RelatedResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_episodes,
            method   = episodes,
            req_ty   = $crate::EpisodesRequest,
            resp_ty  = $crate::EpisodesResponse,
        }

        // ── Legacy StuiPlugin resolve export (untouched) ──────────────────────

        /// Resolve entry point. Input: ResolveRequest JSON. Output: packed (ptr<<32)|len.
        #[no_mangle]
        pub extern "C" fn stui_resolve(ptr: i32, len: i32) -> i64 {
            let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let req: $crate::ResolveRequest = match serde_json::from_slice(input) {
                Ok(r) => r,
                Err(e) => {
                    return $crate::__write_result(
                        &$crate::PluginResult::<$crate::ResolveResponse>::err(
                            $crate::error_codes::PARSE_ERROR,
                            e.to_string(),
                        ),
                    )
                }
            };
            let borrow = get_plugin();
            #[allow(deprecated)]
            let result = <$plugin_ty as $crate::StuiPlugin>::resolve(&*borrow, req);
            $crate::__write_result(&result)
        }
    };
}

/// Export a metadata (CatalogPlugin) plugin to WASM.
///
/// Emits the standard ABI entry points and FFI wrappers for all 6 CatalogPlugin verbs
/// (`stui_search`, `stui_lookup`, `stui_enrich`, `stui_get_artwork`, `stui_get_credits`,
/// `stui_related`) plus `stui_abi_version`, `stui_alloc`, `stui_free`.
///
/// Use [`stui_export_plugin!`] instead for legacy `StuiPlugin` plugins (non-metadata
/// kinds like streams / subtitles / torrents during the transition).
///
/// # Example
/// ```rust
/// stui_export_catalog_plugin!(MyPlugin);
/// ```
///
/// This generates:
/// - `stui_abi_version() -> i32`
/// - `stui_alloc(len: i32) -> i32`
/// - `stui_free(ptr: i32, len: i32)`
/// - `stui_search(ptr: i32, len: i32) -> i64`
/// - `stui_lookup(ptr: i32, len: i32) -> i64`
/// - `stui_enrich(ptr: i32, len: i32) -> i64`
/// - `stui_get_artwork(ptr: i32, len: i32) -> i64`
/// - `stui_get_credits(ptr: i32, len: i32) -> i64`
/// - `stui_related(ptr: i32, len: i32) -> i64`
///
/// Unlike [`stui_export_plugin!`], this macro does NOT emit `stui_resolve`, so the
/// plugin type does not need to implement the deprecated [`StuiPlugin`] trait.
#[macro_export]
macro_rules! stui_export_catalog_plugin {
    ($plugin_ty:ty) => {
        // Plugin instance storage. See `WasmPluginCell` docs for why the
        // wasm path drops the mutex; in short, wasm panics never run drop,
        // so a `std::sync::Mutex` here would stay locked forever after any
        // verb panic and trap every subsequent call with the misleading
        // "cannot recursively acquire mutex" message. Host builds keep the
        // mutex so the existing test infrastructure compiles.
        #[cfg(target_arch = "wasm32")]
        static PLUGIN_INSTANCE: $crate::WasmPluginCell<$plugin_ty> =
            $crate::WasmPluginCell::new();

        #[cfg(not(target_arch = "wasm32"))]
        static PLUGIN_INSTANCE: std::sync::OnceLock<std::sync::Mutex<$plugin_ty>> =
            std::sync::OnceLock::new();

        #[cfg(not(target_arch = "wasm32"))]
        fn __plugin_cell() -> &'static std::sync::Mutex<$plugin_ty> {
            PLUGIN_INSTANCE.get_or_init(|| std::sync::Mutex::new(<$plugin_ty>::default()))
        }

        #[cfg(target_arch = "wasm32")]
        fn get_plugin() -> &'static $plugin_ty {
            // SAFETY: WASI is single-threaded and the host supervisor
            // serializes every verb call, so no two references into
            // PLUGIN_INSTANCE can coexist.
            unsafe { PLUGIN_INSTANCE.borrow_mut() }
        }

        #[cfg(not(target_arch = "wasm32"))]
        fn get_plugin() -> std::sync::MutexGuard<'static, $plugin_ty> {
            __plugin_cell().lock().unwrap_or_else(|p| p.into_inner())
        }

        /// ABI version — host checks this before calling any other function.
        #[no_mangle]
        pub extern "C" fn stui_abi_version() -> i32 {
            $crate::STUI_ABI_VERSION
        }

        /// Memory allocation — host uses this to write request JSON.
        #[no_mangle]
        pub extern "C" fn stui_alloc(len: i32) -> i32 {
            let mut buf = Vec::<u8>::with_capacity(len as usize);
            let ptr = buf.as_mut_ptr() as i32;
            std::mem::forget(buf);
            ptr
        }

        /// Memory free — host calls this after reading response JSON.
        #[no_mangle]
        pub extern "C" fn stui_free(ptr: i32, len: i32) {
            unsafe {
                let _ = Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize);
            }
        }

        // ── Plugin::init export ───────────────────────────────────────────────

        /// Init entry point. Input: InitRequest JSON. Output: packed (ptr<<32)|len
        /// of an `InitResultEnvelope` JSON.
        ///
        /// The host calls this once after instantiation and translates the
        /// response into a `PluginStatus` (Loaded / NeedsConfig / Failed).
        #[no_mangle]
        pub extern "C" fn stui_init(ptr: i32, len: i32) -> i64 {
            let input = unsafe {
                std::slice::from_raw_parts(ptr as *const u8, len as usize)
            };
            let req: $crate::InitRequest = match serde_json::from_slice(input) {
                Ok(r) => r,
                Err(e) => {
                    let env: $crate::InitResultEnvelope = $crate::InitResultEnvelope::Err(
                        $crate::PluginInitError::Fatal(format!("init request parse error: {e}")),
                    );
                    return $crate::__write_result(&env);
                }
            };
            let logger = $crate::DefaultPluginLogger;
            let ctx = $crate::InitContext::from_request(&req, &logger);
            #[cfg(target_arch = "wasm32")]
            let result = {
                // SAFETY: stui_init is the first call into the plugin and runs
                // before any verb dispatch, so no other reference exists.
                let inst: &mut $plugin_ty = unsafe { PLUGIN_INSTANCE.borrow_mut() };
                <$plugin_ty as $crate::Plugin>::init(inst, &ctx)
            };
            #[cfg(not(target_arch = "wasm32"))]
            let result = {
                let mut inst = __plugin_cell().lock().unwrap_or_else(|p| p.into_inner());
                <$plugin_ty as $crate::Plugin>::init(&mut *inst, &ctx)
            };
            let env: $crate::InitResultEnvelope = result.into();
            $crate::__write_result(&env)
        }

        // ── CatalogPlugin verb exports ────────────────────────────────────────

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_search,
            method   = search,
            req_ty   = $crate::SearchRequest,
            resp_ty  = $crate::SearchResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_lookup,
            method   = lookup,
            req_ty   = $crate::LookupRequest,
            resp_ty  = $crate::LookupResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_enrich,
            method   = enrich,
            req_ty   = $crate::EnrichRequest,
            resp_ty  = $crate::EnrichResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_get_artwork,
            method   = get_artwork,
            req_ty   = $crate::ArtworkRequest,
            resp_ty  = $crate::ArtworkResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_get_credits,
            method   = get_credits,
            req_ty   = $crate::CreditsRequest,
            resp_ty  = $crate::CreditsResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_related,
            method   = related,
            req_ty   = $crate::RelatedRequest,
            resp_ty  = $crate::RelatedResponse,
        }

        $crate::__catalog_abi_fn! {
            plugin   = $plugin_ty,
            getter   = get_plugin,
            fn_name  = stui_episodes,
            method   = episodes,
            req_ty   = $crate::EpisodesRequest,
            resp_ty  = $crate::EpisodesResponse,
        }

        // ── StreamProvider verb export ────────────────────────────────
        //
        // Always emitted: the StreamProvider trait has a default
        // `find_streams` returning NotImplemented, so plugins that
        // don't actually serve streams just inherit the stub via an
        // empty `impl StreamProvider for MyPlugin {}` declaration.
        // Stream-capable plugins (jackett, prowlarr, etc.) override
        // with a real body.
        #[no_mangle]
        pub extern "C" fn stui_find_streams(ptr: i32, len: i32) -> i64 {
            let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let req: $crate::FindStreamsRequest = match serde_json::from_slice(input) {
                Ok(r) => r,
                Err(e) => {
                    return $crate::__write_result(
                        &$crate::PluginResult::<$crate::FindStreamsResponse>::err(
                            $crate::error_codes::PARSE_ERROR,
                            e.to_string(),
                        ),
                    );
                }
            };
            let borrow = get_plugin();
            let result = <$plugin_ty as $crate::StreamProvider>::find_streams(&*borrow, req);
            $crate::__write_result(&result)
        }

        // Note: stui_resolve is intentionally absent — catalog-only plugins do not
        // implement the deprecated StuiPlugin trait and have no resolve endpoint.
    };
}

/// Write a serialised result into WASM linear memory and return a fat pointer.
///
/// # Memory model
///
/// The returned value encodes `(ptr << 32) | len` so the host can call
/// `memory.read(ptr, len)` to retrieve the bytes.
///
/// **The allocation is intentionally leaked** (`std::mem::forget`).
/// WASM modules cannot free memory that was allocated for the host to read —
/// the host calls `__dealloc(ptr, len)` via the exported dealloc function after
/// it has finished reading. Freeing here would be a double-free.
///
/// Do not remove the `forget` call. If you need to add pooling for large
/// responses, implement it in the host's dealloc import handler, not here.
#[doc(hidden)]
pub fn __write_result<T: serde::Serialize>(result: &T) -> i64 {
    let json = serde_json::to_vec(result).unwrap_or_else(|e| {
        format!("{{\"status\":\"err\",\"code\":\"SERIALIZE\",\"message\":\"{e}\"}}").into_bytes()
    });
    let len = json.len();
    let ptr = json.as_ptr() as i64;
    std::mem::forget(json);
    (ptr << 32) | (len as i64)
}

// ── Prelude ───────────────────────────────────────────────────────────────────

pub mod prelude {
    pub use crate::cache_get;
    pub use crate::cache_set;
    pub use crate::exec;
    pub use crate::http_get;
    pub use crate::http_get_with_headers;
    pub use crate::http_post_json;
    pub use crate::stui_export_catalog_plugin;
    pub use crate::stui_export_plugin;
    pub use crate::url_encode;
    pub use crate::{plugin_debug, plugin_error, plugin_info, plugin_warn};
    #[allow(deprecated)]
    pub use crate::{
        PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse, SearchRequest,
        SearchResponse, StuiPlugin, SubtitleTrack,
    };
    pub use crate::{
        FindStreamsRequest, FindStreamsResponse, Stream, StreamProvider,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_trait_compiles() {
        // Minimal stub to prove Plugin + CatalogPlugin can actually be implemented.
        struct Stub {
            manifest: PluginManifest,
        }
        impl Plugin for Stub {
            fn manifest(&self) -> &PluginManifest { &self.manifest }
        }
        impl CatalogPlugin for Stub {
            fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
                PluginResult::Ok(SearchResponse { items: vec![], total: 0 })
            }
        }
        fn assert_plugin<T: Plugin>() {}
        fn assert_catalog<T: CatalogPlugin>() {}
        assert_plugin::<Stub>();
        assert_catalog::<Stub>();
    }

    /// Proves that a type implementing only `Plugin + CatalogPlugin` (no `StuiPlugin`)
    /// satisfies the bounds required by `stui_export_catalog_plugin!`. If this test
    /// compiles, Chunk 3 real-plugin expansions will expand cleanly without needing
    /// the deprecated `StuiPlugin` impl.
    #[test]
    fn catalog_only_plugin_satisfies_bounds() {
        struct TestStub { m: PluginManifest }
        impl Plugin for TestStub {
            fn manifest(&self) -> &PluginManifest { &self.m }
        }
        impl CatalogPlugin for TestStub {
            fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
                PluginResult::Ok(SearchResponse { items: vec![], total: 0 })
            }
        }
        fn assert_catalog_only<T: CatalogPlugin>() {}
        assert_catalog_only::<TestStub>();
        // No StuiPlugin impl on TestStub — this compiling proves catalog-only works.
    }

    // These tests run outside WASM (on the host), so the extern "C" functions
    // won't be called. We test the pure Rust mapping/parsing logic.

    fn make_auth_json(code: Option<&str>, state: Option<&str>, error: Option<&str>) -> String {
        let mut map = serde_json::Map::new();
        if let Some(c) = code {
            map.insert("code".into(), serde_json::json!(c));
        }
        if let Some(s) = state {
            map.insert("state".into(), serde_json::json!(s));
        }
        if let Some(e) = error {
            map.insert("error".into(), serde_json::json!(e));
        }
        serde_json::to_string(&serde_json::Value::Object(map)).unwrap()
    }

    #[test]
    fn test_parse_auth_json_success() {
        let json = make_auth_json(Some("mycode"), Some("csrf"), None);
        let result = parse_auth_json(&json);
        assert!(result.is_ok());
        let cb = result.unwrap();
        assert_eq!(cb.code, "mycode");
        assert_eq!(cb.state, Some("csrf".to_string()));
    }

    #[test]
    fn test_parse_auth_json_denied() {
        let json = make_auth_json(None, None, Some("access_denied"));
        let result = parse_auth_json(&json);
        assert_eq!(result.unwrap_err(), "denied: access_denied");
    }

    #[test]
    fn test_parse_auth_json_timed_out() {
        let json = make_auth_json(None, None, Some("timed_out"));
        let result = parse_auth_json(&json);
        assert_eq!(result.unwrap_err(), "timed_out");
    }

    #[test]
    fn test_parse_auth_json_malformed_fallback() {
        // Both code and error absent → safe fallback to timed_out
        let json = r#"{"state":"xyz"}"#;
        let result = parse_auth_json(json);
        assert_eq!(result.unwrap_err(), "timed_out");
    }

    #[test]
    fn test_http_post_form_payload_format() {
        let url = "https://api.example.com/token";
        let body = "grant_type=authorization_code&code=abc";
        let payload = format!(
            "{{\"url\":{url_json},\"body\":{body_json},\"__stui_headers\":{{\"Content-Type\":\"application/x-www-form-urlencoded\"}}}}",
            url_json  = serde_json::to_string(url).unwrap(),
            body_json = serde_json::to_string(body).unwrap(),
        );
        let val: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(val["url"].as_str().unwrap(), url);
        assert_eq!(
            val["__stui_headers"]["Content-Type"].as_str().unwrap(),
            "application/x-www-form-urlencoded"
        );
    }

    #[test]
    fn sdk_search_request_carries_scope() {
        let req = SearchRequest {
            query: "creep".into(),
            scope: SearchScope::Track,
            page: 0,
            limit: 50,
            per_scope_limit: None,
            locale: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"scope\":\"track\""));
        assert!(!s.contains("\"tab\""));
    }

    #[test]
    fn plugin_entry_has_kind_and_source() {
        let entry = PluginEntry {
            id: "spotify:track:abc".into(),
            kind: EntryKind::Track,
            title: "Creep".into(),
            source: "lastfm-provider".into(),
            year: Some(1993),
            artist_name: Some("Radiohead".into()),
            album_name: Some("Pablo Honey".into()),
            track_number: Some(2),
            ..Default::default()
        };
        let s = serde_json::to_string(&entry).unwrap();
        assert!(s.contains("\"kind\":\"track\""));
        assert!(s.contains("\"source\":\"lastfm-provider\""));
    }

    #[test]
    fn plugin_entry_serializes_with_skip_none() {
        let minimal = PluginEntry {
            id: "test:1".into(),
            kind: EntryKind::Movie,
            title: "Test".into(),
            source: "test-provider".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&minimal).unwrap();
        // Should not contain null values for unset optional fields
        assert!(!json.contains("null"));
        // Should contain required fields
        assert!(json.contains("\"id\":\"test:1\""));
        assert!(json.contains("\"kind\":\"movie\""));
        assert!(json.contains("\"title\":\"Test\""));
        assert!(json.contains("\"source\":\"test-provider\""));
    }

    #[test]
    fn err_helper_with_unsupported_scope_code() {
        let r: PluginResult<()> = PluginResult::err(
            error_codes::UNSUPPORTED_SCOPE,
            "track scope unsupported by this plugin",
        );
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"code\":\"unsupported_scope\""));
        assert!(s.contains("track scope unsupported"));
    }

    #[test]
    fn new_error_codes_are_stable() {
        use super::error_codes::*;
        assert_eq!(NOT_IMPLEMENTED, "not_implemented");
        assert_eq!(RATE_LIMITED, "rate_limited");
        assert_eq!(UNKNOWN_ID, "unknown_id");
        assert_eq!(TRANSIENT, "transient");
        assert_eq!(REMOTE_ERROR, "remote_error");
    }

    #[test]
    fn plugin_entry_carries_external_ids() {
        use std::collections::HashMap;
        let mut external = HashMap::new();
        external.insert("imdb".to_string(), "tt1234567".to_string());
        external.insert("musicbrainz".to_string(), "uuid-1".to_string());

        let entry = PluginEntry {
            id: "tmdb-100".into(),
            kind: EntryKind::Movie,
            title: "Test".into(),
            source: "tmdb".into(),
            external_ids: external,
            ..Default::default()
        };
        let s = serde_json::to_string(&entry).unwrap();
        assert!(s.contains("\"external_ids\""));
        assert!(s.contains("tt1234567"));
        assert!(s.contains("uuid-1"));
    }
}

