// Roon plugin for stui
//
// Controls Roon audio server on local network.
// Roon uses DNS-SD for discovery and token-based authentication.
//
// IMPORTANT: Roon is a player controller, not a streaming source.
// This plugin provides integration with Roon's browse/transport services
// for library search and playback control.

use stui_plugin_sdk::{
    cache_get, cache_set, http_get, PluginEntry, PluginResult, PluginType, ResolveRequest,
    ResolveResponse, SearchRequest, SearchResponse, StuiPlugin,
};

const ROON_APP_ID: &str = "stui_roon";
const ROON_APP_NAME: &str = "stui";
const ROON_DISCOVERY_PORT: u16 = 9330;

#[derive(Default)]
pub struct Roon;

impl StuiPlugin for Roon {
    fn name(&self) -> &str {
        "roon"
    }
    fn version(&self) -> &str {
        "0.2.0"
    }
    fn plugin_type(&self) -> PluginType {
        PluginType::Provider
    }

    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let config = match get_roon_config() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("not_configured", e),
        };

        let query = req.query.trim();
        if query.is_empty() {
            return PluginResult::ok(SearchResponse {
                items: vec![],
                total: 0,
            });
        }

        let items = search_roon_library(&config.server, &config.token, query);
        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let config = match get_roon_config() {
            Ok(c) => c,
            Err(e) => return PluginResult::err("not_configured", e),
        };

        let entry_id = req.entry_id.trim();
        if entry_id.is_empty() {
            return PluginResult::err("resolve_failed", "empty entry_id");
        }

        // entry_id format: "roon:{type}:{id}" e.g., "roon:track:12345"
        let (item_type, item_id) = match parse_roon_id(entry_id) {
            Some((t, i)) => (t, i),
            None => return PluginResult::err("resolve_failed", "invalid Roon ID format"),
        };

        let result = match queue_and_play(&config.server, &config.token, item_type, &item_id) {
            Ok(r) => r,
            Err(e) => return PluginResult::err("resolve_failed", e),
        };

        PluginResult::ok(ResolveResponse {
            stream_url: result.0,
            quality: Some(result.1),
            subtitles: vec![],
        })
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct RoonConfig {
    server: String,
    core_id: String,
    token: String,
    port: u16,
}

fn get_roon_config() -> Result<RoonConfig, String> {
    if let Some(cached) = cache_get("roon_config") {
        return serde_json::from_str(&cached).map_err(|e| format!("parse error: {}", e));
    }
    Err("Roon not configured. Please set up Roon server in stui settings.".to_string())
}

fn parse_roon_id(entry_id: &str) -> Option<(&str, &str)> {
    if !entry_id.starts_with("roon:") {
        return None;
    }

    let rest = &entry_id[5..];
    let parts: Vec<&str> = rest.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }

    Some((parts[0], parts[1]))
}

fn search_roon_library(server: &str, token: &str, query: &str) -> Vec<PluginEntry> {
    let config = get_roon_config().ok();
    let port = config.as_ref().map(|c| c.port).unwrap_or(ROON_DISCOVERY_PORT);
    let url = format!("http://{}:{}/api/browse", server, port);

    let payload = serde_json::json!({
        "browse_key": "",
        "input": {
            "search_type": "library_tracks",
            "search_query": query
        },
        "offset": 0,
        "limit": 20
    });

    let body = match make_roon_request(&url, token, &payload.to_string()) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };

    parse_browse_results(&body, server, port)
}

fn queue_and_play(
    server: &str,
    token: &str,
    item_type: &str,
    item_id: &str,
) -> Result<(String, String), String> {
    let config = get_roon_config().ok();
    let port = config.as_ref().map(|c| c.port).unwrap_or(ROON_DISCOVERY_PORT);
    let url = format!("http://{}:{}/api/queue_and_play", server, port);

    let action = match item_type {
        "track" => "play_tracks",
        "album" => "play_album",
        "artist" => "play_artist",
        "playlist" => "play_playlist",
        _ => "play_tracks",
    };

    let payload = serde_json::json!({
        "action": action,
        "item_key": item_id,
        "offset": 0
    });

    let _body = make_roon_request(&url, token, &payload.to_string())?;

    // Get current zone info
    let zones_url = format!("http://{}:{}/api/zones", server, port);
    let zones_body = make_roon_request(&zones_url, token, "{}").ok();

    // Return special URL indicating Roon is handling playback
    // stui will need to handle this specially - it's not a direct stream
    let stream_url = format!("roon://{}:{}", server, ROON_DISCOVERY_PORT);
    let quality = extract_quality_from_zones(&zones_body.unwrap_or_default());

    Ok((stream_url, quality))
}

fn extract_quality_from_zones(body: &str) -> String {
    // Parse zones response to get current quality
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(zones) = val["zones"].as_array() {
            if let Some(zone) = zones.first() {
                if let Some(playback_info) = zone["now_playing"].as_object() {
                    if let Some(quality) = playback_info.get("quality").and_then(|q| q.as_str()) {
                        return quality.to_string();
                    }
                }
            }
        }
    }
    "unknown".to_string()
}

fn make_roon_request(url: &str, token: &str, body: &str) -> Result<String, String> {
    let payload = format!(
        "{{\"url\":{},\"body\":{},\"__stui_headers\":{{\"Roon-Token\":\"{}\"}}}}",
        serde_json::to_string(url).unwrap_or_default(),
        serde_json::to_string(body).unwrap_or_default(),
        token
    );

    #[cfg(target_arch = "wasm32")]
    {
        extern "C" {
            fn stui_http_post(ptr: *const u8, len: i32) -> i64;
        }
        let packed = unsafe { stui_http_post(payload.as_ptr(), payload.len() as i32) };
        if packed == 0 {
            return Err("request failed".into());
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
        let _ = (url, token, body, payload);
        Err("make_roon_request only available in WASM context".into())
    }
}

#[derive(Debug, serde::Deserialize)]
struct HttpResponse {
    status: u16,
    body: String,
}

fn parse_browse_results(body: &str, server: &str, port: u16) -> Vec<PluginEntry> {
    let val: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut items = Vec::new();

    // Parse items from browse response
    // Roon returns items under "items" array
    if let Some(items_array) = val["items"].as_array() {
        for item in items_array {
            let item_type = item["item_type"].as_str().unwrap_or("");
            let id = item["item_key"].as_str().unwrap_or("");
            let title = item["title"].as_str().unwrap_or("Unknown").to_string();

            let subtitle = item["subtitle"].as_str().map(String::from);

            let image = item["image_key"]
                .as_str()
                .map(|k| format!("http://{}:{}/api/image/{}", server, port, k));

            let roon_type = match item_type {
                "track" => "track",
                "album" => "album",
                "artist" => "artist",
                "playlist" => "playlist",
                _ => continue,
            };

            let display_title = if let Some(ref sub) = subtitle {
                format!("{} — {}", title, sub)
            } else {
                title.clone()
            };

            items.push(PluginEntry {
                id: format!("roon:{}:{}", roon_type, id),
                title: display_title,
                year: None,
                genre: item["genre"].as_str().map(String::from),
                rating: None,
                description: subtitle,
                poster_url: image,
                imdb_id: None,
            });
        }
    }

    items
}

// Configuration functions - called from host to set up Roon

pub fn configure_roon(server: &str, core_id: &str, token: &str) {
    let config = RoonConfig {
        server: server.to_string(),
        core_id: core_id.to_string(),
        token: token.to_string(),
        port: ROON_DISCOVERY_PORT,
    };
    let json = serde_json::to_string(&config).unwrap_or_default();
    cache_set("roon_config", &json);
}

pub fn discover_roon() -> Result<Vec<String>, String> {
    // mDNS discovery would happen here
    // For now, return empty or cached server
    if let Some(cached) = cache_get("roon_server") {
        return Ok(vec![cached]);
    }
    Ok(Vec::new())
}

stui_plugin_sdk::stui_export_plugin!(Roon);

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_roon_id() {
        assert_eq!(
            super::parse_roon_id("roon:track:123"),
            Some(("track", "123"))
        );
        assert_eq!(
            super::parse_roon_id("roon:album:456"),
            Some(("album", "456"))
        );
        assert_eq!(
            super::parse_roon_id("roon:artist:789"),
            Some(("artist", "789"))
        );
        assert_eq!(super::parse_roon_id("invalid"), None);
        assert_eq!(super::parse_roon_id("roon:track"), None);
    }
}
