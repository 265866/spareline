#!/usr/bin/env bash
# First install, and safe to rerun: links the units, starts the adb server and
# spareline, waits for healthy, and enables the update and backup timers.
# shellcheck source=common.sh
source "$(dirname "$(realpath "$0")")/common.sh"
[[ $# -eq 0 ]] || fail 'Usage: install.sh'
lock
chmod 700 "$APP/"*.sh
if [[ ! -f "$APP/app.env" ]]; then install -m 600 "$APP/app.env.example" "$APP/app.env"; fi
if ! run podman image exists "$CURRENT"; then
  pull_upstream
  run podman tag "$UPSTREAM" "$CURRENT"
fi
link_units
run systemctl --user enable --now "$ADB_SERVICE"
# No phone may have allowed this host's new adb key yet, so a first install
# only needs the image to reach the adb server.
device_smoke "$(image_id "$CURRENT")" 0
run systemctl --user start "$SERVICE"
wait_healthy
run systemctl --user enable --now "${TIMERS[@]}"
log 'Installation verified healthy.'
