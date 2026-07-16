# Run-Bound Conditional Completion v1

`POST /api/v1/cards/{card_id}/runs/{expected_run_id}/complete` is Powder's strict completion contract for concurrent workers.
It accepts a required stable `operation_id`, optional bounded completion `proof`, and optional bounded `criterion_proofs`.
It returns `powder.operation_status.v1` whose successful result is a `powder.run_bound_completion.v1` receipt.

## Atomic preconditions

Powder validates and completes inside one immediate SQLite transaction.
The card must have the supplied run as its current claim, the claim must be unexpired, the stored run must belong to the card and remain active or awaiting input, and the authority must hold the claim or be an administrator.
Every current criterion must have a latest matching run-scoped review whose decision is `approved`.
Legacy `checked_by` and `checked_at` fields do not satisfy this rule.

A missing, released, expired, terminal, foreign, or reclaimed run produces a rejected operation with no completion, proof, activity, audit, outbound event, or run-B mutation.
The legacy card-only completion endpoint remains the explicit permissive operator-correction path.

## Replay and recovery

The operation request digest binds the completion kind, card, authenticated authority, expected run, proof, and deterministic criterion-proof payload.
An identical retry returns the original outcome without repeating completion effects.
Reusing an operation identity with a different kind, target, authority, expected run, proof, or criterion proof fails with a conflict.
`GET /api/v1/operations/{operation_id}` recovers the same bounded outcome during the documented operation-retention window.

## Receipt

The successful receipt contains the schema version, card identity, expected run identity, operation identity, resulting status, accepted proof metadata, update time, and audit event identity.
Known credential shapes are scrubbed before operation results are persisted or returned.

## Client surfaces

The CLI selects strict completion when `complete-card` receives both `--run RUN_ID` and `--operation-id OPERATION_ID`.
MCP selects strict completion when `complete_card` receives both `expected_run_id` and `operation_id`.
Omitting the expected run selects the separate permissive completion contract.
