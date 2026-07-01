# Powder Vision

## North Star

Powder is the Factory's agent-first work board: the place where a backlog card
becomes a claimable context object, a run produces an auditable activity trail,
and a human can pause, steer, accept, or reject the outcome.

The product exists to make "orchestrating the orchestrators" concrete. Agents
should be able to ask what is ready, claim one unit of work, report activity,
request input, and complete the card with proof. Humans should see the same
state without becoming the scheduler.

Premise Source:

- `sha256:b8a981522941a7623b3310ebdb28e991f684d254261b097fb6f094723ffe7be4` `/Users/phaedrus/artifacts/public/a/factory/index.html`
- `sha256:378a96eb94605bd996953e9e37ecc50d77c678ad11025b0786ae03456bb38175` `/Users/phaedrus/.factory-lanes/_brief.md`
- `sha256:f21d935694d60db0857f25c37100e81826e82a15ac0678a50e9a3590449e0c2a` `/Users/phaedrus/.factory-lanes/powder.md`

## Audience

- Lead orchestrators that need a reliable queue and proof surface across many
  repos.
- Worker agents that need a narrow, callable contract instead of chat-only
  instructions.
- The operator, who needs oversight, approval, and fast recovery from failed
  runs.

## Category

Powder is not a generic project-management clone. It is a composable,
git-native, agent-first work-management service: backlog board, claim system,
run/session log, human-in-loop inbox, and proof ledger.

## Job To Be Done

When a repo has work that is ready for agents, Powder lets an orchestrator find
the best next card, claim it safely, run work through a separate dispatcher,
capture the activity trail, ask for input when needed, and complete only with
reviewable proof.

## Product Standards

- Agent-native first: API, CLI, MCP, SDK later, and shipped skill all speak the
  same domain language.
- One core, many faces: a business-rule change lands in `powder-core` once.
- Trigger-first posture: design for events and webhooks; accept polling only
  as a tactical adapter.
- Proof beats status: "done" means accepted evidence, not process exit zero.
- Human-in-loop is first-class: awaiting input is a run state, not a comment
  convention.
- Failure is structured: retries, backoff, stale claims, and circuit breakers
  are product concepts.
- The board is not the runner: Powder stores work and activity; separate
  daemons spawn Codex, Herdr panes, or Sprites.
- Greenfield means greenfield: do not import Gradient or Hermes implementation
  details. Import only `backlog.d/` content through a clean parser.

## Core Model

### Card

A durable context object with:

- identifier, title, body, acceptance oracle, status, priority, labels
- repo, workspace path, branch name, assignee
- blockers, links, comments, attachments

### Run

A session against one card with:

- state: pending, active, awaiting input, error, complete, stale
- agent, model, claim lock, claim expiry
- turn count, token counts, consecutive failures, last error
- result and proof links

### Activity

An append-only timeline for a run:

- thought, action, response, elicitation, error, prompt
- payload and creation time

### Link and Comment

Supporting context and operator communication. These are attached to cards and
runs but do not drive scheduling by themselves.

## Architecture

Powder follows a functional-core / imperative-shell split:

- `powder-core`: pure domain types, validation, status transitions, ready-card
  eligibility, claim invariants, and completion rules.
- `powder-shell`: traits for stores, clocks, ids, importers, and future
  notification surfaces.
- `powder-api`: HTTP contract and route vocabulary.
- `powder-cli`: local and agent-friendly commands.
- `powder-mcp`: intent-shaped tools for agent orchestrators.
- `SKILL.md`: progressive-disclosure instructions for agents using Powder.

The eventual SDK should be generated from the API contract where practical, but
it is not part of this first scaffold.

## v0 Scope

The v0 product is a local service that can:

- import `backlog.d/*.md` tickets into cards
- answer the `ready` query
- claim and release cards with expiring locks
- update card/run status
- append run activity
- request operator input by moving a run to `awaiting_input`
- complete a card only with a proof payload
- expose the same capabilities through CLI and MCP tools

## Non-Goals

- No dispatch daemon in the core.
- No migration from Gradient or Hermes `kanban.db`.
- No feature-parity project-management UI.
- No one-to-one REST-to-MCP wrapper dump.
- No GitHub remote creation until the operator approves the outbound step.

## GitHub Remote Plan

Target remote: `misty-step/powder`.

Paused outward step:

```sh
gh repo create misty-step/powder --source=. --remote=origin --private
git push -u origin factory/scaffold-vision-backlog
```

Visibility should be confirmed before execution. Default to private unless the
operator explicitly asks for public.

## Excellent In 6-12 Months

Powder is the default work substrate for the Factory. Agents consume it through
MCP and skills, humans inspect it through a thin control surface, and every
factory repo can move from `backlog.d` markdown into claimable, auditable work
without losing git-native history.
