# Add expiring claim locks and release

Priority: P0 | Status: ready | Estimate: M

## Goal
Let one agent safely claim a ready card while preserving recovery from abandoned runs.

## Oracle
- [ ] `powder claim <card-id> --agent <name>` creates a run and moves the card out of ready.
- [ ] A second agent cannot claim the same unexpired lock.
- [ ] Expired locks can be reconciled back into ready or stale state by an explicit command.
- [ ] Releasing a claim records an activity event and leaves the card claimable when no blockers remain.

## Verification System
- Claim: Powder prevents double-claiming and can recover stale claims.
- Falsifier: Two active runs hold the same card lock at the same time.
- Driver: Store fixture plus clock-controlled claim/release tests.
- Grader: Card status, run state, claim expiry, and activity timeline.
- Evidence packet: Unit/integration test output and CLI transcript.
- Cadence: Every claim or reconciliation change.

## Notes
**Why:** Symphony-style claim discipline and Linear-style sessions are Powder's core value.
