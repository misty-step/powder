# Run-scoped Criterion Review V1

Authenticated criterion review is an atomic, idempotent mutation bound to one card, the card's exact current run, one current criterion identity and index, one decision, one authenticated authority, one stable operation identity, and optional bounded proof.
The contract is generic Powder behavior and does not encode an orchestrator or completion policy.

## Mutation

`POST /api/v1/cards/{card_id}/runs/{expected_run_id}/criteria/review` accepts `operation_id`, `criterion`, `criterion_id`, `decision`, and optional `proof`.
`decision` is `approved`, `rejected`, or `cleared`.
The reviewer is always derived from authenticated authority.
There is no reviewer or actor request field.
An authenticated non-admin authority must hold the current claim.
An authenticated administrator may review as an auditable operator correction, but the card must still have the specified live current run.
Unchecked and label-only local authority cannot create authoritative run-scoped review state.
Direct-database CLI and stdio MCP retain the explicit legacy `check-criterion` correction path, whose `checked_by` and `checked_at` fields remain non-authoritative for run-scoped completion.

The operation digest uses `powder.operation_request.v1` with kind `criterion_review`.
Its target is the card, its expected-run component is the specified run, and ordered payload fields are `criterion_index`, `criterion_id`, `decision`, and `proof`.
The result and recovery record use `powder.operation_status.v1` unchanged.
The operation retention window and replay rules are unchanged.

Proof is optional, trimmed, and limited to 4,096 UTF-8 bytes.
An empty supplied proof is rejected.
Known credential shapes are scrubbed before proof appears in history, events, operation results, or read projections, while the operation digest still distinguishes the original bounded request.
The criterion index must fit the platform-independent unsigned 32-bit range before store access.

## Criterion identity

`criterion_id` has the form `powder.criterion.v1:sha256:<exact-text-digest>:<duplicate-occurrence>`.
The digest covers the exact stored UTF-8 criterion text.
The duplicate occurrence is the zero-based count of preceding criteria with identical text.
Reordering distinct criteria preserves identity.
Editing text changes identity and makes all reviews of the old text historical only.
Inserting, deleting, or reordering identical duplicate text may change duplicate occurrence identity and therefore fails closed until the caller reads the new projection.

The request supplies both index and identity.
Powder validates both against the card inside the same immediate transaction that validates the current run, lease, authority, operation replay, and review insert.
This prevents a stale index from approving different text after an edit or reorder.

## Current-run validation

The expected run must exist, belong to the exact card, be the card's current claim run, and have an unexpired claim.
The current claim holder must match a non-admin authenticated reviewer.
Released runs, expired leases, reclaimed runs, another card's runs, and non-current later runs are rejected without a review, card update, activity, or approval side effect.
Rejected domain outcomes are retained as rejected operations when request construction itself was valid.

## History and current state

Every successful action inserts an immutable `CriterionReview` history row and a matching card event.
The row snapshots card, run, criterion index, criterion identity, exact criterion text, decision, reviewer, proof, operation identity, time, and the prior review it supersedes.
An identical operation replay returns the original result and inserts nothing.
A conflicting replay returns conflict and inserts nothing.

The latest successful action for one run and criterion identity is current for that run.
`cleared` is an explicit uncheck action: it supersedes the previous action and is not approval.
Re-review uses a new operation identity, appends history, and supersedes the prior action.
`rejected` is an explicit current decision and is not approval.
Operator correction uses the same append-only re-review or clear mechanism, preserving the corrected row in history.

Card detail exposes `current_run_criteria` only for the card's current claim run and exposes `criterion_reviews` as audit history.
Run detail exposes `criteria` projected for that exact run and its `criterion_reviews` history.
Each projection contains the current criterion index, identity, and text plus the latest matching review when one exists.
P1 must consume only this projection and treat a criterion as approved exactly when its latest matching review has `decision: "approved"`.
Legacy `AcceptanceCriterion.checked_by` and `checked_at` are not authoritative for run-scoped completion.

A later claim creates a different run and begins with no current review state, even when an earlier run approved identical criterion text.
Earlier reviews remain visible in history but never appear as the later run's current review.
After release or expiry, card detail has no `current_run_criteria`; run detail still shows that run's historical projection.
