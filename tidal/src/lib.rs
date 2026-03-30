// Tidal plugin for stui
//
// Uses OAuth with public client_id for authentication.
// Requires a Tidal subscription.
//
// Flow:
// 1. Generate PKCE code_verifier and code_challenge
// 2. Open browser for OAuth login at listen.tidal.com
// 3. Exchange auth code for access_token/refresh_token
// 4. search() - use Tidal API
// 5. resolve() - use Tidal API for stream URL

use stui_plugin_sdk::{
    auth_allocate_port, auth_open_and_wait, cache_get, cache_set, http_post_form, plugin_info,
    PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse, SearchRequest,
    SearchResponse, StuiPlugin,
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

        let url = format!(
            "https://api.tidal.com/v1/search?limit=20&query={}",
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

        // entry_id should be a track ID
        let track_id = match entry_id.parse::<u32>() {
            Ok(id) => id,
            Err(_) => return PluginResult::err("resolve_failed", "invalid track id"),
        };

        // Get track URL from Tidal API
        let url = format!("https://api.tidal.com/v1/tracks/{}/streamUrl", track_id);

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

#[derive(serde::Serialize, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    token_type: String,
}

fn ensure_authenticated() -> Result<String, String> {
    if let Some(cached) = cache_get("tidal_token") {
        if let Ok(token) = serde_json::from_str::<TokenResponse>(&cached) {
            return Ok(token.access_token);
        }
    }

    let (code_verifier, code_challenge) = generate_pkce();

    let port = auth_allocate_port()?;
    let auth_url = build_auth_url(port, &code_challenge);

    plugin_info!("Opening Tidal auth URL");

    let cb = auth_open_and_wait(&auth_url, 120_000)?;
    // Pass both the dynamic port AND the verifier so exchange_code can build
    // the exact redirect_uri that was registered with the auth server.
    let token = exchange_code(&cb.code, &code_verifier, port)?;

    let json = serde_json::to_string(&token).map_err(|e| e.to_string())?;
    cache_set("tidal_token", &json);

    Ok(token.access_token)
}

fn build_auth_url(port: u16, code_challenge: &str) -> String {
    format!(
        "https://listen.tidal.com/login/auth?appMode=WEB&client_id={}&code_challenge={}&code_challenge_method=S256&lang=en&redirect_uri=https://127.0.0.1:{}/login&response_type=code&restrictSignup=true&scope={}&autoredirect=true",
        CLIENT_ID, code_challenge, port, SCOPES
    )
}

fn exchange_code(code: &str, code_verifier: &str, port: u16) -> Result<TokenResponse, String> {
    // redirect_uri MUST match the one used in the authorization request exactly.
    let redirect_uri = format!("https://127.0.0.1:{}/login", port);
    let body = format!(
        "client_id={}&code={}&code_verifier={}&grant_type=authorization_code&redirect_uri={}",
        CLIENT_ID, code, code_verifier, redirect_uri
    );

    let resp = http_post_form("https://login.tidal.com/oauth2/token", &body)
        .map_err(|e| format!("token exchange failed: {}", e))?;

    let token: TokenResponse =
        serde_json::from_str(&resp).map_err(|e| format!("parse token response: {}", e))?;

    Ok(token)
}

fn generate_pkce() -> (String, String) {
    use sha2::{Digest, Sha256};

    // 32 random bytes → 43-char base64url verifier (satisfies RFC 7636 §4.1)
    let mut verifier_bytes = [0u8; 32];
    getrandom::getrandom(&mut verifier_bytes).unwrap_or_else(|_| {
        // Fallback: mix nanosecond timestamp with a simple LCG — not ideal but
        // still unpredictable enough for a short-lived local PKCE verifier.
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

    // challenge = BASE64URL(SHA256(ASCII(verifier)))  — RFC 7636 §4.2, method S256
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64_url_encode(hash.to_vec());

    (verifier, challenge)
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

fn http_get_with_token(url: &str, token: &str) -> Result<String, String> {
    let payload = format!(
        "{{\"url\":{},\"body\":\"\",\"__stui_headers\":{{\"Authorization\":\"Bearer {}\"}}}}",
        serde_json::to_string(url).unwrap_or_default(),
        token
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

    let mut items = Vec::new();

    if let Some(tracks) = val["tracks"]["items"].as_array() {
        for track in tracks {
            let id = track["id"].as_u64().unwrap_or(0).to_string();
            let title = track["title"].as_str().unwrap_or("Unknown").to_string();
            let artist = track["artist"]["name"].as_str().unwrap_or("");
            let album = track["album"]["title"].as_str().unwrap_or("");
            let duration = track["duration"].as_u64().unwrap_or(0);
            let image = track["album"]["coverUrl"]
                .as_str()
                .or_else(|| track["album"]["cover"]["large"].as_str());

            items.push(PluginEntry {
                id,
                title: if artist.is_empty() {
                    title.clone()
                } else {
                    format!("{} — {}", title, artist)
                },
                year: Some(format_duration(duration)),
                genre: None,
                rating: None,
                description: if album.is_empty() {
                    None
                } else {
                    Some(album.to_string())
                },
                poster_url: image.map(String::from),
                imdb_id: None,
            });
        }
    }

    items
}

fn parse_stream_url(body: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;

    if let Some(url) = val["url"].as_str() {
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }

    if let Some(url) = val["streamUrl"].as_str() {
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }

    None
}

fn format_duration(seconds: u64) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    format!("{}:{:02}", mins, secs)
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
}
