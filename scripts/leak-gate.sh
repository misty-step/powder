#!/usr/bin/env bash
# powder-ci-leak-gate: fails CI when a commit introduces a secret shape or an
# operator-topology literal into the tracked tree.
#
# Design note (repo-local script vs. gitleaks-action):
# This gate is a repo-local script rather than a third-party action (e.g.
# zricethezav/gitleaks-action) for three reasons:
#   - The patterns that matter here are Powder-specific (sk_powder_/
#     whsec_powder_ key shapes, our tailnet hostname convention, our card
#     export JSON shape). A generic gitleaks ruleset doesn't know any of
#     this out of the box, so we would still have to hand-author a custom
#     gitleaks TOML -- at which point the action buys nothing but an extra
#     external dependency to pin, trust, and bump.
#   - The card requires an anti-theater self-test (plant one fixture per
#     violation class, assert the detector fires). That's a few lines for a
#     script we own; it's awkward to bolt onto a third-party action's own
#     report format and exit-code contract.
#   - Patterns and the allowlist live in this file, in this repo, reviewed
#     like any other diff -- no separate config-file format to learn or
#     keep in sync with an action's schema.
#
# Modes:
#   scripts/leak-gate.sh --self-test
#       Plant one fixture per violation class (plus a clean-file and an
#       allowlist-marker negative control) in a scratch directory and assert
#       this script's own detector flags exactly what it should. Run this in
#       CI *before* the real scan so a silently-broken detector can't pass a
#       clean-looking build.
#   scripts/leak-gate.sh
#       Scan every git-tracked file for the same violation classes and fail
#       if any is found outside the allowlist below.
#
# Allowlist (inline, documented -- this is the whole allowlist, there is no
# separate config file):
#   - PATH_ALLOWLIST: whole files that are known synthetic test fixtures,
#     e.g. crates/powder-store/src/tests.rs holds long fake "legacy key"
#     literals used to exercise key-migration code paths, not real secrets.
#   - Per-line marker: any line containing the literal text "leak-gate:allow"
#     (put it in a code comment) is skipped regardless of path or pattern.
#     Use this for one-off SKILL.md examples or law fixtures that need a
#     realistic-looking literal without tripping the gate. This marker is a
#     trust boundary: a contributor could suppress a real secret with it, so
#     it depends on code review noticing a marker attached to a live
#     credential -- the same accepted tradeoff as gitleaks' inline allows.
#   - Exported-instance-data detection is skipped for paths that look like
#     test fixtures (tests/, test/, fixtures/, *_test.*) since synthetic
#     fixtures legitimately reuse the field names of a real export. For all
#     other paths it requires actual JSON-key shapes (`"card_id":` etc.)
#     co-occurring within a 5-line window, so source that merely names the
#     fields (column lists, serializers, this script) never matches.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SK_POWDER_RE='sk_powder_[A-Za-z0-9_-]{20,}'
WHSEC_POWDER_RE='whsec_powder_[A-Za-z0-9_-]{20,}'
BEARER_RE='[Bb]earer[[:space:]]+[A-Za-z0-9_.-]{24,}'
API_KEY_RE='[Aa][Pp][Ii][_-]?[Kk][Ee][Yy][[:space:]"'"'"']*[:=][[:space:]"'"'"']*[A-Za-z0-9_-]{24,}'
TAILNET_RE='[a-z0-9-]+\.tail[a-z0-9]+\.ts\.net'

# Exported-instance-data JSON-key shapes. The key names are deliberately
# split across adjacent string literals so this script's own source cannot
# satisfy its own detector (which bit us once: the self-test fixtures used
# to embed the contiguous names, and the gate flagged itself the moment it
# was git-tracked).
EXPORT_CARD_ID_RE='"card_''id" *:'
EXPORT_CLAIM_AGENT_RE='"claim_''agent" *:'
EXPORT_CREATED_AT_RE='"created_''at" *:'
# All three keys must land within this many consecutive lines to count as
# one exported-card object rather than a coincidence across a large file.
EXPORT_WINDOW_LINES=5

ALLOW_MARKER='leak-gate:allow'

# Whole-file allowlist: paths (relative to repo root) known to hold
# secret-shaped literals for legitimate, synthetic test purposes.
PATH_ALLOWLIST=(
  "crates/powder-store/src/tests.rs"
)

path_allowed() {
  local path="$1" allowed
  for allowed in "${PATH_ALLOWLIST[@]}"; do
    [[ "$path" == "$allowed" ]] && return 0
  done
  return 1
}

is_test_fixture_path() {
  case "$1" in
    */tests/*|*/test/*|test/*|*_test.*|*/fixtures/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Prints one "path:lineno:class" line per finding for the file at $2
# (display name $1); prints nothing when clean.
scan_content() {
  local display_path="$1" file="$2"
  path_allowed "$display_path" && return 0

  local class pattern lineno rest
  while IFS='|' read -r class pattern; do
    while IFS=: read -r lineno rest; do
      [[ -z "$lineno" ]] && continue
      [[ "$rest" == *"$ALLOW_MARKER"* ]] && continue
      printf '%s:%s:%s\n' "$display_path" "$lineno" "$class"
    done < <(grep -nEI "$pattern" "$file" 2>/dev/null || true)
  done <<PATTERNS
sk_powder_key|$SK_POWDER_RE
whsec_powder_key|$WHSEC_POWDER_RE
bearer_literal|$BEARER_RE
api_key_literal|$API_KEY_RE
tailnet_hostname|$TAILNET_RE
PATTERNS

  # Exported instance-data shape: a non-fixture file where all three keys
  # appear as real JSON keys (quoted name followed by a colon) within a
  # small line window looks like a raw export of live board data. Requiring
  # the colon and the window keeps source that merely names the fields --
  # SQL column lists, serializer code, this script -- from matching.
  if ! is_test_fixture_path "$display_path"; then
    local window_line
    window_line="$(awk \
      -v p1="$EXPORT_CARD_ID_RE" \
      -v p2="$EXPORT_CLAIM_AGENT_RE" \
      -v p3="$EXPORT_CREATED_AT_RE" \
      -v window="$EXPORT_WINDOW_LINES" '
        $0 ~ p1 { a = NR }
        $0 ~ p2 { b = NR }
        $0 ~ p3 { c = NR }
        a && b && c {
          max = a > b ? a : b; if (c > max) max = c
          min = a < b ? a : b; if (c < min) min = c
          if (max - min < window) { print min; exit }
        }
      ' "$file" 2>/dev/null || true)"
    if [[ -n "$window_line" ]]; then
      printf '%s:%s:exported_card_data\n' "$display_path" "$window_line"
    fi
  fi
}

assert_flagged() {
  local file="$1" expect_class="$2" result
  result="$(scan_content "$file" "$file")"
  if [[ -z "$result" ]]; then
    echo "self-test FAIL: $expect_class not detected in $file" >&2
    return 1
  fi
  if ! grep -q ":${expect_class}\$" <<<"$result"; then
    echo "self-test FAIL: $expect_class detector fired with wrong class: $result" >&2
    return 1
  fi
  return 0
}

assert_clean() {
  local file="$1" label="$2" result
  result="$(scan_content "$file" "$file")"
  if [[ -n "$result" ]]; then
    echo "self-test FAIL: $label unexpectedly flagged: $result" >&2
    return 1
  fi
  return 0
}

self_test() {
  local tmp failures=0
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  # Split like the EXPORT_*_RE definitions, and for the same reason: the
  # fixture generator must not plant matchable literals in this script.
  local key_card='card_''id' key_agent='claim_''agent' key_created='created_''at'

  # 1. Powder API key shape.
  printf 'let leaked = "sk_powder_%s";\n' "$(printf 'a%.0s' $(seq 1 24))" \
    >"$tmp/leaked_key.rs"
  assert_flagged "$tmp/leaked_key.rs" "sk_powder_key" || failures=$((failures + 1))

  # 2. Powder webhook signing-secret shape.
  printf 'let leaked = "whsec_powder_%s";\n' "$(printf 'b%.0s' $(seq 1 24))" \
    >"$tmp/leaked_whsec.rs"
  assert_flagged "$tmp/leaked_whsec.rs" "whsec_powder_key" || failures=$((failures + 1))

  # 3. Generic bearer-token literal.
  printf 'Authorization: Bearer %s\n' "$(printf 'c%.0s' $(seq 1 32))" \
    >"$tmp/leaked_bearer.txt"
  assert_flagged "$tmp/leaked_bearer.txt" "bearer_literal" || failures=$((failures + 1))

  # 4. Generic api-key literal.
  printf 'API_KEY=%s\n' "$(printf 'd%.0s' $(seq 1 32))" \
    >"$tmp/leaked_apikey.env"
  assert_flagged "$tmp/leaked_apikey.env" "api_key_literal" || failures=$((failures + 1))

  # 5. Tailnet hostname literal.
  printf 'TARGET="https://%s.tail%s.ts.net:10001"\n' "opshost" "abcd1234" \
    >"$tmp/leaked_host.sh"
  assert_flagged "$tmp/leaked_host.sh" "tailnet_hostname" || failures=$((failures + 1))

  # 6. Exported instance-data JSON shape (keys with colons, adjacent lines),
  # outside any fixture path.
  printf '{\n  "%s": "c1",\n  "%s": "agent",\n  "%s": 1\n}\n' \
    "$key_card" "$key_agent" "$key_created" >"$tmp/export.json"
  assert_flagged "$tmp/export.json" "exported_card_data" || failures=$((failures + 1))

  # Negative control: a clean file must not be flagged.
  printf 'fn main() { println!("hello"); }\n' >"$tmp/clean.rs"
  assert_clean "$tmp/clean.rs" "clean fixture (false positive)" || failures=$((failures + 1))

  # Negative control: the same exported-data shape under a fixture-style
  # path must not be flagged (mirrors crates/powder-store/tests/fixtures/).
  mkdir -p "$tmp/tests/fixtures"
  printf '{\n  "%s": "c1",\n  "%s": "agent",\n  "%s": 1\n}\n' \
    "$key_card" "$key_agent" "$key_created" >"$tmp/tests/fixtures/export.json"
  assert_clean "$tmp/tests/fixtures/export.json" "fixture-path export (false positive)" \
    || failures=$((failures + 1))

  # Negative control: the field names without JSON-key colons (a Rust/SQL
  # column list) must not match.
  printf 'let columns = ["%s", "%s", "%s"];\n' \
    "$key_card" "$key_agent" "$key_created" >"$tmp/column_list.rs"
  assert_clean "$tmp/column_list.rs" "column-list mention (false positive)" \
    || failures=$((failures + 1))

  # Negative control: real JSON keys scattered further apart than the
  # window are a coincidence across a big file, not one export object.
  {
    printf '"%s": "c1",\n' "$key_card"
    printf 'x\nx\nx\nx\nx\n'
    printf '"%s": "agent",\n' "$key_agent"
    printf 'x\nx\nx\nx\nx\n'
    printf '"%s": 1\n' "$key_created"
  } >"$tmp/scattered.txt"
  assert_clean "$tmp/scattered.txt" "scattered keys outside window (false positive)" \
    || failures=$((failures + 1))

  # Negative control: the leak-gate:allow marker suppresses a real match.
  printf 'let leaked = "sk_powder_%s"; // leak-gate:allow synthetic placeholder\n' \
    "$(printf 'e%.0s' $(seq 1 24))" >"$tmp/allowed.rs"
  assert_clean "$tmp/allowed.rs" "leak-gate:allow marker" || failures=$((failures + 1))

  if [[ "$failures" -gt 0 ]]; then
    echo "leak-gate self-test: $failures assertion(s) failed" >&2
    return 1
  fi
  echo "leak-gate self-test: all violation classes detected, no false positives"
  return 0
}

real_scan() {
  local file result violations=0 scanned=0
  while IFS= read -r -d '' file; do
    [[ -f "$file" ]] || continue
    scanned=$((scanned + 1))
    result="$(scan_content "$file" "$file")"
    if [[ -n "$result" ]]; then
      printf '%s\n' "$result"
      violations=$((violations + $(printf '%s\n' "$result" | wc -l)))
    fi
  done < <(git ls-files -z)

  if [[ "$violations" -gt 0 ]]; then
    echo "leak-gate: $violations violation(s) found across tracked files" >&2
    return 1
  fi
  echo "leak-gate: clean ($scanned tracked files scanned)"
  return 0
}

case "${1:-}" in
  --self-test)
    self_test
    ;;
  "")
    real_scan
    ;;
  *)
    echo "usage: $0 [--self-test]" >&2
    exit 2
    ;;
esac
