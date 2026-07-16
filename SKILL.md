---
name: powder
description: |
  Use when an agent needs to inspect, claim, update, request input for, or
  complete work cards in a Powder instance. Powder is the self-hostable,
  agent-first work board: a durable card store with run sessions, activity,
  audit events, relations, optional proof, and human-in-loop states.
argument-hint: "[list-ready|claim|update-status|update-relations|request-input|complete-card]"
---

# Powder

Powder is a self-hostable work tool. It exposes one core through API, CLI, MCP,
and this skill. Treat cards as context objects with acceptance oracles, not
status rows. Real card data belongs in a deployed instance database, not in the
product repository. Read `VISION.md` before changing Powder's product scope,
card/run model, runner boundary, or self-hosting assumptions.

For local MCP use, set `POWDER_DB_PATH` to the instance SQLite database. A
`POWDER_BACKLOG_DIR` value imports markdown into that database on startup. To
reach a deployed instance instead, set `POWDER_API_BASE_URL` (and
`POWDER_API_KEY`). One of these two must be set — MCP refuses to start
otherwise; there is no ephemeral in-memory mode, since claims and completions
must never silently evaporate on process exit.

By default, `powder-mcp` exposes the agent persona only. Set
`POWDER_MCP_TOOLSETS=admin` or `POWDER_MCP_TOOLSETS=all` before starting the
MCP subprocess to add operator/admin tools to the same server registration.
The value is read once at startup for MCP client cache stability; changing it
requires restarting `powder-mcp`. Calls to hidden admin tools fail with an
error naming `POWDER_MCP_TOOLSETS`.

## Operating Contract

The MCP server `instructions` field is the source of truth for Powder's agent
operating contract. Clients receive it in the initialize response; keep this
skill focused on harness setup, environment variables, and reference details.
When the workflow contract changes, update the server instructions first
(including the claim gate: cards without acceptance criteria cannot be
claimed).

## Expected MCP Tools

Default agent persona:

- `list_ready`: return claimable cards from active repositories sorted by
  priority, age, and identifier.
- `list_cards`: enumerate cards by optional status/repo filter, including
  `blocked`, `review`, and `done` cards `list_ready` never surfaces.
- `list_repositories`: list repository entities with aliases, visibility,
  tier, import provenance, and status counts.
- `manage_claim`: acquire, renew, heartbeat, release, or transfer a claim with
  `action` set to `claim`, `renew`, `heartbeat`, `release`, or `transfer`.
  This pre-1.0 MCP break removed the old `claim_card`, `renew_claim`,
  `heartbeat`, `release_claim`, and `transfer_claim` tools.
- `get_card`: read one card with runs, activities, links, comments, and claim state.
- `get_run`: read one run with its card, activities, links, comments, bound work log, and run state.
- `list_awaiting_input`: list runs paused for human or agent input.
- `answer_input`: append an actor-attributed answer and resume the run.
- `update_status`: set a card to any status in one call and record an audit event.
- `update_relations`: replace a card's `related`, `blocks`, and `blocked_by`
  relation lists.
- `add_link`: attach a PR, CI run, artifact, or reference URL to a card.
- `add_comment`: attach an actor-attributed comment, visible immediately via
  `get_card`/`get_run`.
- `append_run_work_log`: append a retry-safe, fully attributed work-log entry against the exact unexpired current run.
  Supply one stable operation identity and call this often while actively working, not just at completion.
  Powder returns the exact secret-scrubbed stored record through the operation result.
- `append_work_log`: append an explicit permissive unbound/operator note without asserting current-run ownership.
- `request_input`: move the run to `awaiting_input` with the exact question.
- `complete_card`: mark the card done, optionally attaching proof.
- `update_card`: patch title, body, acceptance, proof_plan, status, priority,
  or labels on an existing card. Requires an admin-scope key in remote mode
  (`PATCH /api/v1/cards/{id}`); grooming and editing card content from an
  agent harness no longer requires falling back to raw HTTP.

Admin add-on when `POWDER_MCP_TOOLSETS=admin` or `all`:

- `upsert_repository`: create or update repository settings.
- `merge_repository_alias`: merge duplicate repo strings into one canonical
  repository and audit re-homed cards.
- `delete_repository`: delete an unused repository entity.
- `create_event_subscription`: create a signed webhook subscription.
- `list_event_subscriptions`: list webhook subscriptions without secrets.
- `disable_event_subscription`: disable a webhook subscription while preserving
  delivery history.
- `list_dead_letters`: list webhook deliveries that exhausted retry attempts.
- `tail_events`: read durable card events after an optional sequence cursor.
- `list_keys`: list API-key metadata without raw secrets or hashes.

## Instance CLI

`powder` is remote-capable for the full card and claim-lifecycle workflow:
with `POWDER_API_BASE_URL` and `POWDER_API_KEY` set, `list-ready`,
`list-cards`, `get-card`, `create-card`, `claim`, `heartbeat`, `renew-claim`,
`transfer-claim`, `release-claim`, `update-status`, `check-criterion`, `add-link`,
`add-comment`, `append-run-work-log`, `append-work-log`, `request-input`, and `complete-card` all operate against the
deployed instance when `--db` is omitted -- there is no separate "remote
closeout" wrapper to reach for; the same commands used against `--db` work
unchanged against a deployed instance. `--db` always wins when supplied, so a
local smoke cannot accidentally mutate the deployed board. Run `powder
version` before a lane starts: it reports the git commit the installed
binary was built from, so a stale `~/.cargo/bin/powder` (one that predates a
command's remote-mode support) is obvious instead of surfacing as a bare
`missing --db` error on a command the checkout has long since covered.

A lane closing out a card against a deployed instance -- no local database at
all -- looks like:

```sh
export POWDER_API_BASE_URL=https://powder.internal
export POWDER_API_KEY=sk_powder_...
powder get-card 001
powder add-link 001 --label pr --url https://github.com/misty-step/example/pull/1
powder append-run-work-log 001 --run "$RUN_ID" --operation-id lane:work-log:01 --agent codex --body "narrowed the fix to one function" --model claude-sonnet-5
powder add-comment 001 --author codex --body "shipped, PR linked above"
powder complete-card 001 --proof https://github.com/misty-step/example/pull/1
```

`update-relations`, `get-run`, `list-awaiting-input`, `answer-input`,
`repository-*`, `import*`, `key-*`, and `subscription-*` remain `--db`-only:
they are either bulk/admin operations or read paths with no remote-mode
demand yet. Omitting `--db` on those fails with a bare `missing --db`, not
yet the command-specific transport error the remote-capable commands give.

```sh
powder init-db --db ./data/powder.db --show-secret
powder import backlog.d --db ./data/powder.db
powder list-ready --db ./data/powder.db --limit 10
powder repository-list --db ./data/powder.db --include-hidden
powder repository-upsert --db ./data/powder.db --name canary --aliases misty-step/canary --visibility visible --tier active --import-provenance manual
powder repository-merge-alias --db ./data/powder.db --alias misty-step/canary --into canary --actor operator
powder claim 001 --db ./data/powder.db --agent codex
powder heartbeat 001 --db ./data/powder.db --run run-id
powder renew-claim 001 --db ./data/powder.db --run run-id --ttl 3600
powder transfer-claim 001 --db ./data/powder.db --run run-id --to-agent codex --ttl 3600
powder release-claim 001 --db ./data/powder.db --run run-id
powder get-card 001 --db ./data/powder.db
powder update-relations 001 --db ./data/powder.db --related 002 --blocks 003 --blocked-by 000
powder update-status 001 --db ./data/powder.db --status running
powder request-input run-id --db ./data/powder.db --question "Approve?"
powder list-awaiting-input --db ./data/powder.db
powder answer-input run-id --db ./data/powder.db --actor operator --answer approved
powder get-run run-id --db ./data/powder.db
powder complete-card 001 --db ./data/powder.db
```

## MCP Over HTTP

Set `POWDER_API_BASE_URL` and `POWDER_API_KEY` to run `powder-mcp` against a
live `powder-server` instead of a local SQLite file. A minimal local smoke is:

```sh
DB=/tmp/powder-http-smoke/powder.db
mkdir -p "$(dirname "$DB")"
KEY=$(powder init-db --db "$DB" --show-secret | awk -F '\t' '/bootstrap-key/ {print $4}')
powder import backlog.d --db "$DB"
POWDER_DB_PATH="$DB" POWDER_AUTH_MODE=api-key POWDER_BIND_ADDR=127.0.0.1:4017 powder-server

POWDER_API_BASE_URL=http://127.0.0.1:4017 POWDER_API_KEY="$KEY" powder-mcp
```

For Harness Kit `factory-mcps`, the remote entry shape is `required_env_any:
[[POWDER_API_BASE_URL, POWDER_API_KEY], [POWDER_DB_PATH]]`; the factory remote
variant should populate `POWDER_API_BASE_URL` and `POWDER_API_KEY` from the
Agents vault and run `powder-mcp`.

Registered MCP subprocesses (e.g. a `bash -lc 'source ~/.secrets && exec
powder-mcp'` server entry) resolve `POWDER_API_BASE_URL` from their own
launch environment, which can silently diverge from the value in an
operator's interactive shell (a stale manual export is enough). Send an
`initialize` call and compare `result.serverInfo.baseUrl` against your own
`POWDER_API_BASE_URL` before assuming an add-comment failure is a bug in
Powder rather than two faces pointed at different deployments.

Agents hitting the HTTP API directly, without the CLI or MCP, can read
`GET /api/v1/routes` for the full route contract including example request
bodies -- it names the fields `POST /api/v1/cards` and
`POST /api/v1/cards/{id}/links` actually require instead of leaving that to
deserialize-error trial-and-error.

### Key rotation and stale-key/stale-host runbook (powder-944)

A registered `powder-mcp` subprocess captures `POWDER_API_KEY` (and
`POWDER_API_BASE_URL`) once, at process boot. Rotating the key, or
re-pointing the deployment at a new hostname, does not change an
already-running subprocess's environment -- it keeps sending the old
value until something restarts it. Two ways to handle this:

- **Restart the MCP client** after any key rotation or host cutover. This
  always works and needs no configuration.
- Set `POWDER_API_KEY_CMD` to a shell command that prints a fresh key on
  stdout (e.g. `security find-generic-password -a "$USER" -s
  powder-api-key -w`, or `op read op://Agents/POWDER_API_KEY__bridge/credential`).
  `powder-mcp` runs it once at boot, and again, once, the first time a
  request comes back `401` -- if the command resolves a different key than
  the one that just failed, it transparently retries with the new key and
  the caller never sees the rotation. `POWDER_API_KEY` remains the plain
  fallback; leaving `POWDER_API_KEY_CMD` unset is unchanged behavior.

When both a rotation and a retry are exhausted, or `POWDER_API_KEY_CMD` isn't
set, a `401` error names the key prefix `powder-mcp` used (matching the
`list_keys`/`ApiKeySummary` prefix convention) and says to restart the MCP
client or configure `POWDER_API_KEY_CMD`. A run of three or more consecutive
`404`s on tool calls gets a distinct steer instead: `POWDER_API_BASE_URL` may
be pointed at a stale host (a deployment cutover, powder-965's class of
incident) -- restart the MCP client after fixing the URL.

## Local Gate

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Red Lines

- Do not import from Gradient or Hermes `kanban.db`.
- Do not add personal or operator backlog data to the Powder repository.
- Do not treat exit zero as completion without a status update and audit trail.
