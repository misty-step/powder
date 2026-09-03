#!/bin/sh
set -eu

[ "$#" -eq 0 ] || {
	printf 'usage: %s\n' "$0" >&2
	exit 2
}

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
home=${HOME:?HOME must be set}
bin_dir=$home/.local/bin
out=$bin_dir/powder
sha=$(git -C "$root" rev-parse --verify HEAD)
if [ -n "$(git -C "$root" status --porcelain --untracked-files=normal)" ]; then
	sha=$sha-dirty
fi

mkdir -p "$bin_dir"
tmp=$(mktemp "$bin_dir/.powder.XXXXXX")
cleanup() {
	rm -f "$tmp"
}
trap cleanup 0 1 2 15

(
	cd "$root"
	go build -trimpath -ldflags "-X main.buildSHA=$sha" -o "$tmp" .
)
chmod 0755 "$tmp"

expected="powder $sha"
version=$("$tmp" version)
if [ "$version" != "$expected" ]; then
	printf 'version mismatch: got %s, want %s\n' "$version" "$expected" >&2
	exit 1
fi

"$root/scripts/install-skill.sh"
mv -f "$tmp" "$out"
trap - 0 1 2 15

legacy=$home/.cargo/bin/powder
if [ -e "$legacy" ]; then
	if cmp -s "$legacy" "$out"; then
		rm -f "$legacy"
		printf 'removed byte-identical legacy mirror %s\n' "$legacy"
	else
		printf 'warning: legacy path %s differs from %s; inspect and remove it if obsolete\n' \
			"$legacy" "$out" >&2
	fi
fi

resolved=$(command -v powder 2>/dev/null || true)
if [ "$resolved" != "$out" ]; then
	printf 'warning: powder resolves to %s, not %s; put %s first on PATH\n' \
		"${resolved:-nothing}" "$out" "$bin_dir" >&2
else
	printf '%s\n' "$version"
fi

doctor=$("$out" doctor 2>/dev/null || true)
case "$doctor" in
	*'"origin":'*)
		if "$out" list --takeable --limit 1 >/dev/null 2>&1; then
			printf '%s\n' 'remote smoke passed (read-only list)'
		else
			printf '%s\n' 'warning: remote smoke failed; local install is still complete' >&2
		fi
		;;
esac
