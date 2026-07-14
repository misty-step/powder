#!/usr/bin/env bash
set -euo pipefail

# Boots powder-server against a throwaway, seeded SQLite DB so the law gate
# renders a populated board (cards in different statuses), not an empty
# shell. Used as the `webServer.command` for Playwright (law/playwright.config.ts).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

DB="$(mktemp -d)/law-gate.db"
export POWDER_DB_PATH="$DB"
export POWDER_AUTH_MODE=none
export PORT="${PORT:-4100}"
# powder-942: configured so the law gate exercises the home-affordance link
# on every existing test, not just a dedicated one -- it's real chrome now,
# not a special case.
export POWDER_HOME_URL="https://sanctum.example.test"

cargo run -q -p powder-cli -- init-db --db "$DB" >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id 001 --title "Lifecycle example card" --acceptance "proof exists" --status ready >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id blocked-card --title "Blocked card" --acceptance "dependency clears" --status blocked >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id done-card --title "Done card" --acceptance "proof exists" --status done >/dev/null

# powder-915: init-db seeds ~24 "ratified tier" repository entities
# (powder-916), every one of them at card_count 0 until something is filed
# under it -- so with zero-card repos hidden by default (this card), a
# fixture that never files a card under any registered repo would leave the
# settings list showing nothing until "show empty" is toggled, and the "a
# seeded repo shows its count" law spec would have nothing real to assert
# against. One card filed under the already-registered "powder" repo gives
# it a real, nonzero, visible-by-default row alongside the ~24 still-hidden
# empty ones.
cargo run -q -p powder-cli -- create-card --db "$DB" --id powder-repo-example --title "Repo-scoped example card" --acceptance "proof exists" --status ready --repo powder >/dev/null

# powder-ui-awaiting-you: a claimed, in-flight run parked on an operator
# question so the awaiting-you strip/badge/answer-form law-gate specs have a
# real elicitation to render and answer against. Deliberately no trailing
# numeric id segment (`repo_from_numeric_card_id_prefix`, powder-core) --
# a plain "-NNN" suffix would auto-assign a distinct repo and disturb the
# "local"-repo sort order the existing board-card-link test's "first card
# is 001" assumption depends on.
cargo run -q -p powder-cli -- create-card --db "$DB" --id awaiting-answer --title "Needs an operator answer" --acceptance "operator responds" --status ready >/dev/null
AWAITING_CLAIM="$(cargo run -q -p powder-cli -- claim awaiting-answer --db "$DB" --agent law-gate-agent --ttl 3600)"
AWAITING_RUN_ID="$(printf '%s' "$AWAITING_CLAIM" | cut -f3)"
cargo run -q -p powder-cli -- request-input "$AWAITING_RUN_ID" --db "$DB" --question "Ship this behind a flag or straight to prod?" >/dev/null

# powder-ui-hierarchy-render: an epic with two children in different states,
# one checked criterion, and one piece of link evidence, so detail-view
# children/epic-state rendering and the board's "part of <epic>" child badge
# both have real data. epic-mismatch is a second, deliberately mismatched
# epic (parent already done while its only child is not terminal) so the
# mismatch-as-warning styling has something real to assert against. Same
# no-numeric-suffix id convention as above.
cargo run -q -p powder-cli -- create-card --db "$DB" --id epic-hierarchy --title "Epic: ship the hierarchy view" --acceptance "children roll up cleanly" --status ready >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id epic-hierarchy-child-a --title "Child A: backend endpoint" --acceptance "endpoint returns 200" --status done --parent epic-hierarchy >/dev/null
cargo run -q -p powder-cli -- check-criterion epic-hierarchy-child-a --db "$DB" --criterion 0 --actor law-gate-agent >/dev/null
cargo run -q -p powder-cli -- add-link epic-hierarchy-child-a --db "$DB" --label "proof" --url "https://example.test/pr/1" >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id epic-hierarchy-child-b --title "Child B: board UI" --acceptance "UI renders hierarchy" --status ready --parent epic-hierarchy >/dev/null

cargo run -q -p powder-cli -- create-card --db "$DB" --id epic-mismatch --title "Epic: mismatch example" --acceptance "children complete" --status done >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id epic-mismatch-child-a --title "Child: still running" --acceptance "work finishes" --status ready --parent epic-mismatch >/dev/null

exec cargo run -q -p powder-server
