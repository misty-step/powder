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
  lease         null | { audit_label, until }  # claim token is never stored raw
  lease_token_hash null | SHA-256(claim_token) # internal persistence
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
take(authz, id, audit_label, claim_token) succeeds iff
  authz has promote capability for the job repository and
  ((job is live and claim_token matches) or job is takeable)
```

Atomic. `POST /api/v2/jobs/{id}/take` returns a flat JSON Job plus
`claim_token`, a per-job capability made from 32 random bytes encoded as
base64url. The old take endpoint is absent so version-skewed clients fail before
mutation. A take of a live job returns `held` unless it presents that job's
matching claim token, which resumes the existing claim. Audit labels never
grant resume; distinct jobs may be live under one label. The raw token is never
included in list, show, logs, or notes.

The CLI stores tokens in private XDG state, namespaced by normalized origin and
job id. It resumes by job id and sends the token automatically without printing
it. `release`, `renew`, `ask`, `done`, live-job field edits, and live `abandon`
require the claim token. Missing is `claim_required`; a mismatched or expired
token is `invalid_claim`. The CLI deletes a token after release, ask, done, or
abandon.

## Verbs

`create` `show` `list` `take` `release` `renew` `note` `ask` `answer`
`done` `abandon` `reopen` `set-title` `set-spec` `set-repo` `set-blockers`
`use` `doctor`

- `ask` releases the lease and clears its claim.
- `done` and `abandon` clear the lease, claim, and ask.
- `done` / `abandon` / `ask` / `take` on a terminal job fail `terminal`.
- Field edits (`set-title`, `set-spec`, `set-repo`, `set-blockers`) on a live
  job require both `promote` capability in the job's repository and its claim.
- `note` requires `report` capability and stays claim-independent so reporters
  can append evidence to any scoped job.
- Lifecycle verbs (`renew`, `release`, `ask`, `done`, `abandon`) require
  `promote` capability and the claim for a live job.
- A free-job patch or abandon uses `promote` capability without a claim.
- Creating a job with a nonempty spec is promotion and requires `promote`;
  creating an empty-spec draft requires `report` or `promote`.
- Default TTL is 4h. There is no heartbeat.
- `POWDER_AGENT` is optional audit metadata, including the canonical
  `forest-misty-step/powder` label passed by managed workers; it is never
  authorization. `POWDER_API_KEY` authenticates transport.

## Faces

One Go binary. `powder serve` plus HTTP CLI. The client origin is explicit:
`POWDER_URL` overrides `~/.config/powder/config`; there is no default origin or
local-ledger fallback. Remote origins require HTTPS; HTTP is loopback-only.
`powder use <url>` writes the normalized config and `powder doctor` shows the
resolved connection without exposing key material. No `--db` on the client.
Peek UI is SSR HTML: list, show, create, and answer. Claim-bound
lifecycle actions stay on the CLI/API, where the claimant can present its token.
Auth is `api-key`, or `none` on loopback.

## Non-goals

Ranked `next`, Card/Run/Event, heartbeats, stored status, criteria checklists,
parent/epic, labels, kanban, MCP, SSE, direct-SQLite CLI, Tailscale auth,
Postgres, dispatch, models, telemetry, attachments.
