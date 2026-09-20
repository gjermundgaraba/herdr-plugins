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
micro_bin=${HERDR_MICRO_BIN_PATH:?missing HERDR_MICRO_BIN_PATH}

if [ "$operation" = claude ]; then
    case "${2:-}" in
        raise) key=right ;;
        lower) key=left ;;
        *)
            echo "thinking-effort: Claude direction must be raise or lower" >&2
            exit 2
            ;;
    esac
    "$micro_bin" client input text /effort
    "$micro_bin" client input keys enter
    sleep 0.15
    "$micro_bin" client input keys "$key"
    sleep 0.1
    exec "$micro_bin" client input keys enter
fi

exec "$micro_bin" client input keys "$operation"
