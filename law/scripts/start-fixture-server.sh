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
cargo run -q -p powder-cli -- import crates/powder-core/tests/fixtures/legacy-board-source --db "$DB" >/dev/null

exec cargo run -q -p powder-server
