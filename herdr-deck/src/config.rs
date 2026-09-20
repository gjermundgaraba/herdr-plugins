use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub const CONFIG_ENV: &str = "HERDR_DECK_CONFIG";
pub const DEFAULT_CONFIG_PATH: &str = "~/.config/herdr-deck/config.json";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AgentStatus {
    Unknown,
    Idle,
    Working,
    Done,
    Blocked,
}

impl AgentStatus {
    pub const ALL: [Self; 5] = [
        Self::Unknown,
        Self::Idle,
        Self::Working,
        Self::Done,
        Self::Blocked,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceRole {
    Dashboard,
    Pedal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Down,
    Left,
    Right,
    Up,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CycleDirection {
    Next,
    Previous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    FocusSlot { slot: Option<u8> },
    FocusPane { direction: Direction },
    SendKeys { keys: Vec<String> },
    Prompt { text: String, submit: Option<bool> },
    Submit,
    SystemEnter,
    SystemKey { key: crate::system_input::Key },
    CycleSession { direction: CycleDirection },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EncoderActions {
    pub press: Option<Action>,
    pub clockwise: Option<Action>,
    pub counterclockwise: Option<Action>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceConfig {
    pub brightness: u8,
    pub enabled: bool,
    pub role: DeviceRole,
    pub serial: Option<String>,
    pub model: Option<String>,
    pub buttons: HashMap<String, Action>,
    pub encoders: HashMap<String, EncoderActions>,
    pub touch: HashMap<String, Action>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceIdentity {
    pub model: String,
    pub serial_number: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateColors {
    pub background: String,
    pub foreground: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HerdrConfig {
    pub sessions: Vec<String>,
    pub slot_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub herdr: HerdrConfig,
    pub states: HashMap<AgentStatus, StateColors>,
    pub devices: Vec<DeviceConfig>,
}

pub fn select_device_config<'a>(
    devices: &'a [DeviceConfig],
    device: &DeviceIdentity,
) -> Option<&'a DeviceConfig> {
    let role = if device.model == "pedal" {
        DeviceRole::Pedal
    } else {
        DeviceRole::Dashboard
    };
    let matched = devices
        .iter()
        .find(|rule| {
            rule.serial.as_deref().is_some()
                && rule.serial.as_deref() == device.serial_number.as_deref()
        })
        .or_else(|| {
            devices
                .iter()
                .find(|rule| rule.model.as_deref() == Some(device.model.as_str()))
        })
        .or_else(|| {
            devices
                .iter()
                .find(|rule| rule.serial.is_none() && rule.model.is_none() && rule.role == role)
        });
    matched.filter(|rule| rule.enabled)
}

pub fn expand_home(path: &str) -> Result<PathBuf, String> {
    let expanded = if path == "~" || path.starts_with("~/") {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "cannot resolve home directory: HOME is not set".to_owned())?;
        if path == "~" {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(&path[2..])
        }
    } else {
        PathBuf::from(path)
    };
    std::path::absolute(&expanded)
        .map_err(|error| format!("cannot resolve {}: {error}", expanded.display()))
}

pub fn config_path(override_path: Option<&str>) -> Result<PathBuf, String> {
    match override_path {
        Some(path) => expand_home(path),
        None => match env::var(CONFIG_ENV) {
            Ok(path) => expand_home(&path),
            Err(env::VarError::NotPresent) => expand_home(DEFAULT_CONFIG_PATH),
            Err(error) => Err(format!("{CONFIG_ENV} is not valid Unicode: {error}")),
        },
    }
}

pub fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        escaped.push_str(match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            _ => {
                escaped.push(character);
                continue;
            }
        });
    }
    escaped
}

pub fn parse_config(value: Value) -> Result<Config, String> {
    let input = object(&value, "configuration")?;
    fields(input, &["herdr", "states", "devices"], "configuration")?;

    let herdr = object(required(input, "herdr"), "herdr")?;
    fields(herdr, &["sessions", "slotCount"], "herdr")?;

    let sessions = match herdr.get("sessions") {
        Some(value) => value
            .as_array()
            .ok_or_else(|| "herdr.sessions must be an array".to_owned())?
            .iter()
            .enumerate()
            .map(|(index, value)| session_key(value, &format!("herdr.sessions[{index}]")))
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };

    let raw_states = object(required(input, "states"), "states")?;
    fields(
        raw_states,
        &["unknown", "idle", "working", "done", "blocked"],
        "states",
    )?;
    let mut states = HashMap::with_capacity(AgentStatus::ALL.len());
    for status in AgentStatus::ALL {
        let label = format!("states.{}", status.as_str());
        let raw = object(required(raw_states, status.as_str()), &label)?;
        fields(raw, &["background", "foreground"], &label)?;
        let background = string(required(raw, "background"), &format!("{label}.background"))?;
        let foreground = string(required(raw, "foreground"), &format!("{label}.foreground"))?;
        if !color(&background) || !color(&foreground) {
            return Err(format!("{label} colors must be #RRGGBB"));
        }
        states.insert(
            status,
            StateColors {
                background,
                foreground,
            },
        );
    }

    let raw_devices = required(input, "devices")
        .as_array()
        .ok_or_else(|| "devices must be an array".to_owned())?;
    if raw_devices.is_empty() {
        return Err("devices must contain at least one rule".to_owned());
    }
    let devices = raw_devices
        .iter()
        .enumerate()
        .map(|(index, value)| parse_device(value, index))
        .collect::<Result<Vec<_>, _>>()?;
    let mut selectors = HashSet::new();
    for (index, device) in devices.iter().enumerate() {
        let selector = if let Some(serial) = &device.serial {
            format!("serial:{serial}")
        } else if let Some(model) = &device.model {
            format!("model:{model}")
        } else {
            format!(
                "fallback:{}",
                match device.role {
                    DeviceRole::Dashboard => "dashboard",
                    DeviceRole::Pedal => "pedal",
                }
            )
        };
        if !selectors.insert(selector.clone()) {
            return Err(format!(
                "duplicate device selector at devices[{index}]: {selector}"
            ));
        }
    }

    Ok(Config {
        herdr: HerdrConfig {
            sessions,
            slot_count: integer(required(herdr, "slotCount"), "herdr.slotCount", 1, 128)? as usize,
        },
        states,
        devices,
    })
}

pub fn load_config(path: impl AsRef<Path>) -> Result<Config, String> {
    let path = path.as_ref();
    let result = fs::read_to_string(path)
        .map_err(|error| error.to_string())
        .and_then(|contents| serde_json::from_str(&contents).map_err(|error| error.to_string()))
        .and_then(parse_config);
    result.map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn parse_device(value: &Value, index: usize) -> Result<DeviceConfig, String> {
    let label = format!("devices[{index}]");
    let input = object(value, &label)?;
    fields(
        input,
        &[
            "brightness",
            "enabled",
            "role",
            "serial",
            "model",
            "buttons",
            "encoders",
            "touch",
        ],
        &label,
    )?;
    if input.contains_key("serial") && input.contains_key("model") {
        return Err(format!("{label} must select by serial or model, not both"));
    }
    let role = match string(required(input, "role"), &format!("{label}.role"))?.as_str() {
        "dashboard" => DeviceRole::Dashboard,
        "pedal" => DeviceRole::Pedal,
        _ => return Err(format!("{label}.role must be dashboard or pedal")),
    };
    let encoders = object(required(input, "encoders"), &format!("{label}.encoders"))?;
    let mut parsed_encoders = HashMap::with_capacity(encoders.len());
    for (key, raw) in encoders {
        let item_label = format!("{label}.encoders.{key}");
        let item = object(raw, &item_label)?;
        fields(
            item,
            &["press", "clockwise", "counterclockwise"],
            &item_label,
        )?;
        parsed_encoders.insert(
            key.clone(),
            EncoderActions {
                press: optional_action(item, "press", &item_label)?,
                clockwise: optional_action(item, "clockwise", &item_label)?,
                counterclockwise: optional_action(item, "counterclockwise", &item_label)?,
            },
        );
    }
    Ok(DeviceConfig {
        brightness: integer(
            required(input, "brightness"),
            &format!("{label}.brightness"),
            0,
            100,
        )? as u8,
        enabled: boolean(required(input, "enabled"), &format!("{label}.enabled"))?,
        role,
        serial: optional_string(input, "serial", &format!("{label}.serial"))?,
        model: optional_string(input, "model", &format!("{label}.model"))?,
        buttons: parse_action_map(required(input, "buttons"), &format!("{label}.buttons"))?,
        encoders: parsed_encoders,
        touch: parse_action_map(required(input, "touch"), &format!("{label}.touch"))?,
    })
}

fn parse_action(value: &Value, label: &str) -> Result<Action, String> {
    let input = object(value, label)?;
    let kind = string(required(input, "action"), &format!("{label}.action"))?;
    match kind.as_str() {
        "focus-slot" => {
            fields(input, &["action", "slot"], label)?;
            Ok(Action::FocusSlot {
                slot: match input.get("slot") {
                    Some(value) => Some(integer(value, &format!("{label}.slot"), 0, 127)? as u8),
                    None => None,
                },
            })
        }
        "focus-pane" => {
            fields(input, &["action", "direction"], label)?;
            let direction =
                match string(required(input, "direction"), &format!("{label}.direction"))?.as_str()
                {
                    "down" => Direction::Down,
                    "left" => Direction::Left,
                    "right" => Direction::Right,
                    "up" => Direction::Up,
                    _ => {
                        return Err(format!(
                            "{label}.direction must be down, left, right, or up"
                        ));
                    }
                };
            Ok(Action::FocusPane { direction })
        }
        "send-keys" => {
            fields(input, &["action", "keys"], label)?;
            let keys = required(input, "keys")
                .as_array()
                .filter(|keys| !keys.is_empty())
                .ok_or_else(|| format!("{label}.keys must be a non-empty array"))?;
            Ok(Action::SendKeys {
                keys: keys
                    .iter()
                    .enumerate()
                    .map(|(index, key)| string(key, &format!("{label}.keys[{index}]")))
                    .collect::<Result<_, _>>()?,
            })
        }
        "prompt" => {
            fields(input, &["action", "text", "submit"], label)?;
            Ok(Action::Prompt {
                text: string(required(input, "text"), &format!("{label}.text"))?,
                submit: match input.get("submit") {
                    Some(value) => Some(boolean(value, &format!("{label}.submit"))?),
                    None => None,
                },
            })
        }
        "system-key" => {
            fields(input, &["action", "key"], label)?;
            let key = match string(required(input, "key"), &format!("{label}.key"))?.as_str() {
                "enter" => crate::system_input::Key::Enter,
                "f19" => crate::system_input::Key::F19,
                _ => return Err(format!("{label}.key must be enter or f19")),
            };
            Ok(Action::SystemKey { key })
        }
        "system-enter" => {
            fields(input, &["action"], label)?;
            Ok(Action::SystemEnter)
        }
        "submit" => {
            fields(input, &["action"], label)?;
            Ok(Action::Submit)
        }
        "cycle-session" => {
            fields(input, &["action", "direction"], label)?;
            let direction = match input.get("direction") {
                None => CycleDirection::Next,
                Some(value) => match string(value, &format!("{label}.direction"))?.as_str() {
                    "next" => CycleDirection::Next,
                    "previous" => CycleDirection::Previous,
                    _ => {
                        return Err(format!("{label}.direction must be next or previous"));
                    }
                },
            };
            Ok(Action::CycleSession { direction })
        }
        _ => Err(format!("unknown {label}.action: {kind}")),
    }
}

fn parse_action_map(value: &Value, label: &str) -> Result<HashMap<String, Action>, String> {
    object(value, label)?
        .iter()
        .map(|(key, action)| {
            Ok((
                key.clone(),
                parse_action(action, &format!("{label}.{key}"))?,
            ))
        })
        .collect()
}

fn optional_action(
    input: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<Option<Action>, String> {
    input
        .get(field)
        .map(|value| parse_action(value, &format!("{label}.{field}")))
        .transpose()
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))
}

fn fields(input: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<(), String> {
    if let Some(extra) = input.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown {label} field: {extra}"));
    }
    Ok(())
}

fn required<'a>(input: &'a Map<String, Value>, field: &str) -> &'a Value {
    input.get(field).unwrap_or(&Value::Null)
}

fn string(value: &Value, label: &str) -> Result<String, String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} must be a non-empty string"))
}

fn session_key(value: &Value, label: &str) -> Result<String, String> {
    let key = string(value, label)?;
    let valid = key.split_once('/').is_some_and(|(host, session)| {
        !host.is_empty()
            && !session.is_empty()
            && host.trim() == host
            && session.trim() == session
            && !session.contains('/')
    });
    if !valid {
        return Err(format!("{label} must be a <host>/<session> key"));
    }
    Ok(key)
}

fn optional_string(
    input: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<Option<String>, String> {
    input
        .get(field)
        .map(|value| string(value, label))
        .transpose()
}

fn integer(value: &Value, label: &str, min: i64, max: i64) -> Result<i64, String> {
    let number = value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|number| number.is_finite() && number.fract() == 0.0)
            .map(|number| number as i64)
    });
    number
        .filter(|number| (min..=max).contains(number))
        .ok_or_else(|| format!("{label} must be an integer from {min} to {max}"))
}

fn boolean(value: &Value, label: &str) -> Result<bool, String> {
    value
        .as_bool()
        .ok_or_else(|| format!("{label} must be boolean"))
}

fn color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> Value {
        json!({
            "herdr": { "slotCount": 8, "sessions": [] },
            "states": {
                "unknown": { "background": "#000000", "foreground": "#ffffff" },
                "idle": { "background": "#000000", "foreground": "#ffffff" },
                "working": { "background": "#000000", "foreground": "#ffffff" },
                "done": { "background": "#000000", "foreground": "#ffffff" },
                "blocked": { "background": "#000000", "foreground": "#ffffff" }
            },
            "devices": [{
                "serial": "one", "role": "dashboard", "brightness": 80,
                "enabled": true, "buttons": { "0": { "action": "focus-slot" } },
                "encoders": {}, "touch": {}
            }]
        })
    }

    #[test]
    fn parses_complete_configuration_and_all_actions() {
        let mut value = valid();
        value["herdr"]["sessions"] = json!(["local/default", "local/review"]);
        value["devices"][0]["buttons"] = json!({
            "0": { "action": "focus-slot" },
            "1": { "action": "focus-pane", "direction": "left" },
            "2": { "action": "send-keys", "keys": ["ctrl-c"] },
            "3": { "action": "prompt", "text": "hello", "submit": false },
            "4": { "action": "submit" },
            "5": { "action": "cycle-session" },
            "6": { "action": "system-enter" }
        });
        let config = parse_config(value).unwrap();
        assert_eq!(config.devices[0].buttons["6"], Action::SystemEnter);
        assert_eq!(config.herdr.sessions, ["local/default", "local/review"]);
        assert_eq!(config.devices[0].serial.as_deref(), Some("one"));
        assert_eq!(
            config.devices[0].buttons["0"],
            Action::FocusSlot { slot: None }
        );
        assert_eq!(
            config.devices[0].buttons["5"],
            Action::CycleSession {
                direction: CycleDirection::Next
            }
        );
    }

    #[test]
    fn system_keys_are_explicit_and_strict() {
        for (name, key) in [
            ("enter", crate::system_input::Key::Enter),
            ("f19", crate::system_input::Key::F19),
        ] {
            assert_eq!(
                parse_action(&json!({"action": "system-key", "key": name}), "binding").unwrap(),
                Action::SystemKey { key }
            );
        }
        for invalid in [
            json!({"action": "system-key"}),
            json!({"action": "system-key", "key": "f20"}),
            json!({"action": "system-key", "key": 80}),
            json!({"action": "system-key", "key": "f19", "keys": ["enter"]}),
        ] {
            assert!(parse_action(&invalid, "binding").is_err());
        }
    }

    #[test]
    fn sessions_allow_list_is_optional_and_strict() {
        let mut omitted = valid();
        omitted["herdr"].as_object_mut().unwrap().remove("sessions");
        assert!(parse_config(omitted).unwrap().herdr.sessions.is_empty());

        let mut malformed = valid();
        malformed["herdr"]["sessions"] = json!(["local/default", 1]);
        assert!(
            parse_config(malformed)
                .unwrap_err()
                .contains("herdr.sessions[1] must be a non-empty string")
        );

        let mut bare_name = valid();
        bare_name["herdr"]["sessions"] = json!(["default"]);
        assert!(
            parse_config(bare_name)
                .unwrap_err()
                .contains("herdr.sessions[0] must be a <host>/<session> key")
        );

        let mut legacy = valid();
        legacy["herdr"].as_object_mut().unwrap().remove("sessions");
        legacy["herdr"]["session"] = json!("default");
        assert!(
            parse_config(legacy)
                .unwrap_err()
                .contains("unknown herdr field: session")
        );
    }

    #[test]
    fn rejects_unknown_fields_empty_devices_and_duplicate_selectors() {
        let mut unknown = valid();
        unknown["surprise"] = json!(true);
        assert!(
            parse_config(unknown)
                .unwrap_err()
                .contains("unknown configuration field")
        );

        let mut empty = valid();
        empty["devices"] = json!([]);
        assert!(
            parse_config(empty)
                .unwrap_err()
                .contains("at least one rule")
        );

        let mut duplicate = valid();
        let rule = duplicate["devices"][0].clone();
        duplicate["devices"].as_array_mut().unwrap().push(rule);
        assert!(
            parse_config(duplicate)
                .unwrap_err()
                .contains("duplicate device selector")
        );
    }

    #[test]
    fn rejects_malformed_actions_colors_booleans_and_ranges() {
        let mut action = valid();
        action["devices"][0]["buttons"]["0"] =
            json!({ "action": "focus-pane", "direction": "diagonal" });
        assert!(
            parse_config(action)
                .unwrap_err()
                .contains("direction must be")
        );

        let mut color = valid();
        color["states"]["idle"]["background"] = json!("blue");
        assert!(parse_config(color).unwrap_err().contains("colors must be"));

        let mut boolean = valid();
        boolean["devices"][0]["enabled"] = json!("false");
        assert!(
            parse_config(boolean)
                .unwrap_err()
                .contains("enabled must be boolean")
        );

        let mut range = valid();
        range["herdr"]["slotCount"] = json!(129);
        assert!(
            parse_config(range)
                .unwrap_err()
                .contains("integer from 1 to 128")
        );
    }

    #[test]
    fn selectors_prefer_exact_rules_including_disabled_rules() {
        let mut value = valid();
        value["devices"] = json!([
            { "serial": "one", "role": "dashboard", "brightness": 80, "enabled": false, "buttons": {}, "encoders": {}, "touch": {} },
            { "model": "plus", "role": "dashboard", "brightness": 80, "enabled": true, "buttons": {}, "encoders": {}, "touch": {} },
            { "role": "dashboard", "brightness": 80, "enabled": true, "buttons": {}, "encoders": {}, "touch": {} }
        ]);
        let devices = parse_config(value).unwrap().devices;
        assert!(select_device_config(&devices, &identity("plus", Some("one"))).is_none());
        assert_eq!(
            select_device_config(&devices, &identity("plus", Some("two")))
                .unwrap()
                .model
                .as_deref(),
            Some("plus")
        );
        assert_eq!(
            select_device_config(&devices, &identity("plus", None))
                .unwrap()
                .model
                .as_deref(),
            Some("plus")
        );
        assert!(select_device_config(&devices, &identity("other", None)).is_some());
    }

    #[test]
    fn escapes_xml_and_wraps_load_errors() {
        assert_eq!(escape_xml("<&>\""), "&lt;&amp;&gt;&quot;");
        let error = load_config("/definitely/missing/herdr-deck.json").unwrap_err();
        assert!(error.starts_with("cannot read /definitely/missing/herdr-deck.json:"));
    }

    #[test]
    fn resolves_override_and_home_paths() {
        assert_eq!(config_path(Some(".")).unwrap(), env::current_dir().unwrap());
        let home = PathBuf::from(env::var_os("HOME").unwrap());
        assert_eq!(expand_home("~").unwrap(), home);
        assert_eq!(
            expand_home("~/config.json").unwrap(),
            home.join("config.json")
        );
    }

    fn identity(model: &str, serial_number: Option<&str>) -> DeviceIdentity {
        DeviceIdentity {
            model: model.to_owned(),
            serial_number: serial_number.map(str::to_owned),
        }
    }
}
