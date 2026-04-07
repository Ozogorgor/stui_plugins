//! kitsunekko — stui plugin for subtitle search via kitsunekko.net
//!
//! Kitsunekko is a free subtitle host with a simple directory structure.
//! No API key required - uses direct HTTP and HTML parsing.
//!
//! ## Site structure
//!   https://kitsunekko.net/subtitles/           - root index
//!   https://kitsunekko.net/subtitles/japanese/ - Japanese subtitles
//!   https://kitsunekko.net/subtitles/english/   - English subtitles
//!   https://kitsunekko.net/dirlist.php?dir=subtitles/japanese/{show}/ - browse show
//!
//! ## Search flow
//!   GET https://kitsunekko.net/dirlist.php?dir=subtitles/&search={query}
//!   Parse HTML to extract show directories
//!
//! ## Resolve flow
//!   GET https://kitsunekko.net/dirlist.php?dir=subtitles/{lang}/{show}/
//!   Parse HTML to extract .srt/.zip file links

use stui_plugin_sdk::prelude::*;

// ── Plugin struct ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct KitsunekkoProvider;

fn is_valid_kitsunekko_url(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.contains('@') || authority.contains("%40") {
            return false;
        }
        return authority == "kitsunekko.net" || authority == "www.kitsunekko.net";
    }
    false
}

impl StuiPlugin for KitsunekkoProvider {
    fn name(&self) -> &str {
        "kitsunekko"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn plugin_type(&self) -> PluginType {
        PluginType::Subtitle
    }

    fn search(&self, req: SearchRequest) -> PluginResult<SearchResponse> {
        let query = req.query.trim();
        if query.is_empty() {
            return PluginResult::ok(SearchResponse {
                items: vec![],
                total: 0,
            });
        }

        let url = format!(
            "https://kitsunekko.net/dirlist.php?dir=subtitles/&search={}",
            url_encode(query)
        );

        plugin_info!("kitsunekko: searching '{}'", query);

        let html = match http_get(&url) {
            Ok(h) => h,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let items = parse_search_results(&html, req.limit);
        let total = items.len() as u32;

        plugin_info!("kitsunekko: found {} results", total);
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let url = &req.entry_id;
        if url.is_empty() {
            return PluginResult::err("INVALID_URL", "empty entry_id");
        }

        if !is_valid_kitsunekko_url(url) {
            return PluginResult::err("INVALID_URL", "URL must be from kitsunekko.net");
        }

        plugin_info!("kitsunekko: resolving {}", url);

        let html = match http_get(url) {
            Ok(h) => h,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let files = extract_file_links(&html, url);

        if files.is_empty() {
            return PluginResult::err("PARSE_ERROR", "no subtitle files found");
        }

        let main_file = files.first().cloned().unwrap_or_default();

        PluginResult::ok(ResolveResponse {
            stream_url: main_file,
            quality: Some("kitsunekko".to_string()),
            subtitles: vec![],
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_search_results(html: &str, limit: u32) -> Vec<PluginEntry> {
    let mut entries = Vec::new();

    for line in html.lines() {
        let line = line.trim();

        if line.contains("dirlist.php") && line.contains("dir=subtitles") {
            if let Some(dir) = extract_dir_param(line) {
                let title = dir
                    .split('/')
                    .last()
                    .unwrap_or(&dir)
                    .replace('+', " ")
                    .replace('_', " ");

                if !title.is_empty() && !title.starts_with('.') {
                    let lang = if dir.contains("/japanese") {
                        "Japanese"
                    } else if dir.contains("/english") {
                        "English"
                    } else {
                        "Unknown"
                    };

                    let full_url = format!("https://kitsunekko.net/dirlist.php?dir={}", dir);

                    entries.push(PluginEntry {
                        id: full_url,
                        title: title.clone(),
                        year: None,
                        genre: None,
                        rating: None,
                        description: Some(lang.to_string()),
                        poster_url: None,
                        imdb_id: None,
                        duration: None,
                    });

                    if limit > 0 && entries.len() >= limit as usize {
                        break;
                    }
                }
            }
        }
    }

    entries
}

fn extract_file_links(html: &str, base_url: &str) -> Vec<String> {
    let mut files = Vec::new();

    // Extract the dir parameter for relative path resolution
    let dir_path = base_url
        .find("dir=")
        .map(|i| {
            let rest = &base_url[i + 4..];
            rest.find('&').map(|j| &rest[..j]).unwrap_or(rest)
        })
        .unwrap_or("");
    // Normalize: empty becomes ".", ensure single trailing slash for joining
    let dir_path = if dir_path.is_empty() { "." } else { dir_path };

    for line in html.lines() {
        let line = line.trim();

        if line.contains("href") {
            if let Some(url) = extract_href(line) {
                if !url.ends_with(".srt") && !url.ends_with(".zip") && !url.ends_with(".7z") {
                    continue;
                }

                let full_url = if url.starts_with("http") {
                    url.to_string()
                } else if url.starts_with('/') {
                    format!("https://kitsunekko.net{}", url)
                } else if url.starts_with("../") {
                    let depth = url.matches("../").count();
                    let rest = url.trim_start_matches("../");
                    let mut path = dir_path.trim_end_matches('/');
                    for _ in 0..depth {
                        if let Some(last) = path.rfind('/') {
                            path = &path[..last];
                        }
                    }
                    if rest.contains('.') {
                        format!("https://kitsunekko.net/{}/{}", path, rest)
                    } else {
                        format!("https://kitsunekko.net/dirlist.php?dir={}/{}", path, rest)
                    }
                } else if url.starts_with("./") {
                    let base = dir_path.trim_end_matches('/');
                    let rest = url.trim_start_matches("./");
                    if rest.contains('.') {
                        format!("https://kitsunekko.net/{}/{}", base, rest)
                    } else {
                        format!("https://kitsunekko.net/dirlist.php?dir={}/{}", base, rest)
                    }
                } else {
                    if url.contains('.') {
                        format!("https://kitsunekko.net/{}/{}", dir_path, url)
                    } else {
                        format!(
                            "https://kitsunekko.net/dirlist.php?dir={}/{}",
                            dir_path, url
                        )
                    }
                };

                // SSRF check: validate the resolved URL
                if !is_valid_kitsunekko_url(&full_url) {
                    continue;
                }

                if !files.contains(&full_url) {
                    files.push(full_url);
                }
            }
        }
    }

    files
}

fn extract_dir_param(html: &str) -> Option<String> {
    let pattern = "dir=";
    if let Some(start) = html.find(pattern) {
        let start = start + pattern.len();
        let rest = &html[start..];
        let end = rest
            .find(|c| c == '&' || c == '"' || c == ' ' || c == '>')
            .unwrap_or(rest.len());
        let value = &rest[..end];
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn extract_href(html: &str) -> Option<String> {
    let pattern = "href=\"";
    if let Some(start) = html.find(pattern) {
        let start = start + pattern.len();
        if let Some(end) = html[start..].find('"') {
            return Some(html[start..start + end].to_string());
        }
    }
    None
}

stui_plugin_sdk::stui_export_plugin!(KitsunekkoProvider);
