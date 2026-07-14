#!/usr/bin/env bash
# powder-workstation-cli-convergence: converges the operator's workstation
# executables (~/.cargo/bin/powder, ~/.cargo/bin/powder-mcp, and optionally
# ~/.cargo/bin/powder-server) with the current checkout.
#
# The incident this closes: the workstation `powder` binary sat at 0.1.0
# git 1d1ded8 while the checkout had moved to 414ac7f, silently missing a
# merged fix to repeated `--acceptance` flags -- a live card lost four
# criteria before anyone noticed. The server was fine; nothing on the
# workstation side ever surfaced that the local executable was stale, and
# there was no single repo-owned command to bring it back in sync. This is
# that command.
#
# Usage:
#   scripts/install-workstation.sh [--allow-dirty] [--with-server] [--verify]
#
#   --allow-dirty  Install from a dirty working tree. Refused by default: an
#                   uncommitted local change silently baked into
#                   ~/.cargo/bin is exactly the kind of drift this script
#                   exists to eliminate, so it must be opted into, not
#                   accidental.
#   --with-server  Also install powder-server (skipped by default -- most
#                   workstations never run it; production runs it as a
#                   separately supervised process on its own host, not via
#                   this script).
#   --verify       After installing, exercise the just-installed `powder`
#                   binary's repeated `--acceptance` handling against a
#                   throwaway temp database and fail loudly if a criterion
#                   goes missing -- the exact reproduction of the incident
#                   above, run through the binary a lane would actually
#                   invoke, not just `cargo test` inside the checkout.
#
# On a checkout whose HEAD is exactly an annotated release tag (`git
# describe --tags --exact-match`), this downloads and verifies the matching
# published release tarball (see .github/workflows/release.yml) instead of
# building from source, and falls back to a source build with a notice if
# no published asset matches the local platform. Idempotent: safe to run
# repeatedly, including with nothing to update.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

GITHUB_REPO="misty-step/powder"
INSTALL_DIR="${CARGO_INSTALL_ROOT:-$HOME/.cargo}/bin"
CURL_BIN="${POWDER_INSTALL_CURL_BIN:-curl}"

ALLOW_DIRTY=0
WITH_SERVER=0
VERIFY=0

for arg in "$@"; do
  case "$arg" in
    --allow-dirty) ALLOW_DIRTY=1 ;;
    --with-server) WITH_SERVER=1 ;;
    --verify) VERIFY=1 ;;
    --help|-h)
      sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      printf 'install-workstation: unknown flag: %s (see --help)\n' "$arg" >&2
      exit 2
      ;;
  esac
done

if [[ "$ALLOW_DIRTY" != "1" ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    printf 'install-workstation: refusing to install from a dirty working tree (use --allow-dirty to override)\n' >&2
    git status --porcelain >&2
    exit 1
  fi
fi

report_version() {
  local label="$1" bin="$2"
  if [[ -x "$bin" ]]; then
    local output
    output="$("$bin" version 2>/dev/null || true)"
    if [[ -n "$output" ]]; then
      printf '  %s\n' "$output"
    else
      printf '  %s: installed at %s but does not answer `version`\n' "$label" "$bin"
    fi
  else
    printf '  %s: not installed\n' "$label"
  fi
}

echo "before:"
report_version powder "$INSTALL_DIR/powder"
report_version powder-mcp "$INSTALL_DIR/powder-mcp"
if [[ "$WITH_SERVER" == "1" ]]; then
  report_version powder-server "$INSTALL_DIR/powder-server"
fi

HEAD_SHA="$(git rev-parse --short=12 HEAD)"
TAG="$(git describe --tags --exact-match HEAD 2>/dev/null || true)"

platform_triple() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os-$arch" in
    Darwin-arm64) printf 'aarch64-apple-darwin' ;;
    Linux-x86_64) printf 'x86_64-unknown-linux-gnu' ;;
    Linux-aarch64|Linux-arm64) printf 'aarch64-unknown-linux-gnu' ;;
    *) return 1 ;;
  esac
}

install_from_source() {
  echo "installing from source at $HEAD_SHA..."
  local crates=(powder-cli powder-mcp)
  [[ "$WITH_SERVER" == "1" ]] && crates+=(powder-server)
  for crate in "${crates[@]}"; do
    # --force: the workspace crate version stays "0.1.0" across commits (it
    # is not bumped per-release), so cargo's own "already installed, use
    # --force" version check would otherwise make every install after the
    # first a silent no-op -- exactly the staleness this script exists to
    # eliminate. --locked: build exactly what CI built, from this repo's
    # own committed Cargo.lock. --target-dir: share this workspace's normal
    # target/ across all crates this loop installs, instead of `cargo
    # install`'s own default of an isolated build dir per invocation --
    # cuts a 3-binary install from three full dependency-graph compiles
    # (powder-core, tokio, rustls, sqlite... each duplicated three times)
    # down to one.
    cargo install --path "crates/$crate" --locked --force --target-dir "$ROOT/target"
  done
}

install_from_release() {
  local tag="$1" triple
  if ! triple="$(platform_triple)"; then
    printf 'install-workstation: no published release asset for this platform (%s %s); building from source instead\n' \
      "$(uname -s)" "$(uname -m)" >&2
    install_from_source
    return
  fi

  echo "checkout is on tag $tag -- installing the published $triple release tarball..."
  local tmp tarball
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  tarball="powder-$triple.tar.gz"
  local base_url="https://github.com/$GITHUB_REPO/releases/download/$tag"

  if ! "$CURL_BIN" --fail --location --silent --show-error \
      -o "$tmp/$tarball" "$base_url/$tarball"; then
    printf 'install-workstation: no release asset %s for tag %s; building from source instead\n' \
      "$tarball" "$tag" >&2
    install_from_source
    return
  fi
  "$CURL_BIN" --fail --location --silent --show-error \
    -o "$tmp/$tarball.sha256" "$base_url/$tarball.sha256"

  (cd "$tmp" && shasum -a 256 -c "$tarball.sha256") ||
    { printf 'install-workstation: checksum mismatch for %s, refusing to install\n' "$tarball" >&2; exit 1; }

  tar -C "$tmp" -xzf "$tmp/$tarball"
  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$tmp/powder" "$INSTALL_DIR/powder"
  install -m 0755 "$tmp/powder-mcp" "$INSTALL_DIR/powder-mcp"
  if [[ "$WITH_SERVER" == "1" ]]; then
    install -m 0755 "$tmp/powder-server" "$INSTALL_DIR/powder-server"
  fi
}

if [[ -n "$TAG" ]]; then
  install_from_release "$TAG"
else
  echo "checkout is not on a release tag -- installing from source"
  install_from_source
fi

echo "after:"
report_version powder "$INSTALL_DIR/powder"
report_version powder-mcp "$INSTALL_DIR/powder-mcp"
if [[ "$WITH_SERVER" == "1" ]]; then
  report_version powder-server "$INSTALL_DIR/powder-server"
fi

if [[ "$VERIFY" == "1" ]]; then
  echo "verify: exercising the installed binary's repeated --acceptance handling..."
  vtmp="$(mktemp -d)"
  trap 'rm -rf "$vtmp"' EXIT
  vdb="$vtmp/verify-acceptance.db"
  vbin="$INSTALL_DIR/powder"

  "$vbin" init-db --db "$vdb" --show-secret >/dev/null
  "$vbin" create-card --db "$vdb" --id install-workstation-verify --title "install-workstation --verify" \
    --acceptance "first criterion survives" --acceptance "second criterion survives" >/dev/null

  detail="$("$vbin" get-card install-workstation-verify --db "$vdb")"
  missing=0
  for needle in "first criterion survives" "second criterion survives"; do
    if ! grep -Fq "$needle" <<<"$detail"; then
      printf 'install-workstation --verify: FAILED -- missing criterion "%s"\n' "$needle" >&2
      missing=1
    fi
  done
  if [[ "$missing" != "0" ]]; then
    printf '%s\n' "$detail" >&2
    printf 'install-workstation --verify: the installed binary dropped a repeated --acceptance criterion (the powder-cli-repeated-acceptance regression) -- do not trust this install\n' >&2
    exit 1
  fi
  echo "verify: OK -- both repeated --acceptance criteria persisted through the installed binary"
fi
