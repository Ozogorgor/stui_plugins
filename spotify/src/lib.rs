// Spotify plugin for stui
//
// Uses OAuth PKCE with a public client_id for authentication.
// Requires Spotify Premium.
//
// Flow:
// 1. Generate PKCE code_verifier + code_challenge
// 2. Open browser for OAuth login at accounts.spotify.com
//    redirect_uri MUST be http:// (not https://) for localhost — Spotify rejects https on 127.0.0.1
// 3. Exchange auth code → access_token + refresh_token; store expires_at
// 4. On subsequent calls: check expiry, refresh silently if stale
// 5. search()  — Spotify Web API v1/search
// 6. resolve() — returns spotify:track:{id} URI for runtime librespot/Connect playback
//
// NOTE: resolve() returns a spotify:track: URI, not an HTTP URL. Playback requires
// MPD to be configured with a Spotify backend (e.g. mopidy-spotify or spotifyd) that
// understands spotify:track: URIs. The runtime's mpd_bridge passes the URI to MPD as-is.

use stui_plugin_sdk::{
    auth_allocate_port, auth_open_and_wait, cache_get, cache_set, http_post_form, plugin_info,
    plugin_warn, PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse,
    SearchRequest, SearchResponse, StuiPlugin,
};

/// Default client ID. Users can override with their own Spotify app's client ID
/// via the `client_id` config entry, which protects against this ID being revoked.
const DEFAULT_CLIENT_ID: &str = "515ab11b9e0447278653f43520eea7d9";
const SCOPES: &str = "user-library-read streaming user-read-private";

fn client_id() -> String {
    cache_get("client_id").unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

#[derive(Default)]
pub struct Spotify;

impl StuiPlugin for Spotify {
    fn name(&self) -> &str {
        "spotify"
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

        let url = format!(
            "https://api.spotify.com/v1/search?q={}&type=track&limit=20",
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
        let _token = match ensure_authenticated() {
            Ok(t) => t,
            Err(e) => return PluginResult::err("auth_failed", e),
        };

        let entry_id = req.entry_id.trim();
        if entry_id.is_empty() {
            return PluginResult::err("resolve_failed", "empty entry_id");
        }

        let stream_url = if entry_id.starts_with("spotify:track:") {
            entry_id.to_string()
        } else if entry_id.contains("spotify.com/track/") {
            if let Some(id) = extract_spotify_track_id(entry_id) {
                format!("spotify:track:{}", id)
            } else {
                entry_id.to_string()
            }
        } else {
            entry_id.to_string()
        };

        PluginResult::ok(ResolveResponse {
            stream_url,
            quality: Some("320k".into()),
            subtitles: vec![],
        })
    }
}

// ── Token types ───────────────────────────────────────────────────────────────

/// Raw response from Spotify's token endpoint.
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

/// Persisted token — adds `expires_at` so we can check staleness,
/// and `refresh_token` so we can renew without re-opening a browser.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedToken {
    access_token: String,
    refresh_token: String,
    /// Unix timestamp (seconds) when the access token expires.
    expires_at: u64,
}

fn build_cached_token(
    tr: TokenResponse,
    existing_refresh: Option<&str>,
) -> Result<CachedToken, String> {
    // Spotify only issues a new refresh_token on the initial exchange; refresh
    // calls return the same token without a new one.  Fall back to the existing
    // refresh_token so we never lose it.
    let refresh_token = tr
        .refresh_token
        .or_else(|| existing_refresh.map(str::to_string))
        .ok_or_else(|| "no refresh_token in token response".to_string())?;

    Ok(CachedToken {
        access_token: tr.access_token,
        refresh_token,
        // Subtract 60 s as a safety margin.
        expires_at: unix_now() + tr.expires_in.saturating_sub(60),
    })
}

// ── Authentication ────────────────────────────────────────────────────────────

fn ensure_authenticated() -> Result<String, String> {
    if let Some(cached_json) = cache_get("spotify_token") {
        if let Ok(token) = serde_json::from_str::<CachedToken>(&cached_json) {
            if unix_now() < token.expires_at {
                return Ok(token.access_token);
            }
            // Expired — attempt silent refresh.
            plugin_info!("spotify: access token expired, refreshing");
            match refresh_access_token(&token.refresh_token) {
                Ok(new_token) => {
                    let json = serde_json::to_string(&new_token).map_err(|e| e.to_string())?;
                    cache_set("spotify_token", &json);
                    return Ok(new_token.access_token);
                }
                Err(e) => {
                    plugin_warn!("spotify: token refresh failed ({}), re-authenticating", e);
                    // Fall through to full PKCE flow.
                }
            }
        }
    }

    let (code_verifier, code_challenge) = generate_pkce()?;
    let port = auth_allocate_port()?;
    let auth_url = build_auth_url(port, &code_challenge);

    plugin_info!("spotify: opening OAuth URL");

    let cb = auth_open_and_wait(&auth_url, 120_000)?;
    let tr = exchange_code(&cb.code, &code_verifier, port)?;
    let cached = build_cached_token(tr, None)?;
    let json = serde_json::to_string(&cached).map_err(|e| e.to_string())?;
    cache_set("spotify_token", &json);

    Ok(cached.access_token)
}

fn refresh_access_token(refresh_token: &str) -> Result<CachedToken, String> {
    let cid = client_id();
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencoding(refresh_token),
        urlencoding(&cid)
    );
    let resp = http_post_form("https://accounts.spotify.com/api/token", &body)
        .map_err(|e| format!("token refresh failed: {}", e))?;
    let tr: TokenResponse =
        serde_json::from_str(&resp).map_err(|e| format!("parse refresh response: {}", e))?;
    // Spotify may not return a new refresh_token on refresh — keep the old one.
    build_cached_token(tr, Some(refresh_token))
}

fn build_auth_url(port: u16, code_challenge: &str) -> String {
    let cid = client_id();
    // Spotify requires http:// (not https://) for 127.0.0.1 redirect URIs.
    let scopes_encoded = SCOPES
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "%20".to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect::<String>();
    format!(
        "https://accounts.spotify.com/authorize?client_id={}&redirect_uri=http://127.0.0.1:{}/login&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256",
        cid, port, scopes_encoded, code_challenge
    )
}

fn exchange_code(code: &str, code_verifier: &str, port: u16) -> Result<TokenResponse, String> {
    let cid = client_id();
    // redirect_uri must exactly match the one sent in build_auth_url.
    let redirect_uri = format!("http://127.0.0.1:{}/login", port);
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding(code),
        urlencoding(&redirect_uri),
        urlencoding(&cid),
        urlencoding(code_verifier)
    );
    let resp = http_post_form("https://accounts.spotify.com/api/token", &body)
        .map_err(|e| format!("token exchange failed: {}", e))?;
    serde_json::from_str(&resp).map_err(|e| format!("parse token response: {}", e))
}

// ── HTTP helper ───────────────────────────────────────────────────────────────

fn http_get_with_token(url: &str, token: &str) -> Result<String, String> {
    // Use serde_json::json! to guarantee valid JSON — no raw token injection.
    let payload = serde_json::json!({
        "url": url,
        "body": "",
        "__stui_headers": {
            "Authorization": format!("Bearer {}", token),
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

    let empty_vec: Vec<serde_json::Value> = Vec::new();
    let tracks = val["tracks"]["items"].as_array().unwrap_or(&empty_vec);

    tracks
        .iter()
        .filter_map(|track| {
            let id = track["id"].as_str()?.to_string();
            let name = track["name"].as_str()?.to_string();

            let artist = track["artists"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|a| a["name"].as_str())
                .unwrap_or("");

            let album = track["album"]["name"].as_str().unwrap_or("");
            let duration_ms = track["duration_ms"].as_u64().unwrap_or(0);

            let poster_url = track["album"]["images"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|img| img["url"].as_str())
                .map(String::from);

            Some(PluginEntry {
                id: format!("spotify:track:{}", id),
                title: if artist.is_empty() {
                    name
                } else {
                    format!("{} — {}", name, artist)
                },
                year: None,
                genre: None,
                rating: None,
                description: if album.is_empty() {
                    None
                } else {
                    Some(album.to_string())
                },
                poster_url,
                imdb_id: None,
                duration: Some(format_duration_ms(duration_ms)),
            })
        })
        .collect()
}

fn extract_spotify_track_id(url: &str) -> Option<String> {
    if let Some(pos) = url.find("track/") {
        let rest = &url[pos + 6..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if id.len() > 10 {
            return Some(id);
        }
    }
    None
}

// ── PKCE ─────────────────────────────────────────────────────────────────────

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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_else(|e| {
            plugin_warn!("spotify: failed to get system time: {}", e);
            0
        })
}

fn format_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
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

stui_plugin_sdk::stui_export_plugin!(Spotify);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_spotify_track_id() {
        assert_eq!(
            extract_spotify_track_id("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"),
            Some("4cOdK2wGLETKBW3PvgPWqT".to_string())
        );
    }

    #[test]
    fn test_extract_spotify_track_id_with_query() {
        // Query string after the ID must be stripped
        assert_eq!(
            extract_spotify_track_id(
                "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=abc"
            ),
            Some("4cOdK2wGLETKBW3PvgPWqT".to_string())
        );
    }

    #[test]
    fn test_format_duration_ms() {
        assert_eq!(format_duration_ms(0), "0:00");
        assert_eq!(format_duration_ms(30000), "0:30");
        assert_eq!(format_duration_ms(180000), "3:00");
        assert_eq!(format_duration_ms(214000), "3:34");
    }

    #[test]
    fn test_build_cached_token_uses_existing_refresh_when_none_returned() {
        let tr = TokenResponse {
            access_token: "new_access".to_string(),
            refresh_token: None, // Spotify doesn't always return a new one on refresh
            expires_in: 3600,
        };
        let cached = build_cached_token(tr, Some("old_refresh")).unwrap();
        assert_eq!(cached.access_token, "new_access");
        assert_eq!(cached.refresh_token, "old_refresh");
    }

    #[test]
    fn test_build_cached_token_fails_without_any_refresh_token() {
        let tr = TokenResponse {
            access_token: "new_access".to_string(),
            refresh_token: None,
            expires_in: 3600,
        };
        assert!(build_cached_token(tr, None).is_err());
    }

    #[test]
    fn test_redirect_uri_uses_http_not_https() {
        let url = build_auth_url(8888, "challenge_abc");
        assert!(
            url.contains("http://127.0.0.1:8888/login"),
            "redirect_uri must use http:// for Spotify localhost OAuth"
        );
        assert!(
            !url.contains("https://127.0.0.1"),
            "https:// on localhost is rejected by Spotify"
        );
    }
}
