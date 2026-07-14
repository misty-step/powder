# Status Vocabulary Decision (powder-status-vocabulary)

Ratified 2026-07-14 under the operator constraints recorded on
powder-epic-state-model: collapse toward a smaller lane model only where it
genuinely simplifies; status stays freely settable by any authorized actor
with or without a claim; `awaiting_input` stays first-class and queryable;
the TTL claim/lease model is untouched; the three terminal outcomes stay
distinguishable.

## The Vocabulary

Seven statuses, down from the prior nine:

| Status | Meaning |
| --- | --- |
| `backlog` | Filed but carries no acceptance oracle yet (or deliberately parked). The create-time default for a card with empty acceptance. |
| `ready` | Carries a real oracle and is claimable once its blockers (if any) resolve. The create-time default for a card with acceptance. |
| `in_progress` | An agent is actively working the card. Who holds it, the lease, and liveness live on the claim struct, not in the status. |
| `awaiting_input` | The run is parked on an operator question (first-class, queryable via `list_awaiting_input`/`list_approvals`). |
| `done` | Terminal: completed. |
| `shipped` | Terminal: completed and deployed/released. |
| `abandoned` | Terminal: deliberately not completed. |

## The 9 -> 7 Mapping (schema v17)

Applied by `migrate_16_to_17` in `powder-store` on the next deploy, with one
audit `card_events` row per changed card
(`"status-vocabulary migration: <old> -> <new>"`, actor
`system:status-vocabulary-migration`). Claims, runs, relations, and all
existing events are untouched; only the `status` column on affected cards
changes.

| Legacy status | New status | Why |
| --- | --- | --- |
| `backlog` | `backlog` | Unchanged. |
| `ready` | `ready` | Unchanged. |
| `claimed` | `in_progress` | The claimed/running distinction duplicated claim presence -- the claim struct already carries who/lease/liveness. |
| `running` | `in_progress` | Same collapse; a status bit distinguishing "claimed but not yet running" from "running" was a second, driftable copy of the claim. |
| `blocked` (non-empty acceptance) | `ready` | Blocking is derived from unresolved `blocked_by` relations (`Card::claim_readiness`), not stored as a status -- see below. |
| `blocked` (empty acceptance) | `backlog` | Mirrors `CardStatus::default_for_acceptance`, the same rule a freshly created card is defaulted by ("ready is a query, not vibes"). |
| `awaiting_input` | `awaiting_input` | Unchanged; first-class per operator ruling. |
| `done` | `done` | Unchanged. |
| `shipped` | `shipped` | Unchanged; terminal outcomes stay distinguishable per operator ruling. |
| `abandoned` | `abandoned` | Unchanged. |

The retired names (`claimed`, `running`, `blocked`) are **rejected** by
`update_status`/`create_card`/list filters on every face (HTTP, CLI, MCP)
with an error naming the current vocabulary -- never silently aliased onto a
surviving status. `in-progress`/`in_progress` and `pending` (a long-standing
alias for `backlog`) still parse; they were never statuses of their own.

## Terminal Outcomes Stay Distinguishable

`done`, `shipped`, and `abandoned` remain three distinct statuses in the
enum, the store, the wire vocabulary, and the board's DONE lane (distinct
badges). `CardStatus::is_terminal` remains the single definition of
"terminal" that blocker resolution, reimport lifecycle protection, and the
board's DONE lane all share.

## Why Blocked Is Not A Status

Claim eligibility already derives blocked-ness from relations:
`Card::claim_readiness` (the single seam behind `is_ready_at`,
`can_be_claimed_at`, and `apply_claim`) rejects any card with an unresolved
`blocked_by` entry regardless of its status, failing closed when a blocker id
cannot be found. An explicit `blocked` status was therefore a second,
driftable copy of a derived fact: a card could sit `blocked` after every
blocker resolved, or sit `ready` while genuinely blocked, and nothing
reconciled the two. The 020 migration rehearsal mapped `blocked -> ready`
for exactly this reason. The board still shows a BLOCKED strip -- derived
the same way, from unresolved `blocked_by` relations on ready cards.

## UI Lanes

- **READY** = `ready` without unresolved blockers; ready cards *with*
  unresolved blockers render in the derived BLOCKED strip beneath the lane.
- **IN PROGRESS** = `in_progress` + `awaiting_input` (the awaiting-you
  strip and per-card glyph already differentiate the latter).
- **DONE** = `done` + `shipped` + `abandoned`, with distinct glyphs.
- The backlog rail = `backlog`.

## Rejected Alternatives

- **Assignee-as-claim (the 020 status-model design).** Rejected by operator
  ruling: the TTL claim/lease model stays untouched. Folding the claim into
  a plain `assignee` string would have discarded lease expiry and liveness
  -- the exact machinery that makes concurrent agents safe. The dormant
  `status_model_020` rehearsal machinery built for that design (~1200 LOC
  module, bin, and integration test) was deleted, and a smaller rehearsal
  test (`crates/powder-store/tests/status_vocabulary_migration.rs`) now
  exercises the real `migrate_16_to_17` path against the same synthetic
  snapshot fixture, extended with the two `blocked` edge cases. Deleting and
  rewriting was less total code than repurposing: the old machinery's
  mapping table, rewrite SQL, and all eight oracles encoded the rejected
  three-status/assignee design, so "repurposing" it would have replaced
  every load-bearing line anyway while keeping ~700 lines of
  parallel-simulation scaffolding the real-migration test does not need.
- **Keeping `blocked` as a status.** Rejected because eligibility is
  already derived from `blocked_by` relations (see above); a stored copy of
  a derived fact drifts.
- **Collapsing terminal outcomes into one `done`.** Rejected by operator
  ruling: done-vs-shipped-vs-abandoned is a real distinction operators
  query.
- **Folding `awaiting_input` into `in_progress`.** Rejected by operator
  ruling: awaiting input is a first-class, queryable state ("human input is
  a state", VISION.md), even though the board renders it in the IN PROGRESS
  lane.
