# Powder

Powder is a public, self-hostable work-management app for agent-driven teams: a
durable board for cards, claims, runs, activity, links, comments, and
human-in-loop pauses.

The repo ships the application. A deployment owns the data.

The first milestone is intentionally small:

- `powder-core`: pure domain vocabulary and scheduling rules.
- `powder-shell`: effect ports for storage, time, and ids.
- `powder-api`: HTTP/API contract surface.
- `powder-cli`: human and agent command-line face.
- `powder-mcp`: MCP tool contract for agents.
- `powder-server`: single deployable HTTP app.
- `SKILL.md`: shipped agent-facing usage contract.

The dispatch daemon is not part of the core. It will consume the board through
the API/MCP/CLI surfaces and run agents elsewhere.

Current local smoke paths:

```sh
cargo run -p powder-server
cargo run -q -p powder-cli -- import crates/powder-core/tests/fixtures/backlog.d --dry-run
cargo run -q -p powder-cli -- list-ready crates/powder-core/tests/fixtures/backlog.d --limit 10
POWDER_BACKLOG_DIR=crates/powder-core/tests/fixtures/backlog.d cargo run -q -p powder-mcp
```

## Self-Hosting

Powder follows the Canary-style deployment pattern:

- one Rust service image
- SQLite database at `POWDER_DB_PATH`
- Fly volume mounted at `/data`
- optional Litestream replication to Fly Tigris
- `/healthz`, `/readyz`, and `/api/v1/onboarding`
- auth configured by env (`shared-secret`, `tailscale-header`, or `disabled`)

Local setup:

```sh
cp .env.example .env
POWDER_DB_PATH=./data/powder.db cargo run -p powder-server
```

## Gate

```sh
cargo test --workspace
```
