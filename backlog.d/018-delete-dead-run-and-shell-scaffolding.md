# Delete dead Run/shell scaffolding with zero writers or implementors

Priority: P3 · Status: ready · Estimate: S

## Goal
Remove architecture scaffolding the groom teardown flagged as unadopted and
that no feature has grown into since: `powder-shell`'s `IdGenerator`/
`CardStore` traits (zero implementors), `RunState::Pending` (never
constructed), and `Run`'s progress-tracking fields (`model`, `turn_count`,
`token_count`, `consecutive_failures`, `last_error`, `result` — never set to
a real value by any surface).

## Oracle
- [ ] `rg "trait IdGenerator|trait CardStore"` returns nothing, or exactly
      one real call site if this ticket adopts them instead of deleting them
      (default is delete, per the report's recommendation and the "delete
      before adding" default).
- [ ] `RunState::Pending` variant is removed and all match arms compile
      without it.
- [ ] The six unwritten `Run` progress fields are removed from schema,
      model, and serialization; existing tests updated accordingly.
- [ ] `cargo test --workspace` and
      `cargo clippy --workspace --all-targets -- -D warnings` stay green
      after removal.
- [ ] If this investigation finds a real writer for any field it initially
      believed dead, the ticket closes by wiring that field instead of
      deleting it, with the discovery recorded in Progress rather than
      silently dropped.

## Notes
- `crates/powder-shell/src/lib.rs:64-76` — `IdGenerator`/`CardStore` traits,
  zero `impl` sites anywhere in the workspace (`Clock` already has a real
  implementor, `SystemClock`, at line 58 — leave `Clock` alone).
- `crates/powder-core/src/model.rs:292` — `RunState::Pending`, referenced
  only inside a `matches!(run.state, RunState::Active | RunState::Pending)`
  guard at `crates/powder-core/src/board.rs:506`, never constructed anywhere.
- `crates/powder-core/src/model.rs:626-632` — `Run`'s progress fields, only
  ever round-tripped via `excluded.<field>` in the store's `ON CONFLICT`
  upsert (`crates/powder-store/src/lib.rs:607-610`), i.e. always
  re-persisting whatever was already there (0/None from creation), never a
  real value from any code path.

**Why:** live grep confirms zero implementors/writers for each item as of
2026-07-01, matching the groom teardown's §7 deletion table exactly; none of
tonight's shipped epics (001–009, all closing real product gaps) touched
this scaffolding, so it remains exactly as dead as the original report found
it.
