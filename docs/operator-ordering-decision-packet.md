# Operator ordering decision — evidence and recommendation packet

Subject: `powder-operator-ordering-decision`
Status: **packet only — the operator decides.** No option is selected here,
no schema, take predicate, list order, or client behavior is changed, and no
VISION/ADR status is edited in this pass.

## 1. Decision to make

Powder's VISION.md says agents take a *known* job and explicitly excludes a
ranked `next`. That boundary keeps Powder free of dispatch and policy. The
observed failure is real: operators have a release-gate chain, but Powder's
list order is creation order and the Iron Forest consumer discards source
order, so neither surface represents the operator's intended order.

The decision is which is the **smallest operator-controlled ordering
mechanism**, among:

1. **External bounded ready-set management** — no Powder schema change.
2. **Optional operator-authored ordinal metadata** — list/read surfaces expose
   it, but Powder never interprets it as `next`, never changes takeability,
   and never dispatches.
3. **Ranked `next` owned by Powder** — a new product responsibility.

## 2. Reproduced evidence

### 2.1 The takeable board is scan-ordered, not operator-ordered

`powder list --takeable --repo misty-step/iron-forest` on the current ledger
returns the following in order (14 at capture time; the ticket's 11 was the
snapshot at filing):

```
1  if-294                                       2026-08-21T18:45:45.946Z
2  if-investigator-credential-projection        2026-08-22T15:03:29.153Z
3  if-300-verifier-publishes-before-review      2026-08-22T15:35:59.843Z
4  if-builder-label-follows-scope               2026-09-01T20:32:20.253Z
5  if-kernel-driven-gate-v2                     2026-09-01T20:33:36.294Z
6  if-gate-requires-request-and-passing-checks  2026-09-01T20:35:17.588Z
7  if-explicit-pi-extension-inputs              2026-09-01T23:09:57.215Z
8  if-portable-default-profile                  2026-09-01T23:10:08.026Z
9  if-critic-agentic-foundation                 2026-09-01T23:13:22.224Z
10 if-complete-review-request-evidence-cutover  2026-09-01T23:27:51.251Z
11 if-fence-sibling-factory-revision-precondition 2026-09-02T00:06:42.227Z
12 if-eval-hard-cost-ceilings                   2026-09-02T20:37:00.396Z
13 if-eval-trusted-execution-events             2026-09-02T20:37:22.613Z
14 if-seed-production-replay-suite              2026-09-02T20:37:33.786Z
```

The order is `created_at` ascending, then `id` ascending. This is the
`Store.List` implementation: the read query is
`ORDER BY created_at ASC, id ASC` (`store.go`), and the CLI/HTTP/UI list
surfaces all forward that same query result.

### 2.2 The P0 chain is text in notes, not machine-readable order

The operator recorded the Habitat release-gate chain in job notes, for example
on `if-habitat-cli-contract` (2026-09-01):

> highest-priority Iron Forest product chain. This chain is the release gate
> for running Iron Forest against any R90 repository. Powder has no accepted
> priority field, so the blocker chain enforces order inside this scope;
> managers must select this chain ahead of unrelated takeable work until
> priority-aware selection is separately accepted.

The chain is therefore double-encoded as (a) human prose and (b) a blocker
graph:

```
if-habitat-r90-readiness
  blocked_by: if-habitat-subject-selection
    blocked_by: if-habitat-cli-contract (terminal), if-profile-tool-cutover
      blocked_by: if-kernel-driven-gate-v2 (takeable, not terminal)
```

The prose says "select this chain ahead of unrelated takeable work"; Powder
itself has no field that expresses that. `if-kernel-driven-gate-v2` is
takeable today, but a naive scan-order consumer would take `if-294` (the
oldest record) well before it.

### 2.3 The consumer can discard source order

The downstream problem is recorded in the quarantined spec of
`if-priority-aware-selection` (`repo: misty-step/iron-forest`,
`blocked_by: [powder-operator-ordering-decision]`):

> selection is scan-order only, so the P0 model-checksum fix (cantrip-68)
> queued behind three small refactors (cantrip-17/25/26). Priority exists
> only as text in migrated specs.

The decision ticket's own spec adds that the Iron Forest consumer "can discard
source order." Either way the consumer is the poll-side selector; it is not
Powder and is not the decision authority for ordering.

## 3. Callers that would or would not consume ordering metadata

Traced against the current Powder tree:

| Surface | Path | Ordered by | Interpreted as dispatch authority? |
| --- | --- | --- | --- |
| SQL read | `Store.List` (`store.go`) | `created_at ASC, id ASC` | No. |
| CLI `list` | `runList` (`cli.go`) | forwards envelope | No. |
| CLI `show` | `runShow`/`emitShow` (`cli.go`) | shows one job, no rank | No. |
| CLI `take` | `runTake` (`cli.go`) | takes a known id | No. |
| HTTP `GET /api/jobs` | `apiList` (`http.go`) | forwards `Store.List` | No. |
| HTTP `GET /api/jobs/{id}` | `apiGet` (`http.go`) | one job, no rank | No. |
| HTTP `POST /api/jobs/{id}/take` | `apiTake` (`http.go`) | known id | No. |
| Peek UI list | `uiList` (`http.go`) | renders `Store.List` | No. |
| Peek UI show | `uiShow` (`http.go`) | one job, no rank | No. |
| Skill | `SKILL.md` | states "scan order, not rank" | No. |
| Take predicate | `Job.takeable`/`derive` (`job.go`) | no ordering input | No. |

Conclusion: today there is no field, and no surface, that treats ordering as
dispatch authority. Adding an ordinal field (option 2) would require explicit
discipline across every row in this table: surface it, but never feed it into
`takeable`, `derive`, `List` ordering, or the Poll selector's authority.

## 4. Option comparison

| Dimension | 1 External bounded ready-set | 2 Ordinal metadata (display-only) | 3 Ranked `next` in Powder |
| --- | --- | --- | --- |
| Schema change | none | new optional field + migration | new field + semantics |
| Take predicate change | none | none (must be proven) | yes |
| `take(id, agent)` semantics | unchanged | unchanged | may change to honor rank |
| Ties | operator resolves in external set | stable key required (e.g. `created_at,id` fallback) | deterministic order source required |
| Absent value | everything still takeable; external set only narrows | field absent → current scan order | absent must have defined default |
| Old clients | unaffected | field ignored; still list by creation order | old clients may mis-order |
| Concurrent edits | external set owner serializes | last-write-wins on an ordinal; needs mutation authority | dispatch races with edits |
| Known-id take | unaffected | unaffected | unaffected only if rank never overrides explicit id |
| Powder stays a dumb ledger | yes | mostly, with new display-only surface | no — becomes roadmap authority |
| Owner of roadmap policy | operator, outside Powder | operator authors, Powder only surfaces | Powder owns it |

### 4.1 Option 1 — external bounded ready-set (recommended)

The operator keeps a small "ready set" outside Powder — for example an
operator-maintained allowlist of subject ids, or a groomeed subset — and the
factory consumes only that subset. Ordering, ties, and the P0 chain are the
operator's property. Powder keeps `created_at,id` scan order and its existing
take predicate untouched. This matches the product lock ("agents take a known
job") and the precedent already in the ledger: operators already encode the
P0 chain with blocker edges plus a prose directive.

Cost: ordering lives where the operator already grooms; no new command.

### 4.2 Option 2 — optional ordinal metadata (display-only)

Add an optional operator-authored ordinal that `list`/`show` surface but the
take predicate and `List` order neither interpret as `next` nor use to change
takeability. Before any implementation it needs its own spec: field type,
mutation authority, stable display order, migration, CLI/API/UI/read surfaces,
and proof that `take(id, agent)` and the take predicate are unchanged.

Risk: even a "display-only" field is adjacent to dispatch, so every caller in
section 3 must be reviewed to prevent accidental interpretation as dispatch
authority.

### 4.3 Option 3 — ranked `next` owned by Powder

Powder would own ranking and the "next" concept, which contradicts VISION.md's
current non-goals and the AGENTS.md red line against a dispatch loop/model
call. It only becomes viable if the operator explicitly amends VISION.md and
justifies the new product responsibility.

## 5. Owner of cross-repository roadmap policy

Roadmap policy — which chain is the release gate, which jobs run before which
others — must remain **outside Powder** unless VISION.md is explicitly
amended. The operator owns it. Powder remains an exclusive-work ledger that
stores jobs, blockers, and provenance, and takes a known id; it does not
decide what is next.

## 6. Recommendation

Retain known-job semantics and use an **external bounded ready-set**
(option 1). Accept option 2 only if observed multi-client operation proves an
external selection mechanism cannot preserve the intended order. Reject
option 3 unless VISION.md is expressly amended.

## 7. What the operator must answer

This packet does not decide. The operator must choose one of:

- **A** — Option 1: external bounded ready-set, no Powder schema change.
- **B** — Option 2: authorize a separate implementation spec for optional
  ordinal display-only metadata (no take-predicate change).
- **C** — Option 3: amend VISION.md and adopt a Powder-owned ranked `next`.

No production schema, take predicate, list order, or client behavior changes
in this decision ticket.