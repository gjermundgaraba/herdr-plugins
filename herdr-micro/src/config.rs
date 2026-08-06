use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs, io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::os::unix::fs::PermissionsExt;

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
    ByAgent(std::collections::BTreeMap<String, Option<Action>>),
    Gesture(GestureBinding),
}

impl Binding {
    pub fn resolve(&self, agent: &str) -> Option<Action> {
        match self {
            Self::Action(action) => Some(action.clone()),
            Self::ByAgent(variants) => variants
                .get(agent)
                .or_else(|| variants.get("default"))
                .cloned()
                .flatten(),
            Self::Gesture(_) => None,
        }
    }
}

/// Firmware labels are reversed: `ENC_CC` means clockwise and `ENC_CW` means counterclockwise.
pub fn key_binding(config: &Controls, key: &str, action: i64) -> Option<Binding> {
    if matches!(action, 0 | 1) {
        if let Some(button) = config
            .action_device_keys
            .iter()
            .find_map(|(button, hid_key)| (hid_key.as_deref() == Some(key)).then_some(button))
        {
            return config.buttons.get(button).cloned().flatten();
        }
        if key == "ENC_CLK" {
            return config.dial.press.clone();
        }
    }
    match key {
        "ENC_CC" => config.dial.clockwise.clone().map(Binding::Action),
        "ENC_CW" => config.dial.counterclockwise.clone().map(Binding::Action),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Controls {
    pub buttons: std::collections::BTreeMap<u8, Option<Binding>>,
    pub agent_macos_keys: std::collections::BTreeMap<u8, String>,
    pub action_device_keys: std::collections::BTreeMap<u8, Option<String>>,
    pub action_macos_keys: std::collections::BTreeMap<u8, Option<String>>,
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
    fields(
        root,
        &[
            "version",
            "buttons",
            "agentMacosKeys",
            "actionDeviceKeys",
            "actionMacosKeys",
            "dial",
            "joystick",
        ],
        "control",
    )?;
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
    let mut used_macos_keys = std::collections::BTreeSet::new();
    let agent_macos_keys = object(req(root, "agentMacosKeys")?, "agentMacosKeys")?;
    let mut parsed_agent_macos_keys = std::collections::BTreeMap::new();
    for (slot, key) in agent_macos_keys {
        let slot = slot
            .parse::<u8>()
            .ok()
            .filter(|slot| (1..=6).contains(slot))
            .ok_or_else(|| format!("invalid Agent slot: {slot}"))?;
        let key = key
            .as_str()
            .filter(|key| function_key_number(key).is_some_and(|number| number <= 20))
            .ok_or_else(|| format!("agentMacosKeys.{slot} must be F13 through F20"))?;
        if !used_macos_keys.insert(key.to_owned()) {
            return Err(format!("duplicate macOS key: {key}"));
        }
        parsed_agent_macos_keys.insert(slot, key.to_owned());
    }
    if parsed_agent_macos_keys.len() != 6 {
        return Err("agentMacosKeys must define slots 1 through 6".into());
    }
    let mut used_device_keys = std::collections::BTreeSet::new();
    let action_device_keys = object(req(root, "actionDeviceKeys")?, "actionDeviceKeys")?;
    let mut parsed_action_device_keys = std::collections::BTreeMap::new();
    for (button, key) in action_device_keys {
        let button = button
            .parse::<u8>()
            .ok()
            .filter(|button| (1..=7).contains(button))
            .ok_or_else(|| format!("invalid HID button: {button}"))?;
        let key = match key {
            Value::Null => None,
            Value::String(key) if function_key_number(key).is_some() => Some(key.clone()),
            _ => {
                return Err(format!(
                    "actionDeviceKeys.{button} must be null or F13 through F24"
                ));
            }
        };
        if let Some(key) = &key {
            if !used_device_keys.insert(key.clone()) {
                return Err(format!("duplicate action device key: {key}"));
            }
        }
        parsed_action_device_keys.insert(button, key);
    }
    if parsed_action_device_keys.len() != 7 {
        return Err("actionDeviceKeys must define buttons 1 through 7".into());
    }
    let action_macos_keys = object(req(root, "actionMacosKeys")?, "actionMacosKeys")?;
    let mut parsed_action_macos_keys = std::collections::BTreeMap::new();
    for (button, key) in action_macos_keys {
        let button = button
            .parse::<u8>()
            .ok()
            .filter(|button| (1..=7).contains(button))
            .ok_or_else(|| format!("invalid macOS button: {button}"))?;
        let key = match key {
            Value::Null => None,
            Value::String(key) if function_key_number(key).is_some_and(|number| number <= 20) => {
                Some(key.clone())
            }
            _ => {
                return Err(format!(
                    "actionMacosKeys.{button} must be null or F13 through F20"
                ));
            }
        };
        if let Some(key) = &key {
            if !used_macos_keys.insert(key.clone()) {
                return Err(format!("duplicate macOS key: {key}"));
            }
        }
        parsed_action_macos_keys.insert(button, key);
    }
    if parsed_action_macos_keys.len() != 7 {
        return Err("actionMacosKeys must define buttons 1 through 7".into());
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
        buttons: parsed_buttons,
        agent_macos_keys: parsed_agent_macos_keys,
        action_device_keys: parsed_action_device_keys,
        action_macos_keys: parsed_action_macos_keys,
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

pub fn function_key_number(key: &str) -> Option<u8> {
    let number = key.strip_prefix('F')?.parse::<u8>().ok()?;
    ((13..=24).contains(&number) && key == format!("F{number}")).then_some(number)
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
        return Ok(Some(Binding::ByAgent(by_agent)));
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
fn ensure_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        let json = serde_json::to_string_pretty(value).unwrap() + "\n";
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        std::io::Write::write_all(&mut options.open(path)?, json.as_bytes())?;
    }
    Ok(())
}
pub fn provision_controls(path: &Path) -> Result<(), String> {
    ensure_json(path, &controls_json()).map_err(|error| error.to_string())
}
pub fn provision_effort(path: &Path) -> Result<(), String> {
    ensure_json(path, &default_effort()).map_err(|error| error.to_string())
}
pub fn provision_lighting(path: &Path) -> Result<(), String> {
    ensure_json(path, &default_lighting()).map_err(|error| error.to_string())
}
pub fn load_controls(path: &Path) -> Result<Controls, String> {
    parse_controls(&read_json(path)?)
}
pub fn load_effort(path: &Path) -> Result<EffortConfig, String> {
    parse_effort(read_json(path)?)
}
pub fn load_lighting(path: &Path) -> Result<LightingConfig, String> {
    parse_lighting(read_json(path)?)
}
fn controls_json() -> Value {
    serde_json::json!({"version":1,"buttons":{"1":null,"2":null,"3":{"byAgent":{"codex":{"action":"fast"},"pi":{"action":"fast"},"default":null}},"4":{"action":"prompt","prompt":"/copy","submit":true},"5":null,"6":null,"7":{"action":"submit"}},"agentMacosKeys":{"1":"F13","2":"F14","3":"F15","4":"F16","5":"F17","6":"F18"},"actionDeviceKeys":{"1":"F20","2":"F21","3":"F22","4":"F23","5":"F19","6":null,"7":"F24"},"actionMacosKeys":{"1":null,"2":null,"3":null,"4":null,"5":"F19","6":null,"7":null},"dial":{"clockwise":{"action":"effort","direction":"raise"},"counterclockwise":{"action":"effort","direction":"lower"},"press":{"action":"prompt","prompt":"/model","submit":true}},"joystick":{"engageDistance":0.75,"releaseDistance":0.3,"up":{"action":"scroll","direction":"up","percent":50},"down":{"action":"scroll","direction":"down","percent":50},"left":{"action":"focus-pane","direction":"left"},"right":{"action":"focus-pane","direction":"right"}}})
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
        assert_eq!(controls.agent_macos_keys[&1], "F13");
        assert_eq!(controls.agent_macos_keys[&6], "F18");
        assert_eq!(controls.action_device_keys[&5].as_deref(), Some("F19"));
        assert_eq!(controls.action_device_keys[&6], None);
        assert_eq!(controls.action_macos_keys[&5].as_deref(), Some("F19"));
        assert_eq!(controls.action_macos_keys[&1], None);
        for action in [-1, 0, 1, 2, 3] {
            assert!(matches!(
                key_binding(&controls, "ENC_CC", action),
                Some(Binding::Action(Action::Effort {
                    direction: EffortDirection::Raise
                }))
            ));
        }
        assert!(key_binding(&controls, "ACT10", 1).is_none());
        assert_eq!(
            key_binding(&controls, "F20", 1),
            controls.buttons[&1].clone()
        );
        let mut remapped = controls_json();
        remapped["agentMacosKeys"]["3"] = serde_json::json!("F20");
        let remapped = parse_controls(&remapped).unwrap();
        assert_eq!(remapped.agent_macos_keys[&3], "F20");
        assert_eq!(remapped.action_device_keys[&1].as_deref(), Some("F20"));
        let mut duplicate = controls_json();
        duplicate["actionDeviceKeys"]["1"] = serde_json::json!("F21");
        assert!(parse_controls(&duplicate).is_err());
        let mut duplicate_output = controls_json();
        duplicate_output["actionMacosKeys"]["1"] = serde_json::json!("F13");
        assert!(parse_controls(&duplicate_output).is_err());
        let mut missing = controls_json();
        missing["agentMacosKeys"]
            .as_object_mut()
            .unwrap()
            .remove("6");
        assert!(parse_controls(&missing).is_err());
        let mut invalid = controls_json();
        invalid["agentMacosKeys"]["1"] = serde_json::json!("F21");
        assert!(parse_controls(&invalid).is_err());
        assert_eq!(
            key_binding(&controls, "F24", 1),
            controls.buttons[&7].clone()
        );
        let mut extra = controls_json();
        extra["dial"]["extra"] = Value::Bool(true);
        assert!(parse_controls(&extra).is_err());
        let mut bad_button = controls_json();
        bad_button["buttons"]["8"] = Value::Null;
        assert!(parse_controls(&bad_button).is_err());
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
    #[test]
    fn loading_is_pure_and_provisioning_creates_private_defaults() {
        let root = env::temp_dir().join(format!("herdr-micro-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("controls.json");
        assert!(load_controls(&path).is_err());
        assert!(!path.exists());
        provision_controls(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        load_controls(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_dir_all(root).unwrap();
    }
}
