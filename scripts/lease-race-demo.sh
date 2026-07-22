#!/usr/bin/env bash
# powder-lease-proof-demo: a deterministic, checked-in reproduction of the
# failure mode git-native trackers cannot solve -- an agent claims a card,
# crashes (never heartbeats), and the lease expires so a second actor can
# reclaim the same card automatically, with a full audit trail proving it.
#
# Design notes:
#   - No sleeps "hoping" the race resolves. The claim TTL is a small,
#     explicit constant (CLAIM_TTL_SECONDS); Actor B polls list_ready in a
#     bounded loop and asserts the reclaim happens inside POLL_TIMEOUT_SECONDS
#     (asserted to be well under the TTL's next order of magnitude), not on a
#     fixed sleep that could race the server under load. The race is bounded
#     from BOTH sides: while Actor A's lease is still live, list_ready must
#     EXCLUDE the card (a regression that leaks claimed cards into the ready
#     pool cannot PASS as a suspiciously instant "reclaim"), and the measured
#     reclaim must take at least CLAIM_TTL_SECONDS - 1 (allowing whole-second
#     clock granularity) -- expiry by TTL, not by accident.
#   - Boots a real powder-server (release binary, same pattern as
#     .github/workflows/quickstart.yml) against a fresh ephemeral SQLite DB
#     and a random free TCP port -- no shared state with any other run.
#   - Auth/attribution: the first-run bootstrap key
#     (POWDER_BOOTSTRAP_KEY_FILE=$BOOTSTRAP_KEY_FILE) plays the operator -- it creates
#     the card and mints one agent-scope key per actor via `powder key-create
#     --db` (the documented operational pattern from docs/operations.md).
#     Each actor then authenticates as itself: in api-key mode every audited
#     event is attributed to the authenticated key's actor that performs the
#     transition (there is no request-body actor field on completion). The
#     claim-expired event is emitted while human-with-curl observes the expired
#     lease through list_ready, so its event actor is human-with-curl; the
#     nested claim/change agent fields retain codex-agent as the departed worker.
#     codex-agent's claim and work-log, plus human-with-curl's claim and
#     completion, all carry the right identity in the trail.
#   - Actor A ("codex-agent") and Actor B ("human-with-curl") are both driven
#     over plain curl against the HTTP API -- deliberately not powder-mcp --
#     because the point of the demo is that *any* actor speaking the same
#     API can reclaim a dead agent's work, not that MCP specifically can.
#   - Requires curl and jq (both preinstalled on GitHub-hosted ubuntu-latest
#     runners; install jq locally if missing -- `brew install jq` /
#     `apt-get install jq`).
#
# Usage: scripts/lease-race-demo.sh
# Exit status is nonzero on any assertion failure. stdout is the full race
# transcript; a captured real run is checked in at
# docs/lease-race-transcript.txt.
#
# Recording design note (powder-lease-proof-demo, honest account of how
# site/assets/lease-race-demo.svg was produced -- no staged/fabricated
# terminal output):
#   1. `python3 -m asciinema rec --command "bash scripts/lease-race-demo.sh"`
#      recorded a real run of this exact script to an asciicast (.cast) file
#      -- the timing and every byte of output in the recording are from an
#      actual execution, not typed/edited after the fact.
#   2. `npx svg-term-cli --in <cast> --out site/assets/lease-race-demo.svg
#      --width 82 --height 24 --window --no-cursor` converted that cast into
#      a self-contained animated SVG (CSS keyframe animation over embedded
#      <symbol>/<use> frames -- no external JS, safe to embed via a plain
#      <img> tag in the README and the marketing site).
#   3. Static SVG/GIF was the documented fallback if asciinema/svg-term were
#      unavailable; both installed cleanly here (pip install asciinema; npx
#      svg-term-cli), so the animated recording was used instead of a
#      hand-built static frame.
#   Neither tool nor its output is a repo dependency -- both ran as one-off
#   local conversion steps. Re-run steps 1-2 to refresh the recording after a
#   meaningful change to this script's output.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/powder-lease-race-demo.XXXXXX")"
DB="$WORKDIR/powder.db"
SERVER_LOG="$WORKDIR/server.log"
BOOTSTRAP_KEY_FILE="$WORKDIR/powder-bootstrap.key"
CLAIM_TTL_SECONDS=2
POLL_TIMEOUT_SECONDS=10
POLL_INTERVAL_SECONDS=1
CARD_ID="lease-race-demo-$(date +%s)"

for bin in curl jq; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "lease-race-demo requires '$bin' on PATH" >&2
    exit 1
  fi
done

SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

# A random free TCP port keeps this script collision-free next to any other
# powder-server (quickstart CI, a locally running dev instance, a second
# concurrent run of this same script).
free_port() {
  python3 - <<'PY'
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
}
LISTEN_PORT="$(free_port)"
BASE_URL="http://127.0.0.1:$LISTEN_PORT"

say() { printf '%s\n' "$*"; }
rule() { printf '%s\n' "----------------------------------------------------------------------"; }

say "=== Powder lease-race demo ==="
say "card: $CARD_ID"
say "claim TTL: ${CLAIM_TTL_SECONDS}s | poll timeout: ${POLL_TIMEOUT_SECONDS}s"
rule

say "[boot] building powder-server + powder CLI (release)"
if ! ( cd "$ROOT" && cargo build --release --locked -p powder-server -p powder-cli ) >"$WORKDIR/build.log" 2>&1; then
  echo "cargo build failed; log follows" >&2
  cat "$WORKDIR/build.log" >&2
  exit 1
fi
POWDER_CLI="$ROOT/target/release/powder"

say "[boot] starting powder-server on $BASE_URL against a fresh ephemeral DB"
POWDER_DB_PATH="$DB" \
POWDER_AUTH_MODE=api-key \
POWDER_BOOTSTRAP_KEY_FILE="$BOOTSTRAP_KEY_FILE" \
POWDER_BIND_ADDR="127.0.0.1:$LISTEN_PORT" \
PORT="$LISTEN_PORT" \
"$ROOT/target/release/powder-server" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!

server_up=0
for _ in $(seq 1 30); do
  if curl -fsS "$BASE_URL/healthz" >/dev/null 2>&1; then
    server_up=1
    break
  fi
  sleep 1
done
if [ "$server_up" -ne 1 ]; then
  echo "powder-server never became healthy; log follows" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi
say "[boot] server is up"

KEY="$(cat "$BOOTSTRAP_KEY_FILE")"
if [ -z "$KEY" ]; then
  echo "bootstrap key file was empty or missing" >&2
  cat "$SERVER_LOG" >&2
  exit 1
fi
rm -f "$BOOTSTRAP_KEY_FILE"
say "[boot] operator holds the first-run bootstrap key"
rule

say "[setup] creating card $CARD_ID (as the operator)"
curl -fsS -X POST "$BASE_URL/api/v1/cards" \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -H "Idempotency-Key: lease-race-$CARD_ID-create" \
  -d "{\"id\":\"$CARD_ID\",\"title\":\"Lease race demo card\",\"acceptance\":[\"the race resolves via lease expiry, not a human unsticking it\"]}" \
  | jq -c '{id: .id, status: .status}'

say "[setup] minting one agent-scope key per actor (powder key-create --db, the documented operator pattern)"
KEY_A="$("$POWDER_CLI" key-create --db "$DB" --name codex-agent --scope agent --show-secret 2>/dev/null | grep -oE 'sk_powder_[A-Za-z0-9_-]{25,}' | head -1)"
KEY_B="$("$POWDER_CLI" key-create --db "$DB" --name human-with-curl --scope agent --show-secret 2>/dev/null | grep -oE 'sk_powder_[A-Za-z0-9_-]{25,}' | head -1)"
if [ -z "$KEY_A" ] || [ -z "$KEY_B" ]; then
  echo "key-create did not return a secret for one of the actors" >&2
  exit 1
fi
say "[setup] actors hold their own keys; every audited event below is attributed to its actor"
rule

say "[actor A: codex-agent] claiming with ttl_seconds=$CLAIM_TTL_SECONDS"
CLAIM_A="$(curl -fsS -X POST "$BASE_URL/api/v1/cards/$CARD_ID/claim" \
  -H "Authorization: Bearer $KEY_A" -H "Content-Type: application/json" \
  -d "{\"agent\":\"codex-agent\",\"ttl_seconds\":$CLAIM_TTL_SECONDS}")"
say "$CLAIM_A" | jq -c .
RUN_A="$(printf '%s' "$CLAIM_A" | jq -r '.run_id')"
EXPIRES_A="$(printf '%s' "$CLAIM_A" | jq -r '.expires_at')"
say "[actor A] run_id=$RUN_A expires_at=$EXPIRES_A"

say "[check] while the lease is live, list_ready must EXCLUDE the card (leases actually gate the pool)"
LIVE_READY_IDS="$(curl -fsS "$BASE_URL/api/v1/cards/ready?limit=50" -H "Authorization: Bearer $KEY_B" | jq -r '.cards[].id')"
if printf '%s\n' "$LIVE_READY_IDS" | grep -qx "$CARD_ID"; then
  echo "ASSERTION FAILED: card appeared in list_ready while its lease was still active" >&2
  exit 1
fi
say "[check] confirmed: card is NOT in list_ready while claimed"

say "[actor A] appending a work-log entry, then going dark"
curl -fsS -X POST "$BASE_URL/api/v1/cards/$CARD_ID/work-log" \
  -H "Authorization: Bearer $KEY_A" -H "Content-Type: application/json" \
  -H "Idempotency-Key: lease-race-$CARD_ID-work-log" \
  -d "{\"agent\":\"codex-agent\",\"run_id\":\"$RUN_A\",\"body\":\"starting work, about to crash and never heartbeat\"}" \
  | jq -c '{card_id: .card_id, agent: .agent}'

say "[actor A] *** crash *** (no heartbeat, no release -- the lease is left to expire on its own)"
rule

say "[actor B: human-with-curl] polling list_ready until the card returns (TTL expiry, no human unsticking it)"
POLL_START="$(date +%s)"
reclaimed=0
for _ in $(seq 1 "$POLL_TIMEOUT_SECONDS"); do
  READY_IDS="$(curl -fsS "$BASE_URL/api/v1/cards/ready?limit=50" -H "Authorization: Bearer $KEY_B" | jq -r '.cards[].id')"
  if printf '%s\n' "$READY_IDS" | grep -qx "$CARD_ID"; then
    reclaimed=1
    break
  fi
  sleep "$POLL_INTERVAL_SECONDS"
done
POLL_END="$(date +%s)"
POLL_ELAPSED=$((POLL_END - POLL_START))

if [ "$reclaimed" -ne 1 ]; then
  echo "card never returned to list_ready within ${POLL_TIMEOUT_SECONDS}s" >&2
  exit 1
fi
if [ "$POLL_ELAPSED" -ge "$POLL_TIMEOUT_SECONDS" ]; then
  echo "reclaim took ${POLL_ELAPSED}s, which is not < ${POLL_TIMEOUT_SECONDS}s" >&2
  exit 1
fi
# Lower bound: the reclaim must have been caused by TTL expiry, not by the
# card never leaving (or instantly re-entering) the ready pool. Allow 1s of
# slack for whole-second clock granularity and the pre-poll work-log call.
if [ "$POLL_ELAPSED" -lt "$((CLAIM_TTL_SECONDS - 1))" ]; then
  echo "reclaim took only ${POLL_ELAPSED}s -- faster than the ${CLAIM_TTL_SECONDS}s TTL could expire; leases are not gating the pool" >&2
  exit 1
fi
say "[actor B] card reclaimed via list_ready after ${POLL_ELAPSED}s (asserted >= $((CLAIM_TTL_SECONDS - 1))s and < ${POLL_TIMEOUT_SECONDS}s)"
rule

say "[actor B] claiming the abandoned card"
CLAIM_B="$(curl -fsS -X POST "$BASE_URL/api/v1/cards/$CARD_ID/claim" \
  -H "Authorization: Bearer $KEY_B" -H "Content-Type: application/json" \
  -d '{"agent":"human-with-curl","ttl_seconds":3600}')"
say "$CLAIM_B" | jq -c .
RUN_B="$(printf '%s' "$CLAIM_B" | jq -r '.run_id')"
say "[actor B] run_id=$RUN_B"

say "[actor B] completing the card with proof (attribution comes from actor B's own key)"
PROOF="lease-race-demo local run, card=$CARD_ID, reclaimed after ${POLL_ELAPSED}s"
curl -fsS -X POST "$BASE_URL/api/v1/cards/$CARD_ID/complete" \
  -H "Authorization: Bearer $KEY_B" -H "Content-Type: application/json" \
  -H "Idempotency-Key: lease-race-$CARD_ID-complete" \
  -d "$(jq -n --arg proof "$PROOF" '{proof: $proof}')" \
  | jq -c '{id: .id, status: .status}'
rule

say "[readback] fetching card detail (detail=detailed) for the runs/work-log trail"
DETAIL="$(curl -fsS "$BASE_URL/api/v1/cards/$CARD_ID?detail=detailed" -H "Authorization: Bearer $KEY")"

say "[readback] fetching the outbound event tail (this is where claim-expired/completed live --"
say "           they are webhook-delivery events, not the card_events audit rows, so a card"
say "           detail read alone does not show them; GET /api/v1/events/tail does)"
EVENTS_JSON="$(curl -fsS "$BASE_URL/api/v1/events/tail?after=0" -H "Authorization: Bearer $KEY" \
  | grep '^data: ' | sed 's/^data: //' | jq -cs '[.[] | .event // .]')"

RUN_COUNT="$(printf '%s' "$DETAIL" | jq '.runs | length')"
FINAL_STATUS="$(printf '%s' "$DETAIL" | jq -r '.card.status')"
WORK_LOG_COUNT="$(printf '%s' "$DETAIL" | jq '[.work_log[] | select(.agent == "codex-agent")] | length')"
CARD_CREATED_COUNT="$(printf '%s' "$EVENTS_JSON" | jq '[.[] | select(.event_type == "card-created")] | length')"
CLAIM_EXPIRED_COUNT="$(printf '%s' "$EVENTS_JSON" | jq '[.[] | select(.event_type == "claim-expired" and .actor == "human-with-curl" and .change.agent == "codex-agent")] | length')"
COMPLETED_COUNT="$(printf '%s' "$EVENTS_JSON" | jq '[.[] | select(.event_type == "completed" and .actor == "human-with-curl")] | length')"

fail=0
assert_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [ "$expected" != "$actual" ]; then
    echo "ASSERTION FAILED: $desc (expected $expected, got $actual)" >&2
    fail=1
  fi
}
assert_eq "two runs recorded (Actor A's stale claim, Actor B's completing claim)" 2 "$RUN_COUNT"
assert_eq "final card status is done" "done" "$FINAL_STATUS"
assert_eq "Actor A's work-log entry survived the crash" 1 "$WORK_LOG_COUNT"
assert_eq "one card-created event" 1 "$CARD_CREATED_COUNT"
assert_eq "one claim-expired event observed by human-with-curl for codex-agent" 1 "$CLAIM_EXPIRED_COUNT"
assert_eq "one completed event attributed to human-with-curl" 1 "$COMPLETED_COUNT"

if [ "$fail" -ne 0 ]; then
  echo "--- card detail ---" >&2
  printf '%s' "$DETAIL" | jq . >&2
  echo "--- event tail ---" >&2
  printf '%s' "$EVENTS_JSON" | jq . >&2
  exit 1
fi

rule
say "=== Race transcript (from the audit trail, not narration) ==="
# Printed in known causal order (not a timestamp sort -- TTL=2s means several
# rows share the same whole-second timestamp, and sort stability across
# platforms is not worth relying on for a transcript we assert nothing
# further from).
{
  printf '%s' "$DETAIL" | jq -r '.runs[0] | [(.created_at|tostring), .agent, ("claim, run " + .id)] | @tsv'
  printf '%s' "$EVENTS_JSON" | jq -r '.[] | select(.event_type == "work-log-appended") | [(.occurred_at|tostring), .actor, "work-log-appended"] | @tsv'
  printf '%s' "$EVENTS_JSON" | jq -r '.[] | select(.event_type == "claim-expired") | [(.occurred_at|tostring), .actor, "claim-expired"] | @tsv'
  printf '%s' "$DETAIL" | jq -r '.runs[1] | [(.created_at|tostring), .agent, ("claim, run " + .id)] | @tsv'
  printf '%s' "$EVENTS_JSON" | jq -r '.[] | select(.event_type == "completed") | [(.occurred_at|tostring), .actor, "completed"] | @tsv'
} | awk -F'\t' 'BEGIN { printf "%-12s %-16s %s\n", "at", "actor", "event" } { printf "%-12s %-16s %s\n", $1, $2, $3 }'
rule
say "RESULT: PASS -- codex-agent crashed, its lease expired after ${CLAIM_TTL_SECONDS}s, human-with-curl reclaimed the card via list_ready in ${POLL_ELAPSED}s with no human editing any file, and the full claim A -> claim-expired -> claim B -> completed trail is recorded against the card, each event attributed to the actor that caused it (runs table for both claims, outbound event tail for claim-expired and completed)."
