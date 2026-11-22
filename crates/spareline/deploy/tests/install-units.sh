#!/usr/bin/env bash
# Checks the kit's install_units, which install.sh runs, in a temporary HOME with a stand-in
# systemctl: the services and timers become regular files with the kit's contents, replacing an
# older kit's links, since `systemctl --user disable` deletes a linked unit; exactly the units that
# were enabled are reenabled; the quadlet stays a link; and no temporary file is left behind.
# Needs bash and coreutils, not systemd.
# Run it from the kit's directory, the one that holds podman/ and tests/:
#   bash tests/install-units.sh
# shellcheck source-path=SCRIPTDIR
set -euo pipefail
kit=$(cd "$(dirname "$0")/../podman" && pwd)
work=$(mktemp -d)
trap 'chmod -R u+w "$work"; rm -rf "$work"' EXIT

mkdir -p "$work/bin" "$work/app"
cp -a "$kit/." "$work/app/"
cat >"$work/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
# A stand-in for systemctl that records each call.
printf '%s\n' "$*" >>"$CALLS"
EOF
chmod +x "$work/bin/systemctl"

kit_units=(spareline-adb.service spareline-backup.service spareline-backup.timer
  spareline-update.service spareline-update.timer)
failures=0
fail_check() { echo "FAIL: $1: $2"; failures=$((failures + 1)); }

install_in() {
  local home=$1
  (
    export PATH="$work/bin:$PATH" HOME="$home" CALLS="$home/calls" SPARELINE_APP="$work/app"
    # shellcheck source=../podman/common.sh
    source "$work/app/common.sh"
    trap - ERR
    install_units
  ) >"$home/out" 2>&1
}

# Every unit is a regular file of mode 600 with the kit's contents, the quadlet links to the kit,
# systemd was reloaded, and the unit directory holds nothing else of the kit's.
check_installed() {
  local scenario=$1 home=$2 unit target left
  for unit in "${kit_units[@]}"; do
    target=$home/.config/systemd/user/$unit
    if [[ -L $target || ! -f $target ]]; then fail_check "$scenario" "$unit is not a regular file"; continue; fi
    cmp -s "$target" "$work/app/systemd/$unit" || fail_check "$scenario" "$unit differs from the kit"
    [[ $(stat -c %a "$target") == 600 ]] || fail_check "$scenario" "$unit has mode $(stat -c %a "$target")"
  done
  [[ $(readlink "$home/.config/containers/systemd/spareline.container") == "$work/app/quadlet/spareline.container" ]] ||
    fail_check "$scenario" "the quadlet is not a link to the kit"
  grep -qx -- '--user daemon-reload' "$home/calls" || fail_check "$scenario" "no daemon-reload"
  left=$(find "$home/.config/systemd/user" -maxdepth 1 -name '.spareline*')
  [[ -z $left ]] || fail_check "$scenario" "temporary files left: $left"
}

scenario() {
  local name=$1 before=$failures
  shift
  "$@"
  if [[ $failures -eq $before ]]; then echo "ok: $name"; fi
}

fresh_install() {
  local home=$work/fresh
  mkdir -p "$home"
  install_in "$home" || { fail_check "fresh install" "install_units failed"; sed 's/^/  /' "$home/out"; return; }
  check_installed "fresh install" "$home"
  ! grep -q reenable "$home/calls" || fail_check "fresh install" "reenabled a unit nothing had enabled"
}

# An older kit linked each unit to its file in the kit, and enabling a unit linked it from a .wants
# directory to the same file. The links are replaced, and exactly the enabled units are reenabled,
# which is how systemd moves their wants links to the copies; the stand-in leaves them as they were.
older_links_replaced() {
  local home=$work/older unit dir
  dir=$home/.config/systemd/user
  mkdir -p "$dir/timers.target.wants"
  for unit in "${kit_units[@]}"; do ln -s "$work/app/systemd/$unit" "$dir/$unit"; done
  mkdir -p "$dir/default.target.wants"
  ln -s "$work/app/systemd/spareline-update.timer" "$dir/timers.target.wants/spareline-update.timer"
  ln -s "$work/app/systemd/spareline-adb.service" "$dir/default.target.wants/spareline-adb.service"
  install_in "$home" || { fail_check "older kit's links" "install_units failed"; sed 's/^/  /' "$home/out"; return; }
  check_installed "older kit's links" "$home"
  [[ -f $work/app/systemd/spareline-update.timer && ! -L $work/app/systemd/spareline-update.timer ]] ||
    fail_check "older kit's links" "the kit's own unit file was changed"
  [[ $(readlink "$dir/timers.target.wants/spareline-update.timer") == "$work/app/systemd/spareline-update.timer" ]] ||
    fail_check "older kit's links" "the wants link was changed"
  [[ $(grep reenable "$home/calls") == "--user reenable spareline-adb.service spareline-update.timer" ]] ||
    fail_check "older kit's links" "reenabled: $(grep reenable "$home/calls" || echo nothing)"
}

# A kit update reaches the installed units: one unit changes in the kit before the second run.
second_run() {
  local home=$work/fresh
  : >"$home/calls"
  echo '# changed by a kit update' >>"$work/app/systemd/spareline-backup.timer"
  install_in "$home" || { fail_check "second run" "install_units failed"; sed 's/^/  /' "$home/out"; return; }
  check_installed "second run" "$home"
}

unwritable_directory() {
  local home=$work/readonly
  if [[ $(id -u) -eq 0 ]]; then echo "skipped: unwritable unit directory (root can write to it)"; return; fi
  mkdir -p "$home/.config/systemd/user" "$home/.config/containers/systemd"
  chmod 500 "$home/.config/systemd/user"
  if install_in "$home"; then fail_check "unwritable unit directory" "install_units passed"; fi
  [[ -z $(find "$home/.config/systemd/user" -mindepth 1) ]] ||
    fail_check "unwritable unit directory" "files were written"
}

# A directory where a unit belongs fails the install, and its temporary file is removed.
directory_in_the_way() {
  local home=$work/blocked
  mkdir -p "$home/.config/systemd/user/spareline-update.timer"
  if install_in "$home"; then fail_check "directory in the way" "install_units passed"; fi
  [[ -z $(find "$home/.config/systemd/user" -name '.spareline*') ]] ||
    fail_check "directory in the way" "temporary files left: $(find "$home/.config/systemd/user" -name '.spareline*')"
}

scenario "fresh install" fresh_install
scenario "older kit's links replaced" older_links_replaced
scenario "second run" second_run
scenario "unwritable unit directory fails" unwritable_directory
scenario "a directory in a unit's place fails" directory_in_the_way

[[ $failures -eq 0 ]]
