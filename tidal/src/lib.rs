// Tidal plugin for stui
//
// Uses OAuth PKCE with a public client_id for authentication.
// Requires a Tidal subscription.
//
// Flow:
// 1. Generate PKCE code_verifier + code_challenge
// 2. Open browser for OAuth login at listen.tidal.com
// 3. Exchange auth code for access_token/refresh_token; store expires_at
// 4. On subsequent calls: check expiry, refresh silently if stale
// 5. search()  — Tidal API v1/search with countryCode
// 6. resolve() — Tidal API v1/tracks/{id}/streamUrl with countryCode

use stui_plugin_sdk::{
    auth_allocate_port, auth_open_and_wait, cache_get, cache_set, http_post_form, plugin_info,
    plugin_warn, PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse,
    SearchRequest, SearchResponse, StuiPlugin,
};

const CLIENT_ID: &str = "CzET4vdadNUFQ5JU";
const SCOPES: &str = "r_usr w_usr";

#[derive(Default)]
pub struct Tidal;

impl StuiPlugin for Tidal {
    fn name(&self) -> &str {
        "tidal"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn plugin_type(&self) -> PluginType {
        PluginType::Provider
    }

    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let token = match ensure_authenticated() {
            Ok(t) => t,
            Err(e) => return PluginResult::err("auth_failed", e),
        };

        let query = req.query.trim();
        if query.is_empty() {
            return PluginResult::ok(SearchResponse {
                items: vec![],
                total: 0,
            });
        }

        let cc = country_code();
        let url = format!(
            "https://api.tidal.com/v1/search?limit=20&countryCode={}&query={}",
            cc,
            urlencoding(query)
        );

        let body = match http_get_with_token(&url, &token) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("search_failed", e),
        };

        let items = parse_search_results(&body);
        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let token = match ensure_authenticated() {
            Ok(t) => t,
            Err(e) => return PluginResult::err("auth_failed", e),
        };

        let entry_id = req.entry_id.trim();
        if entry_id.is_empty() {
            return PluginResult::err("resolve_failed", "empty entry_id");
        }

        let track_id = match entry_id.parse::<u64>() {
            Ok(id) => id,
            Err(_) => return PluginResult::err("resolve_failed", "invalid track id"),
        };

        let cc = country_code();
        let url = format!(
            "https://api.tidal.com/v1/tracks/{}/streamUrl?soundQuality=LOSSLESS&countryCode={}",
            track_id, cc
        );

        let body = match http_get_with_token(&url, &token) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("resolve_failed", e),
        };

        let stream_url = match parse_stream_url(&body) {
            Some(url) => url,
            None => return PluginResult::err("resolve_failed", "no stream URL found"),
        };

        PluginResult::ok(ResolveResponse {
            stream_url,
            quality: Some("lossless".into()),
            subtitles: vec![],
        })
    }
}

// ── Token types ───────────────────────────────────────────────────────────────

/// Raw response from Tidal's token endpoint.
#[derive(serde::Serialize, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    token_type: String,
}

/// Persisted token — adds absolute `expires_at` so we can check staleness.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedToken {
    access_token: String,
    refresh_token: String,
    /// Unix timestamp (seconds) at which the access token expires.
    expires_at: u64,
}

fn build_cached_token(tr: TokenResponse) -> Result<CachedToken, String> {
    let now = unix_now()?;
    Ok(CachedToken {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token,
        expires_at: now + tr.expires_in.saturating_sub(60),
    })
}

// ── Authentication ────────────────────────────────────────────────────────────

fn ensure_authenticated() -> Result<String, String> {
    if let Some(cached) = cache_get("tidal_token") {
        if let Ok(token) = serde_json::from_str::<CachedToken>(&cached) {
            let now = match unix_now() {
                Ok(t) => t,
                Err(_) => {
                    return Err("clock unavailable".to_string());
                }
            };
            if now < token.expires_at {
                return Ok(token.access_token);
            }
            // Expired — attempt silent refresh before forcing a browser re-auth.
            plugin_info!("tidal: access token expired, refreshing");
            match refresh_access_token(&token.refresh_token) {
                Ok(new_token) => {
                    let json = serde_json::to_string(&new_token).map_err(|e| e.to_string())?;
                    cache_set("tidal_token", &json);
                    return Ok(new_token.access_token);
                }
                Err(e) => {
                    plugin_warn!("tidal: token refresh failed ({}), re-authenticating", e);
                    // Fall through to full PKCE flow below.
                }
            }
        }
    }

    let (code_verifier, code_challenge) = generate_pkce()?;
    let port = auth_allocate_port()?;
    let auth_url = build_auth_url(port, &code_challenge);

    plugin_info!("tidal: opening OAuth URL");
    let cb = auth_open_and_wait(&auth_url, 120_000)?;
    let tr = exchange_code(&cb.code, &code_verifier, port)?;
    let cached = build_cached_token(tr)?;
    let json = serde_json::to_string(&cached).map_err(|e| e.to_string())?;
    cache_set("tidal_token", &json);

    Ok(cached.access_token)
}

fn refresh_access_token(refresh_token: &str) -> Result<CachedToken, String> {
    let body = format!(
        "client_id={}&refresh_token={}&grant_type=refresh_token",
        CLIENT_ID,
        urlencoding(refresh_token)
    );
    let resp = http_post_form("https://login.tidal.com/oauth2/token", &body)
        .map_err(|e| format!("token refresh failed: {}", e))?;
    let tr: TokenResponse =
        serde_json::from_str(&resp).map_err(|e| format!("parse refresh response: {}", e))?;
    Ok(build_cached_token(tr)?)
}

fn build_auth_url(port: u16, code_challenge: &str) -> String {
    format!(
        "https://listen.tidal.com/login/auth?appMode=WEB&client_id={}&code_challenge={}&code_challenge_method=S256&lang=en&redirect_uri=https://127.0.0.1:{}/login&response_type=code&restrictSignup=true&scope={}&autoredirect=true",
        CLIENT_ID, code_challenge, port, SCOPES
    )
}

fn exchange_code(code: &str, code_verifier: &str, port: u16) -> Result<TokenResponse, String> {
    let redirect_uri = format!("https://127.0.0.1:{}/login", port);
    let body = format!(
        "client_id={}&code={}&code_verifier={}&grant_type=authorization_code&redirect_uri={}",
        CLIENT_ID, code, code_verifier, redirect_uri
    );

    let resp = http_post_form("https://login.tidal.com/oauth2/token", &body)
        .map_err(|e| format!("token exchange failed: {}", e))?;

    serde_json::from_str(&resp).map_err(|e| format!("parse token response: {}", e))
}

// ── HTTP helper ───────────────────────────────────────────────────────────────

fn http_get_with_token(url: &str, token: &str) -> Result<String, String> {
    // Build the payload using serde_json to guarantee valid JSON (no raw string injection).
    let payload = serde_json::json!({
        "url": url,
        "body": "",
        "__stui_headers": {
            "Authorization": format!("Bearer {}", token),
            "X-Tidal-Token": CLIENT_ID,
        }
    })
    .to_string();

    #[cfg(target_arch = "wasm32")]
    {
        extern "C" {
            fn stui_http_post(ptr: *const u8, len: i32) -> i64;
            fn stui_free(ptr: i32, len: i32);
        }
        let packed = unsafe { stui_http_post(payload.as_ptr(), payload.len() as i32) };
        if packed == 0 {
            return Err("http request failed".into());
        }
        let ptr = ((packed >> 32) & 0xFFFFFFFF) as *const u8;
        let len = (packed & 0xFFFFFFFF) as usize;

        let json = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
            .map_err(|e| e.to_string())?
            .to_string();

        unsafe { stui_free(ptr as i32, len as i32) };

        let resp: HttpResponse = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        if resp.status >= 200 && resp.status < 300 {
            Ok(resp.body)
        } else {
            Err(format!("HTTP {}: {}", resp.status, resp.body))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        drop(payload);
        Err("http_get_with_token only available in WASM context".into())
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)] // only constructed inside #[cfg(target_arch = "wasm32")] blocks
struct HttpResponse {
    status: u16,
    body: String,
}

// ── Parsers ───────────────────────────────────────────────────────────────────

fn parse_search_results(body: &str) -> Vec<PluginEntry> {
    let val: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut items = Vec::new();

    if let Some(tracks) = val["tracks"]["items"].as_array() {
        for track in tracks {
            let id = track["id"].as_u64().unwrap_or(0);
            if id == 0 {
                continue;
            }
            let title = track["title"].as_str().unwrap_or("Unknown").to_string();
            let artist = track["artist"]["name"].as_str().unwrap_or("");
            let album = track["album"]["title"].as_str().unwrap_or("");
            let duration = track["duration"].as_u64().unwrap_or(0);
            let image = track["album"]["coverUrl"]
                .as_str()
                .or_else(|| track["album"]["cover"]["large"].as_str());
            let year = track["album"]["releaseDate"]
                .as_str()
                .and_then(|d| d.split('-').next())
                .and_then(|y| y.parse::<u32>().ok());

            items.push(PluginEntry {
                id: id.to_string(),
                title: if artist.is_empty() {
                    title
                } else {
                    format!("{} — {}", title, artist)
                },
                year,
                genre: None,
                rating: None,
                description: if album.is_empty() {
                    None
                } else {
                    Some(album.to_string())
                },
                poster_url: image.map(String::from),
                imdb_id: None,
                duration: Some(format_duration(duration)),
            });
        }
    }

    items
}

fn parse_stream_url(body: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;

    for key in &["url", "streamUrl"] {
        if let Some(url) = val[key].as_str().filter(|s| !s.is_empty()) {
            return Some(url.to_string());
        }
    }

    None
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns the user's country code for API requests.
/// Reads from plugin config; defaults to "US".
fn country_code() -> String {
    cache_get("__config:country").unwrap_or_else(|| "US".to_string())
}

fn unix_now() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| e.to_string())
}

fn format_duration(seconds: u64) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    format!("{}:{:02}", mins, secs)
}

fn generate_pkce() -> Result<(String, String), String> {
    use sha2::{Digest, Sha256};

    let mut verifier_bytes = [0u8; 32];
    getrandom::getrandom(&mut verifier_bytes)
        .map_err(|e| format!("PKCE generation failed: {}", e))?;

    let verifier = base64_url_encode(verifier_bytes.to_vec());
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64_url_encode(hash.to_vec());

    Ok((verifier, challenge))
}

fn base64_url_encode(data: Vec<u8>) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut result = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as usize;
        let b1 = if i + 1 < data.len() {
            data[i + 1] as usize
        } else {
            0
        };
        let b2 = if i + 2 < data.len() {
            data[i + 2] as usize
        } else {
            0
        };

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
        if i + 1 < data.len() {
            result.push(ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        }
        if i + 2 < data.len() {
            result.push(ALPHABET[b2 & 0x3f] as char);
        }

        i += 3;
    }
    result
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '!' => out.push_str("%21"),
            '#' => out.push_str("%23"),
            '$' => out.push_str("%24"),
            '&' => out.push_str("%26"),
            '\'' => out.push_str("%27"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '*' => out.push_str("%2A"),
            '+' => out.push_str("%2B"),
            ',' => out.push_str("%2C"),
            '/' => out.push_str("%2F"),
            ':' => out.push_str("%3A"),
            ';' => out.push_str("%3B"),
            '=' => out.push_str("%3D"),
            '?' => out.push_str("%3F"),
            '@' => out.push_str("%40"),
            '[' => out.push_str("%5B"),
            ']' => out.push_str("%5D"),
            c if c.is_ascii_alphanumeric() || "-_.~".contains(c) => out.push(c),
            c => {
                for byte in c.encode_utf8(&mut [0u8; 4]).as_bytes() {
                    out.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    out
}

stui_plugin_sdk::stui_export_plugin!(Tidal);

#[cfg(test)]
mod tests {
    #[test]
    fn test_urlencoding() {
        assert_eq!(super::urlencoding("test"), "test");
        assert_eq!(super::urlencoding("test user"), "test%20user");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(super::format_duration(0), "0:00");
        assert_eq!(super::format_duration(30), "0:30");
        assert_eq!(super::format_duration(90), "1:30");
    }

    #[test]
    fn test_parse_stream_url_url_key() {
        let body = r#"{"url":"https://example.com/stream.flac","mimeType":"audio/flac"}"#;
        assert_eq!(
            super::parse_stream_url(body),
            Some("https://example.com/stream.flac".to_string())
        );
    }

    #[test]
    fn test_parse_stream_url_fallback_key() {
        let body = r#"{"streamUrl":"https://example.com/stream.flac"}"#;
        assert_eq!(
            super::parse_stream_url(body),
            Some("https://example.com/stream.flac".to_string())
        );
    }

    #[test]
    fn test_parse_search_results_skips_zero_id() {
        // A track with id=0 (missing/invalid) must not appear in results.
        let body = r#"{"tracks":{"items":[{"id":0,"title":"Bad","artist":{"name":""},"album":{"title":""},"duration":0}]}}"#;
        let items = super::parse_search_results(body);
        assert!(items.is_empty());
    }
}
