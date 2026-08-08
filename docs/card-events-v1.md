# Powder Card Event Schema v1

Powder emits deterministic attributed card events through the SQLite outbox and
ordered SSE tail. The schema identifier is `powder.card_event.v1`.

## Envelope

```json
{
  "schema_version": "powder.card_event.v1",
  "event_id": "evt-example",
  "event_type": "moved-to-ready",
  "occurred_at": 1783137600,
  "actor": "operator",
  "card": {},
  "change": {
    "kind": "status",
    "previous": "backlog",
    "current": "ready"
  }
}
```

`change` is a tagged typed change. Audit history and the outbound tail use the
same vocabulary. Retired attachment, repository, rollup, decompose, update,
and import forms remain readable through explicit read-only variants. Unknown
or malformed stored changes fail with event-data errors.

## Vocabulary

| Event type | Emitted when | Change fields |
|---|---|---|
| `card-created` | A card is created through API or CLI | `kind=create`, `source` |
| `moved-to-ready` | A card becomes `ready`, including explicit release | `kind=status`, `previous`, `current` |
| `awaiting-input` | A typed run asks for input and the card enters `awaiting_input` | `kind=input`, `run_id`, `text` |
| `claim-expired` | Powder observes an expired active claim while reclaiming the card | `kind=claim`, `principal`, `run_id`, `agent`, `expires_at` |
| `completed` | A card reaches a terminal completion path | `kind=completion`, `previous`, `current`, optional `proof`, `criteria` |
| `comment-added` | An actor adds a card comment | `kind=comment`, `author`, `body` |
| `work-log-appended` | An actor appends a work log | `kind=work_log`, `agent`, `run_id`, `body` |

## Ordered Event Tail

Powder writes the event before exposing it through the ordered outbound sequence.
`GET /api/v1/events/tail` streams the same sequence as Server-Sent Events. A
consumer resumes after a sequence number and deduplicates by `event_id`.

Powder does not provide signed subscriptions, delivery retries, dead letters,
replay, or a background delivery worker. Integrations own their read cursor and
retry policy.
