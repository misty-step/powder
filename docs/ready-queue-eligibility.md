# Ready-queue eligibility truth

`status=ready` is an operator lane label. Claimability is a separate derived
fact. `list_ready` and `GET /api/v1/cards/ready` return only cards that pass
claim eligibility now.

## Why a ready card may be missing

Call `get_card` or `GET /api/v1/cards/{id}`. Every detail response includes a
`claim_eligibility` object. `eligible` and `code` are always present. `message`
is present when ineligible. `blockers` is present only for
`unresolved_blockers`.

```json
"claim_eligibility": {
  "eligible": false,
  "code": "no_acceptance",
  "message": "card example has no acceptance criteria; add them before claiming"
}
```

| Code | Meaning |
|---|---|
| `eligible` | Claimable now; appears in `list_ready` subject to exact filters |
| `no_acceptance` | Ready-shaped status but empty acceptance oracle |
| `unresolved_blockers` | A direct `blocked_by` card is non-terminal or missing |
| `active_claim` | An unexpired claim already holds the card |
| `status_not_claimable` | Status is not a claimable lane |
| `in_progress_claim_not_expired` | An active in-progress lease remains |

Eligibility uses direct `blocked_by` terminality. Parent edges do not block.
Powder has no run-poison cooldown inside the eligibility query.

## Card `repo` filter

`repo` is an optional opaque Card string. `list_ready?repo=` matches that string
exactly. It does not resolve aliases, tiers, visibility, imports, or registry
entities. Substring and SQL `LIKE` matching are not used.

Search may use broader text matching. Do not treat search hits as the ready
queue.

## Reconcile a missing card

When a worker expects a card and `list_ready` omits it:

1. Call `get_card` for the card id.
2. Log `claim_eligibility.code` and `claim_eligibility.message`.
3. Prefer the structured code over a bare `no_ready_card` error.

```sh
curl -sS -H "Authorization: Bearer $POWDER_API_KEY" \
  "$POWDER_API_BASE_URL/api/v1/cards/$CARD_ID" \
  | jq '.claim_eligibility'
```

Powder does not auto-move ineligible cards out of the Ready lane. Add an
acceptance oracle, resolve blockers, or abandon the card after review.
