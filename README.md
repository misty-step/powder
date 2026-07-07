# Powder

Powder is a public, self-hostable work-management app for agent-driven teams: a
durable board for cards, claims, runs, audit events, relations, links,
comments, a high-frequency attributed work_log, and human-in-loop pauses.

The repo ships the application. A deployment owns the data.

Read the project direction in [`VISION.md`](VISION.md) and the repo contract
(architecture boundaries, gates, red lines) in [`AGENTS.md`](AGENTS.md).

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

Repository identity is operator-facing entity data, not loose card strings.
Each repository has a canonical short name, aliases, visibility, tier
(`active`, `backburner`, or `archived`), import provenance, status counts, and
card counts. Imports may still pass full slugs such as `misty-step/canary`;
card JSON, board filters, and `/api/v1/repositories` return `canary`, while
repo filters accept either spelling. Operators can merge an alias into a
canonical repository; Powder re-homes matching cards and writes `card_events`
entries with the old and new repository names. Ready queues only expose active
repositories, and attempts to move backburner or archived repository cards to
`ready` return a conflict instead of silently reactivating them.

Current local smoke paths:

```sh
DB=/tmp/powder-smoke/powder.db
cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret
cargo run -q -p powder-cli -- create-card --db "$DB" --id smoke-proof --title "Proof plan smoke" --acceptance "detail renders" --proof-plan "PR + HTTP smoke"
cargo run -q -p powder-cli -- import crates/powder-core/tests/fixtures/backlog.d --db "$DB"
cargo run -q -p powder-cli -- list-ready --db "$DB" --limit 10
CLAIM=$(cargo run -q -p powder-cli -- claim 001 --db "$DB" --agent codex)
printf "%s" "$CLAIM"
RUN_ID=$(printf "%s" "$CLAIM" | cut -f3)
cargo run -q -p powder-cli -- heartbeat 001 --db "$DB" --run "$RUN_ID"
cargo run -q -p powder-cli -- renew-claim 001 --db "$DB" --run "$RUN_ID" --ttl 3600
cargo run -q -p powder-cli -- update-relations 001 --db "$DB" --related 002 --blocks 003 --blocked-by 000
cargo run -q -p powder-cli -- update-status 001 --db "$DB" --status running
cargo run -q -p powder-cli -- request-input "$RUN_ID" --db "$DB" --question "Approve completion?"
cargo run -q -p powder-cli -- list-awaiting-input --db "$DB"
cargo run -q -p powder-cli -- answer-input "$RUN_ID" --db "$DB" --actor operator --answer approved
cargo run -q -p powder-cli -- check-criterion 001 --db "$DB" --criterion 0 --actor operator
cargo run -q -p powder-cli -- get-card 001 --db "$DB"
cargo run -q -p powder-cli -- get-run "$RUN_ID" --db "$DB"
cargo run -q -p powder-cli -- complete-card 001 --db "$DB" --criterion-proof 0=https://example.test/proof
cargo run -q -p powder-cli -- repository-list --db "$DB" --include-hidden
cargo run -q -p powder-cli -- repository-upsert --db "$DB" --name canary --aliases misty-step/canary --tier active
cargo run -q -p powder-cli -- repository-merge-alias --db "$DB" --alias misty-step/canary --into canary --actor operator
POWDER_DB_PATH="$DB" cargo run -q -p powder-mcp
```

The CLI can target either SQLite directly or a deployed `powder-server`. For
The production instance lives on the bastion box (`phrazzld-bastion` Fly app,
`/data/apps/powder/powder.db`, litestream-replicated), served on the tailnet at
`bastion.tail5f5eb4.ts.net:10001`; the standalone `powder` Fly app was
decommissioned 2026-07-07. Mint agent keys server-side there:
`fly ssh console -a phrazzld-bastion -C "powder key-create --db /data/apps/powder/powder.db --name <who> --scope agent --show-secret"`.

In
remote mode, set `POWDER_API_BASE_URL` and, for `api-key` deployments,
`POWDER_API_KEY`; `--db` always wins when supplied. Run `powder version`
before a lane starts: it reports the exact git commit the installed binary
was built from (`cargo install --path crates/powder-cli` after every pull
keeps it current), so a stale `~/.cargo/bin/powder` that predates a
command's remote-mode support is obvious up front instead of surfacing mid-lane
as a bare `missing --db` on a command the checkout has long since covered.

| Command | `--db` transport | Remote env transport | Output shape |
| --- | --- | --- | --- |
| `list-ready` | SQLite query, or backlog.d preview when a path is supplied | `GET /api/v1/cards/ready` | `id\tpriority\ttitle` or `no-ready-cards` |
| `list-cards` | SQLite query | `GET /api/v1/cards` | `id\tpriority\tstatus\ttitle` or `no-cards` |
| `get-card` | SQLite detail read | `GET /api/v1/cards/{id}` | Pretty JSON detail |
| `create-card` | SQLite create-only write | `POST /api/v1/cards` | `created\tid\tpriority\tstatus` |
| `claim` | SQLite claim lifecycle | `POST /api/v1/cards/{id}/claim` | `claimed\tcard_id\trun_id\texpires_at` |
| `heartbeat` | SQLite lease liveness | `POST /api/v1/cards/{id}/heartbeat` | `heartbeat\tcard_id\trun_id\texpires_at` |
| `renew-claim` | SQLite lease extension | `POST /api/v1/cards/{id}/renew` | `renewed\tcard_id\trun_id\texpires_at` |
| `release-claim` | SQLite lease release | `POST /api/v1/cards/{id}/release` | `released\tcard_id\trun_id` |
| `update-status` | SQLite status lifecycle | `POST /api/v1/cards/{id}/status` | `status\tid\tstatus` |
| `check-criterion` | SQLite criterion write | `POST /api/v1/cards/{id}/criteria/check` | `criterion\tid\tindex\tchecked|unchecked` |
| `add-link` | SQLite link write | `POST /api/v1/cards/{id}/links` | `link\tcard_id\tid` |
| `add-comment` | SQLite comment write | `POST /api/v1/cards/{id}/comments` | `comment\tcard_id\tauthor\tbody` |
| `append-work-log` | SQLite work_log write | `POST /api/v1/cards/{id}/work-log` | `work-log\tcard_id\tagent\tbody` |
| `request-input` | SQLite run pause | `POST /api/v1/runs/{id}/input` | `awaiting-input\trun_id\tcard_id` |
| `complete-card` | SQLite completion | `POST /api/v1/cards/{id}/complete` | `completed\tid\tstatus` |

When neither `--db` nor `POWDER_API_BASE_URL` is available for a remote-capable
command, the CLI exits with a one-line transport error instead of silently
falling back to ephemeral state. `update-relations`, `get-run`,
`list-awaiting-input`, `answer-input`, `repository-*`, `import*`, `key-*`, and
`subscription-*` remain `--db`-only (bulk/admin operations or reads with no
remote-mode demand yet); omitting `--db` on those still fails with a bare
`missing --db`.

Remote `POST /api/v1/cards/import` calls may submit inline markdown through
the `files` request body when the server cannot see the caller's checkout. A
successful non-dry-run inline import writes those files into the instance-owned
`POWDER_IMPORT_FILES_DIR` before importing them into SQLite, preserving the
same relative `card.source.path` returned by `GET /api/v1/cards/{id}`. By
default this directory is `imported-backlog.d` beside `POWDER_DB_PATH`; on Fly
with the default `/data/powder.db`, that means `/data/imported-backlog.d`.
To edit existing card content, edit the markdown file under that directory and
reimport it, rather than patching reconstructed JSON back into the database.

MCP can also run against a local or deployed `powder-server` over HTTP instead
of opening SQLite directly:

```sh
DB=/tmp/powder-http-smoke/powder.db
mkdir -p "$(dirname "$DB")"
KEY=$(cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret | awk -F '\t' '/bootstrap-key/ {print $4}')
cargo run -q -p powder-cli -- import crates/powder-core/tests/fixtures/backlog.d --db "$DB"
POWDER_DB_PATH="$DB" POWDER_AUTH_MODE=api-key POWDER_BIND_ADDR=127.0.0.1:4017 cargo run -q -p powder-server

# in another shell
export POWDER_API_BASE_URL=http://127.0.0.1:4017
export POWDER_API_KEY="$KEY"
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_ready","arguments":{"limit":1}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"claim_card","arguments":{"card_id":"001","agent":"codex","ttl_seconds":60}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"complete_card","arguments":{"card_id":"001","proof":"http://example.test/proof"}}}' \
  | cargo run -q -p powder-mcp
```

A registered MCP subprocess resolves `POWDER_API_BASE_URL` from its own launch
environment (e.g. sourced from `~/.secrets` in a `bash -lc` wrapper), which can
silently diverge from an operator's interactive shell. `initialize` reports
`result.serverInfo.baseUrl`, so a caller can confirm the two faces agree
instead of guessing at deployment drift from intermittent connection errors.

Agents that talk to the HTTP API directly, without the CLI or MCP, can read
`GET /api/v1/routes` for the full route contract with example request bodies
naming required fields -- `POST /api/v1/cards` and
`POST /api/v1/cards/{id}/links` are two routes agents have previously had to
trial-and-error against raw serde deserialize errors.

`update_card`/`PATCH /api/v1/cards/{id}` patches title, body, acceptance,
proof_plan, status, priority, or labels on an existing card without
replacing protected lifecycle or source metadata; it requires an admin-scope
key.

Harness Kit's `factory-mcps` materializer expects Powder's remote MCP entry to
provide the HTTP environment rather than a local DB when used by factory
profiles:

```yaml
- id: powder
  app: Powder
  source_repo: misty-step/powder
  product_skill: misty-powder
  status: available
  required_env_any:
    - [POWDER_API_BASE_URL, POWDER_API_KEY]
    - [POWDER_DB_PATH]
  env_sources:
    - name: POWDER_API_BASE_URL
      op_ref: op://Agents/POWDER_ENDPOINT/URL
    - name: POWDER_API_KEY
      op_ref: op://Agents/POWDER_API_KEY__bridge/credential
  codex:
    server_name: powder
    command: bash
    args:
      - -lc
      - cd /Users/phaedrus/Development/powder && exec cargo run --locked -q -p powder-mcp
```

## Self-Hosting

Powder follows the Canary-style deployment pattern:

- one Rust service image
- SQLite database at `POWDER_DB_PATH`
- inline import markdown persisted at `POWDER_IMPORT_FILES_DIR`
- dual-stack/private-Fly listener at `POWDER_BIND_ADDR`
- Fly volume mounted at `/data`
- optional Litestream replication to Fly Tigris
- `/healthz`, `/readyz`, and `/api/v1/onboarding`
- auth configured by env (`api-key`, `tailscale-header`, or `none`)
- change webhooks configured by `POWDER_WEBHOOK_URLS` (comma- or newline-separated)
- first-run bootstrap API key, printed once unless
  `POWDER_DISCLOSE_BOOTSTRAP_KEY=false`

Local setup:

```sh
cp .env.example .env
POWDER_DB_PATH=./data/powder.db cargo run -p powder-server
```

Board read routes are reachable without a key in `api-key` mode; the private
Flycast/Tailscale network is the read perimeter. Mutations, card status and
relation changes, claim lifecycle, card authoring, imports, comments, links,
answer-loop writes, and key management require `Authorization: Bearer <key>` in
`api-key` mode. Use
`tailscale-header` only behind a trusted ingress that injects one of the
supported tailnet identity headers and strips spoofed client-supplied identity
headers. Use `none` only for local development.

**Ratified posture (powder-931, 2026-07-06):** the deployed instance runs
`api-key` mode with unauthenticated reads, reachable only via
`bastion.tail5f5eb4.ts.net:10001` on the tailnet — never a public listener.
This was reviewed as a deliberate tradeoff (it serves the operator's
read-only phone use case) rather than an oversight, and the operator
ratified keeping it as-is. If the deployment's network exposure ever
changes (public listener, non-tailnet ingress), this posture must be
re-reviewed before that change ships — read routes are not currently
closed behind read-scope keys.

API keys are bound to actor records. In `api-key` mode, claiming work uses the
authenticated actor; a request-body `agent` value is accepted only when it
matches that actor.

Powder is audit-first, not lifecycle-enforcing: any authorized actor may set any
card status in one call. Claims remain useful leases for coordination, but
status correction and completion do not require the actor to hold the claim or
provide proof. When configured, card create/update/status changes POST
`{"event":"card.*","card":{...}}` to each URL in `POWDER_WEBHOOK_URLS`.

### Field-note seed generator (powder-921)

On a qualifying completion, spawn exactly one draft card carrying the `proof`
field verbatim as raw drafting material, into a shared review-queue pseudo-repo
(`repo=content`) every other content generator is meant to feed. Draft cards
always have empty `acceptance`, so [`Card::is_ready_at`] already excludes them
from `list_ready` and normal claim dispatch -- no separate exclusion mechanism
to keep in sync. Disabled by default; every completion behaves exactly as
before unless `POWDER_FIELD_NOTE_REPOS` is set.

```sh
POWDER_FIELD_NOTE_REPOS=powder,crucible,bitterblossom   # comma-separated allowlist; unset or empty disables the generator
POWDER_FIELD_NOTE_PROOF_MIN_CHARS=120                    # default; trimmed proof length floor
POWDER_FIELD_NOTE_WEEKLY_BUDGET=7                        # default; hard cap on drafts in the trailing 7 days
```

Both gates are deterministic per the content-harness design law
(misty-step-912): a repo not on the allowlist, a `proof` shorter than the
floor, or a weekly budget already spent all produce nothing -- eligibility is
never a model judgment call.

Canary self-report: `crates/powder-server/src/canary.rs` posts a `powder`
check-in every 60s and ad hoc error reports to canary-obs, gated on two Fly
secrets — `CANARY_ENDPOINT` (e.g. `https://canary-obs.fly.dev`) and
`CANARY_INGEST_KEY` (a scoped `ingest-only` key bound to service `powder`,
minted via canary's `POST /api/v1/keys`). Both must be set or
`canary::enabled()` silently no-ops. The check-in name is `powder`; canary
needs a matching monitor (`POST /api/v1/monitors` with `"name":"powder"`) or
check-ins 404.

Fly instance shape for a self-hosted deployment:

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
Fly volume. Misty Step's current operator instance is fronted by Bastion rather
than the checked-in `powder` Fly app; verify the active deployment with
`POWDER_API_BASE_URL` before treating the template app name as live -- see
[`docs/production-deploy.md`](docs/production-deploy.md) for exactly where
that instance runs, how a merged PR here actually reaches it, and the
suspended app's disposition. The Fly profile redacts the first bootstrap key
in logs; create an operator-held key over SSH with `powder key-create --db
/data/powder.db --name operator --scope admin --show-secret` and store it in
a secret manager.

### A scoped key for the board UI on a phone (powder-925)

The board's write actions (quick-add a card, change a card's status, claim,
comment, complete) only need `agent` scope, not `admin` -- `admin` is
reserved for bulk import, repository management, and key management, none
of which the board UI's phone surface exposes. Mint a dedicated,
independently-revocable `agent`-scope key for this instead of pasting the
admin key into Safari:

```sh
powder key-create --db /data/powder.db --name operator-mobile --scope agent --show-secret
```

Paste the printed key into the board's settings panel (the gear icon) --
it's held in the browser's `localStorage`, sent only as a `Bearer` header,
and never appears in the URL or a QR code. Because it's a distinct key
(not the admin key), losing the phone or leaking the key only costs a
`powder key-revoke <id>` against that one key, not against everything the
admin key can touch.

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
