# Operations

This is the operator runbook for a deployed Powder instance. It covers
workstation binaries, remote CLI transport, authentication, ready/search
paging, input and proof, and production operations.

For the install path and backup/restore procedure, see
[`docs/self-hosting.md`](self-hosting.md). For the operator's private
DigitalOcean instance, see [`docs/production-deploy.md`](production-deploy.md).

## Workstation binary installation

The canonical local install location for `powder` and, when needed,
`powder-server` is `~/.cargo/bin`. Keep those binaries aligned with the
checkout through the repository script:

```sh
scripts/install-workstation.sh
scripts/install-workstation.sh --with-server
scripts/install-workstation.sh --verify
```

The script reports the before and after `powder version`, refuses a dirty tree
unless `--allow-dirty` is set, and uses a matching release asset at an exact
release tag. It is idempotent. `--verify` exercises repeated acceptance
criteria through the freshly installed binary, not only checkout tests.

`powder version` reports the build git SHA. With `POWDER_API_BASE_URL` set, it
also compares that SHA with the deployed server's `/readyz` value and prints a
`DRIFT` note when they differ. An unreachable server produces a note, not a
local command failure.

## CLI remote-mode transport

The CLI targets either SQLite directly or a deployed `powder-server`. Set
`POWDER_API_BASE_URL` and, for `api-key` deployments, `POWDER_API_KEY`.
`--db` always wins when supplied.

| Command | Local transport | Remote transport | Output |
|---|---|---|---|
| `list-ready` | SQLite query | `GET /api/v1/cards/ready` | `id\tpriority\ttitle` or `no-ready-cards` |
| `list-cards` | SQLite query | `GET /api/v1/cards` | `id\tpriority\tstatus\ttitle` or `no-cards` |
| `search --json` | SQLite FTS query | `GET /api/v1/cards/search` | JSON matches and cursor |
| `get-card` | SQLite detail | `GET /api/v1/cards/{id}` | JSON detail |
| `create-card` | SQLite write | `POST /api/v1/cards` | created card |
| `update-card` | SQLite write | `PATCH /api/v1/cards/{id}` | updated card |
| `claim` | SQLite claim | `POST /api/v1/cards/{id}/claim` | card, run, expiry |
| `heartbeat` | SQLite liveness | `POST /api/v1/cards/{id}/heartbeat` | card, run, expiry |
| `renew-claim` | SQLite lease | `POST /api/v1/cards/{id}/renew` | card, run, expiry |
| `release-claim` | SQLite release | `POST /api/v1/cards/{id}/release` | card and run |
| `update-status` | SQLite status | `POST /api/v1/cards/{id}/status` | card and status |
| `check-criterion` | SQLite criterion | `POST /api/v1/cards/{id}/criteria/check` | criterion result |
| `add-link` | SQLite link | `POST /api/v1/cards/{id}/links` | link id |
| `add-comment` | SQLite comment | `POST /api/v1/cards/{id}/comments` | comment |
| `append-work-log` | SQLite work log | `POST /api/v1/cards/{id}/work-log` | work log |
| `request-input` | SQLite run pause | `POST /api/v1/runs/{id}/input` | awaiting-input |
| `answer-input` | SQLite run resume | `POST /api/v1/runs/{id}/answer` | answered-input |
| `complete-card` | SQLite completion | `POST /api/v1/cards/{id}/complete` | completed card |
| `update-relations` | SQLite relations | `POST /api/v1/cards/{id}/relations` | relation result |
| `set-parent` | SQLite parent edge | `POST /api/v1/cards/{id}/parent` | parent result |
| `get-run` | SQLite run detail | `GET /api/v1/runs/{id}?detail=detailed` | typed run detail |
| `list-awaiting-input` | SQLite awaiting list | `GET /api/v1/runs/awaiting-input?limit=` | awaiting runs |
| `event-tail` | SQLite event tail | `GET /api/v1/events/tail` via SSE | ordered events |

When neither `--db` nor `POWDER_API_BASE_URL` is available, a remote-capable
command exits with a transport error. It never falls back to ephemeral state.
The CLI remains the supported agent face; HTTP is for integrations and the UI.

Local SQLite mutations use the trusted process principal from
`POWDER_PRINCIPAL`, or the fixed local CLI principal when unset. `--actor`,
`--author`, and `--agent` are semantic audit labels. `--admin` is not a
mutation escape hatch.

## Agent CLI workflow

```sh
DB=/tmp/powder-http-smoke/powder.db
mkdir -p "$(dirname "$DB")"
KEY=$(cargo run -q -p powder-cli -- init-db --db "$DB" --show-secret | awk -F '\t' '/bootstrap-key/ {print $4}')
cargo run -q -p powder-cli -- create-card --db "$DB" --id smoke-proof --title "HTTP smoke" --acceptance "lifecycle works" --status ready
POWDER_DB_PATH="$DB" POWDER_AUTH_MODE=api-key POWDER_BIND_ADDR=127.0.0.1:4017 cargo run -q -p powder-server
```

In another shell:

```sh
export POWDER_API_BASE_URL=http://127.0.0.1:4017
export POWDER_API_KEY="$KEY"
powder list-ready --limit 1
powder claim smoke-proof --agent codex
powder request-input "<run-id>" --question "Approve completion?"
powder answer-input "<run-id>" --actor operator --answer approved
powder complete-card smoke-proof --proof https://example.test/proof
```

## Ready paging

`/api/v1/cards` and `/api/v1/cards/ready` cap one response at `limit` cards.
Pass the opaque `next_after` value to continue:

```text
GET /api/v1/cards?limit=20
GET /api/v1/cards?limit=20&after=<next-after>
GET /api/v1/cards/ready?limit=20&after=<next-after>
```

Ready pages use a durable SQLite snapshot cursor bound to the query filters.
The captured order is immutable. Cards that leave eligibility are skipped
without moving the cursor backwards. Cards that arrive during the walk are
appended after captured positions. Malformed, expired, unknown, or
filter-mismatched cursors return `400 Bad Request`.

The optional Card `repo` filter is an exact string filter. It is not a
repository registry or alias lookup.

## Authentication trust boundary

In `api-key` mode, read routes require `Authorization: Bearer <key>` unless
`POWDER_PUBLIC_READS=true` is set on a loopback bind. Mutations always require a
key. Non-loopback listeners reject keyless-read mode.

In `tailscale-header` mode, the trusted ingress must strip all supported
identity headers from client requests and set exactly one from its verified
peer identity. Configure `POWDER_TAILNET_PROXY_SECRET`; Powder rejects missing
or mismatched proxy secrets before it reads an identity header. Configure admin
scope with exact `POWDER_TAILNET_ADMIN_PRINCIPALS` values.

In `none` mode, the private network boundary is the authorization boundary.
Powder accepts `none` only on a loopback bind. The operator production instance
uses a private tailnet ingress and follows this rule intentionally.

## Search and input

`GET /api/v1/cards/search` and `powder search --json` use the same SQLite FTS
query. Search covers card title, body, criteria, comments, and work logs. It
supports status, exact `repo`, label, date, `limit`, and opaque cursor filters. Escape snippets before rendering them as HTML.

Use `list-awaiting-input` to find typed run questions. Use `request-input` to
append a typed question and pause the run. Use `answer-input` to append the
typed response and resume the run. Use links and proof fields for reviewable
evidence. Do not upload attachments or copy sessions into Powder.

## Production operations

Production runs one `powder-server` process on an operator-owned DigitalOcean
host. SQLite lives on a host volume with WAL enabled. Litestream replication is
optional and uses the active S3-compatible endpoint. Verify the live process
with `/healthz` and `/readyz` after each deployment.

Use a WAL-safe database snapshot before a binary or schema change. Run the
non-destructive restore drill in `docs/self-hosting.md` against a scratch path
and read back a known card before trusting the replica.
