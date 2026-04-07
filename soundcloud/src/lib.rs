// Note: do NOT use #![no_std]. WASM binary size is controlled by Cargo.toml
// profile settings (opt-level = "z", panic = "abort"). Using std enables
// host-side `cargo test` without cfg tricks or feature flags.

// SoundCloud plugin using yt-dlp for searching and streaming.
// SoundCloud disabled OAuth in 2009, so we use yt-dlp to extract stream URLs.

use stui_plugin_sdk::{
    exec, PluginEntry, PluginResult, PluginType, ResolveRequest, ResolveResponse, SearchRequest,
    SearchResponse, StuiPlugin,
};

const YTDLP_TIMEOUT_MS: u32 = 30000;

#[derive(Default)]
pub struct SoundCloud;

impl StuiPlugin for SoundCloud {
    fn name(&self) -> &str {
        "soundcloud"
    }
    fn version(&self) -> &str {
        "0.2.0"
    }
    fn plugin_type(&self) -> PluginType {
        PluginType::Provider
    }

    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let query = req.query.trim();
        if query.is_empty() {
            return PluginResult::ok(SearchResponse {
                items: vec![],
                total: 0,
            });
        }

        // Use yt-dlp's SoundCloud search extractor: "scsearchN:query"
        // (ytsearchN: searches YouTube — the wrong site)
        let search_url = format!("scsearch20:{}", query);

        let output = match exec(
            "yt-dlp",
            &["--flat-playlist", "-j", &search_url],
            YTDLP_TIMEOUT_MS,
        ) {
            Ok(o) => o,
            Err(e) => return PluginResult::err("search_failed", e),
        };

        let items = parse_ytdlp_search_results(&output);
        let total = items.len() as u32;
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let entry_id = req.entry_id.trim();
        if entry_id.is_empty() {
            return PluginResult::err("resolve_failed", "empty entry_id");
        }

        // entry_id should be a SoundCloud URL
        // Use yt-dlp to get the direct stream URL
        let output = match exec("yt-dlp", &["-j", "--", entry_id], YTDLP_TIMEOUT_MS) {
            Ok(o) => o,
            Err(e) => return PluginResult::err("resolve_failed", e),
        };

        // Parse JSON output to get the stream URL
        let stream_url = match parse_ytdlp_resolve(&output) {
            Some(url) => url,
            None => {
                // Fallback: return the original URL and let mpv+yt-dlp handle it
                entry_id.to_string()
            }
        };

        PluginResult::ok(ResolveResponse {
            stream_url,
            quality: Some("audio".into()),
            subtitles: vec![],
        })
    }
}

fn parse_ytdlp_search_results(output: &str) -> Vec<PluginEntry> {
    let mut items = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Check if this is a SoundCloud result
        let url = val["webpage_url"].as_str().unwrap_or("");
        if !url.contains("soundcloud.com") {
            continue;
        }

        let id = url.to_string();
        let title = val["title"].as_str().unwrap_or("Unknown").to_string();
        let artist = val["uploader"].as_str().unwrap_or("").to_string();

        let title = if artist.is_empty() {
            title
        } else {
            format!("{} — {}", title, artist)
        };

        let description = val["description"].as_str().map(String::from);
        let genre = val["categories"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str())
            .map(String::from);

        // upload_date is "YYYYMMDD" — take the first 4 chars as year
        let year = val["upload_date"]
            .as_str()
            .filter(|d| d.len() >= 4)
            .map(|d| d[..4].to_string());

        let poster_url = val["thumbnail"].as_str().map(String::from).or_else(|| {
            val["thumbnails"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                .map(String::from)
        });

        items.push(PluginEntry {
            id,
            title,
            year,
            genre,
            rating: None,
            description,
            poster_url,
            imdb_id: None,
            duration: None,
        });
    }

    items
}

fn parse_ytdlp_resolve(output: &str) -> Option<String> {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        // Top-level `url` is set when there is exactly one format — check first.
        if let Some(url) = val["url"].as_str() {
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }

        // For multi-format outputs (SoundCloud typically has HLS + progressive),
        // traverse `formats[]` and prefer progressive MP3 over HLS so MPD can
        // play without needing HLS demuxer support.
        if let Some(formats) = val["formats"].as_array() {
            // Pass 1: progressive (http/https) audio-only
            for fmt in formats.iter().rev() {
                let protocol = fmt["protocol"].as_str().unwrap_or("");
                let vcodec = fmt["vcodec"].as_str().unwrap_or("none");
                if vcodec == "none" && matches!(protocol, "https" | "http") {
                    if let Some(url) = fmt["url"].as_str() {
                        if !url.is_empty() {
                            return Some(url.to_string());
                        }
                    }
                }
            }
            // Pass 2: any audio-only format (includes HLS)
            for fmt in formats.iter().rev() {
                let vcodec = fmt["vcodec"].as_str().unwrap_or("none");
                if vcodec == "none" {
                    if let Some(url) = fmt["url"].as_str() {
                        if !url.is_empty() {
                            return Some(url.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

fn format_duration(seconds: u64) -> String {
    let mins = seconds / 60;
    let secs = seconds % 60;
    if mins > 0 {
        format!("{}:{:02}", mins, secs)
    } else {
        format!("0:{:02}", secs)
    }
}

stui_plugin_sdk::stui_export_plugin!(SoundCloud);

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_ytdlp_resolve_with_url() {
        let json = r#"{"url":"https://example.com/stream.mp3","title":"Test"}"#;
        let result = super::parse_ytdlp_resolve(json);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "https://example.com/stream.mp3");
    }

    #[test]
    fn test_parse_ytdlp_resolve_fallback() {
        let json = r#"{"webpage_url":"https://soundcloud.com/artist/track"}"#;
        let result = super::parse_ytdlp_resolve(json);
        assert!(result.is_some());
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(super::format_duration(0), "0:00");
        assert_eq!(super::format_duration(30), "0:30");
        assert_eq!(super::format_duration(90), "1:30");
        assert_eq!(super::format_duration(3661), "61:01");
    }
}
