#!/bin/sh
set -eu

exec "${HERDR_BIN_PATH:-herdr}" plugin pane open \
  --plugin gjermundgaraba.herdr-command-palette \
  --entrypoint palette

