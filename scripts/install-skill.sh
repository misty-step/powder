#!/bin/sh
set -eu
umask 077

# Register a confined copy of the repository skill through OMP workspace
# discovery. The destination contains only explicit skill assets and installer
# ownership metadata; repository files and secrets never become skill paths.

DEST=${1:-"$HOME/.agents/skills/powder"}
while [ "$DEST" != "/" ] && [ "${DEST%/}" != "$DEST" ]; do
	DEST=${DEST%/}
done
case "$DEST" in
	"" | "/")
		printf 'refusing unsafe destination: %s\n' "$DEST" >&2
		exit 1
		;;
esac

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_DIR=$(dirname "$SCRIPT_DIR")
REPO_REAL=$(realpath "$REPO_DIR")
MARKER="$DEST.registration"

[ -f "$REPO_DIR/SKILL.md" ] || {
	printf 'missing skill source: %s/SKILL.md\n' "$REPO_DIR" >&2
	exit 1
}

PARENT=$(dirname "$DEST")
mkdir -p "$PARENT"
STAGE=$(mktemp -d "$PARENT/.powder-skill.XXXXXX")
STAGE_MARKER=$(mktemp "$PARENT/.powder-skill-registration.XXXXXX")
trap 'rm -rf "$STAGE"; rm -f "$STAGE_MARKER"' 0 1 2 15
install -m 600 "$REPO_DIR/SKILL.md" "$STAGE/SKILL.md"
printf '%s\n' "$REPO_REAL" >"$STAGE_MARKER"

if [ -L "$DEST" ]; then
	DEST_REAL=$(realpath "$DEST" 2>/dev/null || true)
	if [ "$DEST_REAL" != "$REPO_REAL" ] || [ -e "$MARKER" ]; then
		printf 'refusing to replace unrelated symlink: %s\n' "$DEST" >&2
		exit 1
	fi
	rm -f "$DEST"
elif [ -e "$DEST" ]; then
	if [ ! -d "$DEST" ] ||
		[ ! -f "$DEST/SKILL.md" ] ||
		[ ! -f "$MARKER" ] ||
		[ "$(cat "$MARKER")" != "$REPO_REAL" ] ||
		[ "$(find "$DEST" -mindepth 1 -maxdepth 1 | wc -l)" -ne 1 ]; then
		printf 'refusing to replace unmanaged destination: %s\n' "$DEST" >&2
		exit 1
	fi
	rm -rf "$DEST"
elif [ -e "$MARKER" ]; then
	printf 'refusing orphaned registration marker: %s\n' "$MARKER" >&2
	exit 1
fi

mv "$STAGE" "$DEST"
mv "$STAGE_MARKER" "$MARKER"
trap - 0 1 2 15
printf 'registered confined skill at %s\n' "$DEST"
