use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub key: String,
    pub ssh: String,
}

pub fn path() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")?;
    Ok(home.join(".config/herdr-hub/config.toml"))
}

pub fn load() -> Result<Config> {
    load_from(&path()?)
}

pub(crate) fn load_from(path: &Path) -> Result<Config> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", path.display()));
        }
    };
    let config: Config =
        toml::from_str(&source).with_context(|| format!("parse {}", path.display()))?;
    config.validate()?;
    Ok(config)
}

impl Config {
    fn validate(&self) -> Result<()> {
        let mut keys = HashSet::new();
        for (index, host) in self.hosts.iter().enumerate() {
            if host.key.trim().is_empty() {
                bail!("hosts[{index}].key must not be empty");
            }
            if host.key == "local" {
                bail!("hosts[{index}].key \"local\" is reserved");
            }
            if host.key.contains('/') {
                bail!("hosts[{index}].key must not contain '/'");
            }
            if !keys.insert(&host.key) {
                bail!("duplicate host key {:?}", host.key);
            }
            if host.ssh.trim().is_empty() {
                bail!("hosts[{index}].ssh must not be empty");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Result<Config> {
        let config: Config = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn absent_file_is_empty() {
        let missing = env::temp_dir().join(format!(
            "herdr-hub-missing-config-{}-{}",
            std::process::id(),
            line!()
        ));
        assert_eq!(load_from(&missing).unwrap(), Config::default());
    }

    #[test]
    fn parses_hosts() {
        assert_eq!(
            parse(
                r#"
                [[hosts]]
                key = "workbox"
                ssh = "ssh-alias"

                [[hosts]]
                key = "lab"
                ssh = "user@lab.example"
                "#,
            )
            .unwrap(),
            Config {
                hosts: vec![
                    HostConfig {
                        key: "workbox".into(),
                        ssh: "ssh-alias".into(),
                    },
                    HostConfig {
                        key: "lab".into(),
                        ssh: "user@lab.example".into(),
                    },
                ],
            }
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(parse("unexpected = true").is_err());
        assert!(parse("[[hosts]]\nkey = \"work\"\nssh = \"work\"\nport = 22").is_err());
    }

    #[test]
    fn validates_host_entries() {
        for (source, expected) in [
            (
                "[[hosts]]\nkey = \" \"\nssh = \"work\"",
                "key must not be empty",
            ),
            ("[[hosts]]\nkey = \"local\"\nssh = \"work\"", "is reserved"),
            (
                "[[hosts]]\nkey = \"team/work\"\nssh = \"work\"",
                "must not contain '/'",
            ),
            (
                "[[hosts]]\nkey = \"work\"\nssh = \" \"",
                "ssh must not be empty",
            ),
            (
                "[[hosts]]\nkey = \"work\"\nssh = \"one\"\n[[hosts]]\nkey = \"work\"\nssh = \"two\"",
                "duplicate host key",
            ),
        ] {
            let error = parse(source).unwrap_err().to_string();
            assert!(
                error.contains(expected),
                "{error:?} did not contain {expected:?}"
            );
        }
    }
}
