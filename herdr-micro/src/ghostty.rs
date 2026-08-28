use anyhow::{Result, anyhow, bail};
use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};
use objc2_foundation::NSString;
use serde::Serialize;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::{actions::GHOSTTY_PROCESS, herdr::Session};

#[link(name = "ScriptingBridge", kind = "framework")]
unsafe extern "C" {}

const APPLE_EVENT_TIMEOUT_TICKS: libc::c_long = 5 * 60;
const SCROLL_MOMENTUM_NONE: i32 = i32::from_be_bytes(*b"SMno");

thread_local! {
    static GHOSTTY: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GhosttyTerminal {
    pub id: String,
    pub name: String,
}

fn ghostty_application() -> Result<Retained<AnyObject>> {
    GHOSTTY.with_borrow_mut(|cached| {
        if let Some(app) = cached {
            return Ok(app.clone());
        }
        let bundle = NSString::from_str(GHOSTTY_PROCESS);
        // SAFETY: This is the documented ScriptingBridge application factory.
        let app: Option<Retained<AnyObject>> =
            unsafe { msg_send![class!(SBApplication), applicationWithBundleIdentifier: &*bundle] };
        let app = app.ok_or_else(|| anyhow!("Ghostty scripting bridge unavailable"))?;
        // SAFETY: SBApplication.timeout is a signed long measured in 1/60-second ticks.
        unsafe {
            let _: () = msg_send![&*app, setTimeout: APPLE_EVENT_TIMEOUT_TICKS];
        }
        *cached = Some(app.clone());
        Ok(app)
    })
}

fn require_selector(receiver: &AnyObject, selector: objc2::runtime::Sel) -> Result<()> {
    // SAFETY: respondsToSelector: is an NSObject protocol method and does not invoke `selector`.
    let responds: bool = unsafe { msg_send![receiver, respondsToSelector: selector] };
    if !responds {
        bail!(
            "Ghostty does not support selector {}",
            selector.name().to_string_lossy()
        )
    }
    Ok(())
}

fn object(
    receiver: &AnyObject,
    selector: objc2::runtime::Sel,
) -> Result<Option<Retained<AnyObject>>> {
    require_selector(receiver, selector)?;
    // SAFETY: Callers use object-valued selectors from Ghostty's current SDEF.
    Ok(unsafe { objc2::msg_send![receiver, performSelector: selector] })
}

fn array_items(array: &AnyObject) -> Result<Vec<Retained<AnyObject>>> {
    // SAFETY: ScriptingBridge element arrays and KVC results implement NSArray's API.
    let count: usize = unsafe { msg_send![array, count] };
    (0..count)
        .map(|index| {
            let item: Option<Retained<AnyObject>> =
                unsafe { msg_send![array, objectAtIndex: index] };
            item.ok_or_else(|| anyhow!("Ghostty returned an invalid object list"))
        })
        .collect()
}

fn string(object: &AnyObject) -> Result<String> {
    object
        .downcast_ref::<NSString>()
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("Ghostty returned invalid text"))
}

fn string_property(
    receiver: &AnyObject,
    selector: objc2::runtime::Sel,
    label: &str,
) -> Result<String> {
    let value = object(receiver, selector)?
        .ok_or_else(|| anyhow!("Ghostty returned no terminal {label}"))?;
    string(&value)
}

fn terminal_topology(terminals: &AnyObject) -> Result<Vec<GhosttyTerminal>> {
    array_items(terminals)?
        .into_iter()
        .map(|item| {
            let id = string_property(&item, objc2::sel!(id), "ID")?;
            if id.is_empty() {
                bail!("Ghostty returned invalid terminal state")
            }
            Ok(GhosttyTerminal {
                id,
                name: string_property(&item, objc2::sel!(name), "name")?,
            })
        })
        .collect()
}

fn focused_id(app: &AnyObject) -> Result<Option<String>> {
    let Some(window) = object(app, objc2::sel!(frontWindow))? else {
        return Ok(None);
    };
    let tab = object(&window, objc2::sel!(selectedTab))?
        .ok_or_else(|| anyhow!("Ghostty returned no selected tab"))?;
    let terminal = object(&tab, objc2::sel!(focusedTerminal))?
        .ok_or_else(|| anyhow!("Ghostty returned no focused terminal"))?;
    let id = string_property(&terminal, objc2::sel!(id), "ID")?;
    if id.is_empty() {
        bail!("Ghostty returned an empty focused terminal ID")
    }
    Ok(Some(id))
}

pub fn inspect_ghostty() -> Result<Vec<GhosttyTerminal>> {
    objc2::rc::autoreleasepool(|_| {
        let app = ghostty_application()?;
        let terminals = object(&app, objc2::sel!(terminals))?
            .ok_or_else(|| anyhow!("Ghostty returned no terminals"))?;
        terminal_topology(&terminals)
    })
}

pub fn focused_terminal_id() -> Result<String> {
    objc2::rc::autoreleasepool(|_| {
        let app = ghostty_application()?;
        focused_id(&app)?.ok_or_else(|| anyhow!("Ghostty returned no focused terminal ID"))
    })
}

fn scripting_result(app: &AnyObject, action: &str) -> Result<()> {
    if let Some(error) = object(app, objc2::sel!(lastError))? {
        let description = object(&error, objc2::sel!(description))?
            .ok_or_else(|| anyhow!("Ghostty rejected the {action}"))?;
        bail!("Ghostty rejected the {action}: {}", string(&description)?)
    }
    Ok(())
}

pub fn scroll_terminal(terminal_id: &str, x: f64, y: f64, notches: i32) -> Result<()> {
    objc2::rc::autoreleasepool(|_| {
        if terminal_id.is_empty() {
            bail!("cannot scroll an unidentified Ghostty terminal")
        }
        let app = ghostty_application()?;
        let terminals = object(&app, objc2::sel!(terminals))?
            .ok_or_else(|| anyhow!("Ghostty returned no terminals"))?;
        let terminal_id = NSString::from_str(terminal_id);
        // SAFETY: SBElementArray.objectWithID: returns the exact terminal specifier for this UUID.
        let terminal: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*terminals, objectWithID: &*terminal_id] };
        let terminal = terminal.ok_or_else(|| anyhow!("Ghostty terminal is unavailable"))?;

        require_selector(&app, objc2::sel!(sendMousePositionX:y:modifiers:to:))?;
        // SAFETY: Ghostty's generated position binding accepts double, double,
        // optional NSString, and GhosttyTerminal parameters.
        unsafe {
            let _: () = msg_send![
                &*app,
                sendMousePositionX: x,
                y: y,
                modifiers: None::<&AnyObject>,
                to: &*terminal
            ];
        }
        scripting_result(&app, "mouse position")?;

        require_selector(&app, objc2::sel!(sendMouseScrollX:y:precision:momentum:to:))?;
        // SAFETY: Ghostty 1.3's generated binding is
        // `sendMouseScrollX:y:precision:momentum:to:` with double, double, BOOL,
        // GhosttyScrollMomentum (signed int), and GhosttyTerminal parameters.
        unsafe {
            let _: () = msg_send![
                &*app,
                sendMouseScrollX: 0.0_f64,
                y: f64::from(notches),
                precision: false,
                momentum: SCROLL_MOMENTUM_NONE,
                to: &*terminal
            ];
        }
        scripting_result(&app, "scroll event")
    })
}

fn set_session_title(session: &Session, title: Option<&str>) -> Result<()> {
    let client = session.client();
    let value = match title {
        Some(title) => client.call_value(
            "client.window_title.set",
            &serde_json::json!({ "title": title }),
        )?,
        None => client.call_value("client.window_title.clear", &serde_json::json!({}))?,
    };
    if value.get("changed").and_then(serde_json::Value::as_bool) != Some(true) {
        bail!(
            "Herdr session {} did not change a terminal title",
            session.name
        );
    }
    Ok(())
}

fn find_token<I>(
    inspect: &mut I,
    token: &str,
    stopping: &AtomicBool,
) -> Result<Option<GhosttyTerminal>>
where
    I: FnMut() -> Result<Vec<GhosttyTerminal>>,
{
    for attempt in 0..10 {
        check_stopping(stopping)?;
        let matches: Vec<_> = inspect()?
            .into_iter()
            .filter(|terminal| terminal.name == token)
            .collect();
        match matches.len() {
            0 => {
                if attempt != 9 {
                    thread::sleep(Duration::from_millis(50));
                }
            }
            1 => return Ok(matches.into_iter().next()),
            _ => bail!("Ghostty exposed duplicate probe token {token}"),
        }
    }
    Ok(None)
}

fn wait_for_title<I>(
    inspect: &mut I,
    terminal_id: &str,
    title: &str,
    stopping: &AtomicBool,
) -> Result<bool>
where
    I: FnMut() -> Result<Vec<GhosttyTerminal>>,
{
    for attempt in 0..10 {
        check_stopping(stopping)?;
        if inspect()?
            .iter()
            .any(|terminal| terminal.id == terminal_id && terminal.name == title)
        {
            return Ok(true);
        }
        if attempt != 9 {
            thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTerminalMapping {
    pub session_name: String,
    pub terminal_id: String,
}

fn check_stopping(stopping: &AtomicBool) -> Result<()> {
    if stopping.load(Ordering::Acquire) {
        bail!("Micro bridge is stopping")
    }
    Ok(())
}

pub fn probe_session_terminals_with<I, S, T>(
    sessions: &[String],
    mut inspect: I,
    mut set_title: S,
    mut create_token: T,
    stopping: &AtomicBool,
) -> Result<Vec<SessionTerminalMapping>>
where
    I: FnMut() -> Result<Vec<GhosttyTerminal>>,
    S: FnMut(&str, Option<&str>) -> Result<()>,
    T: FnMut(&str) -> String,
{
    let mut mappings = Vec::new();
    for session_name in sessions {
        check_stopping(stopping)?;
        let before = inspect()?;
        // A stop during the inspection must not start a title mutation.
        check_stopping(stopping)?;
        let originals: std::collections::BTreeMap<_, _> = before
            .iter()
            .map(|terminal| (terminal.id.clone(), terminal.name.clone()))
            .collect();
        let token = create_token(session_name);
        if let Err(error) = set_title(session_name, Some(&token)) {
            let _ = set_title(session_name, None);
            return Err(error);
        }

        let mut restored = false;
        let probed = (|| {
            let terminal = find_token(&mut inspect, &token, stopping)?
                .ok_or_else(|| anyhow!("Herdr session {session_name} did not appear in Ghostty"))?;
            let original = originals
                .get(&terminal.id)
                .ok_or_else(|| anyhow!("Ghostty topology changed while probing {session_name}"))?;
            let duplicate = mappings
                .iter()
                .any(|mapping: &SessionTerminalMapping| mapping.terminal_id == terminal.id);
            set_title(session_name, Some(original))?;
            restored = true;
            if !wait_for_title(&mut inspect, &terminal.id, original, stopping)? {
                bail!("failed to restore {session_name} terminal title");
            }
            Ok((terminal, duplicate))
        })();
        let (terminal, duplicate) = match probed {
            Ok(result) => result,
            Err(error) => {
                if !restored {
                    let _ = set_title(session_name, None);
                }
                return Err(error);
            }
        };
        if duplicate {
            bail!(
                "multiple Herdr sessions targeted Ghostty terminal {}",
                terminal.id
            );
        }
        mappings.push(SessionTerminalMapping {
            session_name: session_name.clone(),
            terminal_id: terminal.id,
        });
    }
    Ok(mappings)
}

pub fn probe_session_terminals(
    sessions: &[Session],
    stopping: &AtomicBool,
) -> Result<Vec<SessionTerminalMapping>> {
    let names: Vec<_> = sessions
        .iter()
        .map(|session| session.name.clone())
        .collect();
    probe_session_terminals_with(
        &names,
        inspect_ghostty,
        |name, title| {
            let session = sessions
                .iter()
                .find(|session| session.name == name)
                .ok_or_else(|| anyhow!("unknown Herdr session {name}"))?;
            set_session_title(session, title)
        },
        |session| format!("__herdr_micro_{session}_{}__", unique_token()),
        stopping,
    )
}

fn unique_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn state(terminals: Vec<(&str, &str)>) -> Vec<GhosttyTerminal> {
        terminals
            .into_iter()
            .map(|(id, name)| GhosttyTerminal {
                id: id.into(),
                name: name.into(),
            })
            .collect()
    }

    #[test]
    fn cancellation_during_inspection_mutates_no_title() {
        let stopping = AtomicBool::new(false);
        let calls = RefCell::new(Vec::new());
        let error = probe_session_terminals_with(
            &["default".into()],
            || {
                stopping.store(true, Ordering::Release);
                Ok(state(vec![("terminal", "original")]))
            },
            |session, title| {
                calls
                    .borrow_mut()
                    .push((session.to_owned(), title.map(str::to_owned)));
                Ok(())
            },
            |_| "probe".into(),
            &stopping,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "Micro bridge is stopping");
        assert!(calls.into_inner().is_empty());
    }

    #[test]
    fn rejects_missing_scripting_selector() {
        let object: Retained<AnyObject> = unsafe { msg_send![class!(NSObject), new] };
        let error = require_selector(&object, objc2::sel!(missingGhosttySelector)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Ghostty does not support selector missingGhosttySelector"
        );
    }

    #[test]
    fn probes_named_sessions_and_restores_titles() {
        let terminals = RefCell::new(vec![
            ("personal-terminal".to_owned(), "custom personal".to_owned()),
            ("work-terminal".to_owned(), "custom work".to_owned()),
        ]);
        let sessions = vec!["default".into(), "werk".into()];
        let mappings = probe_session_terminals_with(
            &sessions,
            || {
                Ok(state(
                    terminals
                        .borrow()
                        .iter()
                        .map(|(id, name)| (id.as_str(), name.as_str()))
                        .collect(),
                ))
            },
            |session, title| {
                let id = if session == "default" {
                    "personal-terminal"
                } else {
                    "work-terminal"
                };
                terminals
                    .borrow_mut()
                    .iter_mut()
                    .find(|(candidate, _)| candidate == id)
                    .unwrap()
                    .1 = title.unwrap_or("Ghostty default").into();
                Ok(())
            },
            |session| format!("probe-{session}"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            mappings,
            vec![
                SessionTerminalMapping {
                    session_name: "default".into(),
                    terminal_id: "personal-terminal".into()
                },
                SessionTerminalMapping {
                    session_name: "werk".into(),
                    terminal_id: "work-terminal".into()
                }
            ]
        );
        assert_eq!(
            terminals.into_inner(),
            [
                ("personal-terminal".into(), "custom personal".into()),
                ("work-terminal".into(), "custom work".into())
            ]
        );
    }

    #[test]
    fn clears_probe_title_after_inspection_failure() {
        let calls = RefCell::new(Vec::new());
        let mut inspections = 0;
        let error = probe_session_terminals_with(
            &["default".into()],
            || {
                inspections += 1;
                if inspections == 1 {
                    Ok(state(vec![("terminal", "original")]))
                } else {
                    Err(anyhow!("inspection failed"))
                }
            },
            |session, title| {
                calls
                    .borrow_mut()
                    .push((session.to_owned(), title.map(str::to_owned)));
                title.map(|_| ()).ok_or_else(|| anyhow!("cleanup failed"))
            },
            |_| "probe".into(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "inspection failed");
        assert_eq!(
            calls.into_inner(),
            [
                ("default".into(), Some("probe".into())),
                ("default".into(), None)
            ]
        );
    }

    #[test]
    fn waits_for_asynchronous_title_restoration() {
        let mut inspections = 0;
        assert!(
            wait_for_title(
                &mut || {
                    inspections += 1;
                    Ok(state(vec![(
                        "terminal",
                        if inspections == 1 {
                            "probe"
                        } else {
                            "original"
                        },
                    )]))
                },
                "terminal",
                "original",
                &AtomicBool::new(false),
            )
            .unwrap()
        );
        assert_eq!(inspections, 2);
    }

    #[test]
    fn does_not_clear_an_exact_restore_that_times_out() {
        let calls = RefCell::new(Vec::new());
        let mut inspections = 0;
        let error = probe_session_terminals_with(
            &["default".into()],
            || {
                inspections += 1;
                Ok(state(vec![(
                    "terminal",
                    if inspections == 1 {
                        "original"
                    } else {
                        "probe"
                    },
                )]))
            },
            |session, title| {
                calls
                    .borrow_mut()
                    .push((session.to_owned(), title.map(str::to_owned)));
                Ok(())
            },
            |_| "probe".into(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "failed to restore default terminal title"
        );
        assert_eq!(
            calls.into_inner(),
            [
                ("default".into(), Some("probe".into())),
                ("default".into(), Some("original".into()))
            ]
        );
    }
}
