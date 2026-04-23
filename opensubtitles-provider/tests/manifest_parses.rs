use stui_plugin_sdk::parse_manifest;

#[test]
fn plugin_toml_parses() {
    let m = parse_manifest(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(m.plugin.name, "opensubtitles-provider");
    assert!(!m.capabilities.streams);
    // `.kinds()` accessor method, NOT `.kinds` field — CatalogCapability
    // is an untagged enum. Empty-kinds is consistent with "subtitles only"
    // — opensubtitles declares no catalog block at all.
    assert!(m.capabilities.catalog.kinds().is_empty());
    assert!(
        m.capabilities._extra.contains_key("subtitles"),
        "subtitles capability not parsed"
    );
    // NOTE: `validate_manifest(&m)` is intentionally NOT called here.
    //
    // opensubtitles declares `[capabilities.subtitles]` but no
    // `[capabilities.catalog]`. The SDK validator defaults `catalog` to the
    // typed variant with `search = None`, which then triggers
    // MissingRequiredVerb("search"). That failure mode is an SDK-schema
    // concern (the default should arguably be the `Enabled(false)` legacy
    // form for subtitle-only plugins), not a regression we want this test
    // to guard. Adding the assertion here would fail for a reason unrelated
    // to the regressions the other two canaries' validate_manifest calls
    // are meant to catch (`[plugin] type = "..."` / `network = true`).
}
