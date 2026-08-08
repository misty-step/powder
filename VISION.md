# Powder Vision

Powder is one self-hosted SQLite work ledger for agent-driven software teams.
It records work, temporary ownership, typed run history, and attributed events.
A deployed instance owns its data; this repository ships the service and its
faces.

Powder is the narrow boundary between an agent that needs durable coordination
and a project-management system that owns every workflow. It does not run
models, dispatch workers, shape intake, or provide analytics. External agents,
workers, and humans make judgments and write their results through the same
contract.

## Product Boundary

Powder answers six operational questions:

1. What work exists?
2. What work is ready?
3. Who holds the current lease?
4. What typed run history explains the work?
5. What event history records who changed what and when?
6. What proof or input is needed to continue or close the work?

The semantic model has four load-bearing concepts:

- **Card** stores context, acceptance criteria, status, priority, labels,
  relations, links, comments, work-log entries, proof, and an optional opaque
  `repo` string. Repository filtering is exact string filtering; `repo` is not a
  registry or identity system.
- **Claim** stores the current principal, worker label, run identifier, lease,
  expiry, and liveness. Claims coordinate concurrent workers and expire without
  changing the truth of the card.
- **Run** is typed claim-scoped history. It stores claim attempts, lifecycle
  state, principal, worker, timestamps, proof, and typed elicitation/response
  activity. A run is not a telemetry, session-forensics, or evaluation product.
- **Event** is immutable, attributed audit history with an ordered outbound
  sequence and an SSE tail. Event payloads remain typed; Powder does not replace
  fields with arbitrary JSON blobs.

Relations include generic parent and blocker edges. A parent edge is not an
epic, rollup, velocity, or portfolio product. Links, comments, work logs,
proof, full-text search, ready snapshots, idempotency, and claim expiry remain
ledger behavior. Awaiting input is a typed run state, not a separate answer
product.

## Product Shape

Powder has one Rust service and one SQLite database. WAL, migrations,
Litestream-safe backup and restore, health/readiness, and authentication remain
part of the deployment contract.

Agents use the `powder` CLI and shipped `SKILL.md`. HTTP serves the human UI and
external integrations. SSE exposes the ordered event tail. These faces share
one deterministic contract; none is a second domain model.

The human face is small and responsive. It provides ledger list or Kanban
views, search, card detail, create/edit input, claim state, timeline, proof,
awaiting-input answer, auth state, keyboard access, routing, empty states,
security, and accessibility. It is not a portfolio dashboard, settings
product, media browser, or theme system.

Powder is a board, not a runner. Dispatch loops, model calls, prompt/session
capture, evaluation, shaping, ingestion, and external analytics run outside
Powder. External workers may use cards, links, relations, proof, and events as
stable anchors.

## Principles

1. **A card carries an oracle.** Acceptance criteria make ready work
   explainable.
2. **A claim coordinates.** A lease prevents duplicate work and expires after
   liveness stops; it does not govern truth.
3. **A run stays typed.** Claim attempts, lifecycle, proof, identity, and input
   survive handoff and crash without telemetry fields.
4. **Events preserve attribution.** Every change records its actor, principal,
   time, operation, and ordered position where applicable.
5. **Typed data beats blobs.** Durable fields and typed activities remain
   inspectable and migratable.
6. **Adapters stay thin.** Domain rules live below HTTP, CLI, UI, and skill
   faces.
7. **One deployment is enough.** One service, one database, and one backup
   story reduce operational failure modes.
8. **Small beats parity.** Add no project-management surface without a direct
   ledger need and observed operator evidence.
9. **No model boundary inside Powder.** Judgment and execution remain in
   external workers.

## Migration Safety

The first cutover wave removes active rejected surfaces while historical tables
and columns remain readable when needed. A later schema migration will
inventory retained data and remove dormant storage.

Migrations preserve row counts, attribution, typed run history, elicitation and
response activity, proof, parent edges, links, claim expiry, event order,
idempotency, and the opaque card `repo` value. Historical data is exported or
backed up before destructive migration. WAL-safe backup and restore proof is a
release prerequisite.

No migration replaces typed fields with arbitrary JSON. No compatibility alias,
fallback, replacement framework, or generic event envelope is added to preserve
a rejected product.

## Current Build And Cutover Notes

Production runs one `powder-server` service with SQLite on a host volume, WAL
enabled, optional Litestream replication, health/readiness routes, and the
configured authentication boundary. The CLI and `SKILL.md` remain the agent
face; HTTP serves integrations and the UI.

During the first wave, code lanes may remove active registry, webhook-delivery,
telemetry, media, portfolio, and static periphery surfaces while their
historical storage remains readable. The surviving contract is the boundary
above. Existing status vocabulary, claim behavior, typed input activity, proof,
links, comments, work logs, FTS, ready snapshots, idempotency, and ordered SSE
events remain load-bearing.

## Non-Goals

Powder does not provide:

- repository registration, aliases, tiers, visibility, import provenance, or
  repository administration; cards keep only an optional opaque exact-filtered
  `repo` string;
- signed webhook subscriptions, delivery retries, dead letters, replay, or a
  background delivery worker; ordered outbound events and the SSE tail remain;
- run telemetry attempts, model or token pricing, cost aggregates, science,
  exports, comparison datasets, or evaluation analytics;
- attachment BLOB upload, attachment storage, attachment UI, or field-note
  generation;
- epic recomposition, rollups, velocity, overview dashboards, or portfolio
  analytics; parent remains a generic relation;
- ingestion, shaping, lineage, session forensics, prompt capture, embedding,
  announcement, theme, or answer-product subsystems;
- a dispatch daemon, model call, hosted multi-tenant SaaS assumption, or
  repository-local backlog;
- an MCP face or another agent protocol beside `powder` plus `SKILL.md`;
- a marketing site or a second UI design system.

Powder remains a boring source of truth for what work exists, who holds its
lease, what typed run history records, what changed, and what proof settled it.
