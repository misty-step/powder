# 0002. Radical Ledger Boundary

## Status

Accepted

## Context

Powder had accumulated product commitments around intake, repository
administration, run telemetry, session and attachment forensics, webhook
delivery, portfolio rollups, answer records, and a marketing surface. Those
commitments widened the service beyond the coordination problem that requires a
lease-backed work ledger.

The durable boundary must remain small enough to keep one truth across the
SQLite store, CLI, skill, HTTP API, SSE tail, and responsive UI. Historical
rows may still be needed for audit and safe migration, but historical storage
is not a product commitment.

## Decision

Powder is one self-hosted Rust service over one SQLite database. Its semantic
model has four concepts:

1. **Card** stores work context, acceptance criteria, status, priority, labels,
   relations, links, comments, work logs, proof, and an optional opaque exact-
   filtered `repo` string.
2. **Claim** stores the current principal, worker, run lease, expiry, and
   liveness so concurrent work can coordinate without making a claim the source
   of truth.
3. **Run** is typed claim-scoped history. It retains claim attempts, lifecycle,
   principal, worker, timestamps, proof, and typed elicitation/response
   activity. It does not store telemetry or session forensics.
4. **Event** is immutable attributed audit history with ordered outbound events
   and an SSE tail. Event fields remain typed.

The supported faces are the `powder` CLI plus `SKILL.md` for agents, HTTP for
the UI and integrations, and SSE for the ordered event tail. The UI is a small
responsive list/Kanban, search, detail, create/edit, claim, timeline,
awaiting-input, proof, and auth surface.

Keep authentication, idempotency, migrations, WAL, Litestream-safe backup and
restore, health/readiness, FTS, ready snapshots, links, comments, work logs,
proof, parent and blocker relations, claim expiry, typed input activity, and
ordered events.

## Rejected Alternatives

- **Keep the broad platform.** Repository registries, aliases, tiers,
  visibility, ingestion, shaping, lineage, and portfolio rollups duplicate
  systems that already own those workflows.
- **Make telemetry and science first-class.** Models, token counts, pricing,
  cost aggregates, comparison datasets, and evaluation analytics belong in
  external analytics over exported ledger events.
- **Store sessions and attachments.** Prompt/tool/session capture and BLOB
  storage duplicate harness or object-storage systems and increase secret and
  retention risk. Proof links and typed events are sufficient anchors.
- **Remove webhook delivery from the service.** Signed subscriptions, retries,
  dead letters, replay, and a delivery worker are a second transport product.
  Consumers read ordered events through SSE or an explicit outbound feed.
- **Promote runs, answers, or activities into new products.** Typed Run history
  and elicitation/response activity already preserve the lifecycle without a
  separate answer subsystem or arbitrary JSON event envelope.
- **Replace typed fields with generic JSON.** A blob would hide invariants,
  weaken migration checks, and make cross-face behavior drift.
- **Add a compatibility adapter or fallback.** The cutover is a clean removal;
  retaining rejected routes or aliases would preserve the boundary failure.
- **Add a dispatch daemon or model boundary.** Workers and orchestrators remain
  external so Powder stays deterministic.

## Migration Safety

The first wave removes active rejected surfaces but leaves historical tables and
columns readable where they are needed for audit or export. A later schema
migration inventories retained data and drops dormant storage.

Before destructive migration, take a WAL-safe backup and complete a restore
readback. Preserve row counts, attribution, typed run history, questions and
answers, proof, parent edges, links, claim expiry, event order, idempotency,
and the exact opaque Card `repo` value. Migrations must be transactional and
retry-safe. Do not advance a schema version until the old data has a verified
read path and the new read path has parity evidence.

## Consequences

- Product prose and operator guidance describe one narrow ledger instead of
  several adjacent products.
- Agents have one supported face and one lifecycle contract.
- External workers own dispatch, judgment, shaping, analytics, media, and
  delivery policy.
- Historical storage can outlive the active surface until a deliberate,
  backed-up schema migration removes it.
- The small UI remains useful for ledger operations without becoming a project
  management suite.

## Reversal Conditions

Reconsider this decision only when all of the following are true:

1. A concrete operator workflow cannot be completed with Card, Claim, typed Run,
   Event, links, relations, proof, and existing faces.
2. Production evidence shows the missing capability belongs inside the ledger,
   not in an external worker or integration.
3. The proposed capability has an explicit data owner, retention policy,
   migration plan, trust boundary, and cross-face contract.
4. The operator accepts the added service, schema, recovery, and review burden
   in a new ADR before implementation.

A request for feature parity, unmeasured scale, convenience, or analytics alone
is not a reversal condition.
