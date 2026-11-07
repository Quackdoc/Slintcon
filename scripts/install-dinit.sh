#!/bin/sh
# Install slintcon login prompts on VTs as dinit services
#
#   sudo scripts/install-dinit.sh [--kms|--fbdev] [VT...]    (default: --fbdev 7 8 9)
#   sudo scripts/install-dinit.sh --uninstall [VT...]
#
# Installs a dinit template service (`/etc/dinit.d/slintcon`, `$1` is the VT
# name) plus a `boot.d/slintcon@ttyN` link per VT, like dinit-rc does for getty.
# The service passes `--vt $1`, so slintcon waits until that VT is shown, and
# restarts on every exit like a getty. `login` runs on a pty, so the utmp INIT
# record for the VT is not written.
set -eu

dinit_dir=${DINIT_DIR:-/etc/dinit.d}
boot_dir=$dinit_dir/boot.d
bin=/usr/local/bin/slintcon
template=slintcon

action=install
backend=--fbdev
case "${1:-}" in
--uninstall)
	action=uninstall
	shift
	;;
--kms | --fbdev)
	backend=$1
	shift
	;;
esac
[ $# -gt 0 ] || set -- 7 8 9

[ "$(id -u)" -eq 0 ] || {
	echo "run as root" >&2
	exit 1
}

# Remove services from earlier versions of this script (names dinit can't load).
remove_legacy() {
	rm -f "$boot_dir/slintcon@tty$1" "$dinit_dir/slintcon@tty$1" \
		"$boot_dir/slintcon-tty$1" "$dinit_dir/slintcon-tty$1"
}

if [ "$action" = uninstall ]; then
	for vt in "$@"; do
		dinitctl stop "slintcon@tty$vt" 2>/dev/null || true
		rm -f "$boot_dir/slintcon@tty$vt"
		remove_legacy "$vt"
		echo "removed slintcon@tty$vt"
	done
	# The template is only useful while some instance is enabled.
	for link in "$boot_dir"/slintcon@*; do
		[ -e "$link" ] || continue
		exit 0
	done
	rm -f "$dinit_dir/$template"
	echo "removed the $template template"
	exit 0
fi

root=$(cd "$(dirname "$0")/.." && pwd)
src=${SLINTCON:-$root/target/release/slintcon}
[ -x "$src" ] || {
	echo "no executable at $src (set SLINTCON=...)" >&2
	exit 1
}
install -m 755 "$src" "$bin"

mkdir -p /var/log/dinit "$boot_dir"
cat >"$dinit_dir/$template" <<TEMPLATE
# slintcon login prompt on a VT (scripts/install-dinit.sh).
# Template: the instance argument is the VT name, e.g. slintcon@tty6.
type            = process
command         = $bin $backend --vt \$1 -- /bin/login
restart         = true
restart-delay   = 1.0
smooth-recovery = true
depends-on      = login.target
logfile         = /var/log/dinit/slintcon-\$1.log
TEMPLATE

for vt in "$@"; do
	# `/usr/lib/dinit/agetty` starts a getty@ttyN for every
	# /etc/dinit.d/getty@ttyN it finds in ACTIVE_CONSOLES, and that getty
	# would fight slintcon for this VT at boot.
	if [ -f "$dinit_dir/getty@tty$vt" ]; then
		echo "warning: $dinit_dir/getty@tty$vt exists, agetty also starts a getty" >&2
		echo "warning: on tty$vt at boot; remove it if slintcon should own the VT" >&2
	fi
	remove_legacy "$vt"
	ln -sf "../$template" "$boot_dir/slintcon@tty$vt"
	echo "installed slintcon@tty$vt (starts at boot; now: dinitctl start slintcon@tty$vt)"
done
