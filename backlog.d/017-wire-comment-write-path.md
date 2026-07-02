# Wire the comment write path (add_comment)

Priority: P2 · Status: done · Estimate: M

## Goal
`Comment` is a modeled, persisted, and now-readable entity — `get_card` and
`get_run` already return `comments` (`crates/powder-store/src/answer_loop.rs:195-207`)
— but there is no way to create one on any surface. The read half of the
feature shipped with the answer-loop epic; the write half never did. Either
finish it or the model stays permanently dead weight the next audit will
flag for deletion.

## Oracle
- [x] A store-level `add_comment` (or equivalent) inserts into the
      `comments` table with actor, body, and timestamp, using the same
      `Authority` checks `add_link` already enforces.
- [x] `POST /api/v1/cards/{id}/comments`, CLI `powder add-comment`, and an
      MCP tool all exist and are tested.
- [x] `get_card`/`get_run` return the new comment in `comments` immediately
      after it is added, in creation order.
- [x] `SKILL.md`'s "Expected MCP Tools" section lists the new tool.
- [x] `cargo test --workspace` stays green with new coverage.

## Progress
- 2026-07-02 slice (overnight autonomous): checked what `add_link` actually
  enforces before copying its "Authority" pattern -- the ticket's premise
  was slightly stale: `add_link`'s HTTP handler only calls `authorize()`
  (any valid, authenticated key) and its store method takes no `Authority`
  parameter at all; it is *not* claim-holder-gated like `claim_card`/
  `update_status`/`complete_card` are. That's the right shape for an
  additive annotation (attaching a PR link, or now a comment, is not an
  exclusive mutation of the card's own state the way claiming or completing
  it is), so `add_comment` matches `add_link` exactly: authenticated but not
  claim-holder-gated, not a new, stricter check the rest of the codebase
  doesn't otherwise apply to this class of operation.
  Added `Store::add_comment(card_id, author, body, now)` next to
  `add_link` in `lib.rs`, generating an internal id for the `comments`
  table's `PRIMARY KEY` but never exposing it on `Comment` -- matching the
  model's existing shape exactly (the read path already never selected or
  exposed the `id` column, ordering only by `created_at ASC, id ASC` for
  tiebreak; this wasn't changed, so nothing downstream needed updating).
  Wired `POST /api/v1/cards/{id}/comments`, CLI `powder add-comment <id>
  --db X --author A --body B`, and MCP tool `add_comment` on both the
  local-store and remote-HTTP dispatch paths. `SKILL.md`'s "Expected MCP
  Tools" updated.
  Proof: 1 new `powder-store` test (comment appears in `get_card_detail` in
  creation order; missing card and empty body both reject), 1 new HTTP
  test, 1 new CLI test, 2 new MCP tests (local-store dispatch + remote
  request-shape assertion). 116 workspace tests green (fmt/clippy/test).

## Notes
`rg "INSERT INTO comments"` across the whole workspace (2026-07-01) returns
zero hits — the `comments` table (`crates/powder-store/src/schema.rs:87`)
and `Comment` model (`crates/powder-core/src/model.rs:657`) exist purely as
read targets. `add_link`'s HTTP/CLI/MCP/store implementation is the exact
pattern to copy for authority and activity semantics.

**Why:** the original groom teardown flagged `Comment` as fully dead
(zero read/write paths); tonight's answer-loop epic (backlog.d/003) added
the read paths as part of `get_card`/`get_run` but did not add a matching
write path, so the model is now half-wired instead of fully dead — worth
finishing rather than leaving in this in-between state.
