# herdr-clanker-picker

An **agent inbox** for [herdr](https://github.com/ogulcancelik/herdr):
a popup listing every agent in the session as one flat list, ranked by who
needs you. Enter jumps to it.

Ranking, top to bottom:

1. **★ focus** — agents you marked with `f`
2. **blocked** — waiting on your input
3. **done** — finished, needs review
4. **working**
5. **idle** / unknown

Within each group, the most recent state change comes first. Panes without
agents don't appear at all — herdr's built-in Goto navigator (and sidebar)
already cover plain pane navigation.

Each agent gets two lines — workspace and `agent · state` on the first,
the live task line (the agent's terminal title) underneath:

```
 ◆ ⠼ herdr                                claude · working
     ~/ws/pers/herdr · fix auth token refresh
```

The second line leads with the agent's working directory (home-shortened,
middle-elided when long); `/` search matches it too.

The line at the bottom of the popup carries the full untruncated context
for the selected row, and the header shows a live summary
(`2 blocked · 1 done · 4 working`).

## Install

```bash
herdr plugin install gjermundgaraba/herdr-plugins/clanker-picker
```

Or for local development:

```bash
cargo build --release
herdr plugin link /path/to/herdr-plugins/clanker-picker
```

Then bind a key in your herdr `config.toml`:

```toml
[[keys.command]]
key = "prefix+g"
type = "plugin_action"
command = "gjermundgaraba.herdr-clanker-picker.open"
description = "agent inbox (clanker picker)"
```

(Optionally set `goto = ""` under `[keys]` to release `prefix+g` from the
built-in navigator first.)

The popup only opens from the normal workspace view; pressing the key while
a modal or settings screen is up shows a toast instead.

## Keys

| Key | Action |
|---|---|
| `enter` | jump to the selected agent's pane and close |
| `/` | focus the search box — matches title, workspace, and agent (`esc` to leave) |
| `f` | mark/unmark the selected agent for focus (★, pinned to the top; survives across opens) |
| `r` | toggle newest/oldest state change first |
| `b` `w` `i` `d` | toggle blocked/working/idle/done state filters (multi-select) |
| `a` / `backspace` | clear all state filters |
| `j`/`k`/`↑`/`↓`, `ctrl+d`/`ctrl+u`, `home`/`end`/`G` | move |
| `esc` | close |

Mouse: hover selects, click jumps, wheel scrolls.

## Configuration

Create `config.toml` in the directory printed by:

```bash
herdr plugin config-dir gjermundgaraba.herdr-clanker-picker
```

To reverse the default recency order:

```toml
recency_order = "oldest-first"
```

The default is `"newest-first"`. Pressing `r` toggles the order for the
currently open popup; it does not rewrite the config file. Theming follows the
host automatically (below).

## Theming

The picker follows your herdr theme by reading the host `config.toml`
(`[theme]` name / auto_switch / custom overrides, plus the legacy
`[ui] accent`) and resolving it against a copy of herdr's palette table.
Notes:

- The palette table is copied from herdr `src/app/state.rs`. Themes added
  to herdr after this copy fall back to catppuccin until the table is
  re-synced.
- `auto_switch` light/dark cannot be observed from a plugin; the dark
  theme is used (herdr's own fallback).

## Notes and limitations

- Recency comes from herdr's `state_change_seq`, a monotonic counter — it
  gives exact ordering but no wall-clock times, and it resets when the
  herdr server restarts.
- Agent state refreshes ~1×/second (snapshot polling) instead of per-frame.
- Panes without a title show the agent name, or `pane N` as a last resort —
  the launch command name is not available over the socket API.

## License and attribution

Apache-2.0 (see `LICENSE`). Portions of this code — the filter/selection
model, parts of the rendering, the palette table, and the text helpers —
are copied from [herdr](https://github.com/ogulcancelik/herdr)
(Apache-2.0) and adapted to run against its socket API. Popup lifecycle
mechanics were informed by
[herdr-configurable-picker](https://github.com/yoshiori/herdr-configurable-picker).
