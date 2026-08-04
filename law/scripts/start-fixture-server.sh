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
cargo run -q -p powder-cli -- create-card --db "$DB" --id 001 --title "Lifecycle example card" --acceptance "proof exists" --status ready --estimate s --risk low >/dev/null
# powder-status-vocabulary: `blocked` is not a status -- a blocked card is a
# ready card with an unresolved blocked_by relation. The blocker card stays
# non-terminal (ready, NOT backlog: a backlog blocker would land first in the
# rail and break board-card-link's "first card is 001" assumption) so the
# board's derived BLOCKED strip has a real row.
cargo run -q -p powder-cli -- create-card --db "$DB" --id blocked-card --title "Blocked card" --acceptance "dependency clears" --status ready --estimate s --risk high --blocked-by blocker-dep >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id blocker-dep --title "Dependency the blocked card waits on" --acceptance "dependency work completes" --status ready >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id done-card --title "Done card" --acceptance "proof exists" --status done --estimate l --risk high >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id done-match --title "Done facet match" --acceptance "proof exists" --status done --estimate s --risk high >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id backlog-match --title "Backlog facet match" --acceptance "proof exists" --status backlog --estimate s --risk high >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id backlog-no-match --title "Backlog facet non-match" --acceptance "proof exists" --status backlog --estimate l --risk low >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id inprogress-match --title "In progress facet match" --acceptance "proof exists" --status in_progress --estimate s --risk high >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id inprogress-no-match --title "In progress facet non-match" --acceptance "proof exists" --status in_progress --estimate l --risk low >/dev/null

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
# "general"-bucket sort order the existing board-card-link test's "first card
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

# powder-ui-overview-hierarchy: pathological rollup shapes mirroring
# production, so the Overview alignment law judges real layout pressure
# (long titles, long slug ids, six-status chip clusters, wide criteria
# fractions, stale freshness) instead of toy data. Deliberate constraints:
#   - no trailing numeric id segment anywhere (repo_from_numeric_card_id_prefix
#     would auto-assign repos and disturb the general-bucket assumptions above)
#   - no --estimate/--risk flags, so the facet-filter law specs never match
#     these cards
#   - bulk calls go through the already-built binary, not `cargo run`, so ~60
#     seeding calls stay well inside the Playwright webServer boot timeout.
POWDER_BIN="$ROOT/target/debug/powder"
cargo build -q -p powder-cli

# (a)+(b)+(c): a 9+ word 'EPIC: '-prefixed title, long slug ids, and six
# children covering five distinct statuses plus awaiting input.
"$POWDER_BIN" create-card --db "$DB" --id crucible-capability-decomposition-eval --title "EPIC: retire dead products and consolidate every overlapping capability evaluation surface" --acceptance "dead products retired" --status ready >/dev/null
for child_status in backlog ready in_progress awaiting_input done shipped; do
  "$POWDER_BIN" create-card --db "$DB" --id "crucible-child-$child_status" --title "Crucible child ($child_status)" --acceptance "child lands" --status "$child_status" --parent crucible-capability-decomposition-eval >/dev/null
done

# (b)+(e)+(f): long slug id, a 10/56 criteria fraction, and a 6d/12d
# freshness spread. updated_at has no CLI setter, so the backdate is one
# direct SQL statement after all writes to those rows are done.
"$POWDER_BIN" create-card --db "$DB" --id estate-digitalocean-only-cutover --title "EPIC: cut the estate over to DigitalOcean-only hosting with zero downtime" --acceptance "estate cut over" --status ready >/dev/null
ESTATE_CRITERIA=()
for i in $(seq 1 56); do ESTATE_CRITERIA+=(--acceptance "runbook step $i"); done
"$POWDER_BIN" create-card --db "$DB" --id estate-cutover-runbook --title "Cutover runbook" --status in_progress --parent estate-digitalocean-only-cutover "${ESTATE_CRITERIA[@]}" >/dev/null
for i in $(seq 0 9); do
  "$POWDER_BIN" check-criterion estate-cutover-runbook --db "$DB" --criterion "$i" --actor law-gate-agent >/dev/null
done
"$POWDER_BIN" create-card --db "$DB" --id estate-cutover-dns --title "DNS flip" --status ready --parent estate-digitalocean-only-cutover >/dev/null
python3 - "$DB" <<'EOF'
import sqlite3, sys, time
now = int(time.time())
db = sqlite3.connect(sys.argv[1])
db.execute("UPDATE cards SET updated_at = ? WHERE id = 'estate-cutover-runbook'", (now - 6 * 86400,))
db.execute("UPDATE cards SET updated_at = ? WHERE id = 'estate-cutover-dns'", (now - 12 * 86400,))
db.commit()
db.close()
EOF

# (e): a 21/30 criteria fraction.
"$POWDER_BIN" create-card --db "$DB" --id powder-overview-visual-hierarchy --title "EPIC: rebuild the Overview rollup visual hierarchy beyond the Linear bar" --acceptance "overview reads clean" --status ready >/dev/null
OVERVIEW_CRITERIA=()
for i in $(seq 1 30); do OVERVIEW_CRITERIA+=(--acceptance "grid criterion $i"); done
"$POWDER_BIN" create-card --db "$DB" --id overview-hierarchy-grid --title "Shared column grid" --status ready --parent powder-overview-visual-hierarchy "${OVERVIEW_CRITERIA[@]}" >/dev/null
for i in $(seq 0 20); do
  "$POWDER_BIN" check-criterion overview-hierarchy-grid --db "$DB" --criterion "$i" --actor law-gate-agent >/dev/null
done

# List depth: five more epics so the alignment law measures rows with
# varied title lengths, chip counts, and meta widths. The lowercase 'Epic:'
# on agent-vault-retirement proves the render-time prefix strip is
# case-insensitive.
"$POWDER_BIN" create-card --db "$DB" --id law-gate-hardening --title "EPIC: harden the law gate so visual drift fails CI before review" --acceptance "drift fails CI" --status ready >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id law-gate-alignment-probe --title "Alignment probe" --acceptance "probe measures offsets" --status ready --parent law-gate-hardening >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id law-gate-contrast-probe --title "Contrast probe" --acceptance "contrast measured" --status done --parent law-gate-hardening >/dev/null

"$POWDER_BIN" create-card --db "$DB" --id agent-vault-retirement --title "Epic: retire Agent Vault so Mint is the only credential broker" --acceptance "vault retired" --status ready >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id vault-callers-cutover --title "Cut vault callers to Mint" --acceptance "callers moved" --status in_progress --parent agent-vault-retirement >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id vault-docs-sweep --title "Sweep vault docs" --acceptance "docs swept" --status backlog --parent agent-vault-retirement >/dev/null

"$POWDER_BIN" create-card --db "$DB" --id rollout-gate-telemetry --title "EPIC: emit rollout gate telemetry for every ship decision" --acceptance "telemetry emitted" --status ready >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id rollout-telemetry-schema --title "Telemetry schema" --acceptance "schema ratified" --status done --parent rollout-gate-telemetry >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id rollout-telemetry-dashboard --title "Telemetry dashboard" --acceptance "dashboard live" --status abandoned --parent rollout-gate-telemetry >/dev/null

"$POWDER_BIN" create-card --db "$DB" --id docs-null-repo-cleanup --title "EPIC: sweep null-repo docs into their owning repositories" --acceptance "docs owned" --status ready >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id docs-null-repo-audit --title "Audit null-repo docs" --acceptance "audit filed" --status ready --parent docs-null-repo-cleanup >/dev/null

"$POWDER_BIN" create-card --db "$DB" --id cli-freshness-surface --title "EPIC: surface card freshness across the CLI list views" --acceptance "freshness listed" --status ready >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id cli-freshness-flag --title "Freshness flag" --acceptance "flag shipped" --status backlog --parent cli-freshness-surface >/dev/null

# (d): a second Unsorted repository bucket beside the existing powder row.
"$POWDER_BIN" repository-upsert --db "$DB" --name misty-step/estate --visibility visible --tier active >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id estate-unscoped-backup --title "Unscoped backup audit" --acceptance "audit filed" --status backlog --repo misty-step/estate >/dev/null
"$POWDER_BIN" create-card --db "$DB" --id estate-unscoped-metrics --title "Unscoped metrics sweep" --acceptance "metrics swept" --status ready --repo misty-step/estate >/dev/null

exec cargo run -q -p powder-server
