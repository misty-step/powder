# Blockers never resolve against terminality

Priority: P1 · Status: done · Estimate: M

## Goal
`Card::is_ready_at` disqualifies any card with a non-empty `blocked_by`
regardless of whether those blocking cards have actually completed. Today a
card stays permanently unclaimable once it lists a blocker, even after that
blocker reaches `done`/`shipped`/`abandoned` — nothing in the codebase ever
re-checks or clears `blocked_by`. Fix the eligibility rule to gate on
non-terminal blockers only.

## Oracle
- [x] `is_ready_at` (or its SQL-native successor) treats a card as blocked
      only when at least one entry in `blocked_by` refers to a card that is
      not yet in a terminal status (done/shipped/abandoned); a card whose
      only blockers are terminal is eligible again.
- [x] A test proves: card A blocks card B; B is not ready while A is
      `ready`/`claimed`/`running`; B becomes ready immediately after A
      transitions to a terminal status, with no manual edit to `blocked_by`.
- [x] The check works across HTTP `list_ready`, CLI `list-ready`, and MCP
      `list_ready` (single implementation, not three copies — same discipline
      backlog.d/001 established for claims).
- [x] `cargo test --workspace` stays green.

## Progress
- 2026-07-02 slice (overnight autonomous): `Card::is_ready_at` and
  `Card::can_be_claimed_at` now take a `blocker_is_terminal: impl Fn(&CardId)
  -> bool` closure -- a `Card` has no access to other cards' state, so the
  caller supplies the lookup. `Card::apply_claim` threads the same closure
  through, so the fix covers both *listing* a card as ready and *actually
  claiming* it, not just the former (claiming a blocked card was previously
  rejected unconditionally by `apply_claim`'s own blocker check, a second
  enforcement point the ticket's original scope didn't call out but which
  needed the identical fix to stay consistent with `list_ready`).
  A missing/unresolvable blocker (never imported, typo'd id) fails closed --
  treated as still non-terminal, so it never silently unblocks the card
  referencing it.
  Both real implementations were fixed, matching backlog.d/001's
  single-implementation discipline: `Store::list_ready`/`Store::claim_card`
  (the real path every face uses -- HTTP, CLI `--db`, and MCP's
  `call_tool_store` all delegate to `Store`) build the blocker lookup from
  data already loaded for the existing full-table scan (list_ready) or a
  per-blocker query inside the claim transaction (claim_card); `Board`'s
  parallel in-memory implementation (powering only the CLI's read-only
  `list-ready <backlog.d-path>` preview with no `--db`) got the identical
  fix using its own in-memory map.
  Proof: 1 new `powder-store` test (blocker non-terminal -> B neither listed
  nor claimable; blocker transitions to `abandoned` -> B is both listed and
  claimable with no edit to `blocked_by`; an unresolvable blocker fails
  closed) and 1 new `powder-core` `Board` test proving the same three-part
  behavior for the CLI path-preview implementation. 110 workspace tests
  green (fmt/clippy/test).

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
