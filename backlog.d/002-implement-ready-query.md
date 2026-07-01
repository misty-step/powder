# Implement the ready-card query

Priority: P0 | Status: ready | Estimate: M

## Goal
Expose the deterministic query that tells agents which cards can be claimed next.

## Oracle
- [ ] `powder list-ready --limit 10` excludes blocked, terminal, oracle-less, and already-claimed cards.
- [ ] Results sort by priority, age, and identifier.
- [ ] The same query is available through API and MCP without duplicated business rules.

## Verification System
- Claim: Ready-card selection is deterministic and shared across all faces.
- Falsifier: CLI, API, and MCP disagree on the same fixture store.
- Driver: Fixture store with mixed statuses, priorities, blockers, and missing oracles.
- Grader: Expected ordered card identifiers.
- Evidence packet: Test fixture, command output, and API/MCP response captures.
- Cadence: Every scheduling-rule change.

## Notes
**Why:** The factory report's work-app design makes deterministic dispatch and ready filtering load-bearing.
