# Wire the comment write path (add_comment)

Priority: P2 · Status: ready · Estimate: M

## Goal
`Comment` is a modeled, persisted, and now-readable entity — `get_card` and
`get_run` already return `comments` (`crates/powder-store/src/answer_loop.rs:195-207`)
— but there is no way to create one on any surface. The read half of the
feature shipped with the answer-loop epic; the write half never did. Either
finish it or the model stays permanently dead weight the next audit will
flag for deletion.

## Oracle
- [ ] A store-level `add_comment` (or equivalent) inserts into the
      `comments` table with actor, body, and timestamp, using the same
      `Authority` checks `add_link` already enforces.
- [ ] `POST /api/v1/cards/{id}/comments`, CLI `powder add-comment`, and an
      MCP tool all exist and are tested.
- [ ] `get_card`/`get_run` return the new comment in `comments` immediately
      after it is added, in creation order.
- [ ] `SKILL.md`'s "Expected MCP Tools" section lists the new tool.
- [ ] `cargo test --workspace` stays green with new coverage.

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
