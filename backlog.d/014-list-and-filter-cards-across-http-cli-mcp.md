# List and filter cards across HTTP, CLI, and MCP

Priority: P1 · Status: done · Estimate: M

## Goal
Add a general card-listing surface (not just ready-eligible cards) so an
operator or agent can enumerate cards by status, repo, or priority without
opening the SQLite file directly. `list_ready` and `list_awaiting_input`
already prove the pattern; this extends it to the full card set.

## Oracle
- [x] `GET /api/v1/cards?status=<status>&repo=<repo>` returns matching cards,
      sorted the same way `list_ready` sorts (priority, age, id), with a
      bounded `limit` param.
- [x] CLI `powder list-cards --db X [--status S] [--repo R] [--limit N]`
      exists and is covered by an integration test.
- [x] An MCP tool exposes the same filtered read, and `SKILL.md`'s "Expected
      MCP Tools" section is updated to list it.
- [x] At minimum, `blocked`, `review`, and `done` cards are enumerable from
      every face without raw SQL access.
- [x] `cargo test --workspace` stays green with new coverage for at least one
      filter combination.

## Progress
- 2026-07-02 slice (overnight autonomous): added `Store::list_cards(&CardFilter,
  limit)` following `list_ready`'s exact shape (select-all, filter in Rust,
  sort by priority/age/id, truncate) -- `CardFilter{status, repo}`, either
  field `None` means unfiltered on that dimension. Wired to `GET
  /api/v1/cards?status=&repo=&limit=` (added as `.get(list_cards)` alongside
  the existing `POST /api/v1/cards` on the same route), CLI `powder
  list-cards --db X [--status S] [--repo R] [--limit N]`, and a new MCP tool
  `list_cards` on both the local-store and remote-HTTP dispatch paths (the
  remote path needed a small percent-encoding helper since repo slugs
  contain `/`, which must not reach the wire unescaped inside a query
  string). Invalid `--status`/`?status=` values reject with a clear error on
  every face rather than silently matching nothing. `SKILL.md`'s "Expected
  MCP Tools" and `powder-api`'s route registry both updated so neither goes
  stale relative to the real routes.
  Proof: 1 new `powder-store` test (status filter, repo filter, both
  together, unfiltered enumerates non-ready cards, limit truncates), 1 new
  HTTP test (status filter, repo filter via an imported card, invalid status
  -> 400), 1 new CLI test, 2 new MCP tests (local-store dispatch + remote
  percent-encoded query, proven against a real recording test server). 108
  workspace tests green (fmt/clippy/test).

## Notes
Live route audit (`crates/powder-server/src/main.rs:294-313`, 2026-07-01)
shows only `/api/v1/cards` (create), `/api/v1/cards/import`,
`/api/v1/cards/ready`, and `/api/v1/cards/{id}` — no way to list cards that
are *not* ready-eligible. `Store::list_ready`
(`crates/powder-store/src/lib.rs:187`) is the closest existing pattern to
extend: same sort, same shape, different predicate (any status/repo filter
instead of `is_ready_at`).

**Why:** the groom teardown flagged "no `list cards by status`, so an
operator cannot even enumerate what's awaiting input with curl" as a product
gap; the answer-loop epic (backlog.d/003) closed the awaiting-input half via
`/api/v1/runs/awaiting-input`, but general card enumeration by status/repo
was never built and remains missing after live route inspection tonight.
