# List and filter cards across HTTP, CLI, and MCP

Priority: P1 · Status: ready · Estimate: M

## Goal
Add a general card-listing surface (not just ready-eligible cards) so an
operator or agent can enumerate cards by status, repo, or priority without
opening the SQLite file directly. `list_ready` and `list_awaiting_input`
already prove the pattern; this extends it to the full card set.

## Oracle
- [ ] `GET /api/v1/cards?status=<status>&repo=<repo>` returns matching cards,
      sorted the same way `list_ready` sorts (priority, age, id), with a
      bounded `limit` param.
- [ ] CLI `powder list-cards --db X [--status S] [--repo R] [--limit N]`
      exists and is covered by an integration test.
- [ ] An MCP tool exposes the same filtered read, and `SKILL.md`'s "Expected
      MCP Tools" section is updated to list it.
- [ ] At minimum, `blocked`, `review`, and `done` cards are enumerable from
      every face without raw SQL access.
- [ ] `cargo test --workspace` stays green with new coverage for at least one
      filter combination.

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
