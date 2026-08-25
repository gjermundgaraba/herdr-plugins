use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::model::{Item, validate_items};

pub const CONFIG_DIR_ENV: &str = "HERDR_PICKER_CONFIG_DIR";

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InputMode {
    #[default]
    Direct,
    Vim,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    #[default]
    Local,
    Provider,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub title: String,
    #[serde(default)]
    pub mode: InputMode,
    pub steps: Vec<Step>,
    pub submit: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub title: String,
    pub source: Option<Vec<String>>,
    #[serde(default)]
    pub search: SearchMode,
    #[serde(default)]
    pub items: Vec<Item>,
}

impl Workflow {
    pub fn load(name: &str, config_dir: &Path) -> Result<Self> {
        validate_name(name)?;
        let path = config_dir.join(format!("{name}.toml"));
        let contents =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let workflow: Self =
            toml::from_str(&contents).with_context(|| format!("invalid {}", path.display()))?;
        workflow
            .validate()
            .with_context(|| format!("invalid {}", path.display()))?;
        Ok(workflow)
    }

    pub fn validate(&self) -> Result<()> {
        nonempty("title", &self.title)?;
        if self.steps.is_empty() {
            bail!("steps must contain at least one step");
        }
        command("submit", &self.submit)?;

        let mut step_ids = HashSet::new();
        for (step_index, step) in self.steps.iter().enumerate() {
            let path = format!("steps[{step_index}]");
            identifier(&format!("{path}.id"), &step.id)?;
            if !step_ids.insert(&step.id) {
                bail!("duplicate step id {:?}", step.id);
            }
            nonempty(&format!("{path}.title"), &step.title)?;
            match (&step.source, step.items.is_empty()) {
                (Some(source), true) => command(&format!("{path}.source"), source)?,
                (None, false) => {
                    if step.search == SearchMode::Provider {
                        bail!("{path}.search requires a source");
                    }
                    validate_items(&step.items).with_context(|| format!("{path}.items"))?
                }
                (Some(_), false) => {
                    bail!("{path} must define source or items, not both");
                }
                (None, true) => bail!("{path} must define source or items"),
            }
        }
        Ok(())
    }
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) = env::var_os(CONFIG_DIR_ENV).filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path).join("herdr-picker").join("pickers"));
    }
    env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".config").join("herdr-picker").join("pickers"))
        .ok_or_else(|| anyhow!("set {CONFIG_DIR_ENV}; HOME is unavailable"))
}

pub fn list(config_dir: &Path) -> Result<Vec<String>> {
    let entries = fs::read_dir(config_dir)
        .with_context(|| format!("cannot read {}", config_dir.display()))?;
    let mut names = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("toml"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .filter(|name| validate_name(name).is_ok())
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

fn validate_name(name: &str) -> Result<()> {
    identifier("picker name", name)
}

fn identifier(path: &str, value: &str) -> Result<()> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(anyhow!(
            "{path} must contain only letters, numbers, '-' and '_'"
        ))
    }
}

fn nonempty(path: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(anyhow!("{path} must not be empty"))
    } else {
        Ok(())
    }
}

fn command(path: &str, value: &[String]) -> Result<()> {
    if value.first().is_none_or(|program| program.is_empty()) {
        Err(anyhow!("{path} must be a non-empty argv array"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
title = "New agent"
mode = "vim"
submit = ["launch-agent"]

[[steps]]
id = "model"
title = "Choose model"
source = ["list-models"]

[[steps]]
id = "harness"
title = "Choose harness"

[[steps.items]]
id = "claude"
title = "Claude Code"
value = { command = "claude" }
"#;

    #[test]
    fn validates_dynamic_and_static_steps() {
        let workflow: Workflow = toml::from_str(VALID).unwrap();
        workflow.validate().unwrap();
        assert_eq!(workflow.mode, InputMode::Vim);
        assert_eq!(workflow.steps[1].items[0].value["command"], "claude");
    }

    #[test]
    fn rejects_ambiguous_step_sources() {
        let mut workflow: Workflow = toml::from_str(VALID).unwrap();
        workflow.steps[0].items.push(Item {
            id: "extra".into(),
            title: "Extra".into(),
            ..Item::default()
        });
        assert!(
            workflow
                .validate()
                .unwrap_err()
                .to_string()
                .contains("not both")
        );
    }

    #[test]
    fn picker_names_cannot_escape_the_config_directory() {
        assert!(validate_name("agent-sessions").is_ok());
        assert!(validate_name("../sessions").is_err());
        assert!(validate_name("-sessions").is_ok());
    }

    #[test]
    fn list_ignores_toml_directories() {
        let directory = env::temp_dir().join(format!("herdr-picker-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(directory.join("ignored.toml")).unwrap();
        fs::write(directory.join("picker.toml"), "").unwrap();

        assert_eq!(list(&directory).unwrap(), ["picker"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn provider_search_requires_a_source() {
        let mut workflow: Workflow = toml::from_str(VALID).unwrap();
        workflow.steps[0].search = SearchMode::Provider;
        workflow.validate().unwrap();

        workflow.steps[1].search = SearchMode::Provider;
        assert!(
            workflow
                .validate()
                .unwrap_err()
                .to_string()
                .contains("requires a source")
        );
    }
}
