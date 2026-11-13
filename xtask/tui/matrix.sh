#!/bin/bash
# xtask/tui/matrix.sh OUT [TUI...]: the TUI fidelity matrix. Each TUI of
# the agents image (examples/agents) runs its scenario (TUI.scenario next to
# this file) through `cargo xtask tui run`, once directly on Docker's
# terminal (direct.sh) and once on each path through needy, and each path's
# report is compared with the direct one (`cargo xtask tui diff`).
#
# Paths (PATHS, all by default):
#   direct      docker run -it, the baseline
#   create      needy run create -it
#   attach      needy run create -it, detach (ctrl-]), needy run attach
#   viewer      needy run attach --read-only, beside `attach`, snapping the
#               moments `attach` snaps
#   background  needy run create -t (no client while the TUI starts), then
#               needy run attach
#   exec        needy run exec -t of direct.sh on a native executor of the run
#
# Needs: a control daemon NEEDY reaches, the agents image (IMAGE, default
# needy-agents) in the local Docker engine and, for the VM drivers, its VM
# image (`needy image build --from IMAGE`); a set IMAGE is also the actor's
# image in the profiles this writes. DRIVER is the actor's
# driver: macos-vz, macos-apple-container, linux-firecracker or
# linux-container. TUIs are opencode pi claude codex herdr. The profiles
# this writes start each run from an empty login directory and mirror it out
# under OUT/state, so no real login is read or written.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=${1:?usage: matrix.sh OUT [TUI...]}
shift
mkdir -p "$out"
out=$(cd "$out" && pwd)
tuis=${*:-opencode pi claude codex herdr}
NEEDY=${NEEDY:-$root/bin/needy}
XTASK=${XTASK:-$root/target/debug/xtask}
DRIVER=${DRIVER:?set DRIVER to the actor driver}
PATHS=${PATHS:-direct create attach viewer background exec}
workspace=${WORKSPACE:-$out/workspace}
mkdir -p "$workspace" "$out/profiles" "$out/state/empty"

# The environment and arguments each profile gives its driving program, so
# the baseline runs the program as the actor does.
direct_env() {
  case $1 in
    claude) echo "CLAUDE_CONFIG_DIR=/home/agent/.claude DISABLE_UPDATES=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1" ;;
    pi) echo "PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0" ;;
    opencode) echo "OPENCODE_DISABLE_AUTOUPDATE=1" ;;
    herdr) echo "CLAUDE_CONFIG_DIR=/home/agent/.claude DISABLE_UPDATES=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1 PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0 OPENCODE_DISABLE_AUTOUPDATE=1" ;;
  esac
}
direct_args() {
  case $1 in
    codex) echo "--sandbox danger-full-access -c check_for_update_on_startup=false -c analytics.enabled=false" ;;
  esac
}

profile() {
  local p=$out/profiles/$1.yaml
  {
    echo "extends: $root/examples/agents/$1.yaml"
    printf 'actor:\n  driver: %s\n' "$DRIVER"
    [ -n "${IMAGE:-}" ] && printf '  image: %s\n' "$IMAGE"
    case $DRIVER in
      macos-vz | macos-apple-container)
        printf '  workspace: { from: "executor:build:%s" }\n' "$workspace"
        printf 'executors:\n  build: { driver: native }\n  host: { driver: native }\n'
        ;;
      linux-container)
        printf '  container: { oci_runtime: runc }\n'
        printf 'executors:\n  host: { driver: native }\n'
        ;;
      *) printf 'executors:\n  host: { driver: native }\n' ;;
    esac
    # Every run starts from an empty login and settings directory and
    # mirrors it out to a directory of its own: a TUI's first-start screens
    # stay the same in every path, and no ~/needy-state is read or written.
    local agents=$1
    [ "$1" = herdr ] && agents="claude codex pi opencode"
    echo "files:"
    for agent in $agents; do
      printf '  %s-in: { from: "host:%s/" }\n  %s-out: { to: "host:%s/{run}-%s/" }\n' \
        "$agent" "$out/state/empty" "$agent" "$out/state" "$agent"
    done
  } > "$p"
  echo "$p"
}

# The lines of a scenario up to its first snap: the wait for the start.
start_of() { awk '/^snap /{exit} {print}' "$1"; }

# A read-only viewer's scenario: the start, then each snap of the client
# that types, once that client marked it.
viewer_of() {
  start_of "$1"
  awk '/^snap /{print "await " $2 " 120"; print "quiet 300 3"; print}' "$1"
  echo "exit 120"
}

harness() {
  local label=$1 scenario=$2
  shift 2
  echo "  $label: $*" >&2
  "$XTASK" tui run --scenario "$scenario" --out "$dir/$label.json" --raw "$dir/$label.raw" "${envs[@]}" "$@" 2> "$dir/$label.log"
  grep -c FAIL "$dir/$label.log" | sed "s/^/    failed steps: /" >&2
}

has() { [[ " $PATHS " == *" $1 "* ]]; }

for tui in $tuis; do
  echo "== $tui ($DRIVER)" >&2
  dir=$out/$tui
  rm -rf "$dir"
  mkdir -p "$dir/marks/attach" "$dir/marks/viewer"
  scenario=$here/$tui.scenario
  start_of "$scenario" > "$dir/detach.scenario"
  printf 'key ctrl-]\nexit 30\n' >> "$dir/detach.scenario"
  viewer_of "$scenario" > "$dir/viewer.scenario"
  envs=()
  viewer=
  for kv in $(direct_env "$tui"); do envs+=(-e "$kv"); done
  envs+=(-e "NEEDY_TUI_ENV=$(direct_env "$tui" | sed 's/=[^ ]*//g')")
  # `exec` passes the same to the executor's process.
  exec_env=()
  for kv in $(direct_env "$tui"); do exec_env+=(-e "$kv"); done
  exec_env+=(-e "NEEDY_TUI_ENV=$(direct_env "$tui" | sed 's/=[^ ]*//g')")
  [ -n "${IMAGE:-}" ] && exec_env+=(-e "IMAGE=$IMAGE")
  p=$(profile "$tui")
  id=tui-$tui-$$
  # shellcheck disable=SC2046 # the arguments are words
  has direct && harness direct "$scenario" -- "$here/direct.sh" "$tui" $(direct_args "$tui")
  has create && harness create "$scenario" -- "$NEEDY" run create -it --name "$id-create" --profile "$p"
  if has attach; then
    harness detach "$dir/detach.scenario" -- "$NEEDY" run create -it --name "$id-attach" --profile "$p"
    if has viewer; then
      harness viewer "$dir/viewer.scenario" --marks "$dir/marks/viewer" --follow "$dir/marks/attach" -- "$NEEDY" run attach --read-only "$id-attach" &
      viewer=$!
      sleep 2
    fi
    follow=()
    has viewer && follow=(--marks "$dir/marks/attach" --follow "$dir/marks/viewer")
    harness attach "$scenario" "${follow[@]}" -- "$NEEDY" run attach "$id-attach"
    [ -n "${viewer:-}" ] && wait "$viewer"
    "$NEEDY" run stop "$id-attach" > /dev/null 2>&1
  fi
  if has background || has exec; then
    "$NEEDY" run create -t --name "$id-bg" --profile "$p" > /dev/null
    # shellcheck disable=SC2046
    has exec && harness exec "$scenario" -- "$NEEDY" run exec "$id-bg" -t "${exec_env[@]}" --executor host -- "$here/direct.sh" "$tui" $(direct_args "$tui")
    has background && harness background "$scenario" -- "$NEEDY" run attach "$id-bg"
    "$NEEDY" run stop "$id-bg" > /dev/null 2>&1
  fi
  "$NEEDY" run stop "$id-create" > /dev/null 2>&1
  for label in create attach viewer background exec; do
    [ -f "$dir/$label.json" ] && [ -f "$dir/direct.json" ] && "$XTASK" tui diff "$dir/direct.json" "$dir/$label.json" > "$dir/diff-$label.json"
  done
done
echo "reports in $out" >&2
