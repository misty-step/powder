# Delete dead Run/shell scaffolding with zero writers or implementors

Priority: P3 · Status: done · Estimate: S

## Goal
Remove architecture scaffolding the groom teardown flagged as unadopted and
that no feature has grown into since: `powder-shell`'s `IdGenerator`/
`CardStore` traits (zero implementors), `RunState::Pending` (never
constructed), and `Run`'s progress-tracking fields (`model`, `turn_count`,
`token_count`, `consecutive_failures`, `last_error`, `result` — never set to
a real value by any surface).

## Oracle
- [x] `rg "trait IdGenerator|trait CardStore"` returns nothing, or exactly
      one real call site if this ticket adopts them instead of deleting them
      (default is delete, per the report's recommendation and the "delete
      before adding" default).
- [x] `RunState::Pending` variant is removed and all match arms compile
      without it.
- [x] The six unwritten `Run` progress fields are removed from schema,
      model, and serialization; existing tests updated accordingly.
- [x] `cargo test --workspace` and
      `cargo clippy --workspace --all-targets -- -D warnings` stay green
      after removal.
- [x] If this investigation finds a real writer for any field it initially
      believed dead, the ticket closes by wiring that field instead of
      deleting it, with the discovery recorded in Progress rather than
      silently dropped.

## Progress
- 2026-07-02 slice (overnight autonomous): `IdGenerator`/`CardStore`
  (and, going further than this ticket's original scope, `Clock`/
  `SystemClock` too, which turned out to also have zero consumers once
  checked directly) were already deleted earlier this session in
  backlog.d/012 -- confirmed via `rg` that nothing remains. No new code
  needed for that part.
  Re-verified live (not stale) that `RunState::Pending` was still never
  constructed anywhere (only referenced in `board.rs`'s
  `matches!(run.state, RunState::Active | RunState::Pending)` guard and a
  raw SQL `WHERE state IN ('active', 'pending')` string in
  `Store::claim_card`) and that the six `Run` fields were still never
  written a real value by any surface (re-grepped for any `.model = Some`/
  `.turn_count = <nonzero>`/etc across the workspace: nothing). Both
  findings held; no real writer was discovered, so both were deleted per
  the ticket's default.
  Removed `RunState::Pending`; fixed the `Board` match arm and the SQL
  string to drop the now-impossible branch. Removed `Run`'s `model`,
  `turn_count`, `token_count`, `consecutive_failures`, `last_error`,
  `result` fields from the model, the `runs` table schema, `RUN_SELECT_SQL`,
  and every `Run { ... }` construction site (`Board::claim_card`,
  `Store::claim_card`) -- `proof` was untouched since `complete_card`
  genuinely writes it. This needed a real schema migration: bumped
  `SCHEMA_VERSION` to 4 and added `MIGRATE_3_TO_4` (`ALTER TABLE runs DROP
  COLUMN ...` x6), extending the version-stepping loop `migrate()` gained in
  backlog.d/013. Also fixed two now-stale raw `SELECT` strings in
  `answer_loop.rs` (`list_awaiting_input`, `load_runs_for_card`) that still
  named the dropped columns directly rather than going through the shared
  `RUN_SELECT_SQL` constant -- these were the two failures caught by running
  the full suite after the model-level change, confirming the value of
  gating each step on `cargo test --workspace` rather than trusting the
  compiler alone (a raw SQL string mismatch doesn't fail to compile, only
  to run).
  Discovered while fixing this: the existing v1->v2 and v2->v3 migration
  tests built artificially minimal hand-rolled databases with no `runs`
  table at all, which isn't what a real historical database ever looked
  like (the original `cards`/`runs`/`activities`/`links`/`comments` schema
  predates the identity/hash-algorithm versioning entirely) -- once
  `MIGRATE_3_TO_4` unconditionally tried to `ALTER TABLE runs`, those tests
  failed with "no such table: runs". Fixed by giving both tests a realistic
  `runs` table matching the actual pre-drop shape, which is what a genuine
  v1/v2 database would have had.
  Proof: 1 new dedicated test constructs a realistic v3 database (full
  `cards`+`runs` tables, a real run row with non-default values in all six
  soon-to-be-dropped columns), migrates to v4, and asserts via
  `pragma_table_info('runs')` that all six columns are gone while the run
  itself and its still-relevant columns (`agent`, `claim_expires_at`)
  survive intact. 117 workspace tests green (fmt/clippy/test).

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
