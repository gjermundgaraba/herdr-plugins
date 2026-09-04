use serde::Deserialize;
use serde_json::Value;
use std::{
    fs, io,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::{env, os::unix::fs::PermissionsExt};

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Prompt {
        prompt: String,
        #[serde(default)]
        submit: Option<bool>,
    },
    Diff,
    Fast,
    Submit,
    Script {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    FocusPane {
        direction: Direction,
    },
    Key {
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        keycode: Option<u16>,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}
impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Deserialize, Default)]
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
    Gesture(Box<GestureBinding>),
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
        "ENC_CC" => config.dial.clockwise.clone(),
        "ENC_CW" => config.dial.counterclockwise.clone(),
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
    pub lighting: LightingConfig,
}

pub fn device_button(key: &str) -> Option<u8> {
    key.strip_prefix("ACT")?
        .parse::<u8>()
        .ok()?
        .checked_sub(5)
        .filter(|button| (1..=7).contains(button))
}

/// Button 5 is always live because the device service owns its Handy action.
pub fn enabled_buttons(controls: &Controls) -> [bool; 7] {
    std::array::from_fn(|index| {
        index == 4
            || controls
                .buttons
                .get(&(index as u8 + 1))
                .is_some_and(Option::is_some)
    })
}

pub fn requires_accessibility(controls: &Controls) -> bool {
    controls
        .buttons
        .values()
        .chain([
            &controls.dial.press,
            &controls.dial.clockwise,
            &controls.dial.counterclockwise,
            &controls.joystick.up,
            &controls.joystick.down,
            &controls.joystick.left,
            &controls.joystick.right,
        ])
        .flatten()
        .any(binding_requires_accessibility)
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dial {
    pub clockwise: Option<Binding>,
    pub counterclockwise: Option<Binding>,
    pub press: Option<Binding>,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Joystick {
    pub engage_distance: f64,
    pub release_distance: f64,
    pub up: Option<Binding>,
    pub down: Option<Binding>,
    pub left: Option<Binding>,
    pub right: Option<Binding>,
}

fn parse_controls(value: &Value) -> Result<Controls, String> {
    let controls: Controls =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    if controls.buttons.keys().any(|id| !(1..=7).contains(id)) {
        return Err("button IDs must be integers from 1 to 7".into());
    }
    if matches!(controls.buttons.get(&5), Some(Some(_))) {
        return Err("button 5 is reserved for the Codex Micro service and must be null".into());
    }
    if !(controls.joystick.release_distance >= 0.0
        && controls.joystick.release_distance < controls.joystick.engage_distance
        && controls.joystick.engage_distance <= 1.0)
    {
        return Err(
            "joystick distances must satisfy 0 <= releaseDistance < engageDistance <= 1".into(),
        );
    }
    for (label, binding) in [
        ("dial.clockwise", &controls.dial.clockwise),
        ("dial.counterclockwise", &controls.dial.counterclockwise),
        ("joystick.up", &controls.joystick.up),
        ("joystick.down", &controls.joystick.down),
        ("joystick.left", &controls.joystick.left),
        ("joystick.right", &controls.joystick.right),
    ] {
        if binding
            .as_ref()
            .is_some_and(|binding| matches!(binding, Binding::Gesture(_)))
        {
            return Err(format!("{label} does not support gesture bindings"));
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
        return Ok(Some(Binding::Gesture(Box::new(binding))));
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
        let action: Action =
            serde_json::from_value(value.clone()).map_err(|error| format!("{label}: {error}"))?;
        validate_action(&action, label)?;
        Ok(Some(action))
    }
}
fn validate_action(action: &Action, label: &str) -> Result<(), String> {
    match action {
        Action::Prompt { prompt, .. } if prompt.trim().is_empty() => {
            Err(format!("{label}.prompt must be a non-empty string"))
        }
        Action::Script { command, .. } if command.trim().is_empty() => {
            Err(format!("{label}.command must be a non-empty string"))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Ord, PartialOrd, Deserialize)]
#[serde(rename_all = "lowercase")]
// Declaration order is agent display priority, highest first.
pub enum AgentStatus {
    Blocked,
    Done,
    Working,
    Idle,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightConfig {
    pub color: String,
    pub brightness: f64,
    pub effect: Effect,
    pub speed: f64,
}
pub use codex_micro::service::Light;
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingConfig {
    pub states: std::collections::BTreeMap<AgentStatus, LightConfig>,
    #[serde(rename = "focusedBrightness")]
    pub focused_brightness: f64,
    pub ambient: bool,
    pub keys: bool,
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
fn ensure_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
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
    fields(root, &["controls", "lighting"], "configuration")?;
    Ok(Config {
        controls: parse_controls(req(root, "controls")?)?,
        lighting: parse_lighting(req(root, "lighting")?.clone())?,
    })
}

fn config_json() -> Value {
    serde_json::json!({
        "controls": {
            "buttons": {"1":null,"2":null,"3":{"byAgent":{"codex":{"action":"fast"},"pi":{"action":"fast"},"default":null}},"4":{"action":"prompt","prompt":"/copy","submit":true},"5":null,"6":null,"7":{"action":"submit"}},
            "dial":{
                "clockwise":{"byAgent":{
                    "codex":{"action":"script","command":"/bin/sh","args":["./integrations/thinking-effort.sh","alt+."]},
                    "claude":{"action":"script","command":"/bin/sh","args":["./integrations/thinking-effort.sh","claude","raise"]},
                    "pi":{"action":"script","command":"/bin/sh","args":["./integrations/thinking-effort.sh","ctrl+shift+right"]},
                    "default":null
                }},
                "counterclockwise":{"byAgent":{
                    "codex":{"action":"script","command":"/bin/sh","args":["./integrations/thinking-effort.sh","alt+,"]},
                    "claude":{"action":"script","command":"/bin/sh","args":["./integrations/thinking-effort.sh","claude","lower"]},
                    "pi":{"action":"script","command":"/bin/sh","args":["./integrations/thinking-effort.sh","ctrl+shift+left"]},
                    "default":null
                }},
                "press":{"action":"prompt","prompt":"/model","submit":true}
            },
            "joystick":{"engageDistance":0.75,"releaseDistance":0.3,"up":null,"down":null,"left":{"action":"focus-pane","direction":"left"},"right":{"action":"focus-pane","direction":"right"}}
        },
        "lighting": {
            "states": {
                "blocked":{"color":"#ffaa00","brightness":1,"effect":"solid","speed":0},
                "done":{"color":"#22cc55","brightness":1,"effect":"solid","speed":0},
                "working":{"color":"#2277ff","brightness":1,"effect":"breath","speed":0.35},
                "idle":{"color":"#ffffff","brightness":0.25,"effect":"solid","speed":0},
                "unknown":{"color":"#ffffff","brightness":0.08,"effect":"solid","speed":0}
            },
            "focusedBrightness":1,
            "ambient":true,
            "keys":false
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
    fn agent_status_declaration_order_is_display_priority() {
        assert!(
            [
                AgentStatus::Blocked,
                AgentStatus::Done,
                AgentStatus::Working,
                AgentStatus::Idle,
                AgentStatus::Unknown,
            ]
            .is_sorted()
        );
    }

    #[test]
    fn controls_reject_unknown_fields_and_map_reversed_dial_labels() {
        let controls = Config::default().controls;
        assert!(controls.joystick.up.is_none());
        assert!(controls.joystick.down.is_none());
        for action in [-1, 0, 1, 2, 3] {
            assert!(key_binding(&controls, "ENC_CC", action).is_some());
        }
        for (key, agent, expected) in [
            ("ENC_CC", "codex", "alt+."),
            ("ENC_CW", "codex", "alt+,"),
            ("ENC_CC", "pi", "ctrl+shift+right"),
            ("ENC_CW", "pi", "ctrl+shift+left"),
        ] {
            match key_binding(&controls, key, 1)
                .unwrap()
                .resolve(agent)
                .unwrap()
            {
                Action::Script { args, .. } => {
                    assert_eq!(args[1], expected);
                }
                action => panic!("unexpected {agent} dial action: {action:?}"),
            }
        }
        for (key, direction) in [("ENC_CC", "raise"), ("ENC_CW", "lower")] {
            match key_binding(&controls, key, 1)
                .unwrap()
                .resolve("claude")
                .unwrap()
            {
                Action::Script { args, .. } => {
                    assert_eq!(
                        args,
                        ["./integrations/thinking-effort.sh", "claude", direction]
                    );
                }
                action => panic!("unexpected Claude dial action: {action:?}"),
            }
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
            [false, false, true, true, true, false, true]
        );
        let mut reserved = config_json();
        reserved["controls"]["buttons"]["5"] = serde_json::json!({"action":"submit"});
        assert_eq!(
            parse_config(&reserved).unwrap_err(),
            "button 5 is reserved for the Codex Micro service and must be null"
        );
        let mut omitted = config_json();
        omitted["controls"]["buttons"]
            .as_object_mut()
            .unwrap()
            .remove("5");
        assert!(parse_config(&omitted).is_ok());
        let mut stale = config_json();
        stale["controls"]["actionDeviceKeys"] = serde_json::json!({});
        assert!(parse_config(&stale).is_err());
        let mut removed_scroll = config_json();
        removed_scroll["controls"]["joystick"]["up"] =
            serde_json::json!({"action":"scroll","direction":"up","percent":50});
        assert!(parse_config(&removed_scroll).is_err());
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
        named["controls"]["buttons"]["6"] = serde_json::json!({"action":"key","key":"F19"});
        let named = parse_config(&named).unwrap().controls;
        assert!(requires_accessibility(&named));
        assert!(matches!(
            named.buttons[&6],
            Some(Binding::Action(Action::Key { .. }))
        ));
        let mut raw = config_json();
        raw["controls"]["buttons"]["6"] =
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
            bad["controls"]["buttons"]["6"] = invalid;
            assert!(parse_config(&bad).is_err());
        }
        assert_eq!(key_action_code(Some("F19"), None), Ok(0x50));
        assert_eq!(key_action_code(None, Some(0x50)), Ok(0x50));

        let mut joystick = config_json();
        joystick["controls"]["joystick"]["left"] =
            serde_json::json!({"byAgent":{"codex":{"action":"submit"},"default":null}});
        joystick["controls"]["joystick"]["right"] = serde_json::json!({"action":"key","key":"F19"});
        let joystick = parse_config(&joystick).unwrap().controls;
        assert_eq!(
            joystick.joystick.left.as_ref().unwrap().resolve("codex"),
            Some(Action::Submit)
        );
        assert!(requires_accessibility(&joystick));
    }

    #[test]
    fn validates_scripts_and_lighting() {
        let mut config = config_json();
        config["controls"]["dial"]["clockwise"] =
            serde_json::json!({"action":"script","command":" "});
        assert!(parse_config(&config).is_err());
        let mut lighting = config_json();
        lighting["lighting"] =
            serde_json::json!({"states":{},"focusedBrightness":1,"ambient":true,"keys":false});
        assert!(parse_config(&lighting).is_err());
    }

    #[test]
    fn continuous_controls_reject_gesture_bindings() {
        for direction in ["clockwise", "counterclockwise"] {
            let mut config = config_json();
            config["controls"]["dial"][direction] = serde_json::json!({"tap":{"action":"submit"}});
            assert!(
                parse_config(&config)
                    .unwrap_err()
                    .contains("does not support gesture bindings")
            );
        }
        for direction in ["up", "down", "left", "right"] {
            let mut config = config_json();
            config["controls"]["joystick"][direction] =
                serde_json::json!({"tap":{"action":"submit"}});
            assert!(
                parse_config(&config)
                    .unwrap_err()
                    .contains("does not support gesture bindings")
            );
        }
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
