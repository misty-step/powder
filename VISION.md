# Powder Vision

A self-hosted exclusive-work service. Agents take a known job. They do
not ask the system what is “next.”

This is a new product. It does not inherit the Rust Powder schema.

## Job record

```
Job
  id            slug, caller-assigned
  title
  spec          markdown; empty allowed
  repo          optional exact string
  blocked_by    []id          # direct edges only
  lease         null | { agent, principal, until }
  ask           null | { question, by, at }
  proof         null | string
  abandoned     bool
  notes         []{ at, by, text }
  created_by    null | principal id   # immutable creator
  promoted_by   null | principal id   # immutable first promoter
  promoted_at   null | timestamp      # first promotion time
  promotions    []{ at, by, spec }    # auditable spec-write history
```

Derived, never stored:

- `terminal` = proof set OR abandoned
- `waiting`  = ask set AND not terminal
- `open`     = not terminal and not waiting
- `live`     = lease present and `until` > now
- `takeable` = open AND spec nonempty AND not live AND every
  **direct** `blocked_by` id exists and is terminal

Missing blocker → not takeable. A cycle of non-terminal jobs is not
takeable because no member is terminal.

Invariant: **`terminal ⇒ ¬live`**.

## Authority

API keys carry repository-scoped capabilities:

- `report`: create a job with an empty spec and add notes in the key's
  repository.
- `promote`: create or set a nonempty spec in the key's repository and
  perform lifecycle work.

A key is scoped to one exact repository, or to every repository when its
scope is null. Existing keys predate capability enforcement and migrate to
report+promote over every repository. New keys are fail-closed and require
explicit capabilities. Existing jobs migrate with null provenance.

## Take

```
take(id, agent) succeeds iff
  job is takeable
  and this agent holds no other live lease
```

Atomic. If you already hold `id`, return it.

## Verbs

`create` `show` `list` `take` `release` `renew` `note` `ask` `answer`
`done` `abandon` `reopen` `set-title` `set-spec` `set-repo` `set-blockers`
`use` `doctor`

- `ask` releases the lease.
- `done` and `abandon` clear lease and ask.
- `done` / `abandon` / `ask` / `take` on a terminal job fail `terminal`.
- Field edits (`set-title`, `set-spec`, `set-repo`, `set-blockers`)
  require `promote` capability in the job's repository.
- `note` requires `report` capability in the job's repository.
- Lifecycle verbs (`take`, `renew`, `release`, `ask`, `answer`, `done`,
  `abandon`, `reopen`) require `promote` capability in the job's
  repository.
- Creating a job with a nonempty spec is promotion and requires `promote`;
  creating an empty-spec draft requires `report` or `promote`.
- One live lease per holder identity. Default TTL 4h. No heartbeat.
- The CLI resolves the holder from `--agent`, `POWDER_AGENT`, config `agent`,
  then `user@host`. `POWDER_AGENT` is workload identity; `POWDER_API_KEY` is
  transport authentication. The default is machine-global across repositories;
  parallel workers use distinct holders and subagents inherit their parent's
  holder.

## Faces

One Go binary. `powder serve` plus HTTP CLI. The client origin is explicit:
`POWDER_URL` overrides `~/.config/powder/config`; there is no default origin or
local-ledger fallback. `powder use <url>` writes the config and `powder doctor`
shows the resolved connection without exposing key material. No `--db` on the
client. Peek UI is SSR HTML: list, show, create, answer, release. Auth is
`api-key`, or `none` on loopback.

## Non-goals

Ranked `next`, Card/Claim/Run/Event, heartbeats, stored status, criteria
checklists, parent/epic, labels, kanban, MCP, SSE, direct-SQLite CLI,
Tailscale auth, Postgres, dispatch, models, telemetry, attachments.
