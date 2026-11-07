#!/bin/sh
# Start slintcon with a login prompt on one or more VTs, for testing.
#
#   sudo scripts/login-vts.sh [--kms|--fbdev] [VT...]     (default: --fbdev 7 8 9)
#
# slintcon takes the VT that is active when its event loop starts, so each one
# is started with `openvt -s` (switch to it, then run) and given a moment to
# set itself up before the next VT is started. Afterwards the VT you started
# from is shown again. When a login session ends, slintcon on that VT exits;
# run this script again to get the prompt back.
set -eu

backend=--fbdev
case "${1:-}" in
--kms | --fbdev)
	backend=$1
	shift
	;;
esac
[ $# -gt 0 ] || set -- 7 8 9

root=$(cd "$(dirname "$0")/.." && pwd)
slintcon=${SLINTCON:-$root/target/release/slintcon}
[ -x "$slintcon" ] || {
	echo "no executable at $slintcon (set SLINTCON=...)" >&2
	exit 1
}

origin=$(sed 's/^tty//' /sys/class/tty/tty0/active)

for vt in "$@"; do
	# `login` builds a clean environment, so nothing of the host session
	# (DISPLAY, WAYLAND_DISPLAY, ...) leaks into the prompt.
	openvt -s -c "$vt" -- "$slintcon" "$backend" -- /bin/login
	sleep 2
done

chvt "$origin"
