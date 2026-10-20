#!/bin/sh
# Remove only this run's supplier container and require bounded absence proof.
set -eu
container=$1
case "$container" in
    "core-linux-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"|"core-android-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT") ;;
    *) echo 'Unexpected supplier container name' >&2; exit 1 ;;
esac
timeout --signal=TERM --kill-after=2s 20s docker rm -f "$container" >/dev/null 2>&1 || true
if absence=$(timeout --signal=TERM --kill-after=2s 10s docker container inspect --format '{{.Id}}' "$container" 2>&1); then
    echo 'Supplier container remains after cleanup' >&2
    exit 1
else
    status=$?
    test "$status" = 1
    test "$absence" = "Error response from daemon: No such container: $container"
fi
