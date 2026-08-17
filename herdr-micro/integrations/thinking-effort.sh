#!/bin/sh
set -eu

operation=${1:-}
pane=${HERDR_PANE_ID:-}

[ -n "$operation" ] || {
    echo "thinking-effort: missing operation" >&2
    exit 2
}
[ -n "$pane" ] || {
    echo "thinking-effort: missing HERDR_PANE_ID" >&2
    exit 2
}
herdr_bin=${HERDR_BIN_PATH:-herdr}

if [ "$operation" = claude ]; then
    case "${2:-}" in
        raise) key=right ;;
        lower) key=left ;;
        *)
            echo "thinking-effort: Claude direction must be raise or lower" >&2
            exit 2
            ;;
    esac
    "$herdr_bin" pane send-text "$pane" /effort
    "$herdr_bin" pane send-keys "$pane" enter
    sleep 0.15
    "$herdr_bin" pane send-keys "$pane" "$key"
    sleep 0.1
    exec "$herdr_bin" pane send-keys "$pane" enter
fi

exec "$herdr_bin" pane send-keys "$pane" "$operation"
