// Qobuz plugin for stui
//
// Uses app_id scraped from Qobuz web player and user credentials for auth.
// Requires a Qobuz subscription.
//
// Flow:
// 1. First run - scrape app_id from play.qobuz.com, login with email/password
// 2. Cache user_auth_token for subsequent calls
// 3. search() - use Qobuz catalog API
// 4. resolve() - use Qobuz file/url API for stream URL

use stui_plugin_sdk::{
    cache_get, cache_set, http_get, http_post_form, PluginEntry, PluginResult, PluginType,
    ResolveRequest, ResolveResponse, SearchRequest, SearchResponse, StuiPlugin,
};

const BASE_URL: &str = "https://www.qobuz.com/api.json/0.2/";

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
        let (app_id, user_token) = match ensure_authenticated() {
            Ok((id, token)) => (id, token),
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

        let body = match http_get_with_auth(&url, &app_id, &user_token) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("search_failed", e),
        };

        let items = parse_search_results(&body);
        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let (app_id, user_token) = match ensure_authenticated() {
            Ok((id, token)) => (id, token),
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

        // Get track URL from Qobuz API
        let url = format!(
            "{}file/url?track_id={}&format_id=27&intent=stream&app_id={}&user_auth_token={}",
            BASE_URL, track_id, app_id, user_token
        );

        let body = match http_get_with_auth(&url, &app_id, &user_token) {
            Ok(b) => b,
            Err(e) => return PluginResult::err("resolve_failed", e),
        };

        let stream_url = match parse_track_url(&body) {
            Some(url) => url,
            None => return PluginResult::err("resolve_failed", "no stream URL found"),
        };

        PluginResult::ok(ResolveResponse {
            stream_url,
            quality: Some("24bit".into()),
            subtitles: vec![],
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AuthCredentials {
    app_id: String,
    user_token: String,
    user_id: i64,
}

fn ensure_authenticated() -> Result<(String, String), String> {
    if let Some(cached) = cache_get("qobuz_auth") {
        if let Ok(creds) = serde_json::from_str::<AuthCredentials>(&cached) {
            return Ok((creds.app_id, creds.user_token));
        }
    }

    let credentials = fetch_credentials()?;
    let json = serde_json::to_string(&credentials).map_err(|e| e.to_string())?;
    cache_set("qobuz_auth", &json);

    Ok((credentials.app_id, credentials.user_token))
}

fn fetch_credentials() -> Result<AuthCredentials, String> {
    let app_id = get_app_id()?;

    let username = cache_get("qobuz_username")
        .ok_or_else(|| "Qobuz username not set. Configure via stui config.")?;
    let password = cache_get("qobuz_password")
        .ok_or_else(|| "Qobuz password not set. Configure via stui config.")?;

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
        .ok_or_else(|| "missing user_auth_token".to_string())?
        .to_string();

    let user_id = val["user"]["id"]
        .as_i64()
        .ok_or_else(|| "missing user id".to_string())?;

    Ok(AuthCredentials {
        app_id,
        user_token,
        user_id,
    })
}

fn get_app_id() -> Result<String, String> {
    if let Some(cached) = cache_get("qobuz_app_id") {
        return Ok(cached);
    }

    let html =
        http_get("https://play.qobuz.com/login").map_err(|e| format!("fetch login page: {}", e))?;

    let bundle_regex = regex::Regex::new(
        r#"<script src="(/resources/\d+\.\d+\.\d+-[a-z0-9]+/bundle\.js)"></script>"#,
    )
    .map_err(|e| e.to_string())?;

    let bundle_path = bundle_regex
        .captures(&html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
        .ok_or_else(|| "bundle path not found".to_string())?;

    let bundle_url = format!("https://play.qobuz.com{}", bundle_path);
    let bundle_html = http_get(&bundle_url).map_err(|e| format!("fetch bundle: {}", e))?;

    let app_id_regex =
        regex::Regex::new(r#"production:\{api:\{appId:"(\d{9})",appSecret:"(\w{32})""#)
            .map_err(|e| e.to_string())?;

    let app_id = app_id_regex
        .captures(&bundle_html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .ok_or_else(|| "app_id not found in bundle".to_string())?;

    cache_set("qobuz_app_id", &app_id);

    Ok(app_id)
}

fn http_get_with_auth(url: &str, _app_id: &str, _user_token: &str) -> Result<String, String> {
    // Auth params (app_id, user_auth_token) are already embedded in the URL
    // by the callers. This function exists as a thin wrapper to make it easy
    // to add header-based auth in future if the Qobuz API requires it.
    http_get(url)
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

    // Parse tracks
    if let Some(tracks) = val["tracks"]["items"].as_array() {
        for track in tracks {
            let id = track["id"].as_u64().unwrap_or(0).to_string();
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
                id,
                title: if artist.is_empty() {
                    title.clone()
                } else {
                    format!("{} — {}", title, artist)
                },
                year: Some(format_duration(duration)),
                genre: track["genre"].as_str().map(String::from),
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

    // Parse albums
    if let Some(albums) = val["albums"]["items"].as_array() {
        for album in albums {
            let id = album["id"].as_u64().unwrap_or(0).to_string();
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
                id,
                title: if artist.is_empty() {
                    title.clone()
                } else {
                    format!("{} — {}", title, artist)
                },
                year,
                genre: album["genre"].as_str().map(String::from),
                rating: None,
                description: None,
                poster_url: image.map(String::from),
                imdb_id: None,
            });
        }
    }

    items
}

fn parse_track_url(body: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(body).ok()?;

    // Try different response formats
    if let Some(url) = val["url"].as_str() {
        if !url.is_empty() {
            return Some(url.to_string());
        }
    }

    if let Some(url) = val["file_url"].as_str() {
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

stui_plugin_sdk::stui_export_plugin!(Qobuz);

#[cfg(test)]
mod tests {
    #[test]
    fn test_urlencoding() {
        assert_eq!(super::urlencoding("test"), "test");
        assert_eq!(super::urlencoding("test user"), "test%20user");
        assert_eq!(super::urlencoding("test@example.com"), "test%40example.com");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(super::format_duration(0), "0:00");
        assert_eq!(super::format_duration(30), "0:30");
        assert_eq!(super::format_duration(90), "1:30");
        assert_eq!(super::format_duration(3661), "61:01");
    }
}
