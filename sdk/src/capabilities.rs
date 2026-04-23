//! Request/response types for `CatalogPlugin` verbs, plus lifecycle + helpers.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::kinds::EntryKind;
use crate::manifest::{ManifestValidationError, PluginManifest};
use crate::{PluginEntry, PluginResult};

// ── InitContext ───────────────────────────────────────────────────────────────

/// Context passed to `Plugin::init`. Carries resolved env, config, cache dir,
/// and a logger handle.
///
/// The `logger` field is NOT serializable — it is attached on the plugin side
/// after deserializing the wire-format [`InitRequest`] via
/// [`InitContext::from_request`].
///
/// `config` is a `HashMap<String, serde_json::Value>` so plugins can read
/// values via the same serde-json helpers they already use for HTTP
/// response parsing (`.as_str()`, `.as_i64()`, `.as_bool()`), without
/// pulling the `toml` crate just to touch their own config.
pub struct InitContext<'a> {
    pub env: &'a HashMap<String, String>,
    pub config: &'a HashMap<String, serde_json::Value>,
    pub cache_dir: &'a PathBuf,
    pub logger: &'a dyn PluginLogger,
}

impl<'a> InitContext<'a> {
    /// Build an `InitContext` from a deserialized [`InitRequest`] plus a
    /// logger handle. The plugin side reassembles the context this way because
    /// the `logger` trait-object cannot cross the ABI boundary.
    pub fn from_request(req: &'a InitRequest, logger: &'a dyn PluginLogger) -> Self {
        Self {
            env: &req.env,
            config: &req.config,
            cache_dir: &req.cache_dir,
            logger,
        }
    }
}

/// Wire-format payload for `stui_init`. This is the serializable subset of
/// [`InitContext`] — the `logger` is attached after deserialization via
/// [`InitContext::from_request`].
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InitRequest {
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub cache_dir: PathBuf,
}

/// Default `PluginLogger` used on the plugin side after deserializing an
/// [`InitRequest`]. Routes to `host_log` when running under WASM, falls back
/// to `eprintln!` on the host (tests).
pub struct DefaultPluginLogger;

impl PluginLogger for DefaultPluginLogger {
    fn debug(&self, msg: &str) { crate::host_log(1, msg); }
    fn info(&self, msg: &str)  { crate::host_log(2, msg); }
    fn warn(&self, msg: &str)  { crate::host_log(3, msg); }
    fn error(&self, msg: &str) { crate::host_log(4, msg); }
}

/// Logging surface exposed to plugins (backed by `stui_log` host import at runtime,
/// no-op or stdout in test harness).
pub trait PluginLogger {
    fn debug(&self, msg: &str);
    fn info(&self, msg: &str);
    fn warn(&self, msg: &str);
    fn error(&self, msg: &str);
}

/// Result of `Plugin::init`. `MissingConfig` is soft — user-fixable via TUI;
/// `Fatal` is hard — code bug or trap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginInitError {
    MissingConfig {
        fields: Vec<String>,
        hint: Option<String>,
    },
    Fatal(String),
}

/// Wire-format envelope for the plugin-side response from `stui_init`.
///
/// Mirrors the shape of [`crate::PluginResult`] but with a fixed success
/// type of `()` — `init` never carries a success payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InitResultEnvelope {
    Ok,
    Err(PluginInitError),
}

impl From<Result<(), PluginInitError>> for InitResultEnvelope {
    fn from(r: Result<(), PluginInitError>) -> Self {
        match r {
            Ok(())  => Self::Ok,
            Err(e)  => Self::Err(e),
        }
    }
}

impl From<InitResultEnvelope> for Result<(), PluginInitError> {
    fn from(e: InitResultEnvelope) -> Self {
        match e {
            InitResultEnvelope::Ok     => Ok(()),
            InitResultEnvelope::Err(e) => Err(e),
        }
    }
}

// ── Lookup ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookupRequest {
    pub id: String,
    pub id_source: String,
    pub kind: EntryKind,
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookupResponse {
    pub entry: PluginEntry,
}

// ── Enrich ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichRequest {
    pub partial: PluginEntry,
    pub prefer_id_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichResponse {
    pub entry: PluginEntry,
    /// 0.0..=1.0 — plugin's own match-confidence score.
    pub confidence: f32,
}

// ── Artwork ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkSize {
    Thumbnail,
    Standard,
    HiRes,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtworkRequest {
    pub id: String,
    pub id_source: String,
    pub kind: EntryKind,
    pub size: ArtworkSize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtworkVariant {
    pub size: ArtworkSize,
    pub url: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtworkResponse {
    pub variants: Vec<ArtworkVariant>,
}

// ── Credits ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditsRequest {
    pub id: String,
    pub id_source: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CastRole {
    Actor,
    Vocalist,
    FeaturedArtist,
    GuestAppearance,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastMember {
    pub name: String,
    pub role: CastRole,
    pub character: Option<String>,
    pub instrument: Option<String>,
    pub billing_order: Option<u32>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub external_ids: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewRole {
    Director,
    Writer,
    Producer,
    ExecutiveProducer,
    Cinematographer,
    Editor,
    Composer,
    Songwriter,
    Lyricist,
    Arranger,
    Instrumentalist,
    ProductionDesigner,
    ArtDirector,
    CostumeDesigner,
    SoundDesigner,
    VfxSupervisor,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrewMember {
    pub name: String,
    pub role: CrewRole,
    pub department: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub external_ids: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditsResponse {
    pub cast: Vec<CastMember>,
    pub crew: Vec<CrewMember>,
}

/// Normalize upstream crew-role strings into canonical `CrewRole` variants.
/// Unrecognized strings map to `CrewRole::Other(s)`.
pub fn normalize_crew_role(s: &str) -> CrewRole {
    match s.to_lowercase().as_str() {
        "director" => CrewRole::Director,
        "writer" | "screenplay" | "screenwriter" => CrewRole::Writer,
        "producer" => CrewRole::Producer,
        "executive producer" => CrewRole::ExecutiveProducer,
        "cinematographer" | "director of photography" | "dp" | "dop" => CrewRole::Cinematographer,
        "editor" => CrewRole::Editor,
        "composer" | "original music composer" => CrewRole::Composer,
        "songwriter" => CrewRole::Songwriter,
        "lyricist" => CrewRole::Lyricist,
        "arranger" => CrewRole::Arranger,
        "instrumentalist" | "session musician" => CrewRole::Instrumentalist,
        "production designer" => CrewRole::ProductionDesigner,
        "art director" => CrewRole::ArtDirector,
        "costume designer" => CrewRole::CostumeDesigner,
        "sound designer" => CrewRole::SoundDesigner,
        "vfx supervisor" | "visual effects supervisor" => CrewRole::VfxSupervisor,
        _ => CrewRole::Other(s.to_string()),
    }
}

// ── Related ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    SameArtist,
    SameDirector,
    SameStudio,
    Similar,
    Sequel,
    Compilation,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedRequest {
    pub id: String,
    pub id_source: String,
    pub kind: EntryKind,
    pub relation: RelationKind,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelatedResponse {
    pub items: Vec<PluginEntry>,
}

// ── err_not_implemented helper ────────────────────────────────────────────────

/// Canonical helper for default-method bodies on optional `CatalogPlugin`
/// verbs. Returns a `PluginResult::Err` with the `NOT_IMPLEMENTED` code.
pub fn err_not_implemented<T>() -> PluginResult<T> {
    PluginResult::err(
        crate::error_codes::NOT_IMPLEMENTED,
        "verb not implemented by this plugin",
    )
}

// ── Manifest validator (used by CLI lint/build) ───────────────────────────────

/// Validate a freshly-parsed manifest against the canonical schema.
///
/// Thin delegator to [`crate::manifest::validate`] — the authoritative
/// validator lives alongside the manifest types. This name is kept here as a
/// stable entry point for the CLI (`stui plugin lint` / `stui plugin build`)
/// so call sites like `stui_plugin_sdk::capabilities::validate_manifest(&m)`
/// continue to compile.
pub fn validate_manifest(manifest: &PluginManifest) -> Result<(), ManifestValidationError> {
    crate::manifest::validate(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_crew_role_common_aliases() {
        assert!(matches!(normalize_crew_role("Director"), CrewRole::Director));
        assert!(matches!(normalize_crew_role("director of photography"), CrewRole::Cinematographer));
        assert!(matches!(normalize_crew_role("DOP"), CrewRole::Cinematographer));
        assert!(matches!(normalize_crew_role("Original Music Composer"), CrewRole::Composer));
    }

    #[test]
    fn normalize_crew_role_unknown_is_other() {
        match normalize_crew_role("Foley Artist") {
            CrewRole::Other(s) => assert_eq!(s, "Foley Artist"),
            _ => panic!("expected Other variant"),
        }
    }

    #[test]
    fn plugin_init_error_serde_tagged() {
        let e = PluginInitError::MissingConfig {
            fields: vec!["api_key".into()],
            hint: Some("Get a key at example.com".into()),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"kind\":\"missing_config\""));
        assert!(s.contains("api_key"));
    }

    #[test]
    fn init_request_round_trips_through_json() {
        let mut env = HashMap::new();
        env.insert("TMDB_API_KEY".into(), "secret".into());

        let mut config: HashMap<String, serde_json::Value> = HashMap::new();
        config.insert("api_key".into(), serde_json::Value::String("secret".into()));

        let req = InitRequest {
            env,
            config,
            cache_dir: std::path::PathBuf::from("/tmp/cache/tmdb"),
        };

        let json = serde_json::to_string(&req).unwrap();
        let back: InitRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.env.get("TMDB_API_KEY").map(String::as_str), Some("secret"));
        assert_eq!(
            back.config.get("api_key").and_then(|v| v.as_str()),
            Some("secret"),
        );
        assert_eq!(back.cache_dir, std::path::PathBuf::from("/tmp/cache/tmdb"));
    }

    #[test]
    fn init_result_envelope_round_trips_ok() {
        let e: InitResultEnvelope = Ok::<(), PluginInitError>(()).into();
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"status\":\"ok\""), "got {s}");
        let back: InitResultEnvelope = serde_json::from_str(&s).unwrap();
        let r: Result<(), PluginInitError> = back.into();
        assert!(r.is_ok());
    }

    #[test]
    fn init_result_envelope_round_trips_missing_config() {
        let e: InitResultEnvelope = Err::<(), _>(PluginInitError::MissingConfig {
            fields: vec!["api_key".into()],
            hint: None,
        }).into();
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"status\":\"err\""));
        let back: InitResultEnvelope = serde_json::from_str(&s).unwrap();
        let r: Result<(), PluginInitError> = back.into();
        match r {
            Err(PluginInitError::MissingConfig { fields, .. }) => {
                assert_eq!(fields, vec!["api_key".to_string()]);
            }
            _ => panic!("expected MissingConfig"),
        }
    }

    #[test]
    fn init_context_from_request_attaches_logger() {
        // Non-WASM host path: DefaultPluginLogger is a ZST that routes to
        // eprintln! outside of WASM, so this test just verifies the shape
        // and that the borrow-through fields match.
        let req = InitRequest {
            env: HashMap::from([("K".to_string(), "V".to_string())]),
            config: HashMap::new(),
            cache_dir: std::path::PathBuf::from("/tmp"),
        };
        let logger = DefaultPluginLogger;
        let ctx = InitContext::from_request(&req, &logger);
        assert_eq!(ctx.env.get("K").map(String::as_str), Some("V"));
        assert_eq!(ctx.cache_dir, &std::path::PathBuf::from("/tmp"));
    }

    #[test]
    fn err_not_implemented_returns_error() {
        let r: PluginResult<i32> = err_not_implemented();
        match r {
            PluginResult::Err(e) => {
                assert_eq!(e.code, crate::error_codes::NOT_IMPLEMENTED);
            }
            _ => panic!("expected Err"),
        }
    }

    #[test]
    fn artwork_size_serializes_snake_case() {
        let s = serde_json::to_string(&ArtworkSize::HiRes).unwrap();
        assert_eq!(s, "\"hi_res\"");
    }

    #[test]
    fn cast_role_other_variant_preserves_string() {
        let r = CastRole::Other("Extra".to_string());
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("Extra"));
        let back: CastRole = serde_json::from_str(&s).unwrap();
        if let CastRole::Other(x) = back {
            assert_eq!(x, "Extra");
        } else {
            panic!("round-trip lost Other variant");
        }
    }
}
