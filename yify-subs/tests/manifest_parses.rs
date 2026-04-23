use stui_plugin_sdk::parse_manifest;

#[test]
fn plugin_toml_parses() {
    let m = parse_manifest(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(m.plugin.name, "yify-subs");
    assert!(!m.capabilities.streams);
    assert!(m.capabilities.catalog.kinds().is_empty());
    assert!(
        m.capabilities._extra.contains_key("subtitles"),
        "subtitles capability not parsed",
    );
    stui_plugin_sdk::validate_manifest(&m)
        .expect("manifest validates under the current SDK schema");
}
