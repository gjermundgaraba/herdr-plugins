//! Every plugin manifest must move versions together with its crate.
//! The version is display-only for daemon plugins (their handshakes compare
//! binary hashes), but drift confuses `herdr plugin list` and releases.

use std::{fs, path::Path};

#[test]
fn plugin_manifests_match_their_crate_versions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut checked = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = entry_name(&path);
            if !path.is_dir() || name == "target" || name.starts_with('.') {
                continue;
            }
            let manifest = path.join("herdr-plugin.toml");
            if manifest.is_file() {
                assert_eq!(
                    version(&manifest),
                    version(&path.join("Cargo.toml")),
                    "{} and its Cargo.toml versions must move together",
                    manifest.display()
                );
                checked.push(manifest);
            }
            pending.push(path);
        }
    }
    assert!(
        checked.len() >= 4,
        "expected to find the plugin manifests under {}, found {checked:?}",
        root.display()
    );
}

fn entry_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn version(path: &Path) -> String {
    let contents = fs::read_to_string(path).unwrap();
    let parsed: toml::Value = toml::from_str(&contents).unwrap();
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
