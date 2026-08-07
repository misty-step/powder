---
name: powder
description: |
  Use when an agent must inspect, claim, update, request input for, or complete
  Powder cards. Powder is the self-hostable agent work board. The agent face is
  the `powder` CLI plus this skill — not MCP, not ad-hoc HTTP.
argument-hint: "[list-ready|claim|get-card|complete-card|papercut]"
---

# Powder

Powder stores cards, claims, runs, relations, audit, and proof. It never calls a
model. Real card data lives in a deployed instance database, not in this repo.

Read `VISION.md` before you change product scope, the card/run model, the runner
boundary, or self-hosting.

## Transport

Set both:

```sh
export POWDER_API_BASE_URL=…   # deployed powder-server
export POWDER_API_KEY=…         # integration key
```

Omit `--db` for the deployed board. Pass `--db <path>` only for a local file;
`--db` always wins over the remote env.

Run `powder version` before a lane starts. It prints the installed binary’s git
sha so a stale `~/.cargo/bin/powder` is obvious.

Flag truth is `powder <command> --help` (also `-h`). Help never mutates.

## Operating contract

1. `powder list-ready` before you claim. Claim one card at a time.
2. Cards without acceptance criteria cannot be claimed.
3. `powder get-card <id>` before you implement. Lists are summaries; the card is
   the spec (goal, criteria, proof plan, relations, claim, recent activity).
4. Every `get-card` includes `claim_eligibility` (`eligible`/`code`; `message`
   when ineligible; `blockers` only for `unresolved_blockers`). Codes:
   `eligible`, `no_acceptance`, `unresolved_blockers`, `active_claim`,
   `status_not_claimable`, `in_progress_claim_not_expired`. Use it when a ready
   lane card is missing from `list-ready`.
5. `powder append-work-log` often while you work (context, progress, blockers,
   evidence, attribution). Use `powder add-comment` only for rare human-facing
   notes.
6. File friction the moment you feel it: `powder papercut '…' --agent <label>
   [--service <repo>]`. One call. Do not stop. Do not fix it in that moment.
7. Leave `repo` unset for cross-repo, process, or ops work. Those cards land in
   the General catch-all.
8. Complete with proof when you have it: `powder complete-card <id> --proof <url>`.

## Lane (remote)

```sh
powder list-ready --limit 10 [--repo powder] [--estimate S]
powder claim <id> --agent <worker-label>
powder get-card <id>
powder append-work-log <id> --agent <worker-label> --body "…" [--model …] [--run-id …]
powder add-link <id> --label pr --url https://…
powder check-criterion <id> --criterion 0 --actor <label>
powder complete-card <id> --proof https://…
```

Claim lease: `powder heartbeat|renew-claim|release-claim|transfer-claim` with
`--run <run_id>` from the claim response.

## Discovery

| Need | Command |
|---|---|
| Claimable queue | `powder list-ready` |
| Board shape counts | `powder board-stats` |
| Epic / Unsorted rollups | `powder board-rollups --json` |
| Epic velocity | `powder epic-velocity <epic-id> --json` |
| Filter any status | `powder list-cards --status backlog\|ready\|…` |
| Search text | `powder search '<q>'` |
| One card | `powder get-card <id>` |
| One run | `powder get-run <run-id>` |
| Awaiting input | `powder list-awaiting-input` / `powder list-approvals` |

`list-ready` is dependency-ordered among the returned set. `repo` filters use
exact canonical short slugs or registered aliases — never substring match.

`list-cards` includes every matching status by default, including
`done`/`shipped`/`abandoned`. Pass `--status` to narrow the page.

## Mutations agents use

| Intent | Command |
|---|---|
| Create | `powder create-card --id … --title … --acceptance …` (repeat `--acceptance`) |
| Patch fields | `powder update-card <id> --title … --body … --acceptance …` |
| Status only | `powder update-status <id> --status …` |
| Relations | `powder update-relations <id> --related a,b --blocks c --blocked-by d` |
| Parent epic | `powder set-parent <id> --parent <epic>` / `--clear` |
| Criterion | `powder check-criterion <id> --criterion N --actor …` |
| Link / comment / log | `add-link` / `add-comment` / `append-work-log` |
| Ask operator | `powder request-input <run-id> --question '…'` |
| Answer input | `powder answer-input <run-id> --actor … --answer …` |
| Papercut | `powder papercut '…' --agent …` |
| Done | `powder complete-card <id> [--proof …]` |

Relation writes mirror onto existing peers in one transaction. Parent edges do
not block children; child completion does not complete the parent.

## Papercuts

Papercuts are backlog cards labeled `papercut`. Sweep with:

```sh
powder list-cards --label papercut
```

## Groom cadence

Curators run this outside Powder. Powder does not enforce lifecycle.

1. Page backlog: `powder list-cards --status backlog --updated-before <unix>`.
2. Abandon stale cards with a comment that names the sweep. Never hard-delete.
3. Flag claim violations: list `ready` / `in_progress`, then `get-card` for each
   with no active claim.
4. Emit one operator digest (stale, abandoned, claim-violation counts).

## Telemetry

```sh
powder record-run-telemetry <run-id> --attempts '[…]' --idempotency-key <key>
powder run-telemetry-aggregate [--agent …] [--model …] [--limit 100]
```

Same contract as HTTP. Cost fields use `estimated_cost_usd_micros`.

## Admin / local-only

Repository upsert/merge/delete, key mint/revoke, webhook subscriptions, event
tail, relations-doctor, and markdown/GitHub import stay operator tools. Prefer
`--db` or the HTTP admin API. Agents on a deployed board do not need them for
the claim→complete loop.

## Authority

Remote mode: the API key’s principal is the transport identity. `--agent`,
`--actor`, and `--author` are semantic labels only.

Local `--db` mutations use `POWDER_PRINCIPAL` (else the fixed trusted local-cli
admin principal). `--admin` is rejected.

## Response skew

Unknown future status values must not crash listings. `get-card` / `get-run`
return server JSON as-is. Deploy server first; clients follow.

## Red lines

- Do not call a model from inside Powder.
- Do not commit instance backlog data to this repository.
- Do not treat process exit zero as completion without card status + audit.
- Do not invent a second agent face beside `powder` + this skill.
