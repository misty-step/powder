# Powder

Powder is a public, self-hostable work-management app for agent-driven teams: a
durable board for cards, claims, runs, activity, links, comments, and
human-in-loop pauses.

The repo ships the application. A deployment owns the data.

Read the project direction in [`VISION.md`](VISION.md).

The first milestone is intentionally small:

- `powder-core`: pure domain vocabulary and scheduling rules.
- `powder-shell`: effect ports for storage, time, and ids.
- `powder-store`: SQLite persistence, migrations, API keys, and transactional
  card lifecycle operations.
- `powder-api`: HTTP/API contract surface.
- `powder-cli`: human and agent command-line face.
- `powder-mcp`: MCP tool contract for agents.
- `powder-server`: single deployable HTTP app.
- `SKILL.md`: shipped agent-facing usage contract.

The dispatch daemon is not part of the core. It will consume the board through
the API/MCP/CLI surfaces and run agents elsewhere.

Current local smoke paths:

```sh
DB=/tmp/powder-smoke/powder.db
cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret
cargo run -q -p powder-cli -- import crates/powder-core/tests/fixtures/backlog.d --db "$DB"
cargo run -q -p powder-cli -- list-ready --db "$DB" --limit 10
CLAIM=$(cargo run -q -p powder-cli -- claim 001 --db "$DB" --agent codex)
printf "%s" "$CLAIM"
RUN_ID=$(printf "%s" "$CLAIM" | cut -f3)
cargo run -q -p powder-cli -- heartbeat 001 --db "$DB" --run "$RUN_ID"
cargo run -q -p powder-cli -- renew-claim 001 --db "$DB" --run "$RUN_ID" --ttl 3600
cargo run -q -p powder-cli -- update-status 001 --db "$DB" --status running
cargo run -q -p powder-cli -- complete-card 001 --db "$DB" --proof https://example.test/proof
POWDER_DB_PATH="$DB" cargo run -q -p powder-mcp
```

## Self-Hosting

Powder follows the Canary-style deployment pattern:

- one Rust service image
- SQLite database at `POWDER_DB_PATH`
- dual-stack/private-Fly listener at `POWDER_BIND_ADDR`
- Fly volume mounted at `/data`
- optional Litestream replication to Fly Tigris
- `/healthz`, `/readyz`, and `/api/v1/onboarding`
- auth configured by env (`api-key`, `tailscale-header`, or `none`)
- first-run bootstrap API key, printed once unless
  `POWDER_DISCLOSE_BOOTSTRAP_KEY=false`

Local setup:

```sh
cp .env.example .env
POWDER_DB_PATH=./data/powder.db cargo run -p powder-server
```

Agent routes require `Authorization: Bearer <key>` in `api-key` mode. Use
`tailscale-header` only behind a trusted ingress that injects one of the
supported tailnet identity headers and strips spoofed client-supplied identity
headers. Use `none` only for local development.

Fly instance shape:

```sh
fly apps create powder --org misty-step
fly volumes create powder_data --size 1 --region iad --app powder
fly deploy --app powder
```

The default `fly.toml` keeps one machine warm, mounts `/data`, listens on
`[::]:4000` for Fly private IPv6, checks `/healthz` and `/readyz`, and sets
`POWDER_PUBLIC_BASE_URL` to `https://powder.internal` for a tailnet-fronted
instance. The companion bastion lane can expose `http://powder.internal:4000`
through Tailscale Serve while Powder keeps its own database and secrets on its
Fly volume. The Fly profile redacts the first bootstrap key in logs; create an
operator-held key over SSH with `powder key-create --db /data/powder.db --name
operator --scope admin --show-secret` and store it in a secret manager.

## Gate

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Pull requests run the same gate through GitHub Actions as
`Rust CI / fmt-clippy-test`. The `main` branch protection rule requires that
status check with strict status checks and admin enforcement enabled. The
Landmark release-note workflow remains release-only and does not replace the
Rust gate.
