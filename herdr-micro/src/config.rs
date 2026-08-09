use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::{env, os::unix::fs::PermissionsExt};

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
    Key {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keycode: Option<u16>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        modifiers: Vec<Modifier>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Cmd,
    Shift,
    Alt,
    Ctrl,
    Fn,
}

fn named_keycode(name: &str) -> Option<u16> {
    Some(match name {
        "F13" => 0x69,
        "F14" => 0x6B,
        "F15" => 0x71,
        "F16" => 0x6A,
        "F17" => 0x40,
        "F18" => 0x4F,
        "F19" => 0x50,
        "F20" => 0x5A,
        _ => return None,
    })
}

/// The macOS virtual keycode a `key` action posts.
pub fn key_action_code(key: Option<&str>, keycode: Option<u16>) -> Result<u16, String> {
    match (key, keycode) {
        (Some(name), None) => {
            named_keycode(name).ok_or_else(|| format!("unknown key name: {name}"))
        }
        (None, Some(code)) if code <= 0x7F => Ok(code),
        (None, Some(code)) => Err(format!("keycode {code} must be at most 127")),
        _ => Err("key action requires exactly one of key or keycode".into()),
    }
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

impl<'de> Deserialize<'de> for Binding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        parse_binding(&value, "binding")
            .map_err(serde::de::Error::custom)?
            .ok_or_else(|| serde::de::Error::custom("binding must not be null"))
    }
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

fn binding_requires_accessibility(binding: &Binding) -> bool {
    match binding {
        Binding::Action(action) => matches!(action, Action::Key { .. }),
        Binding::ByAgent(actions) => actions
            .values()
            .flatten()
            .any(|action| matches!(action, Action::Key { .. })),
        Binding::Gesture(gesture) => [
            &gesture.tap,
            &gesture.double_tap,
            &gesture.hold,
            &gesture.release,
        ]
        .into_iter()
        .flatten()
        .any(|action| matches!(action, Action::Key { .. })),
    }
}

/// Firmware labels are reversed: `ENC_CC` means clockwise and `ENC_CW` means counterclockwise.
pub fn key_binding(config: &Controls, key: &str, action: i64) -> Option<Binding> {
    if matches!(action, 0 | 1) {
        if let Some(button) = device_button(key) {
            return config.buttons.get(&button).cloned().flatten();
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controls {
    pub buttons: std::collections::BTreeMap<u8, Option<Binding>>,
    pub dial: Dial,
    pub joystick: Joystick,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub controls: Controls,
    pub effort: EffortConfig,
    pub lighting: LightingConfig,
}

pub fn device_button(key: &str) -> Option<u8> {
    key.strip_prefix("ACT")?
        .parse::<u8>()
        .ok()?
        .checked_sub(5)
        .filter(|button| (1..=7).contains(button))
}

/// A switch is live exactly when its button is bound.
pub fn enabled_buttons(controls: &Controls) -> [bool; 7] {
    std::array::from_fn(|index| {
        controls
            .buttons
            .get(&(index as u8 + 1))
            .is_some_and(Option::is_some)
    })
}

pub fn requires_accessibility(controls: &Controls) -> bool {
    controls
        .buttons
        .values()
        .flatten()
        .any(binding_requires_accessibility)
        || controls
            .dial
            .press
            .as_ref()
            .is_some_and(binding_requires_accessibility)
        || [
            controls.dial.clockwise.as_ref(),
            controls.dial.counterclockwise.as_ref(),
            controls.joystick.up.as_ref(),
            controls.joystick.down.as_ref(),
            controls.joystick.left.as_ref(),
            controls.joystick.right.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|action| matches!(action, Action::Key { .. }))
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dial {
    pub clockwise: Option<Action>,
    pub counterclockwise: Option<Action>,
    pub press: Option<Binding>,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Joystick {
    pub engage_distance: f64,
    pub release_distance: f64,
    pub up: Option<Action>,
    pub down: Option<Action>,
    pub left: Option<Action>,
    pub right: Option<Action>,
}

fn parse_controls(value: &Value) -> Result<Controls, String> {
    let controls: Controls =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    if controls.buttons.keys().any(|id| !(1..=7).contains(id)) {
        return Err("button IDs must be integers from 1 to 7".into());
    }
    if !(controls.joystick.release_distance >= 0.0
        && controls.joystick.release_distance < controls.joystick.engage_distance
        && controls.joystick.engage_distance <= 1.0)
    {
        return Err(
            "joystick distances must satisfy 0 <= releaseDistance < engageDistance <= 1".into(),
        );
    }
    for (label, action) in [
        ("dial.clockwise", &controls.dial.clockwise),
        ("dial.counterclockwise", &controls.dial.counterclockwise),
        ("joystick.up", &controls.joystick.up),
        ("joystick.down", &controls.joystick.down),
        ("joystick.left", &controls.joystick.left),
        ("joystick.right", &controls.joystick.right),
    ] {
        if let Some(action) = action {
            validate_action(action, label)?;
        }
    }
    Ok(controls)
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
            let parsed = parse_action_or_null(action, &format!("{label}.byAgent.{agent}"))?;
            // Key taps are system-wide and fire without a focused agent, so an
            // agent-conditional key has nothing coherent to condition on.
            if matches!(parsed, Some(Action::Key { .. })) {
                return Err(format!(
                    "{label}.byAgent.{agent} cannot contain a key action"
                ));
            }
            by_agent.insert(agent.clone(), parsed);
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
        Action::Key { key, keycode, .. } => key_action_code(key.as_deref(), *keycode)
            .map(|_| ())
            .map_err(|error| format!("{label}: {error}")),
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
fn parse_effort(value: Value) -> Result<EffortConfig, String> {
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
fn parse_lighting(value: Value) -> Result<LightingConfig, String> {
    let config: LightingConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
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

pub fn config_path() -> Result<PathBuf, String> {
    Ok(crate::plugin_paths()
        .map_err(|error| error.to_string())?
        .config_file("config.json"))
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
pub fn provision(path: &Path) -> Result<(), String> {
    ensure_json(path, &config_json()).map_err(|error| error.to_string())
}

pub fn load(path: &Path) -> Result<Config, String> {
    parse_config(&read_json(path)?)
}

impl Default for Config {
    fn default() -> Self {
        parse_config(&config_json()).expect("built-in configuration must be valid")
    }
}

fn parse_config(value: &Value) -> Result<Config, String> {
    let root = object(value, "configuration")?;
    fields(
        root,
        &["version", "controls", "effort", "lighting"],
        "configuration",
    )?;
    if root.get("version") != Some(&Value::from(1)) {
        return Err("version must be 1".into());
    }
    Ok(Config {
        controls: parse_controls(req(root, "controls")?)?,
        effort: parse_effort(req(root, "effort")?.clone())?,
        lighting: parse_lighting(req(root, "lighting")?.clone())?,
    })
}

fn config_json() -> Value {
    serde_json::json!({
        "version": 1,
        "controls": {
            "buttons": {"1":null,"2":null,"3":{"byAgent":{"codex":{"action":"fast"},"pi":{"action":"fast"},"default":null}},"4":{"action":"prompt","prompt":"/copy","submit":true},"5":null,"6":null,"7":{"action":"submit"}},
            "dial":{"clockwise":{"action":"effort","direction":"raise"},"counterclockwise":{"action":"effort","direction":"lower"},"press":{"action":"prompt","prompt":"/model","submit":true}},
            "joystick":{"engageDistance":0.75,"releaseDistance":0.3,"up":{"action":"scroll","direction":"up","percent":50},"down":{"action":"scroll","direction":"down","percent":50},"left":{"action":"focus-pane","direction":"left"},"right":{"action":"focus-pane","direction":"right"}}
        },
        "effort": {"codex":{"raise":null,"lower":null}},
        "lighting": {
            "states": {
                "blocked":{"color":"#ffaa00","brightness":1,"effect":"solid","speed":0},
                "done":{"color":"#22cc55","brightness":1,"effect":"solid","speed":0},
                "working":{"color":"#2277ff","brightness":1,"effect":"breath","speed":0.35},
                "idle":{"color":"#ffffff","brightness":0.25,"effect":"solid","speed":0},
                "unknown":{"color":"#ffffff","brightness":0.08,"effect":"solid","speed":0}
            },
            "focusedBrightness":1,
            "ambient":"status",
            "keys":null
        },
    })
}
fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_reject_unknown_fields_and_map_reversed_dial_labels() {
        let controls = Config::default().controls;
        for action in [-1, 0, 1, 2, 3] {
            assert!(matches!(
                key_binding(&controls, "ENC_CC", action),
                Some(Binding::Action(Action::Effort {
                    direction: EffortDirection::Raise
                }))
            ));
        }
        assert_eq!(device_button("ACT06"), Some(1));
        assert_eq!(device_button("ACT10"), Some(5));
        assert_eq!(device_button("ACT12"), Some(7));
        assert_eq!(device_button("ACT13"), None);
        assert!(key_binding(&controls, "F20", 1).is_none());
        assert_eq!(
            key_binding(&controls, "ACT12", 1),
            controls.buttons[&7].clone()
        );
        assert_eq!(device_button("F21"), None);
        assert_eq!(device_button("F13"), None);
        assert_eq!(
            enabled_buttons(&controls),
            [false, false, true, true, false, false, true]
        );
        let mut rebound = controls.clone();
        rebound.buttons.insert(5, controls.buttons[&7].clone());
        assert_ne!(enabled_buttons(&rebound), enabled_buttons(&controls));
        let mut wrong_version = config_json();
        wrong_version["version"] = serde_json::json!(2);
        assert!(parse_config(&wrong_version).is_err());
        let mut stale = config_json();
        stale["controls"]["actionDeviceKeys"] = serde_json::json!({});
        assert!(parse_config(&stale).is_err());
        let mut extra = config_json();
        extra["controls"]["dial"]["extra"] = Value::Bool(true);
        assert!(parse_config(&extra).is_err());
        let mut gesture_extra = config_json();
        gesture_extra["controls"]["buttons"]["1"] =
            serde_json::json!({"tap":{"action":"submit"},"unexpected":true});
        assert!(parse_config(&gesture_extra).is_err());
        let mut bad_button = config_json();
        bad_button["controls"]["buttons"]["8"] = Value::Null;
        assert!(parse_config(&bad_button).is_err());
    }

    #[test]
    fn key_actions_resolve_names_or_raw_keycodes() {
        assert!(!requires_accessibility(&Config::default().controls));
        let mut named = config_json();
        named["controls"]["buttons"]["5"] = serde_json::json!({"action":"key","key":"F19"});
        let named = parse_config(&named).unwrap().controls;
        assert!(requires_accessibility(&named));
        assert!(matches!(
            named.buttons[&5],
            Some(Binding::Action(Action::Key { .. }))
        ));
        let mut raw = config_json();
        raw["controls"]["buttons"]["5"] =
            serde_json::json!({"action":"key","keycode":80,"modifiers":["cmd","shift"]});
        assert!(parse_config(&raw).is_ok());
        for invalid in [
            serde_json::json!({"action":"key"}),
            serde_json::json!({"action":"key","key":"F19","keycode":80}),
            serde_json::json!({"action":"key","key":"F99"}),
            serde_json::json!({"action":"key","keycode":300}),
            serde_json::json!({"action":"key","keycode":80,"modifiers":["hyper"]}),
            serde_json::json!({"byAgent":{"codex":{"action":"key","key":"F19"}}}),
        ] {
            let mut bad = config_json();
            bad["controls"]["buttons"]["5"] = invalid;
            assert!(parse_config(&bad).is_err());
        }
        assert_eq!(key_action_code(Some("F19"), None), Ok(0x50));
        assert_eq!(key_action_code(None, Some(0x50)), Ok(0x50));
    }

    #[test]
    fn validates_effort_and_lighting() {
        let mut valid = config_json();
        valid["effort"] = serde_json::json!({"codex":{"raise":null,"lower":"x"}});
        assert!(parse_config(&valid).is_ok());
        let mut blank = config_json();
        blank["effort"] = serde_json::json!({"codex":{"raise":" ","lower":null}});
        assert!(parse_config(&blank).is_err());
        let mut missing = config_json();
        missing["effort"] = serde_json::json!({"codex":{"raise":null}});
        assert!(parse_config(&missing).is_err());
        let mut lighting = config_json();
        lighting["lighting"] =
            serde_json::json!({"states":{},"focusedBrightness":1,"ambient":"status","keys":null});
        assert!(parse_config(&lighting).is_err());
    }

    #[test]
    fn loading_is_pure_and_provisioning_creates_private_defaults() {
        let root = env::temp_dir().join(format!("herdr-micro-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("config.json");
        assert!(load(&path).is_err());
        assert!(!path.exists());
        provision(&path).unwrap();
        assert_eq!(load(&path).unwrap(), Config::default());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        load(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::remove_dir_all(root).unwrap();
    }
}
