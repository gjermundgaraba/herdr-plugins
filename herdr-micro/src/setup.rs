//! One-shot setup and small action helpers for the Micro plugin.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    actions::{layer_identity, HERDR_LAYER},
    config::{config_path, load_controls, load_effort, load_lighting},
    control::{ensure_state_dir, request_status},
    device::{DeviceEvent, MicroDevice, DEFAULT_REQUEST_TIMEOUT},
};

const READ_CHUNK: usize = 512;
const WRITE_CHUNK: usize = 384;
const PI_EXTENSION: &str = ".pi/agent/extensions/herdr-micro-effort.ts";

/// Resolve the plugin directory without depending on the action's current
/// directory. The installed executable lives at `<plugin>/bin/herdr-micro`.
pub fn plugin_root_from(plugin_root: Option<OsString>, executable: &Path) -> Result<PathBuf> {
    if let Some(root) = plugin_root.filter(|root| !root.is_empty()) {
        return Ok(PathBuf::from(root));
    }
    executable
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("cannot derive plugin root from {}", executable.display()))
}

pub fn plugin_root() -> Result<PathBuf> {
    plugin_root_from(env::var_os("HERDR_PLUGIN_ROOT"), &env::current_exe()?)
}

fn keymap_chunk(value: Value) -> Result<(Vec<u8>, usize)> {
    let data = value
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("keymap response is missing data"))?;
    let total = value
        .get("total_size")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("keymap response is missing total_size"))?
        .try_into()
        .map_err(|_| anyhow!("keymap is too large"))?;
    Ok((STANDARD.decode(data).context("invalid keymap data")?, total))
}

pub fn read_keymap_with<F>(mut read: F) -> Result<Vec<u8>>
where
    F: FnMut(usize) -> Result<Value>,
{
    let mut offset = 0;
    let mut body = Vec::new();
    loop {
        let (chunk, total) = keymap_chunk(read(offset)?)?;
        if chunk.is_empty() {
            bail!("empty keymap chunk");
        }
        if offset
            .checked_add(chunk.len())
            .map_or(true, |end| end > total)
        {
            bail!("invalid keymap chunk size");
        }
        offset += chunk.len();
        body.extend(chunk);
        if offset >= total {
            return Ok(body);
        }
    }
}

fn read_keymap(device: &MicroDevice) -> Result<Vec<u8>> {
    read_keymap_with(|offset| {
        device.request(
            "fs.readbin",
            Some(json!({ "file": "keymap.json", "offset": offset, "len": READ_CHUNK })),
            DEFAULT_REQUEST_TIMEOUT,
        )
    })
}

pub fn write_keymap_chunks_with<F>(bytes: &[u8], mut write: F) -> Result<()>
where
    F: FnMut(usize, &[u8], bool) -> Result<()>,
{
    if bytes.is_empty() {
        bail!("keymap is empty");
    }
    for (offset, chunk) in bytes.chunks(WRITE_CHUNK).enumerate() {
        let offset = offset * WRITE_CHUNK;
        write(offset, chunk, offset + chunk.len() == bytes.len())?;
    }
    Ok(())
}

fn write_keymap(device: &MicroDevice, bytes: &[u8]) -> Result<()> {
    write_keymap_chunks_with(bytes, |offset, data, completed| {
        device
            .request(
                "fs.writebin",
                Some(json!({
                    "file": "keymap.json",
                    "offset": offset,
                    "data": STANDARD.encode(data),
                    "append": true,
                    "completed": completed,
                })),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map(|_| ())
    })
}

fn oai_profile(keymap: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    let profiles = keymap
        .get_mut("profiles")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("keymap profiles must be an array"))?;
    let matches: Vec<_> = profiles
        .iter()
        .enumerate()
        .filter(|(_, profile)| {
            profile
                .pointer("/layers/0/layout")
                .map(ToString::to_string)
                .is_some_and(|layout| layout.contains("KV_OAI_AG05"))
        })
        .map(|(index, _)| index)
        .collect();
    if matches.len() != 1 {
        bail!("expected one OAI profile, found {}", matches.len());
    }
    profiles[matches[0]]
        .as_object_mut()
        .ok_or_else(|| anyhow!("OAI profile must be an object"))
}

fn is_blank_layer(layer: &Value) -> bool {
    let Some(layout) = layer.get("layout") else {
        return true;
    };
    let mut codes = layout
        .get("keymap")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|row| {
            row.as_array()
                .map(|codes| codes.iter().collect::<Vec<_>>())
                .unwrap_or_else(|| vec![row])
        })
        .chain(
            layout
                .get("encoders")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .flat_map(|row| {
                    row.as_array()
                        .map(|codes| codes.iter().collect::<Vec<_>>())
                        .unwrap_or_else(|| vec![row])
                }),
        )
        .chain(
            layout
                .pointer("/joystick/sectors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|sector| sector.get("k")),
        );
    codes.all(|code| matches!(code.as_str(), Some("KC_NONE" | "KI_X")))
}

/// Copies the compatible Layer 1 layout into blank Layer 2 and binds both layers.
pub fn configure_micro(keymap: &mut Value) -> Result<()> {
    const REQUIRED: [&str; 16] = [
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
    {
        let profile = oai_profile(keymap)?;
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
        let source_text = source_layout.to_string();
        if !REQUIRED.iter().all(|key| source_text.contains(key)) {
            bail!("Layer 1 is not a compatible Codex Micro OAI layout");
        }
        if layers[HERDR_LAYER - 1].get("layout") != Some(&source_layout) {
            if !is_blank_layer(&layers[HERDR_LAYER - 1]) {
                bail!("Layer {HERDR_LAYER} is not blank; refusing to overwrite it");
            }
            layers[HERDR_LAYER - 1]
                .as_object_mut()
                .ok_or_else(|| anyhow!("Layer {HERDR_LAYER} must be an object"))?
                .insert("layout".into(), source_layout);
        }
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
    let profile = oai_profile(keymap)?;
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
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupReport {
    pub firmware: String,
    pub backup: Option<PathBuf>,
    pub controls: PathBuf,
    pub effort: PathBuf,
    pub lighting: PathBuf,
}

pub fn setup_micro() -> Result<SetupReport> {
    if let Some(owner) = active_owner()? {
        bail!("quit {owner} first");
    }
    if request_status(Duration::from_millis(250)).is_ok() {
        bail!("stop the Micro bridge first");
    }
    let (event_tx, _events) = mpsc::channel::<DeviceEvent>();
    let (mut device, _) = MicroDevice::open(event_tx)?;
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
        configure_micro(&mut keymap)?;
        let after = serde_json::to_vec(&keymap)?;
        let backup = if after == canonical_before {
            None
        } else {
            let backup = backup_keymap(&before)?;
            write_keymap(&device, &after)?;
            if read_keymap(&device)? != after {
                bail!("keymap read-back failed");
            }
            Some(backup)
        };
        let controls = config_path("controls.json");
        let effort = config_path("effort.json");
        let lighting = config_path("lighting.json");
        load_controls(&controls).map_err(|error| anyhow!(error))?;
        load_effort(&effort).map_err(|error| anyhow!(error))?;
        load_lighting(&lighting).map_err(|error| anyhow!(error))?;
        Ok(SetupReport {
            firmware,
            backup,
            controls,
            effort,
            lighting,
        })
    })();
    let close = device.close();
    match (result, close) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub fn configure_controls() -> Result<PathBuf> {
    let path = config_path("controls.json");
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

pub fn install_pi_effort(source: &Path, target: &Path, timestamp: u128) -> Result<PiInstall> {
    let bundled = fs::read(source).with_context(|| format!("read {}", source.display()))?;
    let current = match fs::read(target) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", target.display())),
    };
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
        Some(backup)
    } else {
        None
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(target)?;
    file.write_all(&bundled)?;
    fs::set_permissions(target, fs::Permissions::from_mode(0o644))?;
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
    }

    #[test]
    fn reads_and_writes_exact_chunks() {
        let bytes = b"abcdef".to_vec();
        let read = read_keymap_with(|offset| {
            let chunk = &bytes[offset..bytes.len().min(offset + 2)];
            Ok(json!({ "data": STANDARD.encode(chunk), "total_size": bytes.len() }))
        })
        .unwrap();
        assert_eq!(read, bytes);
        let mut chunks = Vec::new();
        write_keymap_chunks_with(&vec![1; 800], |offset, data, completed| {
            chunks.push((offset, data.len(), completed));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            chunks,
            [(0, 384, false), (384, 384, false), (768, 32, true)]
        );
    }

    #[test]
    fn configures_only_a_blank_layer_two() {
        let required = [
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
        let mut keymap = json!({
            "profiles": [{ "layers": [
                { "layout": { "keymap": [required] } },
                { "layout": { "keymap": [["KC_NONE"]], "encoders": [], "joystick": { "sectors": [] } } }
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
            keymap.pointer("/profiles/0/layers/0/layout"),
            keymap.pointer("/profiles/0/layers/1/layout")
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
}
