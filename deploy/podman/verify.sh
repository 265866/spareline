#!/usr/bin/env bash
# Prints the state of every unit, the container, and /healthz.
# shellcheck source=common.sh
source "$(dirname "$(realpath "$0")")/common.sh"
[[ $# -eq 0 ]] || fail 'Usage: verify.sh'
run systemctl --user is-active "$ADB_SERVICE" "$SERVICE" "${TIMERS[@]}"
run podman healthcheck run "$CONTAINER"
run podman inspect --format 'image={{.Image}} user={{.Config.User}} readonly={{.HostConfig.ReadonlyRootfs}} memory={{.HostConfig.Memory}} ports={{json .NetworkSettings.Ports}}' "$CONTAINER"
run curl -fsS http://127.0.0.1:8740/healthz && echo
run systemctl --user list-timers "${TIMERS[@]}" --no-pager
log 'Units, health check, and timers verified.'
