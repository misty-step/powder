#!/usr/bin/env bash
# Classify remote Powder failures without exposing credentials.
set -euo pipefail

EXPECTED_API_BASE_URL="${POWDER_EXPECTED_API_BASE_URL:-}"
SANCTUM_ROOT_URL="${POWDER_SANCTUM_ROOT_URL:-}"
CARD_ID="${POWDER_DOCTOR_CARD_ID:-powder-agent-reachability}"
SECRETS_FILE="${POWDER_SECRETS_FILE:-$HOME/.secrets}"
POWDER_BIN="${POWDER_DOCTOR_POWDER_BIN:-powder}"
CURL_BIN="${POWDER_DOCTOR_CURL_BIN:-curl}"

# powder-ci-leak-gate: this script used to bake in the operator's tailnet
# hostname as a fallback default, which put an operator-topology literal in
# a public repo. There is no repo-wide default anymore -- every deployment
# target, including the operator's own, must come from the caller's
# environment (harness bootstrap, 1Password item, shell profile), never
# from this file.
if [[ -z "$EXPECTED_API_BASE_URL" ]]; then
  printf 'FAIL CONFIG_MISSING variable=POWDER_EXPECTED_API_BASE_URL\n' >&2
  printf 'guidance: set POWDER_EXPECTED_API_BASE_URL to this deployment'\''s API base URL before running the doctor; there is no built-in default\n' >&2
  exit 19
fi
if [[ -z "$SANCTUM_ROOT_URL" ]]; then
  printf 'FAIL CONFIG_MISSING variable=POWDER_SANCTUM_ROOT_URL\n' >&2
  printf 'guidance: set POWDER_SANCTUM_ROOT_URL to this deployment'\''s root URL before running the doctor; there is no built-in default\n' >&2
  exit 19
fi

if [[ -f "$SECRETS_FILE" ]]; then
  # shellcheck source=/dev/null
  source "$SECRETS_FILE" 2>/dev/null
fi

api_base="${POWDER_API_BASE_URL:-}"
api_base="${api_base%/}"
expected="${EXPECTED_API_BASE_URL%/}"

if [[ -z "$api_base" || "$api_base" != "$expected" ]]; then
  printf 'FAIL ENDPOINT_DRIFT configured=%s expected=%s\n' "${api_base:-<missing>}" "$expected" >&2
  printf 'guidance: refresh POWDER_API_BASE_URL in the harness bootstrap; do not rotate a key for an endpoint mismatch\n' >&2
  exit 20
fi

if ! "$CURL_BIN" --connect-timeout 3 --max-time 8 -fsS "$SANCTUM_ROOT_URL" >/dev/null ||
   ! "$CURL_BIN" --connect-timeout 3 --max-time 8 -fsS "$api_base/healthz" >/dev/null ||
   ! "$CURL_BIN" --connect-timeout 3 --max-time 8 -fsS "$api_base/readyz" >/dev/null; then
  printf 'FAIL SERVICE_OUTAGE endpoint=%s\n' "$api_base" >&2
  printf 'guidance: investigate Sanctum networking or the Powder process before changing harness credentials\n' >&2
  exit 21
fi

key="${POWDER_API_KEY:-}"
command_substitution_marker="\$("
account="${USER:-$(id -un)}"
if [[ -z "$key" || "$key" == op://* || "$key" == *"$command_substitution_marker"* ]]; then
  key="$(security find-generic-password -a "$account" -s powder-admin-key -w 2>/dev/null || true)"
fi
if [[ -z "$key" || "$key" == op://* || "$key" == *"$command_substitution_marker"* ]]; then
  key="$(security find-generic-password -a "$account" -s powder-api-key -w 2>/dev/null || true)"
fi
if [[ -z "$key" || "$key" == op://* || "$key" == *"$command_substitution_marker"* ]]; then
  printf 'FAIL CREDENTIAL_BOOTSTRAP key_source=unresolved\n' >&2
  printf 'guidance: repair the sanctioned Keychain or 1Password bootstrap; Powder itself is healthy\n' >&2
  exit 22
fi

umask 077
result="$(mktemp)"
error="$(mktemp)"
header="$(mktemp)"
trap 'rm -f "$result" "$error" "$header"' EXIT
printf 'Authorization: Bearer %s\nContent-Type: application/json\n' "$key" >"$header"
auth_code="$("$CURL_BIN" --connect-timeout 3 --max-time 8 -sS -o "$error" -w '%{http_code}' \
  --header "@$header" --data '{"status":"powder-doctor-auth-probe","actor":"powder-doctor"}' \
  "$api_base/api/v1/cards/$CARD_ID/status" || true)"
if [[ "$auth_code" == "401" || "$auth_code" == "403" ]]; then
  printf 'FAIL CREDENTIAL_BOOTSTRAP key_source=resolved auth_probe=rejected\n' >&2
  printf 'guidance: verify the key is active and scoped for the canonical endpoint; service health is green\n' >&2
  exit 23
fi
if [[ "$auth_code" != "400" ]]; then
  printf 'FAIL CONTRACT_READBACK auth_probe_status=%s expected=400\n' "${auth_code:-000}" >&2
  exit 24
fi
if ! POWDER_API_BASE_URL="$api_base" POWDER_API_KEY="$key" "$POWDER_BIN" get-card "$CARD_ID" >"$result" 2>"$error"; then
  printf 'FAIL CONTRACT_READBACK card=%s request=failed\n' "$CARD_ID" >&2
  exit 25
fi
if ! grep -Fq "\"id\": \"$CARD_ID\"" "$result" &&
   ! grep -Fq "\"id\":\"$CARD_ID\"" "$result"; then
  printf 'FAIL CONTRACT_READBACK card=%s response=unexpected\n' "$CARD_ID" >&2
  exit 26
fi

# powder-workstation-cli-convergence: a healthy, correctly-configured
# server told nobody that the *workstation's* `powder` binary was several
# merges behind -- 0.1.0 git 1d1ded8 vs. a checkout at 414ac7f, silently
# missing the repeated-`--acceptance` fix, and a live card lost four
# criteria before anyone noticed. This is a warning, not a new FAIL class:
# the doctor's job stays reachability/credential classification, and a
# version mismatch is neither "unreachable" nor "misconfigured" -- it is
# purely informational, so it must never change this script's exit code.
local_sha=""
if version_output="$("$POWDER_BIN" version 2>/dev/null)"; then
  local_sha="$(printf '%s' "$version_output" | sed -n 's/.*(git \([0-9a-f]\{6,\}\).*/\1/p')"
fi
server_readyz="$("$CURL_BIN" --connect-timeout 3 --max-time 8 -fsS "$api_base/readyz" 2>/dev/null || true)"
server_sha="$(printf '%s' "$server_readyz" | sed -n 's/.*"git_sha":"\([^"]*\)".*/\1/p')"
if [[ -n "$local_sha" && -n "$server_sha" && "$server_sha" != "unknown" && "$local_sha" != "$server_sha" ]]; then
  printf 'WARN VERSION_DRIFT installed=%s server=%s\n' "$local_sha" "$server_sha" >&2
  printf 'guidance: run scripts/install-workstation.sh to converge the workstation binary with the deployed server\n' >&2
fi

printf 'PASS powder_remote endpoint=%s sanctum=up health=up ready=up auth_probe=up card=%s\n' "$api_base" "$CARD_ID"
