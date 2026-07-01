# The answer loop

Priority: P1 | Status: backlog | Type: Epic

## Goal
Close the human-in-the-loop circuit. Agents can already request input, but the
question is effectively write-only. Humans and other agents need to read card
and run timelines, see awaiting-input work on every surface, answer the prompt,
and resume the run through the same durable activity ledger.

## Oracle
- [ ] `get_card` and `get_run` return activities, links, comments, claim state, and run state over HTTP, CLI, and MCP.
- [ ] Awaiting-input cards and runs are queryable without scanning raw SQLite tables.
- [ ] `answer_input` emits an actor-attributed user/agent response activity, moves the run out of `awaiting_input`, and preserves the original question.
- [ ] A full ask -> answer -> resume path is walked over HTTP with a transcript committed as synthetic test evidence.
- [ ] The shipped `SKILL.md` names only capabilities that actually exist.

## Children
- Add timeline read models and route/tool/CLI faces.
- Add `answer_input` semantics and tests.
- Add progress/activity append semantics that do not forge user-only prompt events.
- Sync docs and skill prose after the behavior exists.
