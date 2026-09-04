use std::{
    env, io::Write, net::Shutdown, os::unix::net::UnixStream, path::PathBuf, time::Duration,
};

use anyhow::{Context, Result, bail};
use herdr_client::unix::peer_is_current_user;
use herdr_hub_client::{ClientMessage, PROTOCOL};
use serde_json::Value;

const WRITE_TIMEOUT: Duration = Duration::from_millis(75);

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Startup,
    Event,
}

pub(crate) fn command(argument: &str) -> Result<()> {
    let kind = match argument {
        "startup" => Kind::Startup,
        "event" => Kind::Event,
        _ => bail!("notify expects startup or event"),
    };
    if let Err(error) = send(kind) {
        eprintln!("herdr-hub notify: {error:#}");
    }
    Ok(())
}

pub(crate) fn startup() {
    if let Err(error) = send(Kind::Startup) {
        eprintln!("herdr-hub notify: {error:#}");
    }
}

fn send(kind: Kind) -> Result<()> {
    let socket_path = env::var_os("HERDR_SOCKET_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_default();
    let event = match kind {
        Kind::Startup => None,
        Kind::Event => Some(read_event()),
    };
    let mut stream =
        UnixStream::connect(herdr_hub_client::socket_path()).context("hub is not running")?;
    peer_is_current_user(&stream).context("hub belongs to another user")?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let message = ClientMessage::Notify {
        protocol: PROTOCOL,
        socket_path,
        event,
    };
    serde_json::to_writer(&mut stream, &message).context("encode notification")?;
    stream.write_all(b"\n").context("send notification")?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

fn read_event() -> Value {
    let Some(raw) = env::var_os("HERDR_PLUGIN_EVENT_JSON").filter(|value| !value.is_empty()) else {
        return Value::Null;
    };
    match serde_json::from_str(&raw.to_string_lossy()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("herdr-hub notify: invalid HERDR_PLUGIN_EVENT_JSON: {error}");
            Value::Null
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_kind_is_strict() {
        assert!(command("other").is_err());
    }
}
