//! # stui-plugin-sdk
//!
//! The Rust SDK for building stui plugins.
//!
//! ## Quick start
//!
//! ```rust
//! use stui_plugin_sdk::prelude::*;
//!
//! pub struct MyProvider;
//!
//! impl StuiPlugin for MyProvider {
//!     fn name(&self) -> &str { "my-provider" }
//!     fn version(&self) -> &str { "1.0.0" }
//!     fn plugin_type(&self) -> PluginType { PluginType::Provider }
//!
//!     fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
//!         // ... fetch content ...
//!         PluginResult::Ok(SearchResponse { items: vec![], total: 0 })
//!     }
//!
//!     fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
//!         PluginResult::Ok(ResolveResponse {
//!             stream_url: "https://...".into(),
//!             quality: Some("1080p".into()),
//!             subtitles: vec![],
//!         })
//!     }
//! }
//!
//! // Register the plugin — generates all required WASM exports
//! stui_export_plugin!(MyProvider);
//! ```
//!
//! ## Compile to WASM
//!
//! ```bash
//! rustup target add wasm32-wasip1
//! cargo build --target wasm32-wasip1 --release
//! # Output: target/wasm32-wasip1/release/my_provider.wasm
//! ```

// ── ABI types (re-exported for plugin authors) ────────────────────────────────

pub const STUI_ABI_VERSION: i32 = 1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub tab: String,
    pub page: u32,
    pub limit: u32,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
    pub id: String,
    pub title: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub rating: Option<String>,
    pub description: Option<String>,
    pub poster_url: Option<String>,
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub duration: Option<String>,
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

/// The trait every stui plugin implements.
///
/// Implement this trait, then call `stui_export_plugin!(YourPlugin)` to
/// generate the WASM ABI glue automatically.
pub trait StuiPlugin {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn plugin_type(&self) -> PluginType;

    /// Search for content matching `req.query` in the given `req.tab`.
    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse>;

    /// Resolve an entry ID into a playable stream URL.
    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse>;
}

// ── Host function imports (called by plugin at runtime) ───────────────────────

/// Log a message through the stui host logger.
/// Use the `log!` / `info!` / `warn!` macros instead of calling this directly.
#[cfg(target_arch = "wasm32")]
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
        Err(format!(
            "http_get only available in WASM context (url: {url})"
        ))
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)] // fields only read inside #[cfg(target_arch = "wasm32")] blocks
struct HttpResponse {
    pub status: u16,
    pub body: String,
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

// ── ABI glue macro ────────────────────────────────────────────────────────────

/// Registers your plugin and generates all required WASM ABI exports.
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
/// - `stui_resolve(ptr: i32, len: i32) -> i64`
#[macro_export]
macro_rules! stui_export_plugin {
    ($plugin_ty:ty) => {
        // Safety: WASM is single-threaded; we use a global instance.
        static PLUGIN_INSTANCE: std::sync::OnceLock<$plugin_ty> = std::sync::OnceLock::new();

        fn get_plugin() -> &'static $plugin_ty {
            PLUGIN_INSTANCE.get_or_init(|| <$plugin_ty>::default())
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

        /// Search entry point. Input: SearchRequest JSON. Output: packed (ptr<<32)|len.
        #[no_mangle]
        pub extern "C" fn stui_search(ptr: i32, len: i32) -> i64 {
            let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let req: $crate::SearchRequest = match serde_json::from_slice(input) {
                Ok(r) => r,
                Err(e) => {
                    return $crate::__write_result(
                        &$crate::PluginResult::<$crate::SearchResponse>::err(
                            "PARSE_ERROR",
                            e.to_string(),
                        ),
                    )
                }
            };
            let result = get_plugin().search(req);
            $crate::__write_result(&result)
        }

        /// Resolve entry point. Input: ResolveRequest JSON. Output: packed (ptr<<32)|len.
        #[no_mangle]
        pub extern "C" fn stui_resolve(ptr: i32, len: i32) -> i64 {
            let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let req: $crate::ResolveRequest = match serde_json::from_slice(input) {
                Ok(r) => r,
                Err(e) => {
                    return $crate::__write_result(
                        &$crate::PluginResult::<$crate::ResolveResponse>::err(
                            "PARSE_ERROR",
                            e.to_string(),
                        ),
                    )
                }
            };
            let result = get_plugin().resolve(req);
            $crate::__write_result(&result)
        }
    };
}

/// Internal helper — serialises a result to WASM memory and returns packed ptr/len.
/// Not part of the public API; used by the `stui_export_plugin!` macro.
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
    pub use crate::http_post_json;
    pub use crate::stui_export_plugin;
    pub use crate::url_encode;
    pub use crate::{plugin_debug, plugin_error, plugin_info, plugin_warn};
    pub use crate::{
        PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse, SearchRequest,
        SearchResponse, StuiPlugin, SubtitleTrack,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
