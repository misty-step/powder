#!/usr/bin/env bash
set -euo pipefail

# powder-ui-keyboard-firstrun: a second, genuinely-empty fixture instance
# (zero cards, zero repositories) so the law gate can prove the brand-new-
# instance welcome state renders honestly, distinct from a filtered-to-
# nothing board. Runs on its own port/DB alongside the populated fixture
# (law/scripts/start-fixture-server.sh) -- see law/playwright.config.ts's
# `webServer` array.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

DB="$(mktemp -d)/law-gate-empty.db"
export POWDER_DB_PATH="$DB"
export POWDER_AUTH_MODE=none
export PORT="${PORT:-4101}"
export POWDER_HOME_URL="https://sanctum.example.test"

cargo run -q -p powder-cli -- init-db --db "$DB" >/dev/null

exec cargo run -q -p powder-server
