//! subscene — stui plugin for subtitle search via subscene.com
//!
//! Subscene doesn't provide an official API, so this plugin uses web scraping.
//! The website structure requires:
//!   1. Search for a title → get list of subtitle pages
//!   2. Visit each subtitle page → get download link
//!   3. The download link points to a .zip containing .srt files
//!
//! ## Search flow
//!   GET https://www.subscene.com/subtitles/search?q={query}
//!   Parse HTML to extract: title, language, author, hearing_impaired, fps
//!
//! ## Resolve flow
//!   GET {subtitle_page_url}
//!   Parse HTML to extract download link
//!   Return the .zip URL - stui's aria2 will download it

use stui_plugin_sdk::prelude::*;

// ── Plugin struct ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct SubsceneProvider;

fn is_valid_subscene_url(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.contains('@') || authority.contains("%40") {
            return false;
        }
        return authority == "www.subscene.com" || authority == "subscene.com";
    }
    false
}

impl StuiPlugin for SubsceneProvider {
    fn name(&self) -> &str {
        "subscene"
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
            "https://www.subscene.com/subtitles/search?q={}",
            url_encode(query)
        );

        plugin_info!("subscene: searching '{}'", query);

        let html = match http_get(&url) {
            Ok(h) => h,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let items = parse_search_results(&html, req.limit);
        let total = items.len() as u32;

        plugin_info!("subscene: found {} results", total);
        PluginResult::ok(SearchResponse { items, total })
    }

    fn resolve(&self, req: ResolveRequest) -> PluginResult<ResolveResponse> {
        let url = &req.entry_id;
        if url.is_empty() {
            return PluginResult::err("INVALID_URL", "empty entry_id");
        }

        if !is_valid_subscene_url(url) {
            return PluginResult::err("INVALID_URL", "URL must be from subscene.com");
        }

        plugin_info!("subscene: resolving {}", url);

        let html = match http_get(url) {
            Ok(h) => h,
            Err(e) => return PluginResult::err("HTTP_ERROR", &e),
        };

        let download_url = match extract_download_link(&html) {
            Some(url) => url,
            None => return PluginResult::err("PARSE_ERROR", "could not find download link"),
        };

        PluginResult::ok(ResolveResponse {
            stream_url: download_url,
            quality: Some("subscene".to_string()),
            subtitles: vec![],
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_search_results(html: &str, limit: u32) -> Vec<PluginEntry> {
    let mut entries = Vec::new();

    let mut in_tr = false;
    let mut current_title = String::new();
    let mut current_lang = String::new();
    let mut current_url = String::new();
    let mut td_count = 0;
    let mut td_buffer = String::new();
    let mut in_td = false;

    for line in html.lines() {
        let line = line.trim();

        if line.starts_with("<tr") && line.contains("subtitle") {
            in_tr = true;
            current_title.clear();
            current_lang.clear();
            current_url.clear();
            td_count = 0;
            in_td = false;
            td_buffer.clear();
            continue;
        }

        if in_tr {
            if line.starts_with("<td") {
                in_td = true;
                td_buffer.clear();
                td_count += 1;
            }

            if in_td {
                td_buffer.push_str(line);
                td_buffer.push(' ');

                if line.contains("</td>") {
                    in_td = false;

                    if td_count == 1 {
                        if let Some(href) = extract_attr(&td_buffer, "href") {
                            current_url =
                                if href.starts_with("http://") || href.starts_with("https://") {
                                    href.to_string()
                                } else if href.starts_with('/') {
                                    format!("https://www.subscene.com{}", href)
                                } else {
                                    format!("https://www.subscene.com/{}", href)
                                };
                        }
                    } else if td_count == 2 {
                        if let Some(title) = extract_text_between(&td_buffer, "<span", '<') {
                            current_title = title.trim().to_string();
                        }
                    } else if td_count == 3 {
                        if let Some(lang) = extract_text_between(&td_buffer, "<span", '<') {
                            current_lang = lang.trim().to_string();
                        }
                    }

                    td_buffer.clear();
                }
            }

            if line == "</tr>" {
                if !current_url.is_empty() && !current_title.is_empty() {
                    entries.push(PluginEntry {
                        id: current_url.clone(),
                        title: current_title.clone(),
                        year: None,
                        genre: None,
                        rating: None,
                        description: if current_lang.is_empty() {
                            None
                        } else {
                            Some(current_lang.clone())
                        },
                        poster_url: None,
                        imdb_id: None,
                        duration: None,
                    });
                }
                in_tr = false;
                if limit > 0 && entries.len() >= limit as usize {
                    break;
                }
            }
        }
    }

    entries
}

fn extract_download_link(html: &str) -> Option<String> {
    for line in html.lines() {
        if line.contains("download") && line.contains("href") {
            if let Some(url) = extract_attr(line, "href") {
                if url.contains("/subtitle/") || url.ends_with(".zip") {
                    let full_url = if url.starts_with("http") {
                        url
                    } else if url.starts_with('/') {
                        format!("https://www.subscene.com{}", url)
                    } else {
                        format!("https://www.subscene.com/{}", url)
                    };
                    if is_valid_subscene_url(&full_url) {
                        return Some(full_url);
                    }
                }
            }
        }
    }
    None
}

fn extract_attr(html: &str, attr: &str) -> Option<String> {
    let pattern = format!("{}=\"", attr);
    if let Some(start) = html.find(&pattern) {
        let start = start + pattern.len();
        if let Some(end) = html[start..].find('"') {
            return Some(html[start..start + end].to_string());
        }
    }
    None
}

fn extract_text_between(html: &str, open_tag: &str, close_char: char) -> Option<String> {
    if let Some(start) = html.find(open_tag) {
        let rest = &html[start..];
        if let Some(gt) = rest.find('>') {
            let content = &rest[gt + 1..];
            if let Some(end) = content.find(close_char) {
                return Some(content[..end].to_string());
            }
        }
    }
    None
}

stui_plugin_sdk::stui_export_plugin!(SubsceneProvider);
