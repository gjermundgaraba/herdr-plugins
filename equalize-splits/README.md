# herdr-equalize-splits

Automatically equalizes pane sizes after every split in [Herdr](https://herdr.dev/).
Splitting the right pane in a 50/50 two-pane tab therefore produces three
equal-width panes instead of 50/25/25.

Only the connected group in the new split's direction is equalized. Orthogonal
layouts and same-direction groups beyond them keep their existing ratios.

Herdr 0.7.5 does not include the split source or requested ratio in
`pane.created`, so explicit split ratios and panes created by `layout.apply`
can be equalized too.

## Install

```sh
herdr plugin install gjermundgaraba/herdr-plugins/equalize-splits
```

For local development:

```sh
cd /path/to/herdr-plugins/equalize-splits
go build -o equalize-splits .
herdr plugin link "$PWD"
```

No keybinding or configuration is required. Requires Herdr >= 0.7.5. Installing
from GitHub requires `go` to build the binary.

## Development

```sh
go test ./...
herdr plugin log list --plugin gjermundgaraba.herdr-equalize-splits
```
