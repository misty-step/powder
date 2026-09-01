#!/bin/sh
set -eu
# Register the repository skill through OMP workspace discovery.
#
# A symlink in `~/.agents/skills` makes `skill://powder` load from any
# working directory, including Misty Step subrepos that otherwise stop
# discovery at their own git root. The repo SKILL.md stays the source
# of truth; this file only points at it.

DEST=${1:-"$HOME/.agents/skills/powder"}
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_DIR=$(dirname "$SCRIPT_DIR")

[ -f "$REPO_DIR/SKILL.md" ] || {
	printf 'missing skill source: %s/SKILL.md\n' "$REPO_DIR" >&2
	exit 1
}

mkdir -p "$(dirname "$DEST")"

if [ -L "$DEST" ]; then
	rm -f "$DEST"
elif [ -e "$DEST" ]; then
	printf 'refusing to replace existing non-symlink: %s\n' "$DEST" >&2
	exit 1
fi

ln -s "$REPO_DIR" "$DEST"
printf 'registered %s -> %s\n' "$DEST" "$REPO_DIR"
