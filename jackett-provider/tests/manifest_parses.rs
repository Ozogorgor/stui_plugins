use stui_plugin_sdk::{parse_manifest, EntryKind};

#[test]
fn plugin_toml_parses() {
    let m = parse_manifest(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(m.plugin.name, "jackett-provider");
    assert!(m.capabilities.streams, "streams capability missing");
    assert_eq!(
        m.capabilities.catalog.kinds(),
        &[EntryKind::Movie, EntryKind::Series],
    );
    stui_plugin_sdk::validate_manifest(&m).expect("manifest validates");
}
