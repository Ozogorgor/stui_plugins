// Spotify plugin for stui
//
// Uses OAuth for authentication and Spotify Web API for search/playlists.
// Audio streaming is handled by the runtime via Spotify Connect (librespot).
//
// User needs Spotify Premium to stream audio.

use stui_plugin_sdk::{
    auth_allocate_port, auth_open_and_wait, cache_get, cache_set, http_post_form, plugin_info,
    PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse, SearchRequest,
    SearchResponse, StuiPlugin,
};

const CLIENT_ID: &str = "515ab11b9e0447278653f43520eea7d9";
const SCOPES: &str = "user-library-read streaming user-read-private";

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

        // entry_id should be a Spotify track URI or URL
        // We return it as-is - runtime will handle Spotify Connect playback
        let stream_url = if entry_id.starts_with("spotify:track:") {
            entry_id.to_string()
        } else if entry_id.contains("spotify.com/track/") {
            // Convert URL to URI format
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

fn ensure_authenticated() -> Result<String, String> {
    if let Some(result) = token_from_cache(cache_get("spotify_token")) {
        return result;
    }

    let (code_verifier, code_challenge) = generate_pkce();
    let port = auth_allocate_port()?;
    let auth_url = build_auth_url(port, &code_challenge);

    plugin_info!("Opening Spotify auth URL");

    let cb = auth_open_and_wait(&auth_url, 120_000)?;
    // Pass both the dynamic port AND the verifier to match the exact redirect_uri
    // used at authorization and to satisfy PKCE (Spotify requires PKCE for public clients).
    let token_json = exchange_code(&cb.code, &code_verifier, port)?;
    cache_set("spotify_token", &token_json);

    parse_token(&token_json)
}

fn build_auth_url(port: u16, code_challenge: &str) -> String {
    format!(
        "https://accounts.spotify.com/authorize?client_id={}&redirect_uri=https://127.0.0.1:{}/login&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256",
        CLIENT_ID, port, SCOPES, code_challenge
    )
}

fn exchange_code(code: &str, code_verifier: &str, port: u16) -> Result<String, String> {
    // redirect_uri MUST match the one used in the authorization request exactly.
    let redirect_uri = format!("https://127.0.0.1:{}/login", port);
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        code, redirect_uri, CLIENT_ID, code_verifier
    );
    http_post_form("https://accounts.spotify.com/api/token", &body)
}

fn generate_pkce() -> (String, String) {
    use sha2::{Digest, Sha256};

    let mut verifier_bytes = [0u8; 32];
    getrandom::getrandom(&mut verifier_bytes).unwrap_or_else(|_| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut state = now.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        for b in verifier_bytes.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *b = (state >> 56) as u8;
        }
    });

    let verifier = base64_url_encode(verifier_bytes.to_vec());
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64_url_encode(hash.to_vec());

    (verifier, challenge)
}

fn token_from_cache(cached: Option<String>) -> Option<Result<String, String>> {
    cached.map(|j| parse_token(&j))
}

fn parse_token(json: &str) -> Result<String, String> {
    let val: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("parse error: {e}"))?;

    let access_token = val["access_token"]
        .as_str()
        .ok_or_else(|| "missing access_token".to_string())?;

    Ok(access_token.to_string())
}

fn http_get_with_token(url: &str, token: &str) -> Result<String, String> {
    let url_json = serde_json::to_string(url).unwrap_or_default();
    let token = token.to_string();
    let payload = format!(
        "{{\"url\":{},\"body\":\"\",\"__stui_headers\":{{\"Authorization\":\"Bearer {}\"}}}}",
        url_json, token
    );

    #[cfg(target_arch = "wasm32")]
    {
        extern "C" {
            fn stui_http_post(ptr: *const u8, len: i32) -> i64;
        }
        let packed = unsafe { stui_http_post(payload.as_ptr(), payload.len() as i32) };
        if packed == 0 {
            return Err("http request failed".into());
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
        let _ = (url, token, payload);
        Err("http_get_with_token only available in WASM context".into())
    }
}

#[derive(Debug, serde::Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

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

            let title = if artist.is_empty() {
                name.clone()
            } else {
                format!("{} — {}", name, artist)
            };

            let duration_ms = track["duration_ms"].as_u64().unwrap_or(0);
            let duration_str = format_duration_ms(duration_ms);

            let poster_url = track["album"]["images"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|img| img["url"].as_str())
                .map(String::from);

            Some(PluginEntry {
                id: format!("spotify:track:{}", id),
                title,
                year: Some(duration_str),
                genre: None,
                rating: None,
                description: Some(album.to_string()),
                poster_url,
                imdb_id: None,
            })
        })
        .collect()
}

fn extract_spotify_track_id(url: &str) -> Option<String> {
    // Handle URLs like https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT
    if let Some(pos) = url.find("track/") {
        let rest = &url[pos + 6..]; // "track/" is 6 bytes
        let id: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !id.is_empty() && id.len() > 10 {
            return Some(id);
        }
    }
    None
}

fn base64_url_encode(data: Vec<u8>) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut result = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as usize;
        let b1 = if i + 1 < data.len() { data[i + 1] as usize } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] as usize } else { 0 };
        result.push(ALPHABET[(b0 >> 2)] as char);
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

fn format_duration_ms(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = secs / 60;
    let remaining_secs = secs % 60;
    format!("{}:{:02}", mins, remaining_secs)
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
    #[test]
    fn test_extract_spotify_track_id() {
        assert_eq!(
            super::extract_spotify_track_id(
                "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT"
            ),
            Some("4cOdK2wGLETKBW3PvgPWqT".to_string())
        );
    }

    #[test]
    fn test_format_duration_ms() {
        assert_eq!(super::format_duration_ms(0), "0:00");
        assert_eq!(super::format_duration_ms(30000), "0:30");
        assert_eq!(super::format_duration_ms(180000), "3:00");
        assert_eq!(super::format_duration_ms(214000), "3:34");
    }
}
