#!/bin/sh
set -eu

dry_run=false
help=false
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=true ;;
        --help|-h) help=true ;;
    esac
done

if [ "$dry_run" = true ] || [ "$help" = true ]; then
    exec /usr/local/bin/new-ploy-runner "$@"
fi

echo "polymarket-market-recorder requires --dry-run" >&2
exit 64
