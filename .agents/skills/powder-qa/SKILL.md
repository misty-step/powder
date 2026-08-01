---
name: powder-qa
description: |
  QA Powder changes by exercising the real workspace gate and the live card
  lifecycle, not just unit tests. Powder is a self-hosted Rust work board:
  `powder-server` (HTTP API), `powder` CLI, `powder-mcp` (MCP), all over one
  SQLite store. "Tests pass" is not QA for a claim/lease/lifecycle change.
  Use when: "QA this", "verify the feature", "smoke test powder", "check the
  gate", "test powder", "run the card lifecycle". Trigger: /powder-qa.
argument-hint: "[gate|cli-lifecycle|http|mcp]"
---

<!--
Generated via harness-kit's repo-local skill generation pattern
(skills/harness-engineering/references/repo-local-skill-generation.md).
Source repo: misty-step/powder @ f948307 (origin/main). Generated: 2026-07-01.
Generator ref: harness-kit@cbe82137.
Refreshed: 2026-07-14 (powder-self-hosting-docs stale-docs sweep) -- `master`
is now the repo's primary branch (`main` retired), the CLI smoke below was
re-run verbatim against a fresh checkout, and the `cp .env.example .env`
step was replaced (there is no dotenv loader in powder-server).
Facts below are repo-derived at generation/refresh time, not invented.
Re-verify commands against the live repo before trusting this if it has
aged further — a generated skill is a snapshot, not a live view.
-->

# powder-qa

`cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D
warnings && cargo test --workspace` is the deterministic gate — this exact
sequence runs in CI (`.github/workflows/ci.yml`, `Rust CI / fmt-clippy-test`,
required by `master` branch protection). It is **necessary but not sufficient**:
unit tests exercise fixtures, not the live claim/lease/lifecycle path across
CLI ↔ store ↔ API, and not the MCP tool surface an agent actually calls. Do
not confuse this generated QA skill with the repo's own root `SKILL.md`,
which is the *product* skill written for agents that use a deployed Powder
instance as a work board — this skill is for agents building Powder itself.

## Surfaces

| Changed area | Surface | Verification path |
|---|---|---|
| `crates/powder-core`, `powder-shell`, `powder-store` | Domain rules, adapters, SQLite persistence | `cargo test --workspace` (or `-p <crate>` narrowed) |
| `crates/powder-api`, `powder-cli` | `powder` CLI over the card/run lifecycle | Local CLI smoke below against a throwaway DB |
| `crates/powder-mcp` | MCP tool contract | `POWDER_DB_PATH=<db> cargo run -q -p powder-mcp`, then register with a harness and replay a tool call |
| `crates/powder-server` | HTTP API, single deployable app | `POWDER_DB_PATH=<db> cargo run -p powder-server`, then `/healthz` + `/readyz` |

## Commands

Deterministic gate (matches CI exactly):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Live CLI lifecycle smoke against a throwaway DB (README's own convention —
creates synthetic state directly, never real card/instance data):

```sh
DB=/tmp/powder-smoke/powder.db
cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret
cargo run -q -p powder-cli -- create-card --db "$DB" --id 001 --title "Lifecycle smoke" --acceptance "proof exists" --status ready
cargo run -q -p powder-cli -- list-ready --db "$DB" --limit 10
CLAIM=$(cargo run -q -p powder-cli -- claim 001 --db "$DB" --agent codex)
RUN_ID=$(printf "%s" "$CLAIM" | cut -f3)
cargo run -q -p powder-cli -- heartbeat 001 --db "$DB" --run "$RUN_ID"
cargo run -q -p powder-cli -- update-status 001 --db "$DB" --status in_progress
cargo run -q -p powder-cli -- request-input "$RUN_ID" --db "$DB" --question "Approve completion?"
cargo run -q -p powder-cli -- list-awaiting-input --db "$DB"
cargo run -q -p powder-cli -- answer-input "$RUN_ID" --db "$DB" --actor operator --answer approved
cargo run -q -p powder-cli -- get-card 001 --db "$DB"
cargo run -q -p powder-cli -- complete-card 001 --db "$DB" --proof https://example.test/proof
```

HTTP server smoke uses a fresh data parent and the explicit one-shot bootstrap file; do not scrape service logs for credentials:

```sh
DB=/tmp/powder-qa-smoke/powder.db
KEY_FILE=/tmp/powder-qa-smoke/bootstrap-key
mkdir -p "$(dirname "$DB")"
POWDER_DB_PATH="$DB" POWDER_AUTH_MODE=api-key \
POWDER_BIND_ADDR=127.0.0.1:4000 POWDER_BOOTSTRAP_KEY_FILE="$KEY_FILE" \
cargo run -p powder-server
# separate shell:
KEY=$(cat "$KEY_FILE")
curl -s localhost:4000/healthz
curl -s localhost:4000/readyz
# verify the one-shot key was not written to the service log
```

The lease-race smoke uses the same repaired recipe and file channel:

```sh
scripts/lease-race-demo.sh
```
## Gotchas

- **`init-db --show-secret` prints the bootstrap API key to stdout.** Harmless
  for a throwaway `/tmp` smoke DB; never run that flag against a real
  instance DB in a transcript or log you don't control.
- **CI is real now, not honor-system** — `.github/workflows/ci.yml` runs the
  exact fmt/clippy/test sequence above on every PR and push to `master`, and
  `master` branch protection requires the `Rust CI / fmt-clippy-test` check
  (strict status checks, admin enforcement). Do not treat the local gate as
  optional or as the only signal.
- **The lease-race demo is also a CI gate** — `scripts/lease-race-demo.sh`
  (run via the `Quickstart` workflow's `lease-race-demo` job) boots a real
  `powder-server`, drives a crash + reclaim race, and asserts the audit
  trail. A change to claim/lease semantics that breaks this is a real
  regression, not a flaky test.
- **`mint-key`-equivalent / DB targeting**: every CLI/MCP/server invocation
  must point at the *same* `--db`/`POWDER_DB_PATH` — Powder is a single
  SQLite writer per instance, same footgun class as Canary's single-writer
  invariant.
- **Never commit real card/run/claim/instance data** to this repo. A throwaway
  `/tmp` DB populated through `create-card` is the correct target for any live
  smoke.
- **MCP requires an explicit persistence mode.** Set `POWDER_DB_PATH` for a
  local QA run; startup fails closed when neither a DB nor remote API is
  configured.
- **`api-key` mode binds claims to the authenticated actor** — a
  request-body `agent` value is only accepted when it matches that actor;
  do not "QA" auth by spoofing a different agent name in the body.

## Report

Return: **verdict** (PASS / FAIL / UNVERIFIED) · exact command(s) run ·
surface exercised (gate / CLI lifecycle / HTTP / MCP) · artifact inspected
(gate output, CLI JSON responses, `/healthz`+`/readyz` bodies) · what was NOT
covered.
