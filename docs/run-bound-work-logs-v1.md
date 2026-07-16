# Run-Bound Work Logs V1

`powder.work_log_entry.v1` is Powder's authoritative stored record for work-log history.
The strict append path binds that record to the exact current run at one atomic SQLite transaction boundary.

## Surfaces

HTTP clients call `POST /api/v1/cards/{card_id}/runs/{expected_run_id}/work-log`.
The JSON body requires `operation_id`, `agent`, and `body`.
The optional attribution fields are `model`, `reasoning`, and `harness`.
CLI clients call `powder append-run-work-log CARD --run RUN --operation-id OPERATION --agent AGENT --actor ACTOR --body BODY` for direct database access.
Remote CLI identity comes from the configured bearer key, so `--actor` is not transmitted.
MCP clients call `append_run_work_log` with `operation_id`, `card_id`, `expected_run_id`, `agent`, `actor`, and `body` when using the local store.
Remote MCP identity comes from the bearer key, so local `actor` and `admin` arguments are not transmitted.

`POST /api/v1/cards/{card_id}/work-log`, `powder append-work-log`, and MCP `append_work_log` remain the explicit permissive compatibility path for unbound operator notes and corrections.
That path does not assert that an optional run attribution is current.
Strict agent progress must use the run-bound path.

## Request and authority contract

`operation_id` follows the existing `powder.operation_request.v1` limit and alphabet.
The request digest uses the approved `work_log_append` operation kind and the existing ordered work-log payload fields.
`expected_run_id` occupies the operation request's expected-run component.
This contract does not change `powder.operation_status.v1`, its digest algorithm, retention window, replay behavior, or authority-scoped recovery.

`agent` is required, non-empty, and limited to 256 bytes.
Each optional attribution value is non-empty when present and limited to 256 bytes.
`body` is required, non-empty, and limited to 16,384 bytes.
The expected run must exist, belong to the exact card, be active or awaiting input, own the card's current claim, and remain unexpired at the transaction boundary.
The supplied agent must own both the run and the current claim.
The authenticated or explicit actor must be authorized for that agent and current claim.
Unchecked direct database access and HTTP auth mode `none` cannot use the strict path.

Unknown runs, card-run mismatch, stale runs, released runs, expired runs, reclaimed runs, foreign agents, malformed attribution, bound violations, and unauthorized actors append no work-log entry, card audit event, or outbound work-log event.
Domain rejection may still be recorded in the bounded operation recovery ledger as required by `powder.operation_status.v1`.
Validation that cannot form a valid operation request returns an error before operation reservation.

## Atomicity and retry

Powder opens an immediate SQLite transaction, prunes expired recovery metadata, and checks for an existing operation identity before reevaluating current run state.
An identical retry therefore returns the original result even if the claim was released or expired after the successful append.
A new operation against that released or expired run is rejected.
Operation reservation, current-run validation, work-log insertion, card audit insertion, outbound event insertion, and terminal operation status commit together.
Infrastructure failure rolls the entire transaction back.
Concurrent identical delivery converges on one entry and one normalized result.
Conflicting reuse of the operation identity retains the existing conflict behavior.

## Stored and returned record

A successful operation returns `powder.operation_status.v1` with `state` set to `succeeded`.
Its `result` is the exact normalized record stored in `work_log_entries`.

```json
{
  "schema_version": "powder.work_log_entry.v1",
  "id": "work-log-stable-entry-id",
  "card_id": "example-card",
  "actor": "authenticated-actor",
  "agent": "claiming-agent",
  "model": "optional-model",
  "reasoning": "optional-level",
  "harness": "optional-harness",
  "run_id": "run-current",
  "body": "normalized body",
  "created_at": 1784189000,
  "updated_at": 1784189000
}
```

The stable `id` is generated once inside the transaction and begins with `work-log-`.
`created_at` and `updated_at` are public Unix timestamps and are equal for this append-only V1 record.
Known credential shapes are scrubbed from actor, agent, optional attribution, run attribution, and body before persistence.
The stored record, operation result, card detail record, and run detail record serialize identically after scrubbing.

Card detail includes the record in `work_log`.
Run detail includes only records whose `run_id` equals that run.
Concise and detailed ordering follows the existing history section contract.

## Audit and outbound event behavior

The operation's `audit_event_id` identifies a durable card audit event with event type `work_log`.
Its JSON payload contains `schema_version`, `entry_id`, `run_id`, `agent`, `model`, `reasoning`, and `harness`.
The audit actor equals the normalized record actor.
The audit event never stores the work-log body.

Powder also emits one `powder.card_event.v1` envelope with event type `work-log-appended`.
The envelope actor equals the normalized record actor.
The exact normalized stored record appears at `change.work_log`.
An identical retry emits no additional audit or outbound event.

## Compatibility boundary

This strict operation does not change permissive status correction, permissive completion, comments, links, or the unbound work-log path.
It does not implement run-scoped criterion review or expected-run conditional completion.
It reuses the approved operation substrate without changing operation status vocabulary, recovery authorization, digest framing, or retention.

## P3 and P4 migration composition

The authorized integration composes the migrations in this order:

1. P3 work-log actor, update timestamp, and `(run_id, created_at, id)` index migrate schema 14 to 15.
2. P4 `criterion_review` operation kind and its additive `mutation_operations` constraint migrate schema 15 to 16.
3. P4 review history migrates schema 16 to 17.
4. P4 stable reviewer identity and run-authority binding migrate schema 17 to 18.

Fresh schema creation and every supported upgrade boundary end at schema 18.
The composed migration suite preserves existing work-log rows, operation recovery rows, and review history.
