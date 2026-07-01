# Powder Vision

Powder is the self-hostable work substrate for agent-driven software teams.

The repository ships the application. A deployed instance owns the data.
Operators bring their own backlog, store it in their own SQLite database, and
connect their own agents, runners, and humans through Powder's API, CLI, MCP,
and skill surfaces.

Powder should feel like the narrow missing tool between "a chat thread full of
tasks" and "a hosted project-management system that assumes humans are the
primary workers." It is not the operator's backlog, not a static `backlog.d`
repo, and not a private Factory board. It is the public product someone deploys
so their work can be claimed, paused, audited, and completed with proof.

## Why It Exists

Agent work needs durable coordination primitives:

- a card with enough context and an acceptance oracle
- a ready query that deterministic code can answer
- an expiring claim so duplicate agents do not collide
- a run timeline that survives handoff, crash, and compaction
- proof links and completion records a human can inspect
- an explicit awaiting-input state instead of invented approvals

Hosted task tools can store tickets, but they usually treat agents as API
clients bolted onto a human workflow. Powder treats agents as first-class
workers while keeping ownership, policy, auth, persistence, and audit in the
operator's deployment.

## Audience

Powder is for operators and small teams who want a self-hosted alternative to
hosted agent-work boards. It is also for agent orchestrators that need a narrow
work API instead of assigning jobs through chat transcripts.

The primary user is technical: someone comfortable running one service,
mounting one volume, configuring auth by env, and importing their own data. A
future hosted version may exist, but the product core must not assume one.

## Product Shape

**One deployable.** Powder should remain a Rust service with one Docker image,
one SQLite database, one Fly-friendly deployment target, optional Litestream
replication, health/readiness routes, first-run onboarding, and configuration
through environment variables.

**One semantic contract.** HTTP, CLI, MCP, and the shipped skill are adapters
over the same domain language: cards, runs, activity, claims, links, comments,
ready work, input requests, and proof-backed completion.

**A board, not a runner.** Powder stores work, locks, session state, timelines,
and evidence. Codex, Herdr, Sprites, cron jobs, or other dispatchers may claim
work from Powder and execute elsewhere, but the dispatch loop is outside the
core.

**Instance data stays in instances.** The public repo may contain synthetic
fixtures and sample config. Real backlog/card/run data belongs in a deployed
database and must not be committed here.

## Product Principles

1. **Ready is a query, not vibes.** Eligibility must be explainable from card
   status, blockers, acceptance, priority, age, and claim expiry.
2. **Claim before work.** Agents acquire an expiring lock before acting so
   duplicate workers do not silently collide.
3. **Proof beats status.** Completion requires evidence: a PR, artifact, CI
   run, transcript, or other reviewable link.
4. **Human input is a state.** Awaiting a decision is part of the run model,
   not a buried comment convention.
5. **Adapters stay thin.** Business rules live in `powder-core`; API, CLI, MCP,
   and skill surfaces should not grow separate semantics.
6. **Private by deployment, public by repo.** Powder is a public product for
   private instances, tailnet-friendly auth, and bring-your-own-data operation.
7. **Small beats feature parity.** Do not clone a full project-management UI
   before the agent contract is boring and trustworthy.

## Current Product Truth

The current scaffold already establishes the shape:

- `powder-core` defines cards, runs, activity, links, comments, ready
  eligibility, expiring claims, completion proof, and markdown backlog parsing.
- `powder-cli` can dry-run imports and list ready cards from synthetic fixture
  data.
- `powder-mcp` exposes `list_ready`, `claim_card`, `update_status`, `add_link`,
  `request_input`, and `complete_card` over stdio using the same domain model.
- `powder-server` is the single deployable HTTP app with `/healthz`, `/readyz`,
  and first-run onboarding state.
- Docker, Fly, Litestream, and env examples follow the Canary-style
  self-hosted deployment pattern.

The important gap is persistence: the board rules exist in memory today. The
next durable milestone is a SQLite store, migrations, auth enforcement, and
import into the instance database rather than fixture-backed memory.

## Non-Goals

- No real operator backlog, run, claim, or activity data in this repository.
- No dispatch daemon inside `powder-core`.
- No hidden dependency on Gradient, Hermes, or any one operator's `kanban.db`.
- No one-to-one REST-to-MCP wrapper dump that obscures agent intent.
- No hosted multi-tenant SaaS assumption in the product core.
- No feature-parity Linear clone before the agent-first contract is solid.

## Excellent In 6-12 Months

Powder is the obvious self-hosted work ledger for agentic software teams. A new
operator can deploy it on Fly, mount SQLite storage, choose tailnet or shared
secret auth, complete first-run onboarding, import their own backlog markdown,
and let agents safely ask:

> What is ready, can I claim it, what context matters, and what proof do I need
> to finish?

Humans inspect the same state agents use. Each run leaves a durable trail.
Private backlog data stays in the deployment that owns it.
