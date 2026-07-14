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
| `answer-input` | SQLite run resume | `POST /api/v1/runs/{id}/answer` | `answered-input\trun_id\tcard_id` |
| `complete-card` | SQLite completion | `POST /api/v1/cards/{id}/complete` | `completed\tid\tstatus` |

When neither `--db` nor `POWDER_API_BASE_URL` is available for a remote-capable
command, the CLI exits with a one-line transport error instead of silently
falling back to ephemeral state. `update-relations`, `set-parent`, `get-run`,
`list-awaiting-input`, `repository-*`, `import-github-issues`,
`key-*`, `subscription-*`, `dead-letter-list`, and `event-tail` remain
`--db`-only (bulk/admin operations, hierarchy/webhook management, or reads
with no remote-mode demand yet); omitting `--db` on those still fails with a
bare `missing --db`.

Commands with no remote-mode transport, verified against `COMMANDS` in
`crates/powder-cli/src/lib.rs`:

| Command | Purpose | Example |
| --- | --- | --- |
| `set-parent` | Link or clear a card's explicit `parent` edge (epic decomposition) | `powder set-parent 002 --db ./data/powder.db --parent 001` / `powder set-parent 002 --db ./data/powder.db --clear` |
| `repository-get` | Read one repository entity by canonical name or alias | `powder repository-get canary --db ./data/powder.db` |
| `repository-delete` | Delete an unused repository entity and its aliases | `powder repository-delete canary --db ./data/powder.db` |
| `subscription-create` | Register a signed webhook subscription (prints the signing secret once with `--show-secret`) | `powder subscription-create --db ./data/powder.db --url http://127.0.0.1:9000/webhook --event-filter moved-to-ready,completed --show-secret` |
| `subscription-list` | List webhook subscriptions without disclosing signing secrets | `powder subscription-list --db ./data/powder.db` |
| `subscription-disable` | Disable a subscription while preserving its delivery history | `powder subscription-disable sub-id --db ./data/powder.db` |
| `dead-letter-list` | List webhook deliveries that exhausted retry attempts | `powder dead-letter-list --db ./data/powder.db` |
| `event-tail` | Page through durable outbound card events (the same feed `GET /api/v1/events/tail` streams as SSE) after a given sequence number | `powder event-tail --db ./data/powder.db --after 0 --limit 20` |

See [`docs/self-hosting.md#webhooks`](self-hosting.md#webhooks) for a full
`subscription-create` -> trigger an event -> `event-tail`/`dead-letter-list`
readback walkthrough against a real local server.

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

For the copy-pasteable quickstart (Docker, release binary, bare-host +
systemd, Fly), the full env-var reference, and the backup/restore story, see
[`docs/self-hosting.md`](self-hosting.md). This section stays focused on the
production posture and lore specific to this repo's own history.

Powder follows the Canary-style deployment pattern:

- one Rust service image
- SQLite database at `POWDER_DB_PATH`
- dual-stack/private-Fly listener at `POWDER_BIND_ADDR`
- Fly volume mounted at `/data`
- optional Litestream replication to Fly Tigris
- `/healthz`, `/readyz`, and `/api/v1/onboarding`
- auth configured by env (`api-key`, `tailscale-header`, or `none`)
- change webhooks configured at runtime via `POST /api/v1/events/subscriptions`
  (`powder subscription-create`), not an env var -- see
  [`docs/self-hosting.md#webhooks`](self-hosting.md#webhooks)
- first-run bootstrap API key, printed once unless
  `POWDER_DISCLOSE_BOOTSTRAP_KEY=false`

Local setup (there is no dotenv loader -- `cp .env.example .env` alone does
nothing until the file is loaded into the process environment):

```sh
set -a; source .env; set +a
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

### Trust boundary for `tailscale-header` auth (powder-tailnet-backstop)

`tailscale-header` mode trusts any request bearing one of four identity
headers (`Tailscale-User-Login`, `X-Tailscale-User-Login`,
`Tailscale-User-Name`, `X-Forwarded-User`) as an authenticated actor. That is
only as safe as the ingress in front of `powder-server`: the proxy must

- **strip** all four identity headers from anything a client sends itself,
  so a request cannot forge an identity by setting the header before the
  proxy would have;
- **set** exactly one of the four headers itself, sourced only from its own
  verified tailnet peer identity (e.g. Tailscale Serve's own
  `Tailscale-User-Login`), never copied from request-supplied data.

`powder-server` cannot independently verify a header its process boundary
receives came from that proxy rather than a client that reached it directly
(a misrouted request, a bypassed ingress, a proxy misconfiguration). Set
`POWDER_TAILNET_PROXY_SECRET` to add an in-code backstop for that gap: when
set, every `tailscale-header`-mode request must also carry a matching
`X-Powder-Proxy-Secret` header (compared in constant time), and requests
missing it or carrying the wrong value are rejected with `401` before the
identity header is even consulted. Configure the trusted proxy to set this
header on every request it forwards, from a value only it and
`powder-server` know. Leaving it unset preserves the original behavior
(any request with a trusted identity header is authorized) -- exactly as
before this backstop existed.

`POWDER_TAILNET_ADMIN` controls the scope granted to a `tailscale-header`
identity. Default (unset, or explicit `true`): every authenticated tailnet
identity gets `admin` scope, matching the mode's original all-admin
behavior -- no config change means no behavior change. Set
`POWDER_TAILNET_ADMIN=false` once a deployment fronts multiple tailnet users
who should not all hold `admin` (repository management, key management,
bulk import): tailnet-authenticated callers still authenticate and can use
claim-scoped routes, but `require_admin`-gated routes reject them with
`403`.

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
provide proof. Card create/update/status/claim-expiry/completion changes are
delivered to any URL registered via `POST /api/v1/events/subscriptions`
(`powder subscription-create --url ... [--event-filter ...]`), each with its
own HMAC signing secret and independent retry/dead-letter tracking -- see
[`docs/self-hosting.md#webhooks`](self-hosting.md#webhooks) for the full
contract and a working local example.

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

### API key lifecycle: minting, storage, and what's recoverable (powder-918)

**Durable key-drop convention: hand-out-at-mint-only.** `powder key-create`
and `powder init-db --show-secret` print a raw secret exactly once, at the
moment of minting, and the store never persists it (see below) -- there is no
"look it up later" recovery path. Capture it directly into the *consumer's*
own secret store (macOS/Linux keychain, 1Password, a CI secret store) in the
same breath as minting it. Do not park a raw key anywhere on the box itself as
a hand-off mechanism -- not a dotfile, not `/tmp`, not `/var/run`. **Incident
(2026-07-04):** a key was left in `/var/run` to hand off between processes;
`/var/run` is `tmpfs` and is wiped on every reboot and every supervisor
restart, so the key silently vanished on the next deploy and had to be
re-minted. If a key needs to reach a second consumer, mint a fresh key for
that consumer and hand it out at mint time again -- never try to relay an
already-minted raw value you no longer hold.

Because there is no durable drop location, `key-create` refuses to mint at
all unless the caller passes exactly one of `--show-secret` (print the raw
key once, with a store-it-now warning) or `--redacted` (explicit
acknowledgment that the secret will be discarded). Minting with neither flag
used to silently print `redacted` and throw the only copy away; refusing is
the honest behavior; a default that prints secrets unasked is worse.

**What's recoverable at rest, and what isn't:**

- **API keys are not recoverable.** `api_keys.key_hash` stores a one-way
  sha256 hash of the raw key (bcrypt for keys minted before the sha256
  migration -- see `crates/powder-store/src/identity.rs`); the raw value is
  never written to the database. A lost raw key means `powder key-revoke`
  the old id and mint a replacement -- there is no database query that gets
  it back.
- **Webhook signing secrets are recoverable.** Unlike API keys,
  `event_subscriptions.signing_secret` is stored in **plaintext**
  (`crates/powder-store/src/events.rs`) because delivery has to compute an
  HMAC signature against it on every webhook POST. The table also carries a
  `signing_secret_hash` column that nothing in the codebase reads back --
  vestigial from an earlier design. Dropping it needs a schema migration;
  that migration is deferred to a follow-up rather than folded into this
  change, to avoid colliding with another lane's `SCHEMA_VERSION` bump (see
  the powder-918 PR notes).
- **The bootstrap admin key** follows the API-key rule above (hashed, not
  recoverable) but has its own disclosure knob, separate from `key-create`:
  `POWDER_DISCLOSE_BOOTSTRAP_KEY` (default `true`, unset) controls whether
  `powder-server`'s first-run seed prints the raw bootstrap key to stderr.
  Production sets this to `false` (see
  [`docs/production-deploy.md`](production-deploy.md)) so the very first
  admin key never lands in `journald`. The code default stays `true` --
  self-hosters running `cargo run -p powder-server` with zero config still
  need to see their first key -- only the production posture changes; see
  production-deploy.md for how an operator gets a usable admin key on a box
  configured with `POWDER_DISCLOSE_BOOTSTRAP_KEY=false`.

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
