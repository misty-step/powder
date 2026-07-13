# Powder Card Event Schema v1

Powder emits deterministic card events through the SQLite outbox, signed
webhooks, and the SSE tail. The schema identifier is
`powder.card_event.v1`.

## Envelope

```json
{
  "schema_version": "powder.card_event.v1",
  "event_id": "evt-example",
  "event_type": "moved-to-ready",
  "occurred_at": 1783137600,
  "actor": "operator",
  "card": {},
  "change": {}
}
```

`card.status` is a string. The 020 state-model collapse may change Powder's
internal status vocabulary later, but it does not require a v1 schema break as
long as the payload continues to carry status as a string.

## Vocabulary

| event_type | emitted when | change fields |
| --- | --- | --- |
| `card-created` | a card is created through API, CLI, MCP, or import helpers that opt into event emission | `source` |
| `moved-to-ready` | a card's status becomes `ready`, including explicit release to ready | `previous_status`, `status`, or release metadata |
| `claim-expired` | Powder observes an expired active claim while reclaiming the card | `claim_id`, `runtime_ref`, `agent`, `expired_at` |
| `completed` | a card reaches a terminal completion path | `previous_status`, `status`, optional `proof` |
| `comment-added` | an actor adds a card comment | `author`, `body` |

## Webhook Signing

Webhook deliveries send the raw JSON event body with:

```text
X-Signature-256: sha256=<hex hmac-sha256>
```

The HMAC key is the subscription signing secret returned only at creation.
Receivers should compute HMAC-SHA256 over the raw request body and compare the
full header value in constant time. This matches the weave release-events
receiver contract.

## Delivery

Webhook delivery is at least once. Powder stores the event before attempting
delivery, retries failed webhook attempts with backoff, and marks a delivery
`dead_letter` after the retry budget is exhausted. Consumers should dedupe by
`event_id`.
