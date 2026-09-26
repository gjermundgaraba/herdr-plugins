// Saved group assignments for one Herdr session, keyed by stable workspace id.
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Longest group name accepted from the picker.
pub const MAX_GROUP_LEN: usize = 40;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub workspaces: BTreeMap<String, String>,
}

impl State {
    /// A missing file is an empty state; a corrupt one is an error so it is
    /// never silently overwritten.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Writes a sibling temp file and renames it over `path`.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = temp_path(path);
        let mut file = fs::File::create(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(self).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    }

    /// Distinct group names, sorted case-insensitively.
    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = self.workspaces.values().cloned().collect();
        groups.sort_by_key(|group| group.to_lowercase());
        groups.dedup();
        groups
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Trims a typed group name, drops control characters, and caps its length.
pub fn normalize_group(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_GROUP_LEN)
        .collect();
    let name = name.trim_end().to_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_loads_empty_and_saves_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scope/groups.json");
        assert_eq!(State::load(&path).unwrap(), State::default());
        let mut state = State::default();
        state.workspaces.insert("w1".into(), "work".into());
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), state);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groups.json");
        fs::write(&path, "{not json").unwrap();
        assert!(State::load(&path).is_err());
    }

    #[test]
    fn groups_are_distinct_and_sorted() {
        let mut state = State::default();
        for (id, group) in [
            ("w1", "work"),
            ("w2", "Play"),
            ("w3", "work"),
            ("w4", "infra"),
        ] {
            state.workspaces.insert(id.into(), group.into());
        }
        assert_eq!(state.groups(), ["infra", "Play", "work"]);
    }

    #[test]
    fn group_names_are_trimmed_and_capped() {
        assert_eq!(normalize_group("  work \n"), Some("work".into()));
        assert_eq!(normalize_group(" \t "), None);
        assert_eq!(
            normalize_group(&"x".repeat(60)).map(|name| name.len()),
            Some(MAX_GROUP_LEN)
        );
    }
}
