#!/bin/sh
# Install a linux/amd64 powder binary over the live Sanctum supervisor child
# and wait until /healthz and `powder version` match.
set -eu

usage() {
	echo "usage: powder-deploy.sh <binary> <version>" >&2
	exit 2
}

[ $# -eq 2 ] || usage
src=$1
version=$2
[ -n "$src" ] && [ -n "$version" ] || usage
[ -f "$src" ] && [ -x "$src" ] || {
	echo "binary must be an executable file" >&2
	exit 2
}

dest=${POWDER_BIN:-/usr/local/bin/powder}
health=${POWDER_HEALTH_URL:-http://127.0.0.1:4175/healthz}
ready=${POWDER_READY_URL:-http://127.0.0.1:4175/readyz}
stamp=$(echo "$version" | tr -c 'A-Za-z0-9._-' '_')
backup="${dest}.prev.${stamp}"

if ! file -b "$src" | grep -q 'ELF 64-bit'; then
	echo "binary is not a 64-bit ELF" >&2
	exit 2
fi

install -m 0755 "$src" "${dest}.new"
if [ -e "$dest" ]; then
	cp -f "$dest" "$backup"
fi
mv -f "${dest}.new" "$dest"

pid=$(pgrep -x -f '/usr/local/bin/powder serve' || true)
if [ -n "$pid" ]; then
	kill -TERM "$pid" || true
fi

i=0
while [ "$i" -lt 50 ]; do
	if curl -fsS "$health" >/dev/null 2>&1 && curl -fsS "$ready" >/dev/null 2>&1; then
		got=$("$dest" version)
		case "$got" in
		*"$version"*)
			echo "deployed $got"
			exit 0
			;;
		esac
	fi
	i=$((i + 1))
	sleep 0.2
done

echo "deploy failed: health or version mismatch" >&2
if [ -f "$backup" ]; then
	mv -f "$backup" "$dest"
	pid=$(pgrep -x -f '/usr/local/bin/powder serve' || true)
	if [ -n "$pid" ]; then
		kill -TERM "$pid" || true
	fi
	echo "restored $backup" >&2
fi
exit 1
