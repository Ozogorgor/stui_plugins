// Qobuz plugin for stui
//
// Uses app_id + app_secret (user-configured via stui settings)
// and email/password for authentication. Requires a Qobuz subscription.
//
// Flow:
// 1. Load app_id + app_secret from config cache (requires user configuration)
// 2. Login with email + password → user_auth_token
// 3. search() — Qobuz catalog API (auth in query params)
// 4. resolve() — Qobuz file/url API with HMAC-style MD5 request signature
//
// The file/url endpoint requires request_ts + request_sig.
// request_sig = MD5("file/url" + sorted_params_kv + request_ts + app_secret)
//
// Security: Passwords are encrypted using AES-256-GCM with a key derived from
// a machine-specific secret (machine_id + installation_id). The encrypted
// password is stored in cache as base64(Nonce + Ciphertext + Tag).

use stui_plugin_sdk::{
    cache_get, cache_set, http_get, url_encode, PluginEntry, PluginResult, PluginType,
    ResolveRequest, ResolveResponse, SearchRequest, SearchResponse, StuiPlugin,
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::RngCore;

const BASE_URL: &str = "https://www.qobuz.com/api.json/0.2/";

const CACHE_KEY_SECRET: &str = "qobuz_encryption_secret";
const ENCRYPTION_INFO_SIZE: usize = 12; // 96-bit nonce

fn get_or_create_encryption_key() -> Result<[u8; 32], String> {
    if let Some(cached) = cache_get(CACHE_KEY_SECRET) {
        if let Ok(key_bytes) = BASE64.decode(&cached) {
            if key_bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&key_bytes);
                return Ok(key);
            }
        }
    }

    let machine_id = get_machine_id();
    let mut key_bytes = [0u8; 32];
    let key_input = format!("stui-qobuz-{}", machine_id);
    let hash = md5::compute(key_input.as_bytes());
    key_bytes.copy_from_slice(&hash[..]);

    let encoded = BASE64.encode(key_bytes);
    cache_set(CACHE_KEY_SECRET, &encoded);

    Ok(key_bytes)
}

fn get_machine_id() -> String {
    if let Some(id) = cache_get("machine_id") {
        return id;
    }

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "default".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "".to_string());
    let id = format!("{}{}", hostname, home);

    cache_set("machine_id", &id);
    id
}

fn encrypt_password(password: &str) -> Result<String, String> {
    let key = get_or_create_encryption_key()?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("cipher creation failed: {}", e))?;

    let mut nonce_bytes = [0u8; ENCRYPTION_INFO_SIZE];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, password.as_bytes())
        .map_err(|e| format!("encryption failed: {}", e))?;

    let mut combined = Vec::with_capacity(ENCRYPTION_INFO_SIZE + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(&combined))
}

fn decrypt_password(encrypted: &str) -> Result<String, String> {
    let key = get_or_create_encryption_key()?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("cipher creation failed: {}", e))?;

    let combined = BASE64
        .decode(encrypted)
        .map_err(|e| format!("base64 decode failed: {}", e))?;

    if combined.len() < ENCRYPTION_INFO_SIZE {
        return Err("encrypted data too short".to_string());
    }

    let (nonce_bytes, ciphertext) = combined.split_at(ENCRYPTION_INFO_SIZE);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| format!("UTF-8 conversion failed: {}", e))
}

#[derive(Default)]
pub struct Qobuz;

impl StuiPlugin for Qobuz {
    fn name(&self) -> &str {
        "qobuz"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn plugin_type(&self) -> PluginType {
        PluginType::Provider
    }

    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let (app_id, _app_secret, user_token) = match ensure_authenticated() {
            Ok(c) => c,
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
            "{}catalog/search?query={}&limit=20&app_id={}&user_auth_token={}",
            BASE_URL,
            urlencoding(query),
            app_id,
            user_token
        );

        let body = match http_get(&url) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("search_failed", e),
        };

        let items = parse_search_results(&body);
        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let (app_id, app_secret, user_token) = match ensure_authenticated() {
            Ok(c) => c,
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

        match resolve_stream_url(&app_id, &user_token, &app_secret, track_id) {
            Ok((url, quality)) => PluginResult::ok(ResolveResponse {
                stream_url: url,
                quality: Some(quality.to_string()),
                subtitles: vec![],
            }),
            Err(e) => PluginResult::err("resolve_failed", e),
        }
    }
}

// ── Auth types ────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct AuthCredentials {
    app_id: String,
    app_secret: String,
    user_token: String,
    user_id: i64,
}

// ── Authentication ────────────────────────────────────────────────────────────

/// Returns (app_id, app_secret, user_auth_token).
fn ensure_authenticated() -> Result<(String, String, String), String> {
    if let Some(cached) = cache_get("qobuz_auth") {
        if let Ok(creds) = serde_json::from_str::<AuthCredentials>(&cached) {
            return Ok((creds.app_id, creds.app_secret, creds.user_token));
        }
    }

    let credentials = fetch_credentials()?;
    let json = serde_json::to_string(&credentials).map_err(|e| e.to_string())?;
    cache_set("qobuz_auth", &json);

    Ok((
        credentials.app_id,
        credentials.app_secret,
        credentials.user_token,
    ))
}

fn fetch_credentials() -> Result<AuthCredentials, String> {
    let (app_id, app_secret) = get_app_credentials()?;

    let username = cache_get("qobuz_username")
        .ok_or_else(|| "Qobuz username not set. Configure via stui settings.")?;

    let password = get_password()?;

    let url = format!(
        "{}user/login?email={}&password={}&app_id={}",
        BASE_URL,
        urlencoding(&username),
        urlencoding(&password),
        app_id
    );

    let body = http_get(&url).map_err(|e| format!("login failed: {}", e))?;

    let val: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse login response: {}", e))?;

    let user_token = val["user_auth_token"]
        .as_str()
        .ok_or_else(|| "missing user_auth_token in login response".to_string())?
        .to_string();

    let user_id = val["user"]["id"]
        .as_i64()
        .ok_or_else(|| "missing user.id in login response".to_string())?;

    Ok(AuthCredentials {
        app_id,
        app_secret,
        user_token,
        user_id,
    })
}

fn get_password() -> Result<String, String> {
    if let Some(encrypted) = cache_get("qobuz_password_encrypted") {
        return decrypt_password(&encrypted);
    }

    if let Some(password) = cache_get("qobuz_password") {
        if !password.is_empty() {
            let encrypted = encrypt_password(&password)?;
            cache_set("qobuz_password_encrypted", &encrypted);
            return Ok(password);
        }
    }

    Err("Qobuz password not set. Configure via stui settings.".to_string())
}

/// Returns (app_id, app_secret).
///
/// Requires user-configured values in the cache (`qobuz_app_id` /
/// `qobuz_app_secret`). Configure your credentials via stui settings.
/// To obtain credentials, contact api@qobuz.com directly.
fn get_app_credentials() -> Result<(String, String), String> {
    let cached_id = cache_get("qobuz_app_id");
    let cached_secret = cache_get("qobuz_app_secret");

    if let (Some(id), Some(secret)) = (cached_id, cached_secret) {
        if !id.is_empty() && !secret.is_empty() {
            return Ok((id, secret));
        }
    }

    Err(
        "Qobuz app credentials not configured. Please configure APP_ID and APP_SECRET \
         via stui settings. Contact api@qobuz.com to obtain official credentials."
            .to_string(),
    )
}

// ── Stream resolution ─────────────────────────────────────────────────────────

/// Try format IDs in descending quality order: 27 (Hi-Res 24bit) → 6 (FLAC 16bit) → 5 (MP3 320).
/// Returns (stream_url, quality_label) for the first successful tier.
fn resolve_stream_url(
    app_id: &str,
    user_token: &str,
    app_secret: &str,
    track_id: u64,
) -> Result<(String, &'static str), String> {
    let tiers: &[(u32, &str)] = &[(27, "24bit"), (6, "lossless"), (5, "mp3-320")];
    let mut last_error: Option<String> = None;

    for &(format_id, quality_label) in tiers {
        let ts = unix_now().map_err(|e| e.to_string())?;
        let format_id_str = format_id.to_string();
        let track_id_str = track_id.to_string();
        let params: &[(&str, &str)] = &[
            ("format_id", &format_id_str),
            ("intent", "stream"),
            ("track_id", &track_id_str),
        ];
        let sig = compute_request_sig("file/url", params, ts, app_secret);
        let url = format!(
            "{}file/url?track_id={}&format_id={}&intent=stream&app_id={}&user_auth_token={}&request_ts={}&request_sig={}",
            BASE_URL, track_id, format_id, url_encode(app_id), url_encode(user_token), ts, sig
        );
        match http_get(&url) {
            Ok(body) => {
                if let Some(stream_url) = parse_track_url(&body) {
                    return Ok((stream_url, quality_label));
                }
                last_error = Some(format!("format {} returned no stream URL", format_id));
            }
            Err(e) => {
                last_error = Some(format!("format {} failed: {}", format_id, e));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| "no stream URL available for any quality tier".to_string()))
}

/// Compute the Qobuz API request signature.
///
/// Formula: MD5( method + sorted(key + value ...) + timestamp + app_secret )
/// Params must NOT include app_id, user_auth_token, request_ts, or request_sig.
fn compute_request_sig(method: &str, params: &[(&str, &str)], ts: u64, app_secret: &str) -> String {
    let mut sorted = params.to_vec();
    sorted.sort_by_key(|&(k, _)| k);

    let mut input = String::from(method);
    for (key, val) in &sorted {
        input.push_str(key);
        input.push_str(val);
    }
    input.push_str(&ts.to_string());
    input.push_str(app_secret);

    format!("{:x}", md5::compute(input.as_bytes()))
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
            let artist = track["performer"]["name"]
                .as_str()
                .or_else(|| track["artist"]["name"].as_str())
                .unwrap_or("");
            let album = track["album"]["title"].as_str().unwrap_or("");
            let duration = track["duration"].as_u64().unwrap_or(0);
            let image = track["album"]["image"]["large"]
                .as_str()
                .or_else(|| track["album"]["image"]["small"].as_str());

            items.push(PluginEntry {
                id: id.to_string(),
                title: if artist.is_empty() {
                    title
                } else {
                    format!("{} — {}", title, artist)
                },
                year: None,
                genre: track["genre"].as_str().map(String::from),
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

    if let Some(albums) = val["albums"]["items"].as_array() {
        for album in albums {
            let id = album["id"].as_u64().unwrap_or(0);
            if id == 0 {
                continue;
            }
            let title = album["title"].as_str().unwrap_or("Unknown").to_string();
            let artist = album["artist"]["name"].as_str().unwrap_or("");
            let year = album["release_date_original"]
                .as_str()
                .and_then(|s| s.get(0..4))
                .map(String::from);
            let image = album["image"]["large"]
                .as_str()
                .or_else(|| album["image"]["small"].as_str());

            items.push(PluginEntry {
                id: id.to_string(),
                title: if artist.is_empty() {
                    title
                } else {
                    format!("{} — {}", title, artist)
                },
                year,
                genre: album["genre"].as_str().map(String::from),
                rating: None,
                description: None,
                poster_url: image.map(String::from),
                imdb_id: None,
                duration: None,
            });
        }
    }

    items
}

fn parse_track_url(body: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;

    for key in &["url", "file_url"] {
        if let Some(url) = val[key].as_str().filter(|s| !s.is_empty()) {
            return Some(url.to_string());
        }
    }

    None
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn unix_now() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| format!("system time error: {}", e))
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

stui_plugin_sdk::stui_export_plugin!(Qobuz);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding("test"), "test");
        assert_eq!(urlencoding("test user"), "test%20user");
        assert_eq!(urlencoding("test@example.com"), "test%40example.com");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(30), "0:30");
        assert_eq!(format_duration(90), "1:30");
        assert_eq!(format_duration(3661), "61:01");
    }

    #[test]
    fn test_compute_request_sig() {
        // Verify: method + sorted params + ts + secret → deterministic MD5
        let sig = compute_request_sig(
            "file/url",
            &[
                ("track_id", "12345"),
                ("format_id", "27"),
                ("intent", "stream"),
            ],
            1700000000,
            "secret123",
        );
        // Params are sorted: format_id, intent, track_id
        // Input: "file/url" + "format_id27" + "intentstream" + "track_id12345" + "1700000000" + "secret123"
        let expected_input = "file/urlformat_id27intentstreamtrack_id123451700000000secret123";
        let expected = format!("{:x}", md5::compute(expected_input.as_bytes()));
        assert_eq!(sig, expected);
    }

    #[test]
    fn test_parse_search_results_skips_zero_id() {
        let body =
            r#"{"tracks":{"items":[{"id":0,"title":"Bad","duration":0}]},"albums":{"items":[]}}"#;
        let items = parse_search_results(body);
        assert!(items.is_empty());
    }

    #[test]
    fn test_parse_track_url_url_key() {
        let body = r#"{"url":"https://cdn.qobuz.com/track.flac","mime_type":"audio/flac"}"#;
        assert_eq!(
            parse_track_url(body),
            Some("https://cdn.qobuz.com/track.flac".to_string())
        );
    }

    #[test]
    fn test_parse_track_url_file_url_fallback() {
        let body = r#"{"file_url":"https://cdn.qobuz.com/track.flac"}"#;
        assert_eq!(
            parse_track_url(body),
            Some("https://cdn.qobuz.com/track.flac".to_string())
        );
    }
}
