# Blockers never resolve against terminality

Priority: P1 · Status: ready · Estimate: M

## Goal
`Card::is_ready_at` disqualifies any card with a non-empty `blocked_by`
regardless of whether those blocking cards have actually completed. Today a
card stays permanently unclaimable once it lists a blocker, even after that
blocker reaches `done`/`shipped`/`abandoned` — nothing in the codebase ever
re-checks or clears `blocked_by`. Fix the eligibility rule to gate on
non-terminal blockers only.

## Oracle
- [ ] `is_ready_at` (or its SQL-native successor) treats a card as blocked
      only when at least one entry in `blocked_by` refers to a card that is
      not yet in a terminal status (done/shipped/abandoned); a card whose
      only blockers are terminal is eligible again.
- [ ] A test proves: card A blocks card B; B is not ready while A is
      `ready`/`claimed`/`running`; B becomes ready immediately after A
      transitions to a terminal status, with no manual edit to `blocked_by`.
- [ ] The check works across HTTP `list_ready`, CLI `list-ready`, and MCP
      `list_ready` (single implementation, not three copies — same discipline
      backlog.d/001 established for claims).
- [ ] `cargo test --workspace` stays green.

## Notes
`crates/powder-core/src/model.rs:452-455`:
```rust
pub fn is_ready_at(&self, now: i64) -> bool {
    if self.acceptance.is_empty() || !self.blocked_by.is_empty() {
        return false;
    }
    ...
```
`rg blocked_by` across the workspace (2026-07-01) shows `blocked_by` is only
ever stored and round-tripped (schema, upsert, deserialize) — no code path
anywhere checks a blocker's status or clears the list on completion. This
requires reading each blocker card's current status at eligibility-check
time (a join or a follow-up lookup keyed by `blocked_by` ids), which is why
this is scoped as its own ticket rather than folded into a pure SQL
rewrite of `list_ready`.

**Why:** the groom teardown named this exact gap ("Blockers never unblock...
someone must hand-edit `blocked_by`") and live-reading `is_ready_at` today
confirms the logic is byte-for-byte unchanged; none of tonight's shipped
epics (claim lifecycle, answer loop, identity, import, keys) touched
blocker resolution.
