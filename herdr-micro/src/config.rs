use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::PLUGIN_ID;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Prompt {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submit: Option<bool>,
    },
    Diff,
    Fast,
    Submit,
    Effort {
        direction: EffortDirection,
    },
    FocusPane {
        direction: Direction,
    },
    Scroll {
        direction: VerticalDirection,
        percent: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortDirection {
    Raise,
    Lower,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerticalDirection {
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct GestureBinding {
    #[serde(default)]
    pub tap: Option<Action>,
    #[serde(rename = "doubleTap", default)]
    pub double_tap: Option<Action>,
    #[serde(default)]
    pub hold: Option<Action>,
    #[serde(default)]
    pub release: Option<Action>,
    #[serde(rename = "holdMs", default)]
    pub hold_ms: Option<u64>,
    #[serde(rename = "doubleTapMs", default)]
    pub double_tap_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    Action(Action),
    ByAgent(ByAgent),
    Gesture(GestureBinding),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByAgent {
    #[serde(rename = "byAgent")]
    pub by_agent: std::collections::BTreeMap<String, Option<Action>>,
}

impl Binding {
    pub fn resolve(&self, agent: &str) -> Option<Action> {
        match self {
            Self::Action(action) => Some(action.clone()),
            Self::ByAgent(variants) => variants
                .by_agent
                .get(agent)
                .or_else(|| variants.by_agent.get("default"))
                .cloned()
                .flatten(),
            Self::Gesture(_) => None,
        }
    }
    pub fn gesture(&self) -> Option<&GestureBinding> {
        if let Self::Gesture(binding) = self {
            Some(binding)
        } else {
            None
        }
    }
}

/// Firmware labels are reversed: `ENC_CC` means clockwise and `ENC_CW` means counterclockwise.
pub fn key_binding(config: &Controls, key: &str, action: i64) -> Option<Binding> {
    if matches!(action, 0 | 1) {
        if let Some(number) = key.strip_prefix("ACT").and_then(|n| n.parse::<u8>().ok()) {
            if (6..=12).contains(&number) {
                return config.buttons.get(&(number - 5)).cloned().flatten();
            }
        }
        if key == "ENC_CLK" {
            return config.dial.press.clone();
        }
    }
    if action != 2 {
        return None;
    }
    match key {
        "ENC_CC" => config.dial.clockwise.clone().map(Binding::Action),
        "ENC_CW" => config.dial.counterclockwise.clone().map(Binding::Action),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Controls {
    pub version: u8,
    pub buttons: std::collections::BTreeMap<u8, Option<Binding>>,
    pub dial: Dial,
    pub joystick: Joystick,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Dial {
    pub clockwise: Option<Action>,
    pub counterclockwise: Option<Action>,
    pub press: Option<Binding>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Joystick {
    pub engage_distance: f64,
    pub release_distance: f64,
    pub up: Option<Action>,
    pub down: Option<Action>,
    pub left: Option<Action>,
    pub right: Option<Action>,
}

pub fn default_controls() -> Controls {
    parse_controls(&controls_json()).unwrap()
}

pub fn parse_controls(value: &Value) -> Result<Controls, String> {
    let root = object(value, "control configuration")?;
    fields(root, &["version", "buttons", "dial", "joystick"], "control")?;
    if root.get("version") != Some(&Value::from(1)) {
        return Err("version must be 1".into());
    }
    let buttons = object(req(root, "buttons")?, "buttons")?;
    let mut parsed_buttons = std::collections::BTreeMap::new();
    for (key, binding) in buttons {
        let id = key
            .parse::<u8>()
            .ok()
            .filter(|n| (1..=7).contains(n))
            .ok_or_else(|| format!("invalid button: {key}"))?;
        parsed_buttons.insert(id, parse_binding(binding, &format!("buttons.{key}"))?);
    }
    let dial_v = object(req(root, "dial")?, "dial")?;
    fields(dial_v, &["clockwise", "counterclockwise", "press"], "dial")?;
    let joystick_v = object(req(root, "joystick")?, "joystick")?;
    fields(
        joystick_v,
        &[
            "engageDistance",
            "releaseDistance",
            "up",
            "down",
            "left",
            "right",
        ],
        "joystick",
    )?;
    let engage = number(req(joystick_v, "engageDistance")?)?;
    let release = number(req(joystick_v, "releaseDistance")?)?;
    if !(release >= 0.0 && release < engage && engage <= 1.0) {
        return Err(
            "joystick distances must satisfy 0 <= releaseDistance < engageDistance <= 1".into(),
        );
    }
    Ok(Controls {
        version: 1,
        buttons: parsed_buttons,
        dial: Dial {
            clockwise: parse_action_or_null(
                dial_v.get("clockwise").unwrap_or(&Value::Null),
                "dial.clockwise",
            )?,
            counterclockwise: parse_action_or_null(
                dial_v.get("counterclockwise").unwrap_or(&Value::Null),
                "dial.counterclockwise",
            )?,
            press: parse_binding(dial_v.get("press").unwrap_or(&Value::Null), "dial.press")?,
        },
        joystick: Joystick {
            engage_distance: engage,
            release_distance: release,
            up: parse_action_or_null(joystick_v.get("up").unwrap_or(&Value::Null), "joystick.up")?,
            down: parse_action_or_null(
                joystick_v.get("down").unwrap_or(&Value::Null),
                "joystick.down",
            )?,
            left: parse_action_or_null(
                joystick_v.get("left").unwrap_or(&Value::Null),
                "joystick.left",
            )?,
            right: parse_action_or_null(
                joystick_v.get("right").unwrap_or(&Value::Null),
                "joystick.right",
            )?,
        },
    })
}

fn parse_binding(value: &Value, label: &str) -> Result<Option<Binding>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let obj = object(value, label)?;
    if obj.contains_key("byAgent") {
        fields(obj, &["byAgent"], label)?;
        let variants = object(req(obj, "byAgent")?, &format!("{label}.byAgent"))?;
        if variants.is_empty() {
            return Err(format!("{label}.byAgent must not be empty"));
        }
        let mut by_agent = std::collections::BTreeMap::new();
        for (agent, action) in variants {
            if !valid_agent(agent) {
                return Err(format!("invalid agent: {agent}"));
            }
            if action
                .as_object()
                .is_some_and(|a| a.contains_key("byAgent"))
            {
                return Err(format!("{label}.byAgent.{agent} cannot contain byAgent"));
            }
            by_agent.insert(
                agent.clone(),
                parse_action_or_null(action, &format!("{label}.byAgent.{agent}"))?,
            );
        }
        return Ok(Some(Binding::ByAgent(ByAgent { by_agent })));
    }
    if ["tap", "doubleTap", "hold", "release"]
        .iter()
        .any(|key| obj.contains_key(*key))
    {
        let binding: GestureBinding =
            serde_json::from_value(value.clone()).map_err(|e| format!("{label}: {e}"))?;
        if binding.hold_ms.is_some_and(|n| !(50..=5000).contains(&n))
            || binding
                .double_tap_ms
                .is_some_and(|n| !(50..=5000).contains(&n))
        {
            return Err(format!("{label} timing must be an integer from 50 to 5000"));
        }
        validate_gesture_actions(&binding, label)?;
        return Ok(Some(Binding::Gesture(binding)));
    }
    Ok(parse_action_or_null(value, label)?.map(Binding::Action))
}
fn validate_gesture_actions(binding: &GestureBinding, label: &str) -> Result<(), String> {
    for (name, action) in [
        ("tap", &binding.tap),
        ("doubleTap", &binding.double_tap),
        ("hold", &binding.hold),
        ("release", &binding.release),
    ] {
        if let Some(action) = action {
            validate_action(action, &format!("{label}.{name}"))?;
        }
    }
    Ok(())
}
fn parse_action_or_null(value: &Value, label: &str) -> Result<Option<Action>, String> {
    if value.is_null() {
        Ok(None)
    } else {
        let action: Action = serde_json::from_value(value.clone())
            .map_err(|_| format!("{label}.action is invalid"))?;
        validate_action(&action, label)?;
        Ok(Some(action))
    }
}
fn validate_action(action: &Action, label: &str) -> Result<(), String> {
    match action {
        Action::Prompt { prompt, .. } if prompt.trim().is_empty() => {
            Err(format!("{label}.prompt must be a non-empty string"))
        }
        Action::Scroll { percent, .. }
            if !percent.is_finite() || *percent <= 0.0 || *percent > 100.0 =>
        {
            Err(format!(
                "{label}.percent must be greater than 0 and at most 100"
            ))
        }
        _ => Ok(()),
    }
}
fn object<'a>(v: &'a Value, label: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    v.as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}
fn req<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a Value, String> {
    obj.get(key).ok_or_else(|| format!("missing {key}"))
}
fn fields(
    obj: &serde_json::Map<String, Value>,
    allowed: &[&str],
    label: &str,
) -> Result<(), String> {
    obj.keys()
        .find(|key| !allowed.contains(&key.as_str()))
        .map_or(Ok(()), |key| Err(format!("unknown {label} field: {key}")))
}
fn number(v: &Value) -> Result<f64, String> {
    v.as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| "must be a finite number".into())
}
fn valid_agent(agent: &str) -> bool {
    agent == "default" || {
        let mut chars = agent.chars();
        matches!(chars.next(), Some('a'..='z'))
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffortConfig {
    pub codex: EffortKeys,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffortKeys {
    pub raise: Option<String>,
    pub lower: Option<String>,
}
pub fn default_effort() -> EffortConfig {
    EffortConfig {
        codex: EffortKeys {
            raise: None,
            lower: None,
        },
    }
}
pub fn parse_effort(value: Value) -> Result<EffortConfig, String> {
    let codex = value
        .get("codex")
        .and_then(Value::as_object)
        .ok_or_else(|| "codex effort configuration must be an object".to_owned())?;
    for key in ["raise", "lower"] {
        if !codex.contains_key(key) {
            return Err(format!("codex {key} must be a key or null"));
        }
    }
    let config: EffortConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
    for key in [&config.codex.raise, &config.codex.lower] {
        if key.as_ref().is_some_and(|key| key.trim().is_empty()) {
            return Err("codex effort key must be a key or null".into());
        }
    }
    Ok(config)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Blocked,
    Done,
    Working,
    Idle,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Off,
    Solid,
    Snake,
    Rainbow,
    Breath,
    Gradient,
    ShallowBreath,
}
impl Effect {
    pub fn code(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Solid => 1,
            Self::Snake => 2,
            Self::Rainbow => 3,
            Self::Breath => 4,
            Self::Gradient => 5,
            Self::ShallowBreath => 6,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightConfig {
    pub color: String,
    pub brightness: f64,
    pub effect: Effect,
    pub speed: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    pub c: u32,
    pub b: f64,
    pub e: u8,
    pub s: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingConfig {
    pub states: std::collections::BTreeMap<AgentStatus, LightConfig>,
    #[serde(rename = "focusedBrightness")]
    pub focused_brightness: f64,
    pub ambient: Option<String>,
    pub keys: Option<String>,
}
pub fn default_lighting() -> LightingConfig {
    serde_json::from_value(serde_json::json!({"states":{"blocked":{"color":"#ffaa00","brightness":1,"effect":"solid","speed":0},"done":{"color":"#22cc55","brightness":1,"effect":"solid","speed":0},"working":{"color":"#2277ff","brightness":1,"effect":"breath","speed":0.35},"idle":{"color":"#ffffff","brightness":0.25,"effect":"solid","speed":0},"unknown":{"color":"#ffffff","brightness":0.08,"effect":"solid","speed":0}},"focusedBrightness":1,"ambient":"status","keys":null})).unwrap()
}
pub fn parse_lighting(value: Value) -> Result<LightingConfig, String> {
    let config: LightingConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if config.states.len() != 5 {
        return Err("unknown lighting state".into());
    }
    for status in [
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Working,
        AgentStatus::Idle,
        AgentStatus::Unknown,
    ] {
        let light = config
            .states
            .get(&status)
            .ok_or_else(|| format!("invalid {status:?} light"))?;
        if !is_hex(&light.color) || !unit(light.brightness) || !unit(light.speed) {
            return Err(format!("invalid {status:?} light"));
        }
    }
    if !unit(config.focused_brightness) {
        return Err("focusedBrightness must be between 0 and 1".into());
    }
    if !matches!(config.ambient.as_deref(), None | Some("status"))
        || !matches!(config.keys.as_deref(), None | Some("status"))
    {
        return Err("ambient and keys must be \"status\" or null".into());
    }
    Ok(config)
}
impl LightingConfig {
    pub fn light(&self, status: AgentStatus) -> Light {
        let v = &self.states[&status];
        Light {
            c: u32::from_str_radix(&v.color[1..], 16).unwrap(),
            b: v.brightness,
            e: v.effect.code(),
            s: v.speed,
        }
    }
}
fn unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
fn is_hex(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn config_path(name: &str) -> PathBuf {
    env::var_os("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config/herdr/plugins/config")
                .join(PLUGIN_ID)
        })
        .join(name)
}
pub fn ensure_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        use std::os::unix::fs::OpenOptionsExt;
        let json = serde_json::to_string_pretty(value).unwrap() + "\n";
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        match std::io::Write::write_all(&mut options.open(path)?, json.as_bytes()) {
            Ok(()) => {}
            Err(e) => return Err(e),
        };
    }
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
}
pub fn load_controls(path: &Path) -> Result<Controls, String> {
    ensure_json(path, &controls_json()).map_err(|e| e.to_string())?;
    parse_controls(&read_json(path)?)
}
pub fn load_effort(path: &Path) -> Result<EffortConfig, String> {
    ensure_json(path, &default_effort()).map_err(|e| e.to_string())?;
    parse_effort(read_json(path)?)
}
pub fn load_lighting(path: &Path) -> Result<LightingConfig, String> {
    ensure_json(path, &default_lighting()).map_err(|e| e.to_string())?;
    parse_lighting(read_json(path)?)
}
fn controls_json() -> Value {
    serde_json::json!({"version":1,"buttons":{"1":null,"2":null,"3":{"byAgent":{"codex":{"action":"fast"},"pi":{"action":"fast"},"default":null}},"4":{"action":"prompt","prompt":"/copy","submit":true},"5":null,"6":null,"7":{"action":"submit"}},"dial":{"clockwise":{"action":"effort","direction":"raise"},"counterclockwise":{"action":"effort","direction":"lower"},"press":{"action":"prompt","prompt":"/model","submit":true}},"joystick":{"engageDistance":0.75,"releaseDistance":0.3,"up":{"action":"scroll","direction":"up","percent":50},"down":{"action":"scroll","direction":"down","percent":50},"left":{"action":"focus-pane","direction":"left"},"right":{"action":"focus-pane","direction":"right"}}})
}
fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_reject_unknown_fields_and_map_reversed_dial_labels() {
        let controls = default_controls();
        assert!(matches!(
            key_binding(&controls, "ENC_CC", 2),
            Some(Binding::Action(Action::Effort {
                direction: EffortDirection::Raise
            }))
        ));
        assert!(parse_controls(&serde_json::json!({"version":1,"buttons":{},"dial":{"clockwise":null,"counterclockwise":null,"press":null,"extra":true},"joystick":{"engageDistance":0.75,"releaseDistance":0.3}})).is_err());
        assert!(parse_controls(&serde_json::json!({"version":1,"buttons":{"8":null},"dial":{"clockwise":null,"counterclockwise":null,"press":null},"joystick":{"engageDistance":0.75,"releaseDistance":0.3}})).is_err());
    }
    #[test]
    fn validates_effort_and_lighting() {
        assert!(parse_effort(serde_json::json!({"codex":{"raise":null,"lower":"x"}})).is_ok());
        assert!(parse_effort(serde_json::json!({"codex":{"raise":" "}})).is_err());
        assert!(parse_effort(serde_json::json!({"codex":{"raise":null}})).is_err());
        assert!(parse_lighting(serde_json::to_value(default_lighting()).unwrap()).is_ok());
        assert!(parse_lighting(
            serde_json::json!({"states":{},"focusedBrightness":1,"ambient":"status","keys":null})
        )
        .is_err());
    }
}
