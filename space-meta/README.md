# Space Meta

Space numbers and PR badges in the Herdr spaces sidebar.

Publishes two workspace metadata tokens:

- `$numbered_workspace` — `1 workspace-name`, using the space's stable
  expanded group order; grouped worktree children use `1 branch-name #123`
  when a PR exists
- `$branch_line` — an invisibly padded `branch` plus optional `#123`, aligned
  below the workspace label; the padding tracks one- and two-digit numbers

Refreshes on startup, workspace lifecycle events (created / updated / renamed /
closed / moved / reordered / focused), worktree changes, and the `refresh`
action. PR lookups are cached per checkout and current branch for 60 seconds,
so switching between spaces does not hit the network each time.

## Setup

Requires `git`; PR badges additionally require the [GitHub CLI](https://cli.github.com/) (`gh`).

Add the tokens to your `~/.config/herdr/config.toml`:

```toml
[ui.sidebar.spaces]
rows = [
  ["state_icon", { token = "$numbered_workspace", bold = true, dim = false }],
  ["$branch_line", "git_status"],
]
```

Herdr uses a plain space after the built-in `state_icon`, so the first row has
no separator dot. `$branch_line` includes the branch even without a PR and
uses U+2800 Braille blanks for alignment because Herdr trims ordinary leading
spaces. Then:

```sh
herdr server reload-config
herdr plugin action invoke gjermundgaraba.herdr-space-meta.refresh
```

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/space-meta
```

## Local development

Run these commands from the repository root:

```sh
cargo build --release --locked -p herdr-space-meta
mkdir -p space-meta/bin
install -m 750 target/release/herdr-space-meta space-meta/bin/.herdr-space-meta.new
mv -f space-meta/bin/.herdr-space-meta.new space-meta/bin/herdr-space-meta
herdr plugin link "$PWD/space-meta"
herdr plugin action invoke gjermundgaraba.herdr-space-meta.refresh
```

## Notes

- Tokens are display-only metadata held in memory by Herdr: a server restart
  clears them, and the plugin repopulates them on the next event or the
  `refresh` action.
- Branch/PR metadata resolves from a workspace's worktree checkout path when
  available; otherwise it best-effort uses the first snapshot pane in the
  workspace's first tab. Branch switches refresh on the next registered event
  for that workspace. Run the `refresh` action to refresh all workspace
  branches; PR results remain cached for up to 60 seconds.
- Herdr's plugin snapshot does not expose desktop worktree-group collapse
  state, so numbering remains in stable expanded order while a group is
  collapsed.
- Herdr's plugin snapshot also does not expose whether a linked worktree has a
  custom name. Grouped child rows therefore use the branch (with a leading
  `worktree/` removed), even when Herdr's built-in row uses a custom name.
- Keep `$numbered_workspace` in the inline style table shown above so the
  composite number/name remains bold; write `$branch_line` as a bare token.
