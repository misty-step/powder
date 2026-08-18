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

- `ask` releases the lease.
- `done` and `abandon` clear lease and ask.
- `done` / `abandon` / `ask` / `take` on a terminal job fail `terminal`.
- Field edits: holder, or anyone if not live.
- Anyone may `release` or `answer`.
- One live lease per agent. Default TTL 4h. No heartbeat.

## Faces

One Go binary. `powder serve` plus HTTP CLI. Origin is `POWDER_URL` or
`POWDER_API_BASE_URL`. No `--db` on the client. Peek UI is SSR HTML:
list, show, create, answer, release. Auth is `api-key`, or `none` on
loopback.

## Non-goals

Ranked `next`, Card/Claim/Run/Event, heartbeats, stored status, criteria
checklists, parent/epic, labels, kanban, MCP, SSE, direct-SQLite CLI,
Tailscale auth, Postgres, dispatch, models, telemetry, attachments.
