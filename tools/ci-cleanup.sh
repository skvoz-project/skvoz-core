#!/bin/sh
# Remove only this run's supplier container and require bounded absence proof.
set -eu
container=$1
case "$container" in
    "core-linux-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"|"core-android-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT") ;;
    *) echo 'Unexpected supplier container name' >&2; exit 1 ;;
esac
timeout --signal=TERM --kill-after=2s 20s docker rm -f "$container" >/dev/null 2>&1 || true
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
if timeout --signal=TERM --kill-after=2s 10s docker container inspect --format '{{.Id}}' "$container" >"$scratch/stdout" 2>"$scratch/stderr"; then
    echo 'Supplier container remains after cleanup' >&2
    exit 1
else
    status=$?
    if [ "$status" != 1 ]; then
        echo "Supplier absence inspection failed (status $status)" >&2
        exit 1
    fi
    LC_ALL=C tr -d '[:space:]' < "$scratch/stdout" > "$scratch/nonspace"
    if [ -s "$scratch/nonspace" ]; then
        echo 'Supplier absence inspection returned unexpected stdout' >&2
        exit 1
    fi
    if ! printf 'Error response from daemon: No such container: %s\n' "$container" | cmp -s - "$scratch/stderr"; then
        echo 'Supplier absence inspection returned an unexpected error' >&2
        exit 1
    fi
fi
