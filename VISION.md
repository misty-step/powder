# Powder Vision

## North Star

Powder is a public, self-hostable work-management application for agent-driven
software teams. It is not a repository for the operator's backlog data; it is
the tool someone deploys so their own backlog, cards, runs, activity, links,
comments, and human-in-loop pauses live in their own database.

Think Linear, but agent-first and self-hostable: Linear is not any one user's
backlog, it is the application where users put backlog data. Powder should work
the same way.

Premise Source:

- `sha256:b8a981522941a7623b3310ebdb28e991f684d254261b097fb6f094723ffe7be4` `/Users/phaedrus/artifacts/public/a/factory/index.html`
- `sha256:378a96eb94605bd996953e9e37ecc50d77c678ad11025b0786ae03456bb38175` `/Users/phaedrus/.factory-lanes/_brief.md`
- `sha256:f21d935694d60db0857f25c37100e81826e82a15ac0678a50e9a3590449e0c2a` `/Users/phaedrus/.factory-lanes/powder.md`
- 2026-07-01 operator reframe: Powder is a public self-hostable product; personal instance data is deployed separately.

## Audience

- Operators who want a self-hosted alternative to hosted agent-work boards.
- Agent orchestrators that need a narrow, callable work API instead of chat-only
  task assignment.
- Teams that want durable run/session traces, claim locks, proof links, and
  human input pauses without handing backlog data to a hosted SaaS.

## Category

Powder is a self-hostable, agent-first work tool. It is not a generic
project-management clone, not a Factory-private board, and not a static
`backlog.d` repository.

The repo ships the application. A deployed instance owns the data.

## Job To Be Done

When a person or agent has work that should be claimable, auditable, and safe to
pause for human input, Powder hosts that work as cards and runs in a database
the operator controls. Agents can ask what is ready, claim one card, append
activity, request input, add evidence links, and complete work with proof.

## Product Standards

- Self-hostable first: one Rust service, one Docker image, one SQLite database,
  one Fly-friendly deployment target.
- Bring your own data: importers load user-supplied backlog files into an
  instance database; no real backlog data lives in this repo.
- Agent-native first: API, CLI, MCP, and shipped skill all speak the same
  domain language.
- One core, many faces: business-rule changes land in `powder-core` once.
- The board is not the runner: Powder stores cards, claims, runs, activity, and
  proof. External dispatchers spawn Codex, Herdr panes, Sprites, or other
  workers.
- Proof beats status: done means accepted evidence, not process exit zero.
- Human-in-loop is first-class: awaiting input is a run state, not a comment
  convention.
- Tailnet-friendly auth: a deployment can run behind private networking, shared
  secret auth, or a trusted identity header from an ingress layer.
- First-run onboarding is product behavior: a new instance should reveal setup
  state and guide the operator toward the first admin/API key/import.

## Core Model

### Card

A durable context object with:

- identifier, title, body, acceptance oracle, status, priority, labels
- repo, workspace path, branch name, assignee
- blockers, links, comments, attachments
- optional source provenance for imported data

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

Supporting context and operator communication. Links carry proof, PRs, CI,
artifacts, docs, or external references. Comments carry human/agent context but
do not drive scheduling by themselves.

## Architecture

Powder follows a functional-core / imperative-shell split:

- `powder-core`: pure domain types, validation, status transitions, ready-card
  eligibility, claim invariants, importer parsing, and completion rules.
- `powder-shell`: filesystem and future persistence ports.
- `powder-server`: the single deployable HTTP app and onboarding/health surface.
- `powder-api`: HTTP route vocabulary.
- `powder-cli`: local/operator commands and import dry-runs.
- `powder-mcp`: intent-shaped tools for agent orchestrators.
- `SKILL.md`: progressive-disclosure instructions for agents using Powder.

The eventual persistence layer should be SQLite-backed and single-writer, with
Litestream replication in self-hosted deployments. The repo should include
fixtures and sample data only when they are synthetic tests, never real operator
backlog data.

## v0 Scope

The v0 product should:

- serve one deployable HTTP app with `/healthz`, `/readyz`, and onboarding state
- store cards, runs, activities, links, comments, and claims in SQLite
- import user-supplied `backlog.d/*.md` tickets into the instance database
- answer the `ready` query
- claim and release cards with expiring locks
- update card/run status through allowed transitions
- append run activity
- attach proof/reference links to cards
- request operator input by moving a run to `awaiting_input`
- complete a card only with a proof payload
- expose the same capabilities through CLI and MCP tools

## Non-Goals

- No personal or operator backlog data in this public repo.
- No dispatch daemon in the core.
- No migration from Gradient or Hermes `kanban.db`.
- No feature-parity project-management UI before the agent contract is solid.
- No one-to-one REST-to-MCP wrapper dump.
- No hosted multi-tenant SaaS assumption in the product core.

## GitHub Remote

Target remote: `misty-step/powder`.

Status: created and made public on 2026-07-01. The application repository is
public; individual deployments and their SQLite databases are private instance
state.

## Excellent In 6-12 Months

Powder is the obvious self-hosted work substrate for agentic software teams.
Agents consume it through MCP and skills, humans inspect it through a thin
control surface, and each deployment can import existing backlog markdown into
claimable, auditable work without leaking private backlog data into the product
repository.
