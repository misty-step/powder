#!/usr/bin/env bash
# powder-workstation-cli-convergence: exercises scripts/install-workstation.sh
# end to end with fake `git`/`cargo`/`curl` (a real cargo build here would
# take minutes and needs network for the release-tarball path -- exactly
# what CI cannot afford per PR). Every fake is a thin, inspectable script,
# not a mock framework, matching this repo's existing style
# (test/powder-remote-doctor.sh's fake curl/powder).
#
# The script under test always resolves its own repo root from
# `${BASH_SOURCE[0]}`'s location, so each scenario runs a *copy* of it
# inside a disposable fake repo directory -- never the real checkout --
# with fake `git`/`cargo`/`curl` first on PATH, so no scenario here ever
# touches this checkout's real git state, builds a real binary, or hits the
# network.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REAL_SCRIPT="$ROOT/scripts/install-workstation.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

FAKEREPO="$TMP/reporoot"
FAKES="$TMP/fakebin"
mkdir -p "$FAKEREPO/scripts" "$FAKEREPO/crates/powder-cli" "$FAKEREPO/crates/powder-mcp" \
  "$FAKEREPO/crates/powder-server" "$FAKES"
cp "$REAL_SCRIPT" "$FAKEREPO/scripts/install-workstation.sh"
chmod +x "$FAKEREPO/scripts/install-workstation.sh"

# Fake `powder`/`powder-mcp`/`powder-server`: cargo (below) installs a copy
# of this under the right binary name, so `version` and the `--verify`
# create-card/get-card round trip both work without a real store.
# POWDER_TEST_SIMULATE_BUG=1 reproduces the historical
# powder-cli-repeated-acceptance regression on purpose, to prove `--verify`
# actually catches it rather than rubber-stamping.
TEMPLATE="$TMP/fake-binary-template.sh"
cat >"$TEMPLATE" <<'SH'
#!/usr/bin/env bash
name="$(basename "$0")"
verb="${1:-}"
db=""
prev=""
for a in "$@"; do
  if [[ "$prev" == "--db" ]]; then db="$a"; fi
  prev="$a"
done

case "$verb" in
  version|--version|-v)
    printf '%s 0.1.0 (git %s)\n' "$name" "${POWDER_TEST_INSTALLED_SHA:-abcdefabcdef}"
    ;;
  init-db)
    [[ -n "$db" ]] && : >"$db"
    printf 'bootstrap-key\tname\tscope\traw\tsk_powder_test\n'
    ;;
  create-card)
    : # no-op; init-db already created the (fake) db file above
    ;;
  get-card)
    if [[ "${POWDER_TEST_SIMULATE_BUG:-0}" == "1" ]]; then
      printf '{"card":{"id":"install-workstation-verify","criteria":[{"text":"first criterion survives"}]}}\n'
    else
      printf '{"card":{"id":"install-workstation-verify","criteria":[{"text":"first criterion survives"},{"text":"second criterion survives"}]}}\n'
    fi
    ;;
  *)
    exit 0
    ;;
esac
SH
chmod +x "$TEMPLATE"

# Fake `git`: `status --porcelain` reports dirty iff `.dirty-marker` exists
# in the CWD (install-workstation.sh always `cd`s to its own repo root
# first); `rev-parse`/`describe` are driven by env vars the test sets per
# scenario, never real git state.
cat >"$FAKES/git" <<'SH'
#!/usr/bin/env bash
if [[ "$1" == "status" && "$2" == "--porcelain" ]]; then
  [[ -f .dirty-marker ]] && printf ' M some-file\n'
  exit 0
fi
if [[ "$1" == "rev-parse" ]]; then
  printf '%s\n' "${POWDER_TEST_HEAD_SHA:-deadbeefcafe}"
  exit 0
fi
if [[ "$1" == "describe" ]]; then
  if [[ -n "${POWDER_TEST_TAG:-}" ]]; then
    printf '%s\n' "$POWDER_TEST_TAG"
    exit 0
  fi
  exit 1
fi
exit 0
SH
chmod +x "$FAKES/git"

# Fake `cargo`: `install --path crates/<crate> --locked --force` drops a
# copy of $POWDER_TEST_FAKE_BIN_TEMPLATE at the right binary name (mapping
# crate `powder-cli` -> binary `powder`, same as the real workspace) under
# the same install root real cargo would pick -- CARGO_INSTALL_ROOT, then
# CARGO_HOME, then ~/.cargo -- and appends one line per call to
# $POWDER_TEST_CARGO_LOG so scenarios can assert exactly which crates a
# given run actually built. POWDER_TEST_CARGO_FAIL_CRATE makes the install
# of that one crate fail, to exercise mid-loop failure handling.
cat >"$FAKES/cargo" <<'SH'
#!/usr/bin/env bash
if [[ "$1" == "install" ]]; then
  shift
  path=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --path) path="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  crate="$(basename "$path")"
  if [[ "$crate" == "${POWDER_TEST_CARGO_FAIL_CRATE:-}" ]]; then
    printf 'error: fake build failure for %s\n' "$crate" >&2
    exit 101
  fi
  case "$crate" in
    powder-cli) bin_name="powder" ;;
    *) bin_name="$crate" ;;
  esac
  install_dir="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
  mkdir -p "$install_dir"
  cp "$POWDER_TEST_FAKE_BIN_TEMPLATE" "$install_dir/$bin_name"
  chmod +x "$install_dir/$bin_name"
  [[ -n "${POWDER_TEST_CARGO_LOG:-}" ]] && printf 'install --path %s\n' "$path" >>"$POWDER_TEST_CARGO_LOG"
  exit 0
fi
exit 0
SH
chmod +x "$FAKES/cargo"

# Fake `curl`: serves `$POWDER_TEST_RELEASE_DIR/<basename of the URL>` for
# any `-o <dest> <url>` call (install-workstation.sh's only shape), and
# fails like a real 404 (exit 22, curl's own --fail exit code) when that
# asset does not exist -- driving the same "no release asset, build from
# source" fallback a real 404 would.
cat >"$FAKES/curl" <<'SH'
#!/usr/bin/env bash
dest=""
url=""
prev=""
for a in "$@"; do
  if [[ "$prev" == "-o" ]]; then dest="$a"; fi
  prev="$a"
  url="$a"
done
base="$(basename "$url")"
src="${POWDER_TEST_RELEASE_DIR:-}/$base"
if [[ -n "${POWDER_TEST_RELEASE_DIR:-}" && -f "$src" ]]; then
  cp "$src" "$dest"
  exit 0
fi
exit 22
SH
chmod +x "$FAKES/curl"

run_install() {
  # Args up to a literal "--" are VAR=val env overrides; everything after
  # is passed through to install-workstation.sh as its own CLI flags.
  local envs=()
  while [[ "$1" != "--" ]]; do
    envs+=("$1")
    shift
  done
  shift
  env -i \
    HOME="$TMP" \
    PATH="$FAKES:/usr/bin:/bin" \
    POWDER_TEST_FAKE_BIN_TEMPLATE="$TEMPLATE" \
    "${envs[@]}" \
    "$FAKEREPO/scripts/install-workstation.sh" "$@"
}

# 1. Refuses a dirty tree by default.
touch "$FAKEREPO/.dirty-marker"
if run_install CARGO_INSTALL_ROOT="$TMP/root-dirty-refused" -- >"$TMP/dirty-refused.out" 2>&1; then
  echo "expected a dirty tree to be refused without --allow-dirty" >&2
  exit 1
fi
grep -qi 'dirty' "$TMP/dirty-refused.out"

# 2. --allow-dirty proceeds anyway.
cargo_log="$TMP/cargo-dirty-allowed.log"
: >"$cargo_log"
run_install CARGO_INSTALL_ROOT="$TMP/root-dirty-allowed" POWDER_TEST_CARGO_LOG="$cargo_log" \
  -- --allow-dirty >"$TMP/dirty-allowed.out"
grep -q 'crates/powder-cli' "$cargo_log"
rm -f "$FAKEREPO/.dirty-marker"

# 3. Clean tree, no tag, default flags: installs powder + powder-mcp, not
# powder-server; prints before/after.
cargo_log="$TMP/cargo-default.log"
: >"$cargo_log"
out="$(run_install CARGO_INSTALL_ROOT="$TMP/root-default" POWDER_TEST_CARGO_LOG="$cargo_log" --)"
grep -q 'crates/powder-cli' "$cargo_log"
grep -q 'crates/powder-mcp' "$cargo_log"
if grep -q 'crates/powder-server' "$cargo_log"; then
  echo "must not install powder-server without --with-server" >&2
  exit 1
fi
grep -q '^before:$' <<<"$out"
grep -q '^after:$' <<<"$out"
grep -q 'powder 0.1.0 (git' <<<"$out"
grep -q 'powder-mcp 0.1.0 (git' <<<"$out"

# 4. --with-server also installs powder-server.
cargo_log="$TMP/cargo-with-server.log"
: >"$cargo_log"
run_install CARGO_INSTALL_ROOT="$TMP/root-with-server" POWDER_TEST_CARGO_LOG="$cargo_log" \
  -- --with-server >/dev/null
grep -q 'crates/powder-server' "$cargo_log"

# 5. --verify passes when both repeated --acceptance criteria persist.
out="$(run_install CARGO_INSTALL_ROOT="$TMP/root-verify-ok" -- --verify)"
grep -q 'verify: OK' <<<"$out"

# 6. --verify fails loudly when the installed binary reproduces the
# powder-cli-repeated-acceptance regression -- the anti-theater check: if
# this ever stops failing, --verify itself is not exercising anything.
if run_install CARGO_INSTALL_ROOT="$TMP/root-verify-catches-bug" POWDER_TEST_SIMULATE_BUG=1 \
  -- --verify >"$TMP/verify-bug.out" 2>&1; then
  echo "expected --verify to fail against a binary that drops a repeated --acceptance criterion" >&2
  cat "$TMP/verify-bug.out" >&2
  exit 1
fi
grep -q 'second criterion survives' "$TMP/verify-bug.out"

# 7. On a tag, with no matching release asset published, falls back to a
# source build instead of failing.
cargo_log="$TMP/cargo-tag-fallback.log"
: >"$cargo_log"
out="$(run_install CARGO_INSTALL_ROOT="$TMP/root-tag-fallback" POWDER_TEST_CARGO_LOG="$cargo_log" \
  POWDER_TEST_TAG=v9.9.9 -- 2>&1)"
grep -q 'crates/powder-cli' "$cargo_log"
grep -qi 'building from source' <<<"$out"

# 8. On a tag with a matching, checksummed release asset for this host's
# own platform, installs from the tarball instead of building -- skipped
# (not failed) on a platform release.yml does not publish for (no released
# artifact exists to fake against, e.g. Intel macOS).
os="$(uname -s)"
arch="$(uname -m)"
triple=""
case "$os-$arch" in
  Darwin-arm64) triple="aarch64-apple-darwin" ;;
  Linux-x86_64) triple="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64|Linux-arm64) triple="aarch64-unknown-linux-gnu" ;;
esac
if [[ -n "$triple" ]]; then
  reldir="$TMP/release-assets"
  stage="$reldir/stage"
  mkdir -p "$stage"
  cp "$TEMPLATE" "$stage/powder"
  cp "$TEMPLATE" "$stage/powder-mcp"
  cp "$TEMPLATE" "$stage/powder-server"
  chmod +x "$stage"/*
  tarball="powder-$triple.tar.gz"
  (cd "$stage" && tar -czf "$reldir/$tarball" powder powder-mcp powder-server)
  (cd "$reldir" && shasum -a 256 "$tarball" >"$tarball.sha256")

  cargo_log="$TMP/cargo-tag-release.log"
  : >"$cargo_log"
  out="$(run_install CARGO_INSTALL_ROOT="$TMP/root-tag-release" POWDER_TEST_CARGO_LOG="$cargo_log" \
    POWDER_TEST_TAG=v9.9.9 POWDER_TEST_RELEASE_DIR="$reldir" -- --with-server)"
  if [[ -s "$cargo_log" ]]; then
    echo "expected the release tarball path to skip cargo entirely" >&2
    cat "$cargo_log" >&2
    exit 1
  fi
  grep -qi 'installing the published' <<<"$out"
  [[ -x "$TMP/root-tag-release/bin/powder" ]]
else
  echo "SKIP release-tarball scenario: no published release.yml target for $os-$arch"
fi

# 9. CARGO_HOME (with no CARGO_INSTALL_ROOT) must resolve exactly like
# cargo itself: binaries land in $CARGO_HOME/bin and the after-report and
# --verify must look there too -- the historical bug had cargo installing
# into $CARGO_HOME/bin while the script reported/verified against
# ~/.cargo/bin.
out="$(run_install CARGO_HOME="$TMP/cargo-home" -- --verify)"
grep -q 'verify: OK' <<<"$out"
grep -q 'powder 0.1.0 (git' <<<"$out"
[[ -x "$TMP/cargo-home/bin/powder" ]]
[[ ! -e "$TMP/.cargo/bin/powder" ]]

# 10. CARGO_INSTALL_ROOT beats CARGO_HOME, same as cargo's own precedence.
run_install CARGO_INSTALL_ROOT="$TMP/root-precedence" CARGO_HOME="$TMP/cargo-home-loser" -- >/dev/null
[[ -x "$TMP/root-precedence/bin/powder" ]]
[[ ! -e "$TMP/cargo-home-loser/bin/powder" ]]

# 11. A mid-loop install failure must not die silently between the before
# and after reports: it names the crate that failed and prints the partial
# state, so the operator can see powder was replaced while powder-mcp
# stayed stale.
if run_install CARGO_INSTALL_ROOT="$TMP/root-midfail" POWDER_TEST_CARGO_FAIL_CRATE=powder-mcp \
  -- >"$TMP/midfail.out" 2>&1; then
  echo "expected a mid-loop cargo failure to fail the script" >&2
  exit 1
fi
grep -q 'FAILED installing powder-mcp' "$TMP/midfail.out"
grep -q 'after (partial):' "$TMP/midfail.out"
grep -q 'powder 0.1.0 (git' "$TMP/midfail.out"
grep -q 'powder-mcp: not installed' "$TMP/midfail.out"

# 12. Unknown flags are rejected with a clear message, not silently ignored.
if run_install CARGO_INSTALL_ROOT="$TMP/root-badflag" -- --not-a-real-flag \
  >"$TMP/badflag.out" 2>&1; then
  echo "expected an unrecognized flag to fail" >&2
  exit 1
fi
grep -qi 'unknown flag' "$TMP/badflag.out"

# 13. --help documents itself without doing anything.
out="$(run_install CARGO_INSTALL_ROOT="$TMP/root-help" -- --help)"
grep -qi 'allow-dirty' <<<"$out"
[[ ! -e "$TMP/root-help/bin/powder" ]]

echo "PASS install-workstation tests"
