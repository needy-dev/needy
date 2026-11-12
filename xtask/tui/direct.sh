#!/bin/sh
# xtask/tui/direct.sh PROGRAM [ARGS...]: the TUI matrix's baseline. Runs a
# program of the agents image (examples/agents) on Docker's terminal, as the
# image's user in its workspace, with the client's TERM and COLORTERM, as a
# person would run it on a local terminal. `--init` keeps the program from
# being PID 1, which ignores the signals it sends itself to exit or stop.
#
# The image's programs are the shims a container actor routes; the real
# ones are under /.needy/real, which PATH puts first, since no needy is
# injected here. IMAGE overrides the image (needy-agents by default);
# NEEDY_TUI_ENV names more variables to pass in (a profile's actor.env).
set -eu
program=$1
shift
case $program in
  /*) entrypoint=$program ;;
  *) entrypoint=/.needy/real/usr/local/bin/$program ;;
esac
pass=""
for name in ${NEEDY_TUI_ENV:-}; do pass="$pass -e $name"; done
# shellcheck disable=SC2086 # $pass is words
exec docker run --rm -it --init --user 1000:1000 -e HOME=/home/agent -e TERM -e COLORTERM -e LANG=C.UTF-8 $pass \
  -e PATH=/.needy/real/usr/local/bin:/.needy/real/usr/bin:/usr/local/bin:/usr/bin:/bin \
  -w /home/agent/workspace --entrypoint "$entrypoint" "${IMAGE:-needy-agents}" "$@"
