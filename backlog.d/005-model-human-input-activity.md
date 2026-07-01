# Model human input and activity timelines

Priority: P1 | Status: pending | Estimate: M

## Goal
Make human-in-loop pauses and run activity first-class, queryable product state.

## Oracle
- [ ] `request_input` moves the run to `awaiting_input` with the exact question and card context.
- [ ] Activity events preserve type, payload, run id, and creation time in append-only order.
- [ ] Completing a card after input links the operator answer to the final proof.

## Verification System
- Claim: Powder can pause a run for a human without losing context or corrupting the timeline.
- Falsifier: An awaiting-input run cannot be found, resumed, or audited from stored state.
- Driver: Fixture run with activity append, input request, answer, and completion.
- Grader: Run state and ordered activity records.
- Evidence packet: CLI/API transcript and store snapshot.
- Cadence: Every run-state or activity-schema change.

## Notes
**Why:** Linear's Agent Interaction model is valuable because sessions and activity are product primitives, not comments.
