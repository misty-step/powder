# Delete the dead powder-shell effect-trait/port layer

Priority: P2 | Status: done | Type: Bug

## Goal
The original groom teardown flagged this and it was never fixed:
`powder-shell`'s `CardStore`, `Clock`, `IdGenerator` traits (plus the
`SystemClock` implementor) have zero consumers anywhere in the workspace.
`Store` is called concretely everywhere; nothing takes a `Clock` or
`IdGenerator` parameter, and nothing implements `CardStore`. `AGENTS.md`
still claimed "`powder-shell` owns effect traits and ports. Concrete stores
and clients implement those traits outside the core" — a description of a
hexagonal-ports pattern the codebase never actually adopted. Delete the dead
code and correct the doc.

## Oracle
- [x] `Clock`, `SystemClock`, `IdGenerator`, `CardStore` are deleted from
      `powder-shell`.
- [x] The workspace builds and every existing test still passes (confirms
      nothing outside `powder-shell` depended on any of the four).
- [x] `AGENTS.md`'s architecture section describes what `powder-shell`
      actually does today, not an unadopted ports pattern.

## Progress
- 2026-07-02 slice (overnight autonomous): confirmed live via grep that all
  four items (`CardStore`, `IdGenerator`, `Clock`, and `Clock`'s only
  implementor `SystemClock`) had zero consumers anywhere in the workspace —
  not even `Clock`/`SystemClock`, which the original groom report didn't
  flag as unconsumed (it only called out `CardStore`/`IdGenerator` as
  having zero implementors) but which turned out to be equally dead once
  checked directly against the live repo. Deleted all four; `powder-shell`
  now owns exactly what it's actually used for: backlog.d parsing/loading,
  repo id-namespacing, the GitHub issue adapter, and the shared adapter
  error type. Rewrote the stale `AGENTS.md` architecture bullet that
  described an effect-trait/port layer to instead state the real shape and
  warn against reintroducing one without a concrete second implementation
  that needs it.
  Proof: pure deletion, no behavior change — `cargo build --workspace` and
  `cargo test --workspace` both green with zero test count change (101
  tests, same as before this slice), confirming nothing depended on the
  removed traits.
