# Powder Vision

Powder is the self-hostable work substrate for agent-driven software teams: a
dumb, reliable ledger for work, claims, timelines, relations, and audit
history.

The repository ships the application. A deployed instance owns the data.
Operators bring their own backlog, store it in their own SQLite database, and
connect their own agents, runners, and humans through Powder's API, CLI, MCP,
and skill surfaces.

Powder should feel like the narrow missing tool between "a chat thread full of
tasks" and "a hosted project-management system that assumes humans are the
primary workers." It is not the operator's backlog, not an instance-data dump,
and not a private Factory board. It is the public product someone deploys so
their work can be claimed, paused, audited, related, and completed with or
without proof.
Powder never calls a model. Intelligence belongs in orchestrators such as
Bitterblossom workloads that read and write through Powder's deterministic
interfaces.

## Why It Exists

Agent work needs durable coordination primitives:

- a card with enough context and an acceptance oracle
- a ready query that deterministic code can answer
- an expiring claim so duplicate agents do not collide
- a run timeline that survives handoff, crash, and compaction
- proof links, completion records, and audit events a human can inspect
- an explicit awaiting-input state instead of invented approvals
- structured run telemetry — agents, models, token counts, estimated cost,
  outcomes — so ticket work doubles as evaluation data
- attached session history at each lifecycle step, hidden by default, for
  forensics, debugging, and agent-composition optimization
- a pile-first intake path: raw fodder lands repo-less by default and is
  shaped later through explicit, audited resolutions

Hosted task tools can store tickets, but they usually treat agents as API
clients bolted onto a human workflow. Orchestrators can remember their own
leases, but that partitions work by runner. Powder's differentiated bet is
that the board is the lock manager: Bitterblossom, Codex, Herdr, cron, and a
human with curl can share one pool without trusting chat memory or duplicate
dispatch loops.

Operator ruling, 2026-07-03: "powder is unopinionated — audit over
enforcement."

That ruling makes auditable history the product's load-bearing invariant.
Powder records who changed what, when, and through which surface. Ready queries
and claims remain useful coordination primitives, but they are not lifecycle
law: an authorized actor may set any card status in one call, with or without a
claim or proof artifact. A stale runner must not wedge the queue, and external
systems must be able to reconcile by writing the truth they know while Powder
preserves the trail.

## Audience

Powder is for operators and small teams who want a self-hosted alternative to
hosted agent-work boards. It is also for agent orchestrators that need a narrow
work API instead of assigning jobs through chat transcripts.

The primary user is technical: someone comfortable running one service,
mounting one volume, configuring auth by env, and importing their own data. A
future hosted version may exist, but the product core must not assume one.

## Product Shape

**One deployable.** Powder should remain a Rust service with one Docker image,
one SQLite database, one plain-Linux-host deployment target (production runs on
a DigitalOcean droplet), optional Litestream replication, health/readiness
routes, first-run onboarding, and configuration through environment variables.

**One semantic contract.** HTTP, CLI, MCP, and the shipped skill are adapters
over the same domain language: cards, runs, activity, audit events, claims,
relations, links, comments, ready work, input requests, and optional proof.

**A board, not a runner.** Powder stores work, locks, session state, timelines,
events, and evidence. Codex, Herdr, Sprites, cron jobs, Bitterblossom agents,
or other dispatchers may claim work from Powder and execute elsewhere, but the
dispatch loop and every model call are outside the core.

**A human face on the same state.** The API/MCP/CLI contract comes first, but
the product should still feel excellent to operate. The human UI is a thin
layer over the same cards, claims, timelines, relations, blockers,
awaiting-input states, and proof links that agents consume -- never a
separate human-only data model or a feature-parity project-management clone.
At small scale that thin layer is a gorgeous raw Kanban board. At fleet
scale -- thousands of cards, dozens of repos, agents generating work
continuously and without it being an event -- a raw ticket-by-ticket board
stops being legible to a human, so the default human view becomes live
rollups (epics, themes, velocity) computed from the same card graph, with
raw per-ticket browsing one click away, not the landing page. Agents keep
raw, full-fidelity access through the API/CLI/MCP contract regardless of
what the human default renders (operator ruling, 2026-07-17: see
`powder-epic-first-human-board`).
At every scale the human face gains a durable answer loop (operator ruling,
2026-07-21): a question asked on any surface becomes an audited request
record, and an external worker writes back a cited answer — with principal,
worker label, model, cost, and duration — through the same deterministic
interfaces agents use. Answers accrete as inspectable rows; Powder still
never calls a model.

**Instance data stays in instances.** The public repo may contain Powder's own
product-development epics, synthetic fixtures, and sample config. Imported or
operator/customer backlog, card, run, claim, activity, and proof data belongs in
a deployed database and must not be committed here.

## Product Principles

1. **Ready is a query, not law.** Eligibility must be explainable from card
   status, blockers, acceptance, priority, age, and claim expiry.
2. **Claims coordinate; they do not govern truth.** Agents may acquire an
   expiring lock before acting so duplicate workers do not silently collide,
   but a claim is not required to correct status.
3. **Audit beats enforcement.** Completion may include evidence: a PR,
   artifact, CI run, transcript, or other reviewable link. Powder records the
   actor, time, and change even when proof is absent.
4. **Human input is a state.** Awaiting a decision is part of the run model,
   not a buried comment convention.
5. **Adapters stay thin.** Business rules live in `powder-core`; API, CLI, MCP,
   and skill surfaces should not grow separate semantics.
6. **Private by deployment, public by repo.** Powder is a public product for
   private instances, tailnet-friendly auth, and bring-your-own-data operation.
7. **Small beats feature parity.** Do not clone a full project-management UI
   before the agent contract is boring and trustworthy.
8. **Triggers beat polling.** Ready queries must exist, but Powder should also
   emit deterministic events that other systems can subscribe to.
9. **No model boundary inside Powder.** Rules, persistence, identity, policy,
   locks, and event delivery are deterministic. Judgment happens in external
   workers that write their results back.

## Current Build Shape And Proof Debt

The current scaffold establishes the intended shape, but the contract is not
yet trustworthy enough for a fleet to depend on:

- `powder-core` defines cards, runs, activity, audit events, relations, links,
  comments, ready eligibility, expiring claims, permissive status changes,
  and optional completion proof. The card status vocabulary is seven states
  (`backlog`, `ready`, `in_progress`, `awaiting_input`, `done`, `shipped`,
  `abandoned`); see `docs/status-vocabulary.md` for the ratified decision,
  the 9->7 migration mapping, and the rejected alternatives.
- `powder-store` persists the instance database in SQLite, enables WAL, owns
  migrations, stores hashed API keys, seeds the first bootstrap key once, and
  runs transactional card lifecycle operations.
- API keys authenticate neutral integration principals. Claims and runs keep
  that principal distinct from the declared worker label and unique run id, so
  one orchestrator credential can coordinate many workers without lying in the
  audit trail or minting a key per persona.
- `powder-cli` can initialize an instance database, create cards, list ready
  work, claim, transition, and complete cards.
- `powder-mcp` exposes the full agent toolset (21 default tools plus a
  9-tool admin add-on gated by `POWDER_MCP_TOOLSETS`) over stdio using the
  same domain model; it uses SQLite when `POWDER_DB_PATH` is set, or a
  deployed instance over HTTP when `POWDER_API_BASE_URL`/`POWDER_API_KEY`
  are set instead. See `SKILL.md`'s "Expected MCP Tools" for the current
  list.
- `powder-server` is the single deployable HTTP app with `/healthz`, `/readyz`,
  first-run onboarding state, API-key auth, and tailnet/none modes.
- Docker, systemd, Litestream, and env examples follow the Canary-style
  self-hosted deployment pattern; a hosted-platform example survives only as
  the self-hoster reference in `docs/self-hosting.md`.

The important remaining gaps are not polish. The audit trail needs to stay
consistent across SQLite, HTTP, CLI, MCP, and the Kanban board; relations and
webhooks need live-operator proof; the answer loop, real identity and
authority, private-ingress conformance, deterministic event emission, and a
Kanban surface must keep making the same state legible to humans.

## Non-Goals

- No real operator backlog, run, claim, or activity data in this repository.
- No dispatch daemon inside `powder-core`.
- No model calls inside Powder.
- No hidden dependency on Gradient, Hermes, or any one operator's `kanban.db`.
- No one-to-one REST-to-MCP wrapper dump that obscures agent intent.
- No hosted multi-tenant SaaS assumption in the product core.
- No feature-parity Linear clone before the agent-first contract is solid.

## Excellent In 6-12 Months

Powder is the obvious self-hosted work ledger for agentic software teams. A new
operator can deploy it on any plain Linux host or DigitalOcean droplet, mount
SQLite storage, choose tailnet or shared
secret auth, complete first-run onboarding, create or integrate cards through
explicit APIs, configure rules and webhooks, inspect a beautiful Kanban board, and let agents
safely ask:

> What exists, what is ready, can I claim it, what context matters, and what
> history or proof explains its current state?

Humans inspect the same state agents use. Each run and card change leaves a
durable trail.
Terminal history compresses into compact summaries by default while full
events and proof stay retrievable. Run telemetry makes cost and
effectiveness per agent and per model queryable, so the board doubles as
the fleet's evaluation dataset.
Private backlog data stays in the deployment that owns it. External workers can
make intelligent judgments, but Powder remains the boring source of truth for
what work exists, who holds it, what happened, and what proof settled it.
