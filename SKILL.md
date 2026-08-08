---
name: powder
description: |
  Use when an agent must inspect, claim, update, request input for, or complete
  Powder cards. Powder is a self-hosted Card, Claim, typed Run, and Event
  ledger. The supported agent face is the `powder` CLI plus this skill.
argument-hint: "[create-card|list-awaiting-input]"
---

# Powder

Powder stores cards, current claims, typed run history, relations, audit events,
comments, work logs, input activity, and proof. It never calls a model. Real
card data lives in a deployed SQLite database, not in this repository.

Read `VISION.md` before changing product scope, the Card/Claim/Run/Event model,
the runner boundary, or self-hosting.

## Transport

For a deployed board, set:

```sh
export POWDER_API_BASE_URL=<deployed-powder-server>
export POWDER_API_KEY=<integration-key>
```

Omit `--db` for the deployed board. Pass `--db <path>` only for a local file;
`--db` always wins over remote environment variables.

Run `powder version` before a lane starts. It prints the installed binary git
SHA so a stale CLI is visible.

Flag truth is `powder <command> --help` (also `-h`). Help never mutates data.

## Operating Contract

1. Run `powder list-ready` before claiming. Claim one card at a time.
2. A card without acceptance criteria cannot be claimed.
3. Run `powder get-card <id>` before implementation. The card is the spec:
   goal, criteria, proof plan, relations, claim, and recent activity.
4. Use `claim_eligibility` when a card is missing from `list-ready`. It reports
   `eligible` or a reason code such as `no_acceptance`,
   `unresolved_blockers`, `active_claim`, or `status_not_claimable`.
5. Append a work log during work. Include context, progress, blockers,
   evidence, and attribution. Use `add-comment` only for a human-facing note.
6. Leave `repo` unset for cross-repository, process, or operations work. When
   set, it is an opaque card string and filters match it exactly.
7. Complete only after the card status, audit event, and proof are present.

## Lifecycle

```sh
powder list-ready --limit 10
powder claim <card-id> --agent <worker-label>
powder get-card <card-id>
powder heartbeat <card-id> --run <run-id>
powder append-work-log <card-id> --agent <worker-label> --body '...'
powder request-input <run-id> --question '...'
powder answer-input <run-id> --actor <label> --answer '...'
powder check-criterion <card-id> --criterion <index> --actor <label>
powder complete-card <card-id> --proof <url>
```

The claim response supplies the `run_id` and lease expiry. Use that run for
heartbeat, renewal, release, input, and completion operations. A claim expires
when its worker stops renewing it.

## Discovery

| Need | Command |
|---|---|
| Claimable cards | `powder list-ready` |
| Any status | `powder list-cards --status backlog\|ready\|…` |
| Text search | `powder search '<q>'` |
| One card | `powder get-card <id>` |
| One run | `powder get-run <run-id>` |
| Awaiting input | `powder list-awaiting-input` |
| Ordered events | `powder event-tail --after 0 --limit 20` |

`list-ready` is dependency-ordered among returned cards. Ready snapshots use
opaque continuation values; pass the returned value unchanged when paging.
Search uses the store full-text index and returns untrusted snippets. Escape
snippets before rendering HTML.

## Mutations

| Intent | Command |
|---|---|
| Create | `powder create-card --id … --title … --acceptance …` |
| Patch fields | `powder update-card <id> --title … --body … --acceptance …` |
| Status only | `powder update-status <id> --status …` |
| Relations | `powder update-relations <id> --related a,b --blocks c --blocked-by d` |
| Parent edge | `powder set-parent <id> --parent <id>` or `--clear` |
| Criterion | `powder check-criterion <id> --criterion N --actor …` |
| Link | `powder add-link <id> --label proof --url <url>` |
| Comment | `powder add-comment <id> --author <label> --body '…'` |
| Work log | `powder append-work-log <id> --agent <label> --body '…'` |
| Ask operator | `powder request-input <run-id> --question '…'` |
| Answer input | `powder answer-input <run-id> --actor … --answer …` |
| Done | `powder complete-card <id> [--proof <url>]` |

Relation writes mirror existing peers in one transaction. Parent edges are
reference edges; they do not create an epic or rollup product. Child completion
does not complete a parent.

## Input And Proof

Use `list-awaiting-input` to find runs that need an operator response. Use
`request-input` to append a typed question activity and move the run to
`awaiting_input`. Use `answer-input` to append the typed response and resume the
run. Do not create a separate answer record or put typed activity into an
arbitrary JSON blob.

Use links and proof fields for reviewable evidence. Do not upload attachments
or copy prompts, tool traces, or sessions into Powder.

## Authority

In remote mode, the API key principal is the transport identity. `--agent`,
`--actor`, and `--author` are semantic labels in the audit trail.

Local `--db` mutations use `POWDER_PRINCIPAL`, or the fixed trusted local CLI
principal when it is unset. `--admin` is not a supported escape hatch.

## Response Skew

Unknown future status values must not crash listings. `get-card` and `get-run`
return server JSON as-is. Deploy the server first; clients follow.

## Red Lines

- Do not call a model from inside Powder.
- Do not commit instance backlog data to this repository.
- Do not treat process exit zero as completion without card status and audit.
- Do not add a second agent face beside `powder` plus this skill.
- Do not add repository registries, telemetry analytics, media storage,
  portfolio rollups, signed webhook delivery, ingestion, shaping, or session
  forensics.
