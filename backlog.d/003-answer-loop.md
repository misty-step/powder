# The answer loop

Priority: P1 | Status: done | Type: Epic

## Goal
Close the human-in-the-loop circuit. Agents can already request input, but the
question is effectively write-only. Humans and other agents need to read card
and run timelines, see awaiting-input work on every surface, answer the prompt,
and resume the run through the same durable activity ledger.

## Oracle
- [x] `get_card` and `get_run` return activities, links, comments, claim state, and run state over HTTP, CLI, and MCP.
- [x] Awaiting-input cards and runs are queryable without scanning raw SQLite tables.
- [x] `answer_input` emits an actor-attributed user/agent response activity, moves the run out of `awaiting_input`, and preserves the original question.
- [x] A full ask -> answer -> resume path is walked over HTTP with a transcript committed as synthetic test evidence.
- [x] The shipped `SKILL.md` names only capabilities that actually exist.

## Evidence
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- HTTP ask -> answer -> resume transcript lives in `crates/powder-server/src/main.rs` test `http_answer_loop_reads_and_resumes_awaiting_input`.
- CLI smoke on a fresh SQLite DB: `request-input` -> `list-awaiting-input` JSON -> `answer-input` -> `get-card`/`get-run` returned activity order `action`, `elicitation`, `response`, preserved the original elicitation, emitted `answered by operator: approved`, then `complete-card` succeeded.

## Children
- Add timeline read models and route/tool/CLI faces.
- Add `answer_input` semantics and tests.
- Add progress/activity append semantics that do not forge user-only prompt events.
- Sync docs and skill prose after the behavior exists.
