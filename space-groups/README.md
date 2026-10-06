# Space Groups

Put spaces into named groups that the spaces sidebar shows under collapsible
headers. You pick or create a group in Herdr's native picker; the fork build
does the rest natively and keeps the assignment across server restarts and
live handoffs.

Requires the [Herdr fork build](../README.md#herdr-build): stock Herdr's
sidebar does not group spaces.

## How it works

The fork's sidebar groups spaces by the `space_group` workspace metadata
token. This plugin is only the picker: it applies your choice with
`workspace.set_group`, and Herdr handles placement and regrouping.

- The sidebar shows spaces in Herdr's workspace order and starts a
  `▾ name ── count` header wherever the group changes, with a divider where
  ungrouped spaces follow a group. With no groups the sidebar stays flat.
- A worktree family shares its checkout's group. Running the action on a
  linked worktree assigns its checkout, and with it the whole family.
- Click a header to collapse or expand it. A collapsed header shows the most
  urgent agent state in the group, and the focused space stays visible.
- Drag a header to move the whole group, and drag a space to move just that
  space. Both drop anywhere. A moved space takes the group of where it lands
  (see below). A group dropped inside another stays together and splits the
  other group into two runs.

Assigning a group also moves the space into place: after the group's last
member, or, for a new group, above the first ungrouped space. That keeps
each group together, with groups above ungrouped spaces. The sidebar never
reorders anything itself, so Herdr's workspace numbers and number keys always
match it.

A space moved any other way, by a sidebar drag or `herdr workspace move`, is
regrouped by Herdr:

- Dropped inside a group, it joins that group. Dropped among ungrouped
  spaces, it leaves its group.
- Dropped between two runs, it keeps its group if the space above or below
  shares it, or if it is the group's only space. Otherwise it joins the group
  above it (the one below at the very top).

Herdr saves the token with the session, so it survives restarts and live
handoffs; closing a space drops it. The plugin keeps no state of its own. Its
`startup` hook imports the assignments earlier versions saved per session and
renames that file to `groups.json.imported`.

## Setup

```sh
herdr plugin install gjermundgaraba/herdr-plugins/space-groups
```

Bind the action in `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+g"
type = "plugin"
command = "gjermundgaraba.herdr-space-groups.assign"
description = "Set space group"
```

The action opens Herdr's native picker for the focused space. On the fork
build, **Set space group** also appears in a space's right-click menu and
targets the space you clicked. Type to filter the existing groups or to name a
new one, move with `↑`/`↓` (or `ctrl+n`/`ctrl+p`), and press `Enter`.
**Remove from group** clears a grouped space's group. `Esc` cancels.

## Notes

- A group exists while at least one space is in it; there is nothing to
  create or delete separately.
- Collapse state belongs to each TUI client, like worktree groups.
- `workspace.set_group` is a plain socket method, so scripts can assign groups
  too. `herdr workspace report-metadata` can set the `space_group` token
  directly, but then the space is not moved, and the token is saved only with
  `--persist`. A group whose
  members are not next to each other shows a header for each run; drag them
  together or reassign through the picker. Collapsing one run of a group
  collapses all of its runs.

## Local development

Run these commands from the repository root:

```sh
cargo build --release --locked -p herdr-space-groups
mkdir -p space-groups/bin
install -m 750 target/release/herdr-space-groups space-groups/bin/.herdr-space-groups.new
mv -f space-groups/bin/.herdr-space-groups.new space-groups/bin/herdr-space-groups
herdr plugin link "$PWD/space-groups"
```

The plugin runs its `bin/` copy; `target/` is only used while building. A
linked plugin's startup hook first runs at the next server start or handoff.
