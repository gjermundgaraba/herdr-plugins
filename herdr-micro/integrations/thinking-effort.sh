#!/bin/sh
set -eu

operation=${1:-}
repeat=${HERDR_MICRO_REPEAT:-}
pane=${HERDR_PANE_ID:-}

[ -n "$operation" ] || {
    echo "thinking-effort: missing operation" >&2
    exit 2
}
[ -n "$pane" ] || {
    echo "thinking-effort: missing HERDR_PANE_ID" >&2
    exit 2
}
case "$repeat" in
    ''|*[!0-9]*|0)
        echo "thinking-effort: invalid HERDR_MICRO_REPEAT: ${repeat:-unset}" >&2
        exit 2
        ;;
esac

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
else
    key=$operation
fi

set -- "$pane"
i=0
while [ "$i" -lt "$repeat" ]; do
    set -- "$@" "$key"
    i=$((i + 1))
done

if [ "$operation" = claude ]; then
    "$herdr_bin" pane send-keys "$@"
    sleep 0.1
    exec "$herdr_bin" pane send-keys "$pane" enter
fi

exec "$herdr_bin" pane send-keys "$@"
