#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCTOR="$ROOT/bin/powder-remote-doctor.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat >"$TMP/curl" <<'SH'
#!/usr/bin/env bash
if [[ "${POWDER_TEST_CURL_FAIL:-0}" == "1" ]]; then
  exit 7
fi
if [[ "$*" == *'/status'* ]]; then
  if [[ "${POWDER_TEST_AUTH_FAIL:-0}" == "1" ]]; then
    printf '401'
  else
    printf '400'
  fi
  exit 0
fi
printf '{"ok":true}\n'
SH
chmod +x "$TMP/curl"

cat >"$TMP/powder" <<'SH'
#!/usr/bin/env bash
[[ -n "${POWDER_API_KEY:-}" ]] || exit 41
printf '{"card":{"id":"powder-agent-reachability"}}\n'
SH
chmod +x "$TMP/powder"

run_doctor() {
  env -i \
    HOME="$TMP" \
    PATH="$TMP:/usr/bin:/bin" \
    POWDER_SECRETS_FILE=/dev/null \
    POWDER_EXPECTED_API_BASE_URL=https://sanctum.example:10001 \
    "$@" \
    "$DOCTOR"
}

success="$(run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key)"
grep -q 'PASS powder_remote' <<<"$success"

if run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key >"$TMP/drift.out" 2>&1; then
  echo "expected endpoint drift to fail" >&2
  exit 1
fi
grep -q 'ENDPOINT_DRIFT' "$TMP/drift.out"

if run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key POWDER_TEST_CURL_FAIL=1 >"$TMP/outage.out" 2>&1; then
  echo "expected service outage to fail" >&2
  exit 1
fi
grep -q 'SERVICE_OUTAGE' "$TMP/outage.out"

if run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 >"$TMP/credential.out" 2>&1; then
  echo "expected missing credential to fail" >&2
  exit 1
fi
grep -q 'CREDENTIAL_BOOTSTRAP' "$TMP/credential.out"

if run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=bad-key POWDER_TEST_AUTH_FAIL=1 >"$TMP/auth.out" 2>&1; then
  echo "expected rejected credential to fail" >&2
  exit 1
fi
grep -q 'CREDENTIAL_BOOTSTRAP' "$TMP/auth.out"

echo "PASS powder-remote-doctor tests"
