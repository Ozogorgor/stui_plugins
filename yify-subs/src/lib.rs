//! yify-subs — stui plugin for YIFY Subtitles (yts-subs.com)
//!
//! YIFY provides subtitles for movies indexed by IMDB ID.
//! No API - uses web scraping.
//!
//! ## Site structure
//!   https://yts-subs.com/movie-imdb/{imdb}     → subtitles for a movie
//!   https://yts-subs.com/browse?search={query} → search results
//!   https://yts-subs.com/subtitles/{slug}      → download link
//!
//! ## Search flow
//!   - If query looks like IMDB ID (tt\d+), search by that
//!   - Otherwise search by title on browse page
//!
//! ## Resolve flow
//!   - Visit the subtitle page, extract download link

use stui_plugin_sdk::prelude::*;
use url::Url;

fn is_trusted_url(url_str: &str) -> bool {
    let Ok(url) = Url::parse(url_str) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.contains('@') || host.contains('%40') {
        return false;
    }
    let host_lower = host.to_lowercase();
    TRUSTED_DOMAINS
        .iter()
        .any(|d| host_lower == *d || host_lower == format!("www.{}", d))
}

const BASE_URL: &str = "https://yts-subs.com";
const TRUSTED_DOMAINS: &[&str] = &["yts-subs.com", "yifysubtitles.org"];

pub struct YifySubsProvider;

impl YifySubsProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for YifySubsProvider {
    fn default() -> Self {
        Self
    }
}

impl StuiPlugin for YifySubsProvider {
    fn name(&self) -> &str {
        "yify-subs"
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

        plugin_info!("yify-subs: searching '{}'", query);

        let items = if is_imdb_id(query) {
            search_by_imdb(query, req.limit)
        } else {
            search_by_title(query, req.limit)
        };

        let total = items.len() as u32;
        plugin_info!("yify-subs: found {} results", total);
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let entry_id = &req.entry_id;
        if entry_id.is_empty() {
            return PluginResult::err("INVALID_SLUG", "empty entry_id");
        }

        // Browse-page entries use the full movie URL as id; movie-page entries use a slug.
        // For movie URLs, fetch the page, pick the first subtitle slug, then resolve it.
        let subtitle_url = if entry_id.starts_with("http") {
            if !is_trusted_url(entry_id) {
                return PluginResult::err("INVALID_URL", "URL must be from yts-subs.com");
            }
            plugin_info!("yify-subs: resolving movie page {}", entry_id);
            let movie_html = match http_get(entry_id) {
                Ok(h) => h,
                Err(e) => return PluginResult::err("HTTP_ERROR", &e),
            };
            let entries = parse_movie_page(&movie_html, 1);
            match entries.into_iter().next() {
                Some(e) => format!("{}/subtitles/{}", BASE_URL, e.id),
                None => {
                    return PluginResult::err("NO_SUBTITLES", "no subtitles found for this title")
                }
            }
        } else {
            if !entry_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return PluginResult::err("INVALID_SLUG", "slug contains invalid characters");
            }
            format!("{}/subtitles/{}", BASE_URL, entry_id)
        };

        plugin_info!("yify-subs: resolving {}", subtitle_url);

        let html = match http_get(&subtitle_url) {
            Ok(h) => h,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let download_url = match extract_download_url(&html) {
            Some(url) => url,
            None => return PluginResult::err("PARSE_ERROR", "no download link found"),
        };

        PluginResult::ok(ResolveResponse {
            stream_url: download_url,
            quality: Some("yify-subs".to_string()),
            subtitles: vec![],
        })
    }
}

fn is_imdb_id(s: &str) -> bool {
    s.starts_with("tt") && s.len() >= 5 && s[2..].chars().all(|c| c.is_ascii_digit())
}

fn search_by_imdb(imdb: &str, limit: u32) -> Vec<PluginEntry> {
    let url = format!("{}/movie-imdb/{}", BASE_URL, imdb);
    let html = match http_get(&url) {
        Ok(h) => h,
        Err(e) => {
            plugin_warn!("yify-subs: failed to fetch {}: {}", url, e);
            return vec![];
        }
    };

    parse_movie_page(&html, limit)
}

fn search_by_title(query: &str, limit: u32) -> Vec<PluginEntry> {
    let encoded = url_encode(query);
    let url = format!("{}/browse?search={}", BASE_URL, encoded);
    let html = match http_get(&url) {
        Ok(h) => h,
        Err(e) => {
            plugin_warn!("yify-subs: failed to fetch {}: {}", url, e);
            return vec![];
        }
    };

    parse_browse_page(&html, limit)
}

fn parse_movie_page(html: &str, limit: u32) -> Vec<PluginEntry> {
    let mut entries = Vec::new();

    for line in html.lines() {
        let line = line.trim();

        if line.contains("/subtitles/") && line.contains("download") {
            if let Some(slug) = extract_subtitle_slug(line) {
                if let Some(lang) = extract_language(line) {
                    let title = extract_subtitle_title(line).unwrap_or_else(|| slug.clone());

                    entries.push(PluginEntry {
                        id: slug,
                        title,
                        year: None,
                        genre: None,
                        rating: None,
                        description: Some(lang),
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

fn parse_browse_page(html: &str, limit: u32) -> Vec<PluginEntry> {
    let mut entries = Vec::new();

    for line in html.lines() {
        let line = line.trim();

        if line.contains("/movie-imdb/") && line.contains("<a") {
            if let Some(imdb) = extract_imdb_id(line) {
                let title = extract_movie_title(line).unwrap_or_else(|| imdb.clone());

                entries.push(PluginEntry {
                    id: format!("{}/movie-imdb/{}", BASE_URL, imdb),
                    title,
                    year: None,
                    genre: None,
                    rating: None,
                    description: None,
                    poster_url: None,
                    imdb_id: Some(imdb),
                    duration: None,
                });

                if limit > 0 && entries.len() >= limit as usize {
                    break;
                }
            }
        }
    }

    entries
}

fn extract_download_url(html: &str) -> Option<String> {
    for line in html.lines() {
        let line = line.trim();
        if line.contains("download") && line.contains("href=") {
            if let Some(url) = extract_href(line) {
                if url.ends_with(".zip") || url.ends_with(".rar") || url.contains("/download/") {
                    let full_url = if url.starts_with("http") {
                        url.clone()
                    } else if url.starts_with("//") {
                        format!("https:{}", url)
                    } else if url.starts_with('/') {
                        format!("{}{}", BASE_URL, url)
                    } else {
                        format!("{}/{}", BASE_URL, url)
                    };
                    if is_trusted_url(&full_url) {
                        return Some(full_url);
                    }
                }
            }
        }
    }
    None
}

fn extract_subtitle_slug(line: &str) -> Option<String> {
    if let Some(start) = line.find("/subtitles/") {
        let rest = &line[start + 11..];
        if let Some(end) = rest.find('"') {
            let slug = &rest[..end];
            if slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Some(slug.to_string());
            }
        } else if let Some(end) = rest.find(' ') {
            let slug = &rest[..end];
            if slug
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Some(slug.to_string());
            }
        }
    }
    None
}

fn extract_language(line: &str) -> Option<String> {
    for part in line.split('<') {
        if part.contains('>') {
            let tag_content = part.split('>').last()?;
            if !tag_content.is_empty() && tag_content.len() < 30 {
                let lang = tag_content.trim();
                if !lang.is_empty() && !lang.contains("download") {
                    return Some(lang.to_string());
                }
            }
        }
    }
    None
}

fn extract_subtitle_title(line: &str) -> Option<String> {
    for part in line.split('<') {
        if let Some(end) = part.find('>') {
            let content = &part[end + 1..];
            if !content.is_empty()
                && !content.contains("download")
                && !content.chars().all(|c| c.is_whitespace())
            {
                return Some(content.trim().to_string());
            }
        }
    }
    None
}

fn extract_imdb_id(line: &str) -> Option<String> {
    if let Some(start) = line.find("/movie-imdb/tt") {
        let rest = &line[start + 14..]; // skip the full "/movie-imdb/tt" prefix (14 bytes)
        let mut id = String::new();
        for c in rest.chars() {
            if c.is_ascii_digit() {
                id.push(c);
            } else {
                break;
            }
        }
        if !id.is_empty() {
            return Some(format!("tt{}", id));
        }
    }
    None
}

fn extract_movie_title(line: &str) -> Option<String> {
    if let Some(start) = line.find('>') {
        let rest = &line[start + 1..];
        if let Some(end) = rest.find('<') {
            let title = rest[..end].trim().to_string();
            if !title.is_empty() {
                return Some(title);
            }
        }
    }
    None
}

fn extract_href(line: &str) -> Option<String> {
    if let Some(start) = line.find("href=\"") {
        let start = start + 6;
        if let Some(end) = line[start..].find('"') {
            return Some(line[start..start + end].to_string());
        }
    }
    None
}

stui_plugin_sdk::stui_export_plugin!(YifySubsProvider);
