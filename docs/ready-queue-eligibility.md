# Ready-queue eligibility truth

`status=ready` is an operator lane label. Claimability is a separate derived
fact. `list_ready` / `GET /api/v1/cards/ready` only returns cards that pass
claim eligibility right now.

## Why a ready card may be missing from the queue

Call `get_card` / `GET /api/v1/cards/{id}`. Every detail response includes:

```json
"claim_eligibility": {
  "eligible": false,
  "code": "no_acceptance",
  "message": "card example has no acceptance criteria; add them via update (acceptance: [...]) before claiming",
  "blockers": []
}
```

| `code` | Meaning |
|---|---|
| `eligible` | Claimable now; appears in `list_ready` (subject to repo/estimate filters) |
| `no_acceptance` | Ready-shaped status but empty acceptance oracle |
| `unresolved_blockers` | At least one direct `blocked_by` id is non-terminal or missing; `blockers` lists those ids |
| `active_claim` | Ready card already held by an unexpired claim |
| `status_not_claimable` | Status is not a claimable lane (or `in_progress` without a reclaimable claim) |
| `in_progress_claim_not_expired` | Active in-progress lease; reclaim only after expiry |

Eligibility rules are unchanged: direct `blocked_by` terminality only, no
run-poison cooldown inside Powder, parent edges do not block, repository tier
does not gate the queue.

## Repo filter is exact

`list_ready?repo=` accepts a comma-separated allowlist of **exact** canonical
short slugs or registered aliases after normal repo resolution:

- `canary` and `misty-step/canary` match the same registered repo
- `bitterblossom` never matches `memory-engine`
- substring / SQL `LIKE` matching is not used on this path
- null-repo cards with a numeric id prefix (`bitterblossom-001`) still match
  that prefix repo as before

`search_cards` may use broader text matching; do not treat search hits as the
ready queue.

## Factory / Bitterblossom reconciler note

When a tick expects a routed card and `list_ready` returns empty (or omits the
routed id):

1. Call `get_card` for the routed card id (or the board's ready-status id).
2. Log `claim_eligibility.code` and `claim_eligibility.message`.
3. Prefer that structured code over a bare `no_ready_card` when the card still
   exists with `status=ready`.

Example remote check:

```sh
curl -sS -H "Authorization: Bearer $POWDER_API_KEY" \
  "$POWDER_API_BASE_URL/api/v1/cards/$CARD_ID" \
  | jq '.claim_eligibility'
```

Powder does not auto-move ineligible cards out of the Ready lane. Groom them
(add acceptance, resolve blockers, or abandon) once the code is visible.
