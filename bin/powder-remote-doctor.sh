#!/usr/bin/env bash
# Classify remote Powder failures without exposing credentials.
set -euo pipefail

EXPECTED_API_BASE_URL="${POWDER_EXPECTED_API_BASE_URL:-https://sanctum.tail5f5eb4.ts.net:10001}"
SANCTUM_ROOT_URL="${POWDER_SANCTUM_ROOT_URL:-https://sanctum.tail5f5eb4.ts.net/}"
CARD_ID="${POWDER_DOCTOR_CARD_ID:-powder-agent-reachability}"
SECRETS_FILE="${POWDER_SECRETS_FILE:-$HOME/.secrets}"
POWDER_BIN="${POWDER_DOCTOR_POWDER_BIN:-powder}"
CURL_BIN="${POWDER_DOCTOR_CURL_BIN:-curl}"

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

printf 'PASS powder_remote endpoint=%s sanctum=up health=up ready=up auth_probe=up card=%s\n' "$api_base" "$CARD_ID"
