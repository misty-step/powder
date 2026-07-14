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
if [[ "$*" == *'/readyz'* && -n "${POWDER_TEST_SERVER_SHA:-}" ]]; then
  printf '{"ok":true,"version":"0.1.0","git_sha":"%s"}\n' "$POWDER_TEST_SERVER_SHA"
  exit 0
fi
printf '{"ok":true}\n'
SH
chmod +x "$TMP/curl"

cat >"$TMP/powder" <<'SH'
#!/usr/bin/env bash
if [[ "${1:-}" == "version" ]]; then
  # POWDER_TEST_VERSION_HANG simulates a pre-read-timeout binary whose
  # version probe wedges against a server that accepts and stalls. exec,
  # not a child: a child sleep would survive the shim's kill as an orphan
  # still holding the caller's stdout pipe, stalling the test's command
  # substitution for the full 60s.
  if [[ "${POWDER_TEST_VERSION_HANG:-0}" == "1" ]]; then
    exec sleep 60
  fi
  printf 'powder 0.1.0 (git %s)\n' "${POWDER_TEST_LOCAL_SHA:-abcdefabcdef}"
  exit 0
fi
[[ -n "${POWDER_API_KEY:-}" ]] || exit 41
printf '{"card":{"id":"powder-agent-reachability"}}\n'
SH
chmod +x "$TMP/powder"

# Fake `timeout`: stock macOS has no coreutils timeout, so without this
# shim the doctor's bounded-version-probe path would only ever be
# exercised on Linux CI. Same contract as coreutils: run the command,
# kill it after N seconds, exit nonzero if it was killed.
cat >"$TMP/timeout" <<'SH'
#!/usr/bin/env bash
secs="$1"; shift
"$@" & pid=$!
# The watcher (and the sleep it may orphan) must not inherit the caller's
# stdout/stderr: a command substitution around the doctor would otherwise
# block on the open pipe until the full sleep elapsed, even after the
# probe itself finished instantly.
( sleep "$secs"; kill "$pid" 2>/dev/null ) >/dev/null 2>&1 & watcher=$!
wait "$pid"; rc=$?
kill "$watcher" 2>/dev/null
exit "$rc"
SH
chmod +x "$TMP/timeout"

run_doctor() {
  env -i \
    HOME="$TMP" \
    PATH="$TMP:/usr/bin:/bin" \
    POWDER_SECRETS_FILE=/dev/null \
    POWDER_EXPECTED_API_BASE_URL=https://sanctum.example:10001 \
    POWDER_SANCTUM_ROOT_URL=https://sanctum.example/ \
    "$@" \
    "$DOCTOR"
}

# powder-ci-leak-gate: the doctor no longer bakes in an operator tailnet
# default, so both POWDER_EXPECTED_API_BASE_URL and POWDER_SANCTUM_ROOT_URL
# must come from the caller -- assert it fails closed, with guidance, when
# either is missing, before exercising the rest of the classification tree.
if run_doctor POWDER_EXPECTED_API_BASE_URL= POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key \
  >"$TMP/config_missing_expected.out" 2>&1; then
  echo "expected missing POWDER_EXPECTED_API_BASE_URL to fail" >&2
  exit 1
fi
grep -q 'CONFIG_MISSING' "$TMP/config_missing_expected.out"
grep -q 'POWDER_EXPECTED_API_BASE_URL' "$TMP/config_missing_expected.out"

if run_doctor POWDER_SANCTUM_ROOT_URL= POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key \
  >"$TMP/config_missing_root.out" 2>&1; then
  echo "expected missing POWDER_SANCTUM_ROOT_URL to fail" >&2
  exit 1
fi
grep -q 'CONFIG_MISSING' "$TMP/config_missing_root.out"
grep -q 'POWDER_SANCTUM_ROOT_URL' "$TMP/config_missing_root.out"

success="$(run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key)"
grep -q 'PASS powder_remote' <<<"$success"

if run_doctor POWDER_API_BASE_URL=https://drifted.example:10001 POWDER_API_KEY=test-key >"$TMP/drift.out" 2>&1; then
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

# powder-workstation-cli-convergence: a version mismatch between the
# workstation binary and the deployed server is a WARN, never a FAIL --
# still PASS powder_remote overall, exit 0, but a distinct stderr line an
# operator (or an alert on stderr) can act on.
drift_stderr="$(run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key \
  POWDER_TEST_LOCAL_SHA=abcdefabcdef POWDER_TEST_SERVER_SHA=deadbeefcafe 2>&1 1>/dev/null)"
grep -q 'WARN VERSION_DRIFT' <<<"$drift_stderr"
grep -q 'abcdefabcdef' <<<"$drift_stderr"
grep -q 'deadbeefcafe' <<<"$drift_stderr"
grep -q 'install-workstation.sh' <<<"$drift_stderr"
drift_exit_status_stdout="$(run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key \
  POWDER_TEST_LOCAL_SHA=abcdefabcdef POWDER_TEST_SERVER_SHA=deadbeefcafe)"
grep -q 'PASS powder_remote' <<<"$drift_exit_status_stdout"

# A `powder version` that hangs (a pre-read-timeout binary against a
# wedged server) must not hang the doctor: the bounded call kills it, the
# drift check degrades to "no local sha" (no WARN, no FAIL), and the run
# still passes -- within the bound, not after the fake's 60s sleep.
hang_out="$(run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key \
  POWDER_TEST_VERSION_HANG=1 POWDER_TEST_SERVER_SHA=deadbeefcafe POWDER_DOCTOR_VERSION_TIMEOUT_SECS=1 2>"$TMP/hang.err")"
grep -q 'PASS powder_remote' <<<"$hang_out"
if grep -q 'VERSION_DRIFT' "$TMP/hang.err"; then
  echo "expected no VERSION_DRIFT warning when the local version probe times out" >&2
  exit 1
fi

# No drift, no warning, when the workstation and server shas agree.
no_drift_stderr="$(run_doctor POWDER_API_BASE_URL=https://sanctum.example:10001 POWDER_API_KEY=test-key \
  POWDER_TEST_LOCAL_SHA=abcdefabcdef POWDER_TEST_SERVER_SHA=abcdefabcdef 2>&1 1>/dev/null)"
if grep -q 'VERSION_DRIFT' <<<"$no_drift_stderr"; then
  echo "expected no VERSION_DRIFT warning when local and server shas match" >&2
  exit 1
fi

echo "PASS powder-remote-doctor tests"
