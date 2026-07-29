# Herdr evidence note for a Codex Micro layer

Accessed: **2026-07-25**  
Research scope: official Herdr website and documentation, official GitHub repository and releases, the release binary's local help, API schema/source, integration assets, and configuration source. Secondary reviews and community claims are excluded except where the official site itself identifies the plugin marketplace.

## Version and evidence anchor

- The latest stable release is **v0.7.5**, published 2026-07-21. Its tagged commit is `ef4c23f5775bb8cfec05f05d0844226ff959a07a`; the official release notes call out the live-agent CLI facade (`start`, atomic `prompt`, logical `send-keys`, server-owned `wait`) as a v0.7.5 addition. [Official v0.7.5 release](https://github.com/ogulcancelik/herdr/releases/tag/v0.7.5)
- I downloaded the official `herdr-macos-aarch64` v0.7.5 asset to a temporary path without installing it. `herdr --version` returned `herdr 0.7.5`; the SHA-256 observed on 2026-07-25 was `37350546b0012555943b92eaf962665de4e264395baeb44227b8015e8ff5b0d6`. The release page is the source for the asset. [Official v0.7.5 release assets](https://github.com/ogulcancelik/herdr/releases/tag/v0.7.5)
- The same stable binary's `herdr api schema` reports client/server protocol **17** and generated schema version **1**. The exact bundled stable schema is committed with the release. [v0.7.5 JSON Schema](https://github.com/ogulcancelik/herdr/blob/ef4c23f5775bb8cfec05f05d0844226ff959a07a/docs/next/api/herdr-api.schema.json)
- The official `master` branch inspected on 2026-07-25 was commit `d4e0dd3d903c50d2edb8c3cec71952a83989b310` (`perf: eliminate repeated workspace git discovery (#1842)`, committed 2026-07-25). The crate still declares version `0.7.5`. [Repository at inspected commit](https://github.com/ogulcancelik/herdr/tree/d4e0dd3d903c50d2edb8c3cec71952a83989b310), [Cargo.toml](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/Cargo.toml)
- **License changed after the release without a version bump.** The v0.7.5 tag is AGPL-3.0-or-later or a commercial license; the inspected, unreleased `master` branch is Apache-2.0. Apply the tag's license to the downloaded stable binary, not `master`'s license metadata. [v0.7.5 Cargo.toml](https://github.com/ogulcancelik/herdr/blob/v0.7.5/Cargo.toml), [v0.7.5 dual-license text](https://github.com/ogulcancelik/herdr/blob/v0.7.5/LICENSE), [`master` Cargo.toml at inspected commit](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/Cargo.toml), [`master` Apache-2.0 text](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/LICENSE)
- The live documentation says “Last updated: Jul 24, 2026” and may contain post-release `master` behavior. Claims below link to the live docs and, for absence/security checks, to the exact inspected source commit. Treat new API methods as requiring a server version/protocol check. [Socket protocol stability](https://herdr.dev/docs/socket-api/#protocol-stability)
- No `herdr` executable was installed on the researched Mac before this work; only the temporary official v0.7.5 binary was executed for read-only `--version` and `--help` checks.

## Bottom line for the keyboard layer

**Verified**

1. Herdr is a strong routing and session layer for a keyboard: it has configurable direct or prefix keybindings, recognizes the focused pane and its agent, and exposes logical key injection (`agent send-keys`) plus atomic text submission (`agent prompt`). [Keyboard configuration](https://herdr.dev/docs/configuration/#keybindings), [Agent CLI](https://herdr.dev/docs/cli-reference/#agents)
2. A Codex Micro key can emit a reserved chord that Herdr handles directly. Built-in bindings cover workspace/tab/pane operations and agent navigation; `[[keys.command]]` can run a shell command, popup, temporary pane, or plugin action. Custom commands receive the active workspace/tab/pane IDs and focused-pane cwd. [Custom command keybindings](https://herdr.dev/docs/configuration/#custom-command-keybindings)
3. Herdr has **no documented semantic method such as `agent.set_model` or `agent.set_effort`**. The documented agent methods are list/get/read/explain/send-keys/prompt/wait/rename/focus/start/view; the exact source `Method` enum likewise has no model or reasoning-control method. [Raw method list](https://herdr.dev/docs/socket-api/#raw-methods), [Method enum at inspected commit](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/api/schema.rs)
4. Therefore cross-agent model/thinking controls must be implemented by an adapter that branches on the recognized agent and invokes that agent's own supported control: preferably a structured agent API/extension, otherwise `herdr agent send-keys` against that agent's TUI. Herdr supplies targeting and transport, not a universal interpretation of “effort.”
5. Model/effort can be reflected in the Herdr UI using display-only metadata tokens (for example `model` or an application-defined `effort` token), but metadata does not change the agent or semantic lifecycle state. [Pane metadata contract](https://herdr.dev/docs/socket-api/#agent-state-reporting)

**Recommended Herdr-side integration order**

1. Bind layer-2 hardware keys to rare direct chords (for example unused `ctrl+alt` chords or function keys), then map ordinary layout/navigation actions in `[keys]`. Herdr recommends `ctrl+alt` as the least-conflicted direct-chord family, with listed exceptions for desktop/terminal reservations. [Prefix-free guidance](https://herdr.dev/docs/keyboard/#going-prefix-free)
2. Bind agent-specific semantic actions such as `effort.cycle`, `model.select`, `interrupt`, and `approve` to `[[keys.command]]` entries that call one small dispatcher. The command environment provides `HERDR_BIN_PATH` and `HERDR_ACTIVE_PANE_ID`, so the dispatcher can resolve the active agent and call Herdr back without guessing the pane. [Custom command environment](https://herdr.dev/docs/configuration/#custom-command-keybindings)
3. Move the dispatcher into a local Herdr plugin only when it needs a popup selector, persistent plugin state, declared events, an Agent-view projection, or shareable packaging. A plugin action can be bound to a key and can call the full Herdr CLI or raw socket. [Plugin model and keybindings](https://herdr.dev/docs/plugins/)
4. Use a long-lived raw-socket client only if the keyboard companion needs event subscriptions or lower latency. For ordinary hotkeys, the CLI wrappers are the documented starting point. [Choosing an integration layer](https://herdr.dev/docs/socket-api/#choose-an-integration-layer)

## Product and session model

**Verified**

- Herdr is a terminal workspace manager/agent multiplexer, not a terminal emulator or a rebuilt chat UI. Processes run in real PTYs inside a background server; clients attach and render the session. [Concepts](https://herdr.dev/docs/concepts/), [Official home page](https://herdr.dev/)
- The hierarchy is **session → workspace → tab → pane**. A recognized agent is the current process occupant of a pane, not a separate durable chat object. Workspaces are project containers, tabs are layouts, and panes are real terminals. [Concepts](https://herdr.dev/docs/concepts/)
- Agent states are `blocked`, `working`, `done`, `idle`, and `unknown`. State rolls up from pane to tab/workspace and drives waits and notifications. `done` is unseen finished/idle work; focusing it marks it seen and changes the visible state to `idle`. [Concepts: Agent](https://herdr.dev/docs/concepts/#agent), [Agent automation state semantics](https://herdr.dev/docs/agent-automation/#choose-the-control-surface)
- A named session is an independent background server namespace with its own panes, workspaces, tabs, sockets, and runtime state; named sessions share the global config file. [Named sessions](https://herdr.dev/docs/persistence-remote/#named-sessions)
- The TUI is mouse-native as well as keyboard-controllable: click panes/tabs/workspaces/agents, drag splits, right-click for actions, select/copy text, and use responsive narrow-screen navigation over SSH. [Quick start](https://herdr.dev/docs/quick-start/), [Mobile/remote workflow](https://herdr.dev/docs/how-to-work/)
- Git worktree operations are first-class through the UI, CLI, and socket: list/create/open/remove, with workspace provenance and grouping. Removal runs `git worktree remove` and does not delete the branch. [Socket API worktree methods](https://herdr.dev/docs/socket-api/#raw-methods), [Configuration: worktrees](https://herdr.dev/docs/configuration/#worktrees)
- Notifications can be off, in-app, outer-terminal, or OS-system delivery; agent state can also produce per-agent sound notifications. [Config reference: notifications and sound](https://herdr.dev/docs/config-reference/#notifications)

## Keyboard and user commands

**Verified defaults**

| Action | Default |
|---|---|
| Prefix / active-key help | `ctrl+b` / `prefix+?` |
| New tab | `prefix+c` |
| Split right / down | `prefix+v` / `prefix+minus` |
| Focus pane | `prefix+h/j/k/l` |
| Swap pane | `prefix+shift+h/j/k/l` |
| Zoom / close pane / resize mode | `prefix+z` / `prefix+x` / `prefix+r` |
| Next / previous tab | `prefix+n` / `prefix+p` |
| Jump to tab 1–9 | `prefix+1..9` |
| Workspace navigation / global goto | `prefix+w` / `prefix+g` |
| Toggle sidebar / detach | `prefix+b` / `prefix+q` |
| Copy mode | `prefix+[` |

Source: [Keyboard guide](https://herdr.dev/docs/keyboard/), [complete searchable config reference](https://herdr.dev/docs/config-reference/#keybindings).

Particularly useful optional actions for a hardware layer are unset by default:

- `previous_agent` and `next_agent`;
- `focus_agent` with an indexed `1..9` binding;
- `previous_workspace`, `next_workspace`, and indexed workspace switching;
- `last_pane`;
- `open_worktree` and `remove_worktree`.

Source: [Config reference: keybindings](https://herdr.dev/docs/config-reference/#keybindings).

Bindings may be a string or an array, so the normal Herdr binding can coexist with a Codex Micro chord. Key syntax supports ordinary keys, modifier chords, special keys, and named punctuation. A direct printable key is unsafe because Herdr would intercept normal typing. [Configuration: keybindings](https://herdr.dev/docs/configuration/#keybindings)

Custom command binding types:

- `popup`: session-modal terminal popup, optional cell or percentage size;
- `pane`: temporary zoomed pane;
- `shell`: detached background command;
- `plugin_action`: invoke a declared installed-plugin action.

Commands receive `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH`, `HERDR_ACTIVE_WORKSPACE_ID`, `HERDR_ACTIVE_TAB_ID`, `HERDR_ACTIVE_PANE_ID`, and `HERDR_ACTIVE_PANE_CWD` when available. [Custom command keybindings](https://herdr.dev/docs/configuration/#custom-command-keybindings)

## Agent support and integrations

**Verified**

Any terminal agent runs as a normal process. Rich state depends on process/screen detection, an official integration, or a custom state reporter. [Supported agents](https://herdr.dev/docs/agents/#supported-agents)

| Agent | Detection/state authority | Official integration contribution |
|---|---|---|
| Pi | Lifecycle extension when installed; screen manifest otherwise | Semantic state and native session identity |
| Claude Code | Screen manifest | Native session identity only |
| Codex | Screen manifest | Native session identity only |
| OMP | Lifecycle extension | State (and current docs also describe session resume support) |
| Kimi, OpenCode, Kilo, Hermes, MastraCode | Lifecycle hook/plugin when installed, with documented fallback differences | State and session where documented |
| Copilot CLI, Devin, Droid, Qoder, Cursor Agent CLI | Screen manifest | Session identity |
| Amp, Grok, Antigravity, Kiro, Maki | Screen manifest | No official integration role listed |
| Gemini CLI, Cline | Detected but “less thoroughly tested” | No richer role promised |

Sources: [Agents table and authority model](https://herdr.dev/docs/agents/#supported-agents), [Integration roles](https://herdr.dev/docs/integrations/#how-herdr-uses-integrations).

### Pi, Claude Code, and Codex exact behavior

- `herdr integration install pi` writes the bundled extension to `~/.pi/agent/extensions/herdr-agent-state.ts`, or `$PI_CODING_AGENT_DIR/extensions/herdr-agent-state.ts`. The bundled source reports lifecycle and session data over Herdr's local socket. [Pi integration docs](https://herdr.dev/docs/integrations/#pi), [bundled Pi extension at inspected commit](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/integration/assets/pi/herdr-agent-state.ts)
- `herdr integration install claude` writes `hooks/herdr-agent-state.sh` and updates Claude `settings.json` under `~/.claude` or `CLAUDE_CONFIG_DIR`. It reports session identity on session start; Claude lifecycle state remains screen-manifest-derived because the hooks are not complete lifecycle authority. [Claude integration docs](https://herdr.dev/docs/integrations/#claude-code)
- `herdr integration install codex` writes `herdr-agent-state.sh`, updates `hooks.json`, and ensures `[features] hooks = true` in Codex `config.toml` under `~/.codex` or `CODEX_HOME`. It reports session identity; Codex lifecycle state remains screen-manifest-derived. [Codex integration docs](https://herdr.dev/docs/integrations/#codex)
- Herdr deliberately keeps Claude Code and Codex state on screen manifests because their hooks can miss permission results, escape interrupts, and other transitions. [Status authority](https://herdr.dev/docs/agents/#status-authority)
- Native restore minimum integration versions are Pi `2`, Claude `6`, and Codex `5`; resume commands are respectively `pi --session <path-or-id>`, `claude --resume <id>`, and `codex resume <id>`. [Native agent session restore](https://herdr.dev/docs/session-state/#native-agent-session-restore)

Detection manifests are bundled, may be updated from herdr.dev, and may be overridden locally at `~/.config/herdr/agent-detection/<agent>.toml`. `herdr agent explain` reports the active source/version, matched rule, evidence, fallback, and remote-update diagnostics. [Detection manifests](https://herdr.dev/docs/agents/#detection-manifests)

### Herdr maintainer's Pi-specific companion extensions

These are **experimental Pi extensions in the Herdr maintainer's personal collection**, not Herdr core APIs and not Herdr marketplace plugins:

- `@ogulcancelik/pi-herdr` v0.4.0 gives Pi structured `herdr_layout`, `herdr_pane`, and `herdr_agent` tools over Herdr's existing interfaces. Its README requires Pi 0.80+, Herdr 0.7.5+, and Pi running inside a Herdr-managed pane. [pi-herdr source and README](https://github.com/ogulcancelik/pi-extensions/tree/f9cdcd9d902dbc7cf79261be8aaf43c6617b9ffe/packages/pi-herdr)
- `@ogulcancelik/pi-model-thinking` v0.1.0 remembers and reapplies thinking levels per Pi model/provider. It is a strong Pi adapter building block, but it does not create a Herdr-wide effort API. [pi-model-thinking source and README](https://github.com/ogulcancelik/pi-extensions/tree/f9cdcd9d902dbc7cf79261be8aaf43c6617b9ffe/packages/pi-model-thinking)

The collection's own README labels both packages experimental. [Maintainer's Pi extension collection](https://github.com/ogulcancelik/pi-extensions)

## External control surfaces

### 1. CLI wrappers

**Verified.** The CLI is the simplest stable scripting surface. Relevant commands include:

```text
herdr agent list
herdr agent get <target>
herdr agent read <target> ...
herdr agent send-keys <target> <key> [key ...]
herdr agent prompt <target> <text> [--wait] [--until STATUS]... [--timeout MS]
herdr agent focus <target>
herdr agent wait <target> ...
herdr agent start <name> --kind KIND --pane ID [-- <agent-args...>]
```

Agent targets are a unique live agent name or the pane ID currently hosting the agent. `agent send-keys` validates logical keys before writing and supports keys such as `enter`, `up`, `esc`, and `ctrl+c`. `agent prompt` atomically submits text and Enter while respecting bracketed-paste mode. [CLI agent reference](https://herdr.dev/docs/cli-reference/#agents)

Use raw pane commands for non-agent terminals or deliberately unguarded input:

```text
herdr pane send-text <pane_id> <text>
herdr pane send-keys <pane_id> <key> [key ...]
herdr pane run <pane_id> <command>
```

`pane run` is the safe atomic “text plus Enter” command; separate `send-text` and `send-keys` remain low level. [CLI pane reference](https://herdr.dev/docs/cli-reference/#panes)

### 2. Local JSON socket

**Verified.** Herdr uses newline-delimited JSON over a Unix-domain socket on Unix and a named pipe on Windows. CLI wrappers, the agent skill, and raw socket share the same control surface. The installed CLI can emit its bundled full JSON Schema via `herdr api schema --json` or write it with `--output`. [Socket API](https://herdr.dev/docs/socket-api/)

The API controls workspaces, worktrees, tabs, pane topology/input/output, recognized agents, semantic state reporting, metadata, notifications, integration install/uninstall, plugins, config reload, server stop, waits, and event subscriptions. The canonical raw method list is in the docs and the exact method enum is in source. [Raw methods](https://herdr.dev/docs/socket-api/#raw-methods), [schema source](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/api/schema.rs)

Long-lived event subscriptions include agent-status, output, focus, layout, and resource lifecycle events; clients can bootstrap with `session.snapshot` and then maintain a cache from events. [Event subscriptions](https://herdr.dev/docs/socket-api/#event-subscriptions), [raw method notes](https://herdr.dev/docs/socket-api/#raw-methods)

### 3. Plugins

**Verified.** Plugin v1 is an executable-package model, not a language SDK. A plugin is a directory containing `herdr-plugin.toml` and commands in any locally runnable language. It may declare build commands, one-shot startup hooks, actions, event hooks, terminal panes, keybindings through config, and URL link handlers. Plugin processes call the full Herdr CLI or raw socket. [Plugins](https://herdr.dev/docs/plugins/)

Important security boundary: plugin code runs unsandboxed as the user, with the user's environment and full Herdr CLI access. Herdr validates the manifest but does not review or sandbox the code. [Plugin trust and security](https://herdr.dev/docs/plugins/#trust-and-security)

Plugin panes can be overlay, popup, split, tab, or zoomed. Plugin state/config directories are provided, but there is no Herdr-managed storage API in plugin v1; the plugin owns its files/database. Runtime action registration and native non-terminal plugin UI are not part of v1. [Plugin panes and storage](https://herdr.dev/docs/plugins/#panes)

### 4. Direct terminal streams

**Verified.** Third-party bridges can observe a single terminal as newline-delimited base64 ANSI frames or control it interactively:

```text
herdr terminal session observe w1:p1 --cols 120 --rows 40
herdr terminal session control w1:p1 --takeover --cols 120 --rows 40
```

Control mode accepts JSON `terminal.input`, `terminal.resize`, `terminal.scroll`, and `terminal.release` commands on stdin. Only one controller owns input/resize; multiple read-only observers are allowed. [Direct terminal attach and streams](https://herdr.dev/docs/persistence-remote/#direct-terminal-attach)

### 5. Agent skill

**Verified.** Herdr publishes an agent guide/skill so an agent inside a pane can learn to use the same CLI and socket control surface. This is documentation/instruction, not a distinct privileged API. [Documentation overview](https://herdr.dev/docs/), [Agent skill file](https://herdr.dev/agent-guide.md)

### Surfaces not found

**Verified as “not documented or present in the inspected official command/schema/source surface,” not as a timeless impossibility.**

- No built-in MCP server/client surface for controlling Herdr was found.
- No HTTP/REST API or webhook receiver was found; the control API is local socket/pipe JSON.
- No application deep-link/custom URL scheme was found. Plugin **link handlers** are different: they intercept modified clicks on matching URLs already visible inside terminal panes and invoke a plugin action. [Plugin link handlers](https://herdr.dev/docs/plugins/#link-handlers)
- No general extension SDK exists beyond executable plugin packages and the CLI/socket protocol. [Plugin overview](https://herdr.dev/docs/plugins/)
- A proposed standalone TypeScript socket SDK was closed without being added; the issue itself describes current Node/TypeScript choices as shelling out to the CLI or hand-rolling socket requests. [TypeScript SDK issue #87](https://github.com/ogulcancelik/herdr/issues/87)

Repository searches were against the exact inspected commit, including the API method enum and plugin/integration source: [source tree](https://github.com/ogulcancelik/herdr/tree/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src).

## What can and cannot change model/thinking effort

### Verified Herdr capabilities

- At launch, `agent start` forwards arguments after `--` unchanged to the selected agent executable. This can set model/effort if that particular agent has startup flags. Herdr's supported `--kind` values include `pi`, `claude`, `codex`, and many others. [Agent identity and launch](https://herdr.dev/docs/agent-automation/#agent-identity-and-launch)
- During a live TUI session, Herdr can send logical key sequences or text to the recognized agent, guarded against the agent no longer owning the pane. [Agent automation control surface](https://herdr.dev/docs/agent-automation/#choose-the-control-surface)
- A dispatcher can read the agent record first, branch on `agent`, send the appropriate TUI key sequence, and then optionally call `pane report-metadata --token model=... --token effort=...` for display. [Agent CLI](https://herdr.dev/docs/cli-reference/#agents), [metadata CLI](https://herdr.dev/docs/cli-reference/#panes)

### Inference and design consequence

- A cross-agent “effort cycle” key is **partly a Herdr integration**: Herdr accurately resolves and targets the focused agent, but each adapter must define what “cycle effort” means and how to verify the result.
- `agent prompt` is appropriate when the target agent exposes a textual command that is safe to submit as a line. `agent send-keys` is more appropriate for picker navigation, Escape/interrupt, shortcut invocation, or confirmation UI.
- Blind UI macros are state-sensitive. A robust adapter should inspect agent kind/status, send one supported action, and report failure instead of injecting a sequence into an unknown/blocked screen.
- Use display metadata only after the agent adapter confirms the new value. Herdr explicitly treats tokens as presentation, not truth about the agent's internal configuration.
- If an agent exposes a structured control API or extension mechanism, let the adapter use that and use Herdr only for focus/identity/state/presentation. Herdr's terminal input path should be the fallback.

### Concrete dispatcher shape

```text
Codex Micro key
  → reserved direct Herdr chord
  → [[keys.command]] or plugin_action
  → HERDR_ACTIVE_PANE_ID
  → herdr agent get <pane>
  → adapter: codex | claude | pi | …
  → structured agent control, else herdr agent send-keys
  → confirm/read result
  → optional Herdr metadata token + visible notification
```

The v0.7.5 release notes and current docs support every Herdr step above except the agent-specific adapter behavior, which belongs to each agent. [v0.7.5 release](https://github.com/ogulcancelik/herdr/releases/tag/v0.7.5), [Custom command environment](https://herdr.dev/docs/configuration/#custom-command-keybindings), [Agent CLI](https://herdr.dev/docs/cli-reference/#agents)

## Persistence, state, auth, privacy, and platform

### Persistence and local state

**Verified**

- Detach/reattach preserves live PTYs and processes because the background server continues running. A full server restart restores layout/cwd/focus, not arbitrary old processes. [Session state matrix](https://herdr.dev/docs/session-state/#what-survives)
- Optional pane-history replay is off by default because pane output may contain secrets, tokens, prompts, and command output. When enabled it writes `session-history.json` beside `session.json`. [Pane screen history](https://herdr.dev/docs/session-state/#pane-screen-history-replay)
- Native agent session restore is on by default for valid official integration-reported session references. Missing/invalid/stale/duplicated references restore as shells. [Native agent session restore](https://herdr.dev/docs/session-state/#native-agent-session-restore)
- Default persistence source documents `~/.config/herdr/session.json`; named sessions have their own session directories. [Persistence source at inspected commit](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/persist.rs), [Named sessions](https://herdr.dev/docs/persistence-remote/#named-sessions)
- Config is `~/.config/herdr/config.toml` on Linux/macOS and `%APPDATA%\herdr\config.toml` on Windows. Common logs are `herdr.log`, `herdr-client.log`, and `herdr-server.log` under the config directory. [Configuration](https://herdr.dev/docs/configuration/#config-file), [Logs](https://herdr.dev/docs/configuration/#logs)

### Authentication and access boundary

**Verified**

- Herdr advertises no account and no hosted control plane. [Official home page](https://herdr.dev/)
- Local control is through a per-session local Unix socket or Windows named pipe. Named sessions have separate sockets. [Socket paths](https://herdr.dev/docs/socket-api/#socket-paths)
- Unix API and client socket files are explicitly set to owner-read/write mode `0600` in the inspected source. [API server source](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/api/server.rs), [client socket source](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/server/socket_paths.rs)
- `herdr --remote` uses normal OpenSSH authentication; Herdr recommends verifying `ssh <target>` and using `ssh-agent` for non-interactive passphrase-protected keys. [Remote auth](https://herdr.dev/docs/persistence-remote/#remote-attach-over-ssh)

**Inference / caution**

- The raw JSON protocol does not document an application bearer token or per-method authorization. On Unix, possession of access to the owner-only socket is effectively the control boundary; any same-user code with socket access can read panes, inject input, launch commands, or stop the server.
- Windows uses a named pipe, but the inspected cross-platform helper makes Unix `chmod` a no-op on Windows. The exact named-pipe ACL inherited from the Rust `interprocess` implementation is not documented by Herdr; do not claim Unix-equivalent `0600` semantics on Windows without a dedicated ACL audit. [IPC source at inspected commit](https://github.com/ogulcancelik/herdr/blob/d4e0dd3d903c50d2edb8c3cec71952a83989b310/src/ipc.rs)

### Telemetry and outbound traffic

**Verified vendor claims and settings**

- Herdr's official site says “no account, no telemetry.” [Official home page](https://herdr.dev/)
- “No telemetry” does not mean no network requests. By default, Herdr performs a background version check and a background agent-manifest update check against herdr.dev. Both can be disabled with `update.version_check = false` and `update.manifest_check = false`. [Config reference: updates](https://herdr.dev/docs/config-reference/#updates), [Detection manifest updates](https://herdr.dev/docs/agents/#detection-manifests)
- Remote attach uses SSH and may download/install a matching Herdr binary on the remote host after an interactive prompt; plugin install clones GitHub repositories and may run their build commands. [Remote bootstrap](https://herdr.dev/docs/persistence-remote/#remote-attach-over-ssh), [Plugin install](https://herdr.dev/docs/plugins/#install-and-link)

The source search found no telemetry/analytics subsystem in the inspected tree, but this is supporting negative evidence rather than a formal privacy audit.

### Platform support

**Verified**

- Stable binaries support Linux and macOS on x86_64 and aarch64. Native Windows x86_64 is preview-only beta. [Install and assets](https://herdr.dev/docs/install/)
- Native `herdr --remote` is not part of the Windows beta; from Windows the documented route is SSH into the server and run Herdr there. Direct terminal attach is Unix-only in the Windows beta. [Remote platform limitations](https://herdr.dev/docs/persistence-remote/#remote-attach-over-ssh)
- `herdr --remote` supports Linux/macOS remote hosts on x86_64/aarch64 and can bridge local clipboard images to a remote temp file. Local keybindings are used by default; `--remote-keybindings server` selects the remote config. Local custom-command bindings are not sent because they would run on the remote host. [Remote attach behavior](https://herdr.dev/docs/persistence-remote/#remote-attach-over-ssh)

## Important limitations and unknowns

1. **No universal model/effort abstraction.** Herdr can launch with per-agent args and inject per-agent UI controls, but it cannot itself tell Codex, Claude Code, and Pi what a common effort level means. This is the main adapter responsibility.
2. **Screen-derived state can lag new UI shapes.** Claude/Codex blocked detection is intentionally strict; an unfamiliar prompt may show `idle` until the manifest is updated. It affects status/waits, not destructive auto-input. [Blocked state](https://herdr.dev/docs/agents/#blocked-state)
3. **Alternate-screen history is incomplete.** Full-screen agents such as Claude Code may render output that never enters host scrollback; `agent read` cannot recover vanished alternate-screen rows. [Alternate-screen caveat](https://herdr.dev/docs/agent-automation/#known-caveat-alternate-screen-output)
4. **Protocol compatibility must be checked.** Current live docs may be newer than a v0.7.5 server. Clients should call `ping`/`herdr status`, tolerate unknown fields, and use `herdr api schema --json` from the actual installed binary. [Protocol stability](https://herdr.dev/docs/socket-api/#protocol-stability)
5. **Plugin v1 is powerful but unsandboxed.** A keyboard-control plugin is code execution as the user; pin and inspect any third-party source. [Plugin trust](https://herdr.dev/docs/plugins/#trust-and-security)
6. **Windows local-control security needs separate validation.** Herdr documents the named-pipe transport but not its ACL contract.
7. **No official acknowledgement protocol for injected TUI shortcuts.** Confirmation must come from the agent's own structured API, its changed screen/state, or adapter-owned state; `agent send-keys` success only confirms that Herdr wrote the logical keys.
8. **Community plugins are not equivalent to official support.** The marketplace auto-indexes public GitHub repositories tagged `herdr-plugin`; entries are third-party and must be vetted. [Marketplace model](https://herdr.dev/docs/plugins/#marketplace)
9. **Approval keys need stronger guards than lifecycle state.** Because an unfamiliar question/approval screen can be classified as `idle`, a generic hardware approve/deny adapter should validate the exact target and visible prompt before sending input. [Blocked-state limits](https://herdr.dev/docs/agents/#blocked-state)
10. **Deduplicate socket events.** Open issue #1270 reports that a newly created `events.subscribe` stream can replay retained historical lifecycle events instead of only future events. A keyboard LED/status bridge should key updates by resource revision/transition and verify behavior on the installed version. [Issue #1270](https://github.com/ogulcancelik/herdr/issues/1270)

## Primary source index

- [Herdr home](https://herdr.dev/)
- [Documentation overview](https://herdr.dev/docs/)
- [Concepts](https://herdr.dev/docs/concepts/)
- [Keyboard](https://herdr.dev/docs/keyboard/)
- [Agents](https://herdr.dev/docs/agents/)
- [Agent automation](https://herdr.dev/docs/agent-automation/)
- [Session state and restore](https://herdr.dev/docs/session-state/)
- [Persistence and remote access](https://herdr.dev/docs/persistence-remote/)
- [Configuration](https://herdr.dev/docs/configuration/)
- [Config reference](https://herdr.dev/docs/config-reference/)
- [CLI reference](https://herdr.dev/docs/cli-reference/)
- [Socket API](https://herdr.dev/docs/socket-api/)
- [Integrations](https://herdr.dev/docs/integrations/)
- [Plugins](https://herdr.dev/docs/plugins/)
- [Install/platform support](https://herdr.dev/docs/install/)
- [Official repository](https://github.com/ogulcancelik/herdr)
- [v0.7.5 release](https://github.com/ogulcancelik/herdr/releases/tag/v0.7.5)
- [Exact source tree inspected](https://github.com/ogulcancelik/herdr/tree/d4e0dd3d903c50d2edb8c3cec71952a83989b310)
