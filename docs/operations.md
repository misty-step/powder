# Operations

This is the operator/runbook reference for a deployed Powder instance:
remote-mode transport details, MCP registration and key-rotation lore, the
self-hosting deployment shape, and the field-note/canary/mobile-key knobs.
It was relocated here from the README (which now stays a short pitch +
quickstart) verbatim -- nothing here was rewritten, only moved.

For the five-minute path to a running instance, see the
[README quickstart](../README.md#quickstart). For where the operator's own
production instance actually runs, see
[`docs/production-deploy.md`](production-deploy.md).

## CLI remote-mode transport

The CLI can target either SQLite directly or a deployed `powder-server`. The
production instance is run by a companion box, not this repo's own checked-in
Fly app (destroyed 2026-07-07 after its data was verified migrated) -- see
[`docs/production-deploy.md`](production-deploy.md) for where it
actually lives, how a merged PR here reaches it, and how to mint an agent key
against it.

In remote mode, set `POWDER_API_BASE_URL` and, for `api-key` deployments,
`POWDER_API_KEY`; `--db` always wins when supplied. Run `powder version`
before a lane starts: it reports the exact git commit the installed binary
was built from (`cargo install --path crates/powder-cli` after every pull
keeps it current), so a stale `~/.cargo/bin/powder` that predates a
command's remote-mode support is obvious up front instead of surfacing mid-lane
as a bare `missing --db` on a command the checkout has long since covered.

| Command | `--db` transport | Remote env transport | Output shape |
| --- | --- | --- | --- |
| `list-ready` | SQLite query | `GET /api/v1/cards/ready` | `id\tpriority\ttitle` or `no-ready-cards` |
| `list-cards` | SQLite query | `GET /api/v1/cards` | `id\tpriority\tstatus\tautonomy\ttitle` or `no-cards` |
| `get-card` | SQLite detail read | `GET /api/v1/cards/{id}` | Pretty JSON detail |
| `create-card` | SQLite create-only write | `POST /api/v1/cards` | `created\tid\tpriority\tstatus\tautonomy` |
| `update-card` | SQLite patch write | `PATCH /api/v1/cards/{id}` | `updated\tid\tpriority\tstatus\tautonomy` |
| `list-approvals` | SQLite approval queue read | `GET /api/v1/approvals` | Pretty JSON approval queue |
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
`list-awaiting-input`, `answer-input`, `repository-*`, `import-github-issues`, `key-*`, and
`subscription-*` remain `--db`-only (bulk/admin operations or reads with no
remote-mode demand yet); omitting `--db` on those still fails with a bare
`missing --db`.

MCP can also run against a local or deployed `powder-server` over HTTP instead
of opening SQLite directly:

MCP claim lifecycle operations are consolidated under `manage_claim` with an
`action` enum (`claim`, `renew`, `heartbeat`, `release`, `transfer`). This is a
pre-1.0 MCP break: the former `claim_card`, `renew_claim`, `heartbeat`,
`release_claim`, and `transfer_claim` tools are removed from `tools/list`.

By default, `powder-mcp` advertises the agent persona only: card discovery,
card/runs reads, card creation/update, status/relations/criteria writes,
claim management, comments, work logs, links, input requests/answers,
completion, and `list_repositories` for repo filters. Operator/admin tools are
hidden from both `tools/list` and `tools/call`: `create_event_subscription`,
`list_event_subscriptions`, `disable_event_subscription`, `list_dead_letters`,
`tail_events`, `list_keys`, `upsert_repository`, `delete_repository`, and
`merge_repository_alias`. Set `POWDER_MCP_TOOLSETS=admin` or
`POWDER_MCP_TOOLSETS=all` before starting the MCP subprocess to add those
admin tools to the same server registration. The value is read once at startup
for MCP client cache stability; changing it requires restarting `powder-mcp`.
A hidden-tool call returns an error naming `POWDER_MCP_TOOLSETS`.

```sh
DB=/tmp/powder-http-smoke/powder.db
mkdir -p "$(dirname "$DB")"
KEY=$(cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret | awk -F '\t' '/bootstrap-key/ {print $4}')
cargo run -q -p powder-cli -- create-card --db "$DB" --id smoke-proof --title "HTTP smoke" --acceptance "lifecycle works" --status ready
POWDER_DB_PATH="$DB" POWDER_AUTH_MODE=api-key POWDER_BIND_ADDR=127.0.0.1:4017 cargo run -q -p powder-server

# in another shell
export POWDER_API_BASE_URL=http://127.0.0.1:4017
export POWDER_API_KEY="$KEY"
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_ready","arguments":{"limit":1}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"manage_claim","arguments":{"card_id":"smoke-proof","action":"claim","agent":"codex","ttl_seconds":60}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"complete_card","arguments":{"card_id":"smoke-proof","proof":"http://example.test/proof"}}}' \
  | cargo run -q -p powder-mcp
```

A registered MCP subprocess resolves `POWDER_API_BASE_URL` from its own launch
environment (e.g. sourced from `~/.secrets` in a `bash -lc` wrapper), which can
silently diverge from an operator's interactive shell. `initialize` reports
`result.serverInfo.baseUrl`, so a caller can confirm the two faces agree
instead of guessing at deployment drift from intermittent connection errors.

A long-lived `powder-mcp` subprocess also captures `POWDER_API_KEY` once, at
boot; rotating the key does not change the running process's environment, so
it keeps sending the old value until something restarts it (powder-944).
Restarting the MCP client always fixes this. To avoid the restart, set
`POWDER_API_KEY_CMD` to a shell command that prints a fresh key on stdout
(e.g. `security find-generic-password -a "$USER" -s powder-api-key -w`);
`powder-mcp` runs it once at boot and again, once, on the first `401` a
request hits, transparently retrying with whatever key that produces if it
differs from the one that just failed. `POWDER_API_KEY` remains the plain
fallback and is unchanged when `POWDER_API_KEY_CMD` is unset. A `401` that
survives the retry (or has no `POWDER_API_KEY_CMD` to retry with) names the
key prefix `powder-mcp` used and says to restart the client or configure
`POWDER_API_KEY_CMD`; three or more consecutive `404`s on tool calls get a
distinct steer toward a stale `POWDER_API_BASE_URL` (a deployment host
cutover, powder-965's class of incident) instead.

The repo also includes a deterministic MCP tool-use eval harness. It creates
throwaway fixture SQLite DBs, starts the real `powder-mcp` binary over stdio,
runs four scripted scenarios, and prints one compact baseline table. `response
chars` is the total visible tool-result JSON text, plus JSON-RPC error message
text for recovery scenarios:

```sh
cargo build -q -p powder-mcp --bin powder-mcp
cargo run -q -p powder-mcp --example eval
```

Set `POWDER_MCP_BIN=/path/to/powder-mcp` to force a specific binary. The
integration test runs the same harness without any LLM calls:

```sh
cargo test -p powder-mcp --test tool_use_eval
```

To add a scenario, extend `crates/powder-mcp/src/eval_harness.rs` with a seed
function, a stdio tool-call script, and persisted end-state assertions, then
add the scenario to `run_eval`. Keep setup synthetic and repo-local: fixture
data is written only to temp SQLite DBs, never to checked-in backlog data.

Agents that talk to the HTTP API directly, without the CLI or MCP, can read
`GET /api/v1/routes` for the full route contract with example request bodies
naming required fields -- `POST /api/v1/cards` and
`POST /api/v1/cards/{id}/links` are two routes agents have previously had to
trial-and-error against raw serde deserialize errors.

`update_card`/`PATCH /api/v1/cards/{id}` patches title, body, acceptance,
proof_plan, status, autonomy, priority, or labels on an existing card without
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
      - cd /path/to/powder && exec cargo run --locked -q -p powder-mcp
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
relation changes, claim lifecycle, card authoring, comments, links,
answer-loop writes, and key management require `Authorization: Bearer <key>` in
`api-key` mode. Use
`tailscale-header` only behind a trusted ingress that injects one of the
supported tailnet identity headers and strips spoofed client-supplied identity
headers. Use `none` only for local development.

**Ratified posture (powder-931, 2026-07-06):** the deployed instance runs
`api-key` mode with unauthenticated reads, reachable only over its private
tailnet hostname (see [`docs/production-deploy.md`](production-deploy.md))
— never a public listener.
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
instance. A private host can expose `http://powder.internal:4000`
through Tailscale Serve while Powder keeps its own database and secrets on its
Fly volume. Misty Step's current operator instance is hosted by Sanctum rather
than the checked-in `powder` Fly app; verify the active deployment with
`POWDER_API_BASE_URL` before treating the template app name as live -- see
[`docs/production-deploy.md`](production-deploy.md) for exactly where
that instance runs, how a merged PR here actually reaches it, and this app's
disposition (destroyed 2026-07-07 -- `fly.toml`'s header explains why and
prevents accidentally re-creating it as a decoy). The Fly profile redacts the
first bootstrap key
in logs; create an operator-held key over SSH with `powder key-create --db
/data/powder.db --name operator --scope admin --show-secret` and store it in
a secret manager.

Set `POWDER_HOME_URL` (unset by default) to render a plain text link back to
that URL in the board's always-visible chrome -- for a deployment fronted by
a portal/home surface Powder itself doesn't own (powder-942). Self-hosters
with no such portal leave it unset and see no change.

### A scoped key for the board UI on a phone (powder-925)

The board's write actions (quick-add a card, change a card's status, claim,
comment, complete) only need `agent` scope, not `admin` -- `admin` is
reserved for repository management and key management, neither
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
