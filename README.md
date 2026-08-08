# Powder

Powder is one self-hosted SQLite work ledger for agent-driven teams. It keeps
cards, expiring claims, typed run history, and attributed events in one service
with one deterministic contract.

A deployed instance owns its data. This repository ships `powder-server`, the
`powder` CLI, and the agent `SKILL.md`. Powder never calls a model or runs a
dispatch loop.

## Quickstart

Run a server, read the one-shot bootstrap key, create a card, and claim it.
Use Docker or a release binary.

### Docker

```sh
docker volume create powder-data
docker run --rm -p 4000:4000 -v powder-data:/data \
  -e POWDER_AUTH_MODE=api-key \
  -e POWDER_BOOTSTRAP_KEY_FILE=/data/powder-bootstrap.key \
  ghcr.io/misty-step/powder:latest
```

A named volume gives the container user write access without host UID mapping.

### Release binary

```sh
curl -fsSL -o powder.tar.gz \
  https://github.com/misty-step/powder/releases/latest/download/powder-aarch64-apple-darwin.tar.gz
tar -xzf powder.tar.gz
mkdir -p ./data && chmod 700 ./data
POWDER_DB_PATH=./data/powder.db \
POWDER_BOOTSTRAP_KEY_FILE=./data/powder-bootstrap.key \
POWDER_AUTH_MODE=api-key ./powder-server
```

Use the Linux tarball name on Linux. The tarball contains `powder` and
`powder-server`.

The first boot writes the bootstrap API key to the configured 0600 file. It
never prints the secret. Read it once, store it in a secret manager, and remove
the file.

```sh
KEY="$(cat ./data/powder-bootstrap.key)"
rm ./data/powder-bootstrap.key

curl -s http://localhost:4000/healthz
curl -s -X POST http://localhost:4000/api/v1/cards \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -H "Idempotency-Key: first-card-create" \
  -d '{"id":"first-card","title":"My first card","acceptance":["it exists"]}'

curl -s -X POST http://localhost:4000/api/v1/cards/first-card/claim \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"agent":"me"}'
```

Production uses one Rust service and one SQLite database on a host volume. WAL
is enabled, Litestream replication is optional, and ingress uses the configured
private boundary. See [`docs/operations.md`](docs/operations.md) for the
operator runbook.

## The Ledger Contract

Powder has four durable concepts:

- **Card** stores context, acceptance criteria, status, priority, labels,
  relations, links, comments, work logs, proof, and an optional opaque `repo`
  string. `repo` supports exact filtering; it is not a registry.
- **Claim** stores the current principal, worker, run identifier, lease expiry,
  and liveness. A stale claim expires so another worker can continue.
- **Run** stores typed claim-attempt history, lifecycle state, identity,
  timestamps, proof, and elicitation/response activity. It does not store
  telemetry or session forensics.
- **Event** is immutable attributed audit history. The ordered outbound event
  sequence and SSE tail let integrations observe changes without a delivery
  worker.

Parent and blocker edges remain generic relations. Full-text search, ready
snapshots, idempotency, comments, work logs, proof, and awaiting-input state
remain part of the ledger.

Agents use the `powder` CLI and `SKILL.md`. HTTP serves the UI and integrations.
SSE serves the ordered event tail. The human UI stays small and responsive:
list or Kanban, search, detail, create/edit, claim state, timeline, input,
proof, and auth.

## Why Powder

- **Claims expire.** A crashed worker does not hold work forever.
- **Runs stay typed.** Handoffs, questions, proof, and lifecycle state remain
  inspectable without a telemetry product.
- **Events stay attributable.** Every change records its actor, principal, time,
  operation, and ordered position where applicable.
- **One pool, any actor.** Agents, cron jobs, HTTP clients, and humans share the
  same card and claim contract.
- **External workers stay external.** Dispatch, models, shaping, analytics,
  media, and delivery policy do not enter the ledger.

## Current Cutover Boundary

The first cutover wave removes active registry, delivery, telemetry, media,
portfolio, and static periphery surfaces. Historical tables and columns may
remain readable until a later schema migration.

That migration must preserve typed run history, questions and answers, proof,
attribution, parent edges, links, claim expiry, event order, idempotency, and
the exact opaque `repo` value. Use a WAL-safe backup and restore proof before
dropping dormant storage. Do not replace typed fields with arbitrary JSON or
add compatibility fallbacks.

## Learn More

- [`docs/self-hosting.md`](docs/self-hosting.md) — Docker, release binaries,
  systemd, environment, auth, and backup/restore.
- [`docs/operations.md`](docs/operations.md) — workstation, remote CLI, auth,
  paging, and production operations.
- [`SKILL.md`](SKILL.md) — the supported agent workflow.
- [`VISION.md`](VISION.md) — product boundary and non-goals.
- [`docs/decisions/0002-radical-ledger-boundary.md`](docs/decisions/0002-radical-ledger-boundary.md)
  — the radical ledger decision.
- [`AGENTS.md`](AGENTS.md) — repository contract and red lines.

## What's In The Repository

The repository ships the application. A deployment owns the data.

- `powder-core`: domain vocabulary and ledger rules.
- `powder-store`: SQLite persistence, migrations, auth, idempotency, and
  transactional lifecycle operations.
- `powder-api`: HTTP and SSE contract surface.
- `powder-cli`: human and agent command-line face.
- `powder-server`: single deployable HTTP app.
- `SKILL.md`: shipped agent-facing workflow contract.

## Local Lifecycle Smoke

This sequence exercises SQLite-direct CLI behavior through claim, liveness,
awaiting input, proof, and completion:

```sh
DB=/tmp/powder-smoke/powder.db
mkdir -p "$(dirname "$DB")"
cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret
cargo run -q -p powder-cli -- create-card --db "$DB" --id smoke-proof --title "Proof plan smoke" --acceptance "detail renders"
cargo run -q -p powder-cli -- list-ready --db "$DB" --limit 10
CLAIM=$(cargo run -q -p powder-cli -- claim smoke-proof --db "$DB" --agent codex)
printf "%s" "$CLAIM"
RUN_ID=$(printf "%s" "$CLAIM" | cut -f3)
cargo run -q -p powder-cli -- heartbeat smoke-proof --db "$DB" --run "$RUN_ID"
cargo run -q -p powder-cli -- request-input "$RUN_ID" --db "$DB" --question "Approve completion?"
cargo run -q -p powder-cli -- list-awaiting-input --db "$DB"
cargo run -q -p powder-cli -- answer-input "$RUN_ID" --db "$DB" --actor operator --answer approved
cargo run -q -p powder-cli -- check-criterion smoke-proof --db "$DB" --criterion 0 --actor operator
cargo run -q -p powder-cli -- get-card smoke-proof --db "$DB"
cargo run -q -p powder-cli -- get-run "$RUN_ID" --db "$DB"
cargo run -q -p powder-cli -- complete-card smoke-proof --db "$DB" --criterion-proof 0=https://example.test/proof
```

For remote CLI transport and deployment operations, see
[`docs/operations.md`](docs/operations.md).

## Gate

```sh
test -z "$(find . -type d -name backlog.d -not -path './.git/*' -print -quit)"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Pull requests run the same Rust gate through GitHub Actions. The `master`
branch protection rule requires the `Rust CI / fmt-clippy-test` status check.
