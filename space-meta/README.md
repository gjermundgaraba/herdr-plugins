# Space Meta

Adds space numbers, branch names, git-dirty markers, and PR badges to the Herdr
spaces sidebar through tokens you place in your config.

Publishes three workspace metadata tokens:

- `$git_dirty`: amber Nerd Font pencil `` (U+F448) when `git status` reports
  staged, unstaged, or untracked changes; absent when clean or not a repository
- `$numbered_workspace`: `1 workspace-name`, using the space's stable
  expanded group order; grouped worktree children use `1 branch-name #123`
  when an open PR exists
- `$branch_line`: an invisibly padded `branch` plus optional `#123`, aligned
  below the workspace label; the padding tracks the space number's digit count

One daemon per Herdr session does all the work; its only periodic work is
the PR refresh:

- Workspace, worktree, tab, and pane lifecycle events arrive over the
  socket's `events.subscribe`; a burst becomes one `session.snapshot` fetch.
- Each space's repository root (and, for linked worktrees, the worktree's own
  git directory) is watched with FSEvents, which batches activity for 0.75 s.
  Each batch is answered with one `git status --porcelain=v2 --branch`, giving
  the branch and dirty state together. The repository's own `status.*`
  settings apply, so `status.showUntrackedFiles=no` is honoured. A linked
  worktree's shared object store and refs are not watched; a main checkout's
  `.git` lies inside its tree, so a fetch there costs one scan.
- PR numbers come from `gh pr list --head <branch> --state open`, looked up
  when a checkout's branch changes and refreshed every five minutes. An answer
  for a branch no longer checked out is discarded; a failed lookup (offline,
  rate-limited, no `gh`) keeps the previous badge until the next refresh.
  Answers live in memory only.
- Only changed tokens are reported, and a space's row is first reported once
  its directory has been scanned, so a restart never flashes unscanned rows.

## Setup

Requires `git`; PR badges additionally require the [GitHub CLI](https://cli.github.com/) (`gh`).
macOS only (FSEvents).

Add the tokens to your `~/.config/herdr/config.toml`:

```toml
[ui.sidebar.spaces]
rows = [
  ["state_icon", { token = "$numbered_workspace", bold = true, dim = false }],
  ["$branch_line", { token = "$git_dirty", fg = "#e5c07b", dim = false }, "git_status"],
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

The `refresh` action stops the running daemon, starts the installed
executable, and republishes every token, so it doubles as the way to pick up
a rebuild.

## Notes

- Tokens are display-only metadata held in memory by Herdr: a server restart
  clears them, and the `startup` hook starts a fresh daemon that republishes.
- Branch/PR metadata resolves from a workspace's worktree checkout path when
  available; otherwise from the first snapshot pane in the workspace's first
  tab, whichever directory of the repository that is. Herdr emits no event
  when a pane changes directory, so that fallback is re-sampled only at
  workspace, worktree, tab, and pane lifecycle events or `refresh`. A
  directory that is not (or no longer) a repository is not watched; it is
  re-checked at those same events, so a `git init` shows up at the next one.
- Herdr's plugin snapshot does not expose desktop worktree-group collapse
  state, so numbering remains in stable expanded order while a group is
  collapsed.
- Herdr's plugin snapshot also does not expose whether a linked worktree has a
  custom name. Grouped child rows therefore use the branch (with a leading
  `worktree/` removed), even when Herdr's built-in row uses a custom name.
- Keep `$numbered_workspace` in the inline style table shown above so the
  composite number/name remains bold; write `$branch_line` as a bare token.

## Daemon lifecycle

The `startup` hook and the `refresh` action both start the daemon; a lock
file in the plugin's run directory keeps one per session socket and records
its pid. The daemon exits on its own when the session socket closes. If it dies for any other reason, badges stay
as last published until `refresh` or a server restart; errors are appended to
`logs/space-meta.log` under the plugin state directory.
