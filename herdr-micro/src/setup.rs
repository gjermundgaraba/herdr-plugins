//! One-shot setup and small action helpers for the Micro plugin.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    actions::{HERDR_LAYER, layer_identity},
    config::{
        Controls, config_path, function_key_number, load_controls, load_effort, load_lighting,
        provision_controls, provision_effort, provision_lighting,
    },
    control::{ensure_state_dir, request_status},
    device::{
        DEFAULT_REQUEST_TIMEOUT, DeviceEvent,
        keymap::{read_keymap, update_keymap_with, write_keymap},
    },
    hid::HidClient,
};

const PI_EXTENSION: &str = ".pi/agent/extensions/herdr-micro-effort.ts";
const REQUIRED_OAI_CODES: [&str; 16] = [
    "KV_OAI_AG00",
    "KV_OAI_AG01",
    "KV_OAI_AG02",
    "KV_OAI_AG03",
    "KV_OAI_AG04",
    "KV_OAI_AG05",
    "KV_OAI_ACT06",
    "KV_OAI_ACT07",
    "KV_OAI_ACT08",
    "KV_OAI_ACT09",
    "KV_OAI_ACT10",
    "KV_OAI_ACT11",
    "KV_OAI_ACT12",
    "KV_OAI_ENC_CC",
    "KV_OAI_ENC_CW",
    "KV_OAI_ENC_CLK",
];
const BUTTON_KEY_SLOTS: [(u8, &str, &str); 7] = [
    (1, "/keymap/2/0", "KV_OAI_ACT06"),
    (2, "/keymap/2/1", "KV_OAI_ACT07"),
    (3, "/keymap/2/2", "KV_OAI_ACT08"),
    (4, "/keymap/2/3", "KV_OAI_ACT09"),
    (5, "/keymap/3/0", "KV_OAI_ACT10"),
    (6, "/keymap/3/1", "KV_OAI_ACT11"),
    (7, "/keymap/3/2", "KV_OAI_ACT12"),
];
const AGENT_KEY_SLOTS: [(u8, &str, &str); 6] = [
    (1, "/keymap/0/0", "KV_OAI_AG00"),
    (2, "/keymap/0/1", "KV_OAI_AG01"),
    (3, "/keymap/1/0", "KV_OAI_AG02"),
    (4, "/keymap/1/1", "KV_OAI_AG03"),
    (5, "/keymap/1/2", "KV_OAI_AG04"),
    (6, "/keymap/1/3", "KV_OAI_AG05"),
];

/// Resolve the plugin directory without depending on the action's current
/// directory. The installed executable lives at `<plugin>/bin/herdr-micro`.
pub fn plugin_root_from(plugin_root: Option<OsString>, executable: &Path) -> Result<PathBuf> {
    if let Some(root) = plugin_root.filter(|root| !root.is_empty()) {
        return Ok(PathBuf::from(root));
    }
    if executable.parent().and_then(Path::file_name) == Some("bin".as_ref()) {
        return executable
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("cannot derive plugin root from {}", executable.display()));
    }
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

pub fn plugin_root() -> Result<PathBuf> {
    plugin_root_from(env::var_os("HERDR_PLUGIN_ROOT"), &env::current_exe()?)
}

fn oai_profile_index(keymap: &Value, managed_link_id: Option<&Value>) -> Result<usize> {
    let profiles = keymap
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("keymap profiles must be an array"))?;
    let matches: Vec<_> = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| {
            profile.pointer("/layers/0/layout").is_some_and(|layout| {
                layout_codes(layout)
                    .iter()
                    .any(|code| code.as_str() == Some("KV_OAI_AG05"))
            })
        })
        .map(|(index, _)| index)
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0]);
    }
    let managed: Vec<_> = managed_link_id
        .into_iter()
        .flat_map(|id| {
            matches
                .iter()
                .copied()
                .filter(move |index| profiles[*index].pointer("/layers/1/linkedAppId") == Some(id))
        })
        .collect();
    if managed.len() == 1 {
        return Ok(managed[0]);
    }
    bail!(
        "expected one OAI or managed profile, found {} OAI and {} managed",
        matches.len(),
        managed.len()
    )
}

fn layout_codes(layout: &Value) -> Vec<&Value> {
    fn collect<'a>(value: &'a Value, codes: &mut Vec<&'a Value>) {
        match value {
            Value::Array(values) => values.iter().for_each(|value| collect(value, codes)),
            value => codes.push(value),
        }
    }

    let mut codes = Vec::new();
    if let Some(keymap) = layout.get("keymap") {
        collect(keymap, &mut codes);
    }
    if let Some(encoders) = layout.get("encoders") {
        collect(encoders, &mut codes);
    }
    if let Some(sectors) = layout
        .pointer("/joystick/sectors")
        .and_then(Value::as_array)
    {
        sectors
            .iter()
            .filter_map(|sector| sector.get("k"))
            .for_each(|code| collect(code, &mut codes));
    }
    codes
}

fn is_blank_layer(layer: &Value) -> bool {
    layer.get("layout")
        == Some(&json!({
            "keymap": [
                ["KC_NONE", "KC_NONE"],
                ["KC_NONE", "KC_NONE", "KC_NONE", "KC_NONE"],
                ["KC_NONE", "KC_NONE", "KC_NONE", "KC_NONE"],
                ["KC_NONE", "KC_NONE", "KC_NONE"]
            ],
            "encoders": [["KC_NONE", "KC_NONE", "KC_NONE"]],
            "joystick": {
                "type": "RADIAL",
                "sectors": [
                    {"k": "KI_X", "a1": 0.1875, "a2": 0.3125},
                    {"k": "KC_NONE", "a1": 0.3125, "a2": 0.1875}
                ]
            }
        }))
}

/// Copies the compatible Layer 1 layout into blank or previously managed Layer 2.
pub fn configure_micro(keymap: &mut Value) -> Result<()> {
    configure_micro_with_hid(
        keymap,
        &crate::config::default_controls().action_device_keys,
    )
}

fn configure_micro_with_hid(
    keymap: &mut Value,
    hid_keys: &std::collections::BTreeMap<u8, Option<String>>,
) -> Result<()> {
    let managed_process = layer_identity(HERDR_LAYER).process;
    let mut managed_ids: Vec<Value> = keymap
        .get("linkedApps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|app| app.get("process").and_then(Value::as_str) == Some(&managed_process))
        .filter_map(|app| app.get("id").cloned())
        .collect();
    let managed_link_id = (managed_ids.len() == 1).then(|| managed_ids.pop().unwrap());
    let profile_index = oai_profile_index(keymap, managed_link_id.as_ref())?;
    let profile_id = keymap
        .pointer(&format!("/profiles/{profile_index}/id"))
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("OAI profile ID must be a non-negative integer"))?;
    {
        let profile = keymap
            .get_mut("profiles")
            .and_then(Value::as_array_mut)
            .and_then(|profiles| profiles.get_mut(profile_index))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("OAI profile must be an object"))?;
        let layers = profile
            .get_mut("layers")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("OAI profile layers must be an array"))?;
        if layers.len() < HERDR_LAYER {
            bail!("Layer {HERDR_LAYER} does not exist");
        }
        let source_layout = layers[0]
            .get("layout")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let source_codes = layout_codes(&source_layout);
        if !REQUIRED_OAI_CODES
            .iter()
            .all(|key| source_codes.iter().any(|code| code.as_str() == Some(key)))
        {
            bail!("Layer 1 is not a compatible Codex Micro OAI layout");
        }
        validate_agent_slots(&source_layout)?;
        validate_button_slots(&source_layout)?;
        let target_is_managed = managed_link_id
            .as_ref()
            .is_some_and(|id| layers[HERDR_LAYER - 1].get("linkedAppId") == Some(id));
        if layers[HERDR_LAYER - 1].get("layout") != Some(&source_layout) {
            if !is_blank_layer(&layers[HERDR_LAYER - 1]) {
                if !target_is_managed {
                    bail!("Layer {HERDR_LAYER} is not blank or managed; refusing to overwrite it");
                }
                let mut normalized = layers[HERDR_LAYER - 1]
                    .get("layout")
                    .cloned()
                    .ok_or_else(|| anyhow!("Layer {HERDR_LAYER} layout is missing"))?;
                reset_hid_codes(&mut normalized)?;
                if normalized != source_layout {
                    bail!("Layer {HERDR_LAYER} is not blank or managed; refusing to overwrite it");
                }
            } else {
                layers[HERDR_LAYER - 1]
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("Layer {HERDR_LAYER} must be an object"))?
                    .insert("layout".into(), source_layout);
            }
        }
        let target = layers[HERDR_LAYER - 1]
            .get_mut("layout")
            .ok_or_else(|| anyhow!("Layer {HERDR_LAYER} layout is missing"))?;
        reset_hid_codes(target)?;
        set_hid_codes(target, hid_keys)?;
    }

    let linked = keymap
        .as_object_mut()
        .ok_or_else(|| anyhow!("keymap must be an object"))?
        .entry("linkedApps")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("linkedApps must be an array"))?;
    let mut ids = Vec::new();
    for app in linked.iter() {
        let id = app.get("id").and_then(Value::as_u64);
        match id.filter(|id| *id <= i64::MAX as u64) {
            Some(id) => ids.push(id),
            None => bail!("linked app IDs must be non-negative integers"),
        }
    }
    let mut layer_ids = Vec::new();
    for layer in [1, HERDR_LAYER] {
        let identity = layer_identity(layer);
        let matches: Vec<_> = linked
            .iter()
            .enumerate()
            .filter(|(_, app)| {
                app.get("process").and_then(Value::as_str) == Some(&identity.process)
            })
            .map(|(index, _)| index)
            .collect();
        if matches.len() > 1 {
            bail!("duplicate {} bindings", identity.process);
        }
        let index = if let Some(index) = matches.first() {
            *index
        } else {
            let id = ids
                .iter()
                .copied()
                .max()
                .map_or(0, |id| id.saturating_add(1));
            ids.push(id);
            linked.push(json!({ "id": id }));
            linked.len() - 1
        };
        let id = linked[index]
            .get("id")
            .cloned()
            .ok_or_else(|| anyhow!("linked app ID is missing"))?;
        let app = linked[index]
            .as_object_mut()
            .ok_or_else(|| anyhow!("linked app must be an object"))?;
        app.insert("name".into(), Value::String(identity.app_name));
        app.insert("process".into(), Value::String(identity.process));
        app.insert("path".into(), Value::String(String::new()));
        layer_ids.push(id);
    }
    let profile = keymap
        .get_mut("profiles")
        .and_then(Value::as_array_mut)
        .and_then(|profiles| profiles.get_mut(profile_index))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("OAI profile must be an object"))?;
    let layers = profile
        .get_mut("layers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("OAI profile layers must be an array"))?;
    for (index, id) in layer_ids.into_iter().enumerate() {
        layers[index]
            .as_object_mut()
            .ok_or_else(|| anyhow!("Layer {} must be an object", index + 1))?
            .insert("linkedAppId".into(), id);
    }
    keymap
        .as_object_mut()
        .ok_or_else(|| anyhow!("keymap must be an object"))?
        .insert("activeProfileId".into(), Value::from(profile_id));
    Ok(())
}

fn validate_agent_slots(layout: &Value) -> Result<()> {
    for (slot, pointer, stock) in AGENT_KEY_SLOTS {
        if layout.pointer(pointer).and_then(Value::as_str) != Some(stock) {
            bail!("Layer 1 Agent slot {slot} must use {stock}");
        }
    }
    Ok(())
}

fn validate_button_slots(layout: &Value) -> Result<()> {
    for (button, pointer, stock) in BUTTON_KEY_SLOTS {
        if layout.pointer(pointer).and_then(Value::as_str) != Some(stock) {
            bail!("Layer 1 button {button} must use {stock}");
        }
    }
    Ok(())
}

fn reset_hid_codes(layout: &mut Value) -> Result<()> {
    for (slot, pointer, stock) in AGENT_KEY_SLOTS {
        let value = layout
            .pointer_mut(pointer)
            .ok_or_else(|| anyhow!("Layer 2 Agent slot {slot} is missing"))?;
        let current = value
            .as_str()
            .ok_or_else(|| anyhow!("Layer 2 Agent slot {slot} must be a key code"))?;
        if current != stock {
            bail!("Layer 2 Agent slot {slot} must use {stock}");
        }
    }
    for (slot, pointer, stock) in BUTTON_KEY_SLOTS {
        let value = layout
            .pointer_mut(pointer)
            .ok_or_else(|| anyhow!("Layer 2 key slot {slot} is missing"))?;
        let current = value
            .as_str()
            .ok_or_else(|| anyhow!("Layer 2 key slot {slot} must be a key code"))?;
        if current != stock
            && current != "KC_NONE"
            && current
                .strip_prefix("KC_")
                .and_then(function_key_number)
                .is_none()
        {
            bail!("Layer 2 key slot {slot} has unexpected code {current}");
        }
        *value = Value::String(stock.into());
    }
    Ok(())
}

fn set_hid_codes(
    layout: &mut Value,
    hid_keys: &std::collections::BTreeMap<u8, Option<String>>,
) -> Result<()> {
    for (button, pointer, _) in BUTTON_KEY_SLOTS {
        let slot = layout
            .pointer_mut(pointer)
            .ok_or_else(|| anyhow!("Layer 2 button {button} is missing"))?;
        *slot = Value::String(
            match hid_keys
                .get(&button)
                .ok_or_else(|| anyhow!("button HID key {button} is missing"))?
            {
                Some(key) => format!("KC_{key}"),
                None => "KC_NONE".into(),
            },
        );
    }
    Ok(())
}

fn active_owner() -> Result<Option<String>> {
    let output = Command::new("/bin/ps").args(["-axo", "comm="]).output()?;
    if !output.status.success() {
        bail!("could not inspect device owners");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .find(|command| command.ends_with("/input") || command.ends_with("/ChatGPT"))
        .map(str::to_owned))
}

fn backup_keymap(bytes: &[u8]) -> Result<PathBuf> {
    let dir = ensure_state_dir()?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = dir.join(format!("keymap-before-setup-{stamp}.json"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::File::open(&dir)?.sync_all()?;
    if fs::read(&path)? != bytes {
        bail!("keymap backup verification failed: {}", path.display());
    }
    Ok(path)
}

fn provision_and_validate_configs() -> Result<(PathBuf, PathBuf, PathBuf, Controls)> {
    let controls = config_path("controls.json");
    let effort = config_path("effort.json");
    let lighting = config_path("lighting.json");
    provision_controls(&controls).map_err(|error| anyhow!(error))?;
    provision_effort(&effort).map_err(|error| anyhow!(error))?;
    provision_lighting(&lighting).map_err(|error| anyhow!(error))?;
    let parsed_controls = load_controls(&controls).map_err(|error| anyhow!(error))?;
    load_effort(&effort).map_err(|error| anyhow!(error))?;
    load_lighting(&lighting).map_err(|error| anyhow!(error))?;
    Ok((controls, effort, lighting, parsed_controls))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupReport {
    pub firmware: String,
    pub backup: Option<PathBuf>,
    pub controls: PathBuf,
    pub effort: PathBuf,
    pub lighting: PathBuf,
}

fn update_micro_keymap(
    configure: impl FnOnce(&mut Value) -> Result<()>,
) -> Result<(String, Option<PathBuf>)> {
    if let Some(owner) = active_owner()? {
        bail!("quit {owner} first");
    }
    if let Ok(status) = request_status(Duration::from_millis(250)) {
        // A stopping bridge still holds the device while it drains.
        if status.get("error").is_some() {
            bail!("the Micro bridge is still stopping; retry in a few seconds");
        }
        bail!("stop the Micro bridge first");
    }
    let (event_tx, _events) = mpsc::channel::<DeviceEvent>();
    let mut device = HidClient::connect(event_tx)?;
    let result = (|| {
        let status = device.request("device.status", None, DEFAULT_REQUEST_TIMEOUT)?;
        let firmware = status
            .get("version")
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
            .ok_or_else(|| anyhow!("device did not report a firmware version"))?
            .to_owned();
        let before = read_keymap(&device)?;
        let mut keymap: Value = serde_json::from_slice(&before).context("invalid keymap JSON")?;
        let canonical_before = serde_json::to_vec(&keymap)?;
        configure(&mut keymap)?;
        let after = serde_json::to_vec(&keymap)?;
        let backup = if after == canonical_before {
            None
        } else {
            update_keymap_with(
                &before,
                &after,
                backup_keymap,
                || read_keymap(&device),
                |bytes| write_keymap(&device, bytes),
            )?
        };
        Ok((firmware, backup))
    })();
    let close = device.close();
    match (result, close) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), _) => Err(error),
        (Ok((_, Some(backup))), Err(error)) => Err(error).with_context(|| {
            format!(
                "device close failed after the verified keymap update; backup: {}",
                backup.display()
            )
        }),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub fn setup_micro() -> Result<SetupReport> {
    let (controls, effort, lighting, parsed_controls) = provision_and_validate_configs()?;
    let (firmware, backup) = update_micro_keymap(|keymap| {
        configure_micro_with_hid(keymap, &parsed_controls.action_device_keys)
    })?;
    Ok(SetupReport {
        firmware,
        backup,
        controls,
        effort,
        lighting,
    })
}

pub fn configure_controls() -> Result<PathBuf> {
    let path = config_path("controls.json");
    provision_controls(&path).map_err(|error| anyhow!(error))?;
    load_controls(&path).map_err(|error| anyhow!(error))?;
    let status = Command::new("/usr/bin/open")
        .args(["-t", path.to_string_lossy().as_ref()])
        .status()
        .context("open controls configuration")?;
    if !status.success() {
        bail!("open controls configuration exited with {status}");
    }
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiInstall {
    pub target: PathBuf,
    pub backup: Option<PathBuf>,
    pub unchanged: bool,
}

fn read_pi_target(target: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to replace symlink {}", target.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", target.display())),
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)
        .with_context(|| format!("read {}", target.display()))?;
    if !file.metadata()?.is_file() {
        bail!(
            "Pi extension target is not a regular file: {}",
            target.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub fn install_pi_effort(source: &Path, target: &Path, timestamp: u128) -> Result<PiInstall> {
    let bundled = fs::read(source).with_context(|| format!("read {}", source.display()))?;
    let current = read_pi_target(target)?;
    if current.as_deref() == Some(&bundled) {
        return Ok(PiInstall {
            target: target.into(),
            backup: None,
            unchanged: true,
        });
    }
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("Pi extension target has no parent"))?;
    fs::create_dir_all(parent)?;
    let backup = if let Some(bytes) = current {
        let backup = PathBuf::from(format!("{}.bak-{timestamp}", target.display()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&backup)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Some(backup)
    } else {
        None
    };
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Pi extension target has no file name"))?;
    let temporary = parent.join(format!(".{name}.tmp-{}-{timestamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&temporary)?;
    let written = (|| -> Result<()> {
        file.write_all(&bundled)?;
        file.sync_all()?;
        fs::rename(&temporary, target)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written?;
    Ok(PiInstall {
        target: target.into(),
        backup,
        unchanged: false,
    })
}

pub fn setup_pi_effort() -> Result<PiInstall> {
    let source = plugin_root()?.join("integrations/pi/herdr-effort.js");
    let home = env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    let target = PathBuf::from(home).join(PI_EXTENSION);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    install_pi_effort(&source, &target, timestamp)
}

/// Forward Herdr's status output without adding a Rust-specific wrapper.
/// The action entrypoint should exit with the returned status.
pub fn forward_herdr_status() -> Result<i32> {
    let bin = env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into());
    let output = Command::new(&bin)
        .arg("status")
        .output()
        .with_context(|| format!("run {bin} status"))?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    Ok(output.status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("herdr-micro-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn blank_layer() -> Value {
        json!({
            "layout": {
                "keymap": [
                    ["KC_NONE", "KC_NONE"],
                    ["KC_NONE", "KC_NONE", "KC_NONE", "KC_NONE"],
                    ["KC_NONE", "KC_NONE", "KC_NONE", "KC_NONE"],
                    ["KC_NONE", "KC_NONE", "KC_NONE"]
                ],
                "encoders": [["KC_NONE", "KC_NONE", "KC_NONE"]],
                "joystick": {
                    "type": "RADIAL",
                    "sectors": [
                        { "k": "KI_X", "a1": 0.1875, "a2": 0.3125 },
                        { "k": "KC_NONE", "a1": 0.3125, "a2": 0.1875 }
                    ]
                }
            }
        })
    }

    fn oai_layout() -> Value {
        json!({
            "keymap": [
                ["KV_OAI_AG00", "KV_OAI_AG01"],
                ["KV_OAI_AG02", "KV_OAI_AG03", "KV_OAI_AG04", "KV_OAI_AG05"],
                ["KV_OAI_ACT06", "KV_OAI_ACT07", "KV_OAI_ACT08", "KV_OAI_ACT09"],
                ["KV_OAI_ACT10", "KV_OAI_ACT11", "KV_OAI_ACT12"]
            ],
            "encoders": [["KV_OAI_ENC_CC", "KV_OAI_ENC_CW", "KV_OAI_ENC_CLK"]],
            "joystick": { "type": "VENDOR", "sectors": [] }
        })
    }

    #[test]
    fn derives_plugin_root_without_cwd() {
        assert_eq!(
            plugin_root_from(None, Path::new("/plugins/micro/bin/herdr-micro")).unwrap(),
            PathBuf::from("/plugins/micro")
        );
        assert_eq!(
            plugin_root_from(
                Some(OsString::from("/override")),
                Path::new("/ignored/bin/x")
            )
            .unwrap(),
            PathBuf::from("/override")
        );
        assert_eq!(
            plugin_root_from(None, Path::new("/project/target/debug/herdr-micro")).unwrap(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        );
    }

    #[test]
    fn configures_hid_keys_on_a_blank_layer_two() {
        let source = oai_layout();
        let mut keymap = json!({
            "activeProfileId": 7,
            "profiles": [{ "id": 7, "layers": [
                { "layout": source },
                blank_layer()
            ] }]
        });
        configure_micro(&mut keymap).unwrap();
        assert_eq!(
            keymap.pointer("/profiles/0/layers/0/linkedAppId"),
            Some(&json!(0))
        );
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/linkedAppId"),
            Some(&json!(1))
        );
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/layout/keymap/0"),
            Some(&json!(["KV_OAI_AG00", "KV_OAI_AG01"]))
        );
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/layout/keymap/3/0"),
            Some(&json!("KC_F19"))
        );
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/layout/keymap/2/0"),
            Some(&json!("KC_F20"))
        );
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/layout/keymap/3/1"),
            Some(&json!("KC_NONE"))
        );
        let configured = keymap.clone();
        configure_micro(&mut keymap).unwrap();
        assert_eq!(keymap, configured);
        *keymap
            .pointer_mut("/profiles/0/layers/1/layout/keymap/0/0")
            .unwrap() = json!("KC_F13");
        assert!(
            configure_micro(&mut keymap)
                .unwrap_err()
                .to_string()
                .contains("must use KV_OAI_AG00")
        );
    }

    #[test]
    fn prefers_the_existing_managed_profile_and_activates_it() {
        let source = oai_layout();
        let blank = blank_layer();
        let mut keymap = json!({
            "activeProfileId": 1,
            "linkedApps": [
                {"id": 10, "process": "gjermundgaraba.herdr-micro.layer-2"},
                {"id": 11, "process": "com.mitchellh.ghostty"}
            ],
            "profiles": [
                {"id": 0, "layers": [
                    {"layout": source},
                    {"linkedAppId": 10, "layout": oai_layout()}
                ]},
                {"id": 1, "layers": [
                    {"layout": oai_layout()},
                    {"linkedAppId": 11, "layout": blank["layout"].clone()}
                ]}
            ]
        });
        configure_micro(&mut keymap).unwrap();
        assert_eq!(keymap["activeProfileId"], json!(0));
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/layout/keymap/0/0"),
            Some(&json!("KV_OAI_AG00"))
        );
        assert_eq!(
            keymap.pointer("/profiles/0/layers/1/layout/keymap/1/3"),
            Some(&json!("KV_OAI_AG05"))
        );
        assert_eq!(
            keymap.pointer("/profiles/1/layers/1/linkedAppId"),
            Some(&json!(11))
        );
        assert_eq!(
            keymap.pointer("/profiles/1/layers/1/layout"),
            Some(&blank["layout"])
        );
    }

    #[test]
    fn refuses_an_unmarked_layer_with_function_keys() {
        let source = oai_layout();
        let mut target = source.clone();
        *target.pointer_mut("/keymap/3/0").unwrap() = json!("KC_F19");
        let mut keymap = json!({
            "profiles": [{ "id": 0, "layers": [
                { "layout": source },
                { "layout": target }
            ] }]
        });
        assert!(
            configure_micro(&mut keymap)
                .unwrap_err()
                .to_string()
                .contains("not blank or managed")
        );
    }

    #[test]
    fn refuses_unknown_or_missing_layer_two_layouts() {
        for layer in [
            json!({}),
            json!({ "layout": { "keymap": [["KC_NONE"]] } }),
            json!({ "layout": { "keymap": [["KC_NONE"]], "encoders": [], "joystick": { "type": "RADIAL", "sectors": [] }, "extra": true } }),
            json!({ "layout": { "keymap": [], "encoders": [], "joystick": { "type": "RADIAL", "sectors": [] } } }),
            json!({ "layout": { "keymap": [["KC_NONE"]], "encoders": [["KC_NONE", "KC_NONE", "KC_NONE"]], "joystick": { "type": "RADIAL", "sectors": [{ "k": "KI_X", "a1": 0.1875, "a2": 0.3125 }] } } }),
            json!({ "layout": { "keymap": [["KC_NONE"]], "encoders": [], "joystick": { "type": "RADIAL", "sectors": [{ "k": "KI_X", "a1": "bad", "a2": 0.25 }] } } }),
        ] {
            let mut keymap = json!({
                "profiles": [{ "id": 0, "layers": [
                    { "layout": { "keymap": [REQUIRED_OAI_CODES] } },
                    layer
                ] }]
            });
            assert!(configure_micro(&mut keymap).is_err());
        }
    }

    #[test]
    fn rejects_metadata_and_partial_layout_code_matches() {
        let mut required = REQUIRED_OAI_CODES;
        required[5] = "KV_OAI_AG05-extra";
        let mut keymap = json!({
            "profiles": [{ "id": 0, "layers": [
                {
                    "layout": { "keymap": [required], "metadata": "KV_OAI_AG05" },
                    "description": "KV_OAI_AG05"
                },
                { "layout": { "keymap": [["KC_NONE"]] } }
            ] }]
        });
        assert_eq!(
            configure_micro(&mut keymap).unwrap_err().to_string(),
            "expected one OAI or managed profile, found 0 OAI and 0 managed"
        );
    }

    #[test]
    fn installs_pi_extension_once_and_backs_up_changes() {
        let root = temp("pi-install");
        let source = root.join("source.js");
        let target = root.join("nested/extension.ts");
        fs::write(&source, b"one").unwrap();
        let first = install_pi_effort(&source, &target, 10).unwrap();
        assert!(!first.unchanged);
        assert_eq!(fs::read(&target).unwrap(), b"one");
        assert!(install_pi_effort(&source, &target, 11).unwrap().unchanged);
        fs::write(&source, b"two").unwrap();
        let changed = install_pi_effort(&source, &target, 12).unwrap();
        assert_eq!(fs::read(changed.backup.unwrap()).unwrap(), b"one");
        assert_eq!(fs::read(&target).unwrap(), b"two");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pi_install_refuses_symlinks_without_touching_their_target() {
        use std::os::unix::fs::symlink;

        let root = temp("pi-symlink");
        let source = root.join("source.js");
        let outside = root.join("outside.ts");
        let target = root.join("extension.ts");
        fs::write(&source, b"new").unwrap();
        fs::write(&outside, b"old").unwrap();
        symlink(&outside, &target).unwrap();
        assert!(install_pi_effort(&source, &target, 10).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pi_install_failure_leaves_live_extension_unchanged() {
        let root = temp("pi-failure");
        let source = root.join("source.js");
        let target = root.join("extension.ts");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"old").unwrap();
        fs::write(
            root.join(format!(".extension.ts.tmp-{}-10", std::process::id())),
            b"occupied",
        )
        .unwrap();
        assert!(install_pi_effort(&source, &target, 10).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(fs::read(root.join("extension.ts.bak-10")).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }
}
