#!/usr/bin/env bash
# Checks the kit's show_units, which verify.sh runs, with a stand-in systemctl: it fails when any
# one of the units isn't active, since `systemctl is-active a b c` passes when only one of them is.
# Needs bash and coreutils, not systemd.
#   bash deploy/tests/show-units.sh
# shellcheck source-path=SCRIPTDIR
set -euo pipefail
kit=$(cd "$(dirname "$0")/../podman" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/bin" "$work/app"
cp -a "$kit/." "$work/app/"
cat >"$work/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
# A stand-in for systemctl: every unit listed in $INACTIVE is inactive, the rest are active, and
# several units in one call pass when any one is active, as systemd does.
shift 2
status=4
for unit in "$@"; do
  if grep -qx -- "$unit" "$INACTIVE"; then echo inactive; else echo active; status=0; fi
done
exit "$status"
EOF
chmod +x "$work/bin/systemctl"

failures=0
check() {
  local scenario=$1 stopped=$2 want=$3 got
  printf '%s\n' "$stopped" >"$work/inactive"
  if (
    export PATH="$work/bin:$PATH" INACTIVE="$work/inactive" SPARELINE_APP="$work/app"
    # shellcheck source=../podman/common.sh
    source "$work/app/common.sh"
    trap - ERR
    show_units
  ) >"$work/out" 2>&1; then got=passes; else got=fails; fi
  if [[ $got == "$want" ]]; then
    echo "ok: $scenario: $got"
  else
    echo "FAIL: $scenario: want $want, got $got"
    sed 's/^/  /' "$work/out"
    failures=$((failures + 1))
  fi
}

check "every unit active" "" passes
check "the service stopped" spareline.service fails
check "the adb server stopped" spareline-adb.service fails
check "the update timer off" spareline-update.timer fails

[[ $failures -eq 0 ]]
