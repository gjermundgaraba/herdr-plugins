// Saved priority spaces for one Herdr session, keyed by stable workspace id.
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Spaces marked as priority.
    #[serde(default)]
    pub workspaces: BTreeSet<String>,
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
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_loads_empty_and_saves_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scope/priority.json");
        assert_eq!(State::load(&path).unwrap(), State::default());
        let mut state = State::default();
        state.workspaces.insert("w1".into());
        state.save(&path).unwrap();
        assert_eq!(State::load(&path).unwrap(), state);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("priority.json");
        fs::write(&path, "{not json").unwrap();
        assert!(State::load(&path).is_err());
    }
}
