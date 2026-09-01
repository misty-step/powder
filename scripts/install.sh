#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
out=${1:-$HOME/.local/bin/powder}
mkdir -p "$(dirname "$out")"
(cd "$root" && go build -o "$out" .)
if [ -d "$HOME/.cargo/bin" ]; then
  cp "$out" "$HOME/.cargo/bin/powder"
fi
"$out" version
"$root/scripts/install-skill.sh"
