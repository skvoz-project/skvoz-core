#[test]
fn sdk_manifest_resolves_inheritance_without_losing_local_features() {
    let workspace: toml::Table = r#"
        [package]
        edition = "2024"
        [dependencies]
        serde = { version = "=1.0.228", features = ["std"], default-features = false }
    "#
    .parse()
    .unwrap();
    let source = r#"
        [package]
        name = "consumer"
        edition.workspace = true
        [dependencies]
        serde = { workspace = true, features = ["derive"], optional = true }
        [build-dependencies]
        serde = { workspace = true }
        [dev-dependencies]
        private-fixture = { path = "../private" }
        [lib]
        path = "lib.rs"
        [[example]]
        name = "local-only"
        path = "../local-only.rs"
    "#
    .parse()
    .unwrap();
    let exported: toml::Table = super::sdk_manifest(source, &workspace)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(exported["package"]["edition"].as_str(), Some("2024"));
    assert_eq!(exported["lib"]["path"].as_str(), Some("lib.rs"));
    assert!(exported.get("dev-dependencies").is_none());
    assert!(exported.get("example").is_none());
    let dependency = &exported["dependencies"]["serde"];
    assert_eq!(dependency["version"].as_str(), Some("=1.0.228"));
    assert_eq!(dependency["default-features"].as_bool(), Some(false));
    assert_eq!(dependency["optional"].as_bool(), Some(true));
    assert_eq!(
        dependency["features"].as_array().unwrap(),
        &[toml::Value::from("std"), toml::Value::from("derive")]
    );
    assert!(dependency.get("workspace").is_none());
    assert_eq!(
        exported["build-dependencies"]["serde"]["features"]
            .as_array()
            .unwrap(),
        &[toml::Value::from("std")]
    );
}
