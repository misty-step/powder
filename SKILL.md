---
name: powder
description: |
  Use when an agent needs to inspect, claim, update, request input for, or
  complete Factory work cards in Powder. Powder is the agent-first work board:
  a durable card store with run sessions, activity, proof, and human-in-loop
  states.
argument-hint: "[list-ready|claim|update-status|request-input|complete-card]"
---

# Powder

Powder is the Factory work board. It exposes one core through API, CLI, MCP,
and this skill. Treat cards as context objects with acceptance oracles, not
status rows.

## Operating Contract

- Use `list_ready` before claiming work.
- Claim exactly one card at a time unless the operator authorizes a batch.
- Keep the card updated through activity events and status transitions.
- Use `request_input` when a human decision is needed; do not invent approvals.
- Use `complete_card` only when a proof artifact exists.
- Do not spawn agents from Powder core. Dispatch belongs to a separate runner.

## Expected MCP Tools

- `list_ready`: return claimable cards sorted by priority, age, and identifier.
- `claim_card`: acquire an expiring lock for one card and open a run.
- `update_status`: move a card or run through an allowed transition.
- `request_input`: move the run to `awaiting_input` with the exact question.
- `complete_card`: attach proof and mark the card complete for human review.

## Local Gate

```sh
cargo test --workspace
```

## Red Lines

- Do not create or push the `misty-step/powder` GitHub remote without operator
  approval.
- Do not import from Gradient or Hermes `kanban.db`.
- Do not treat exit zero as completion without proof.
