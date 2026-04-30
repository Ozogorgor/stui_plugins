// rjm-rating-weights — a stui plugin that applies custom weights to rating sources
//
// This plugin is a "rating weights" provider: it doesn't fetch metadata itself,
// but instead adjusts how existing ratings from other providers (OMDB, TMDB,
// MusicBrainz, Last.fm, RYM, etc.) are interpreted when displayed in the UI.
//
// The weights are applied multiplicatively to each provider's raw rating. For
// example, a weight of 2.0 doubles the perceived rating, while 0.5 halves it.
// This allows users to prioritize certain sources (e.g. RYM) over others
// (e.g. OMDB) in the Music/Browse grid.

use stui_plugin_sdk::prelude::*;
use stui_plugin_sdk::{parse_manifest, Plugin, PluginManifest, StuiPlugin, CatalogPlugin};

pub struct RatingWeightsPlugin {
    manifest: PluginManifest,
    // weights are interpreted as multipliers on the raw provider rating
    weights: std::collections::HashMap<String, f32>,
}

impl Default for RatingWeightsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl RatingWeightsPlugin {
    pub fn new() -> Self {
        let manifest: PluginManifest = parse_manifest(include_str!("../plugin.toml"))
            .expect("plugin.toml failed to parse at compile time");
        Self {
            manifest,
            weights: [
                ("omdb".to_string(), 1.0f32),
                ("tmdb".to_string(), 1.0),
                ("musicbrainz".to_string(), 1.0),
                ("lastfm".to_string(), 1.0),
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Adjust a raw rating by the configured weight, clamped to [0.0, 10.0].
    pub fn weighted_rating(&self, provider: &str, raw: f32) -> f32 {
        let weight = self.weights.get(provider).copied().unwrap_or(1.0);
        (raw * weight).clamp(0.0, 10.0)
    }

    /// Set a weight for a specific provider. Returns the previous weight, if any.
    pub fn set_weight(&mut self, provider: impl Into<String>, weight: f32) -> Option<f32> {
        self.weights.insert(provider.into(), weight)
    }

    /// Get a weight for a specific provider.
    pub fn get_weight(&self, provider: &str) -> Option<f32> {
        self.weights.get(provider).copied()
    }
}

impl Plugin for RatingWeightsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
}

impl CatalogPlugin for RatingWeightsPlugin {
    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::Ok(SearchResponse { items: vec![], total: 0 })
    }
}

#[allow(deprecated)]
impl StuiPlugin for RatingWeightsPlugin {
    fn name(&self) -> &str { "rjm-rating-weights" }
    fn version(&self) -> &str { "1.0.0" }
    fn plugin_type(&self) -> PluginType { PluginType::Metadata }

    fn search(&self, _req: SearchRequest) -> PluginResult<SearchResponse> {
        PluginResult::Ok(SearchResponse { items: vec![], total: 0 })
    }

    fn resolve(&self, _req: ResolveRequest) -> PluginResult<ResolveResponse> {
        PluginResult::Ok(ResolveResponse { stream_url: "".to_string(), quality: None, subtitles: vec![] })
    }
}

stui_export_plugin!(RatingWeightsPlugin);