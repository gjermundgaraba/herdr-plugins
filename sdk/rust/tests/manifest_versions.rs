//! Every plugin manifest must move versions together with its crate.
//! The version is display-only for daemon plugins (their handshakes compare
//! binary hashes), but drift confuses `herdr plugin list` and releases.

use std::{fs, path::Path};

#[test]
fn plugin_manifests_match_their_crate_versions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = read_toml(&root.join("Cargo.toml"));
    let members = workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members");
    assert!(!members.is_empty());

    // Filtered builds (the Nix flake trims the workspace to one plugin's
    // crates) still check every member they carry.
    for member in members {
        let member = root.join(member.as_str().expect("member path"));
        let manifest = member.join("herdr-plugin.toml");
        if manifest.is_file() {
            assert_eq!(
                version(&manifest),
                version(&member.join("Cargo.toml")),
                "{} and its Cargo.toml versions must move together",
                manifest.display()
            );
        }
    }
}

fn read_toml(path: &Path) -> toml::Value {
    let contents = fs::read_to_string(path).unwrap();
    toml::from_str(&contents).unwrap()
}

fn version(path: &Path) -> String {
    let parsed = read_toml(path);
    let table = match parsed.get("package") {
        Some(package) => package,
        None => &parsed,
    };
    table
        .get("version")
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("{} declares no version", path.display()))
        .to_owned()
}
