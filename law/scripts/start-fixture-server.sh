#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
DB="$(mktemp -d)/law-gate.db"
export POWDER_DB_PATH="$DB"
export POWDER_AUTH_MODE=none
export PORT="${PORT:-4100}"

cargo run -q -p powder-cli -- init-db --db "$DB" >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id 001 --title "Lifecycle example card" --acceptance "proof exists" --status ready >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id blocked-card --title "Blocked card" --acceptance "dependency clears" --status ready --blocked-by blocker-dep >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id blocker-dep --title "Dependency card" --acceptance "dependency work completes" --status ready >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id done-card --title "Done card" --acceptance "proof exists" --status done >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id shipped-card --title "Shipped card" --acceptance "proof exists" --status shipped >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id abandoned-card --title "Abandoned card" --acceptance "reason recorded" --status abandoned >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id backlog-match --title "Backlog card" --acceptance "proof exists" --status backlog >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id inprogress-match --title "In progress card" --acceptance "proof exists" --status in_progress >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id repo-card --title "Repo tagged card" --acceptance "proof exists" --status ready --repo powder >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id parent-card --title "Parent card" --acceptance "child is linked" --status ready >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id child-card --title "Child card" --acceptance "child is visible" --status ready --parent parent-card >/dev/null
cargo run -q -p powder-cli -- create-card --db "$DB" --id awaiting-answer --title "Needs an operator answer" --acceptance "operator responds" --status ready >/dev/null
AWAITING_CLAIM="$(cargo run -q -p powder-cli -- claim awaiting-answer --db "$DB" --agent law-gate-agent --ttl 3600)"
AWAITING_RUN_ID="$(printf '%s' "$AWAITING_CLAIM" | cut -f3)"
cargo run -q -p powder-cli -- request-input "$AWAITING_RUN_ID" --db "$DB" --question "Ship this behind a flag or straight to prod?" >/dev/null

exec cargo run -q -p powder-server
