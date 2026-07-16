# Mutation Operations V1

`powder.operation_request.v1` and `powder.operation_status.v1` provide durable idempotency and bounded outcome recovery for work-log append and permissive completion mutations.
This contract is generic Powder behavior and contains no orchestrator-specific identity or policy.

## Scope

Clients opt in by supplying `operation_id` on `POST /api/v1/cards/{id}/work-log` or `POST /api/v1/cards/{id}/complete`.
The existing routes remain compatible when `operation_id` is absent.
Work-log current-run enforcement, expected-run conditional completion, and run-scoped criterion review are separate contracts.
This contract does not turn claims into lifecycle law or remove the authorized permissive completion and status-correction paths.

## Identity and digest

An operation identity is a caller-generated ASCII string of at most 128 bytes using letters, digits, `-`, `_`, `.`, and `:`.
The caller must allocate one identity before sending the mutation and retain it through response loss, reconnect, and retry.
Powder computes a SHA-256 digest from a length-prefixed canonical stream.
The stream covers schema version, mutation kind, target type, card identity, authenticated authority, expected run when the mutation accepts one, and every bounded payload field in documented order.
HTTP API-key requests use the stable actor identifier behind the verified key, while trusted tailnet and explicit local actors use their stable identity strings.
Human-readable display names remain audit labels and are not treated as authenticated identity when Powder has a stronger identifier.
Length prefixes distinguish absent values, empty values, delimiters inside values, and adjacent fields without relying on JSON object ordering.
The canonical request is limited to 65,536 bytes before hashing.

Work-log operation payloads are ordered as `agent`, `model`, `reasoning`, `harness`, and `body`.
The optional work-log `run_id` is represented as the request's expected-run digest component, but P2 does not validate that it is the card's current run.
The digest covers the caller's original bounded fields before redaction, so distinct raw requests remain conflicting even when their safe projections use the same redaction marker.
Powder scrubs known credential shapes from `agent`, `model`, `reasoning`, `harness`, `run_id`, and `body` before storing the work log, authoritative result, recovery projection, card audit, or outbound event.
Completion operation payloads are ordered as `proof` and the deterministic JSON representation of `criterion_proofs`.

## Lifecycle and replay

Powder reserves the operation, applies the mutation, stores the authoritative result, links the effect event, and records the terminal operation state in one immediate SQLite transaction.
The stored lifecycle vocabulary is `pending`, `succeeded`, `rejected`, and `failed`.
The read contract also returns `unknown` when no retained record exists.
Current SQLite mutations do not expose a committed `pending` record because reservation and terminal outcome share one transaction.
The state remains part of the versioned contract for implementations that can honestly expose an indeterminate committed operation later.

An identical replay returns the original stored operation outcome without applying another effect.
Reuse of the same operation identity with any different digest component returns conflict and applies no part of the second request.
Validation, not-found, conflict, forbidden, and expired-claim domain outcomes may be committed as bounded `rejected` operation records.
An infrastructure error before or during commit rolls back the mutation effect, event, and operation record together, so later status is `unknown`.
The `failed` state is reserved for a terminal failure that Powder can durably and safely record without claiming that a rolled-back effect committed.

## Status recovery

`GET /api/v1/operations/{id}` returns one `powder.operation_status.v1` object.
The creating authenticated authority may read the record, and an administrator may read any record.
Unchecked direct-database operator surfaces retain their existing local trust behavior.
The response contains only bounded digests, identifiers, safe failure metadata, timestamps, the authoritative result, and an audit event identity.
`audit_event_id` always names a durable card-audit event in the `event-*` namespace.
It never names an outbound delivery event in the separate `evt-*` namespace.
Resolve the link by reading `GET /api/v1/cards/{target_card_id}?detail=detailed` and locating the matching `events[].id` value.
It never stores or returns bearer credentials, credential commands, request headers, or unsanitized work-log secrets.

An unknown response is not proof of failure.
Unknown can mean the request never reached Powder, its transaction rolled back, or its recovery record expired.
A client that receives unknown inside the retention window may retry only the byte-equivalent canonical request with the same authenticated authority.
A client that receives succeeded or rejected must converge on that stored outcome.
A client must stop when operation identity reuse returns conflict.

## Failure and delivery behavior

If the response is lost after commit, status lookup returns the committed outcome and an identical retry returns the same result without a duplicate effect.
If the process exits before commit, SQLite rolls back and status returns unknown after restart.
If persistence fails after mutation preparation but before commit, SQLite rolls back the mutation, event, and operation reservation together.
If a connection drops before the client receives a response, the client reconnects and performs status lookup before deciding whether to retry.
Duplicate delivery with the same identity and request converges on one result even when deliveries race.
Conflicting deliveries with one identity serialize at the immediate transaction boundary, so one complete request wins and the other fails without mixed state.

## Bounds and retention

Work-log agent identity is limited to 256 bytes.
Each optional work-log attribution field is limited to 256 bytes.
Work-log body is limited to 16,384 bytes for the operation contract.
Every work-log attribution field and body is scrubbed before any mutation or recovery value is stored.
Completion proof is limited to 4,096 bytes.
Completion accepts at most 128 criterion proof items, and each proof URL is limited to 4,096 bytes.
Safe failure messages are limited to 512 bytes.

Operation recovery records expire seven days after creation and are pruned by operation mutations, status reads, or an explicit store prune.
Retention deletes only recovery metadata and never deletes the card mutation, work log, proof, card event, outbound event, or other audit history.
After retention expires, operation status is unknown and the `event-*` card-audit record remains in detailed card history.
Clients that need the audit identity after expiry must retain it from the successful status response because an unknown response intentionally contains no target or audit metadata.
Clients must not assume an operation identity remains deduplicating after its retention deadline.
Clients needing a longer retry horizon must reconcile the card, run, event, or audit history before issuing a new operation.

## Credential-free examples

The following request appends one retryable work-log entry.

```http
POST /api/v1/cards/example/work-log
Content-Type: application/json

{"operation_id":"client:work-log:01","agent":"worker","run_id":"run-example","body":"focused tests passed"}
```

The following request performs the existing permissive completion with retry recovery.

```http
POST /api/v1/cards/example/complete
Content-Type: application/json

{"operation_id":"client:completion:01","proof":"https://example.test/proof"}
```

The following request recovers either outcome.

```http
GET /api/v1/operations/client:completion:01
```

A successful bounded response has this shape.

```json
{
  "schema_version": "powder.operation_status.v1",
  "operation_id": "client:completion:01",
  "state": "succeeded",
  "request_digest": "sha256:...",
  "kind": "completion",
  "target_card_id": "example",
  "result": {"card_id": "example", "status": "done", "updated_at": 1784179000},
  "audit_event_id": "event-example",
  "created_at": 1784179000,
  "updated_at": 1784179000,
  "expires_at": 1784783800
}
```
