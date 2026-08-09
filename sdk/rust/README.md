# herdr-client for Rust

Small synchronous client for Herdr's public plugin socket API. It provides:

- typed session, workspace, tab, pane, agent, layout, and plugin-context models
- generic typed calls for every socket method
- typed helpers for common read operations
- long-lived event subscriptions
- Unix socket and Windows named-pipe transport

```rust
use herdr_client::Client;

fn main() -> Result<(), herdr_client::Error> {
    let client = Client::from_env()?;
    let snapshot = client.snapshot()?;
    println!("{} panes", snapshot.panes.len());
    Ok(())
}
```

Methods added after this crate's tested Herdr version remain usable:

```rust
let value = client.call_value("some.future.method", &serde_json::json!({}))?;
```

Plugin entrypoints can require Herdr's managed directories instead of deriving
paths from `HOME` or XDG variables:

```rust
use herdr_client::Environment;

let plugin = Environment::load()?.require_plugin()?;
let config = plugin.config_file("config.toml");
let state = plugin.data_dir().join("state.json");
let lock = plugin.run_dir().join("plugin.lock");
let log = plugin.logs_dir().join("plugin.log");
```

`data`, `cache`, `run`, and `logs` are repository conventions beneath
`HERDR_PLUGIN_STATE_DIR`; create only the directories an entrypoint uses.
`open_rotating_log` creates private append-only logs with bounded retention.

Validated against Herdr 0.8.0, socket protocol 19.
