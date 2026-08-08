# Powder Repo Contract

Powder is a Rust-first, public, self-hostable SQLite work ledger for
agent-driven teams. A deployed instance owns its data; this repository ships
the service and its supported faces.

Read `VISION.md` before changing product scope, the Card/Claim/Run/Event model,
the runner boundary, or the self-hosting shape.

## Architecture

- `powder-core` owns Card, Claim, typed Run, Event, status, readiness, claim,
  relation, proof, and input rules. It imports no adapter, shell, runtime, DB,
  network, filesystem, or process-launching crates.
- `powder-store` owns SQLite schema, migrations, WAL pragmas, API keys,
  idempotency, ordered events, and transactional persistence. Adapters do not
  assemble lifecycle SQL directly.
- `powder-api` and `powder-cli` are thin faces over the same domain and store
  contracts. Agents use the CLI plus `SKILL.md`; HTTP serves the responsive UI
  and integrations; SSE serves the ordered event tail.
- The human UI is limited to ledger list/Kanban, search, detail, create/edit,
  claim state, timeline, input, proof, auth, routing, keyboard, responsive,
  security, and accessibility behavior.
- The board store is separate from any runner. Dispatch loops and model calls
  are external and are not part of Powder.
- The optional Card `repo` field is an opaque exact-filtered string. Powder has
  no repository registry, alias, tier, visibility, or import product.
- Repository-local ticket directories are forbidden. Powder product work lives
  in the deployed Powder instance; R90 work lives in Habitat. Do not commit
  imported/operator/customer card, run, claim, activity, or instance export
  data. Instance data lives in the deployed SQLite database.
- Production runs one Rust service (`powder-server`) on a DigitalOcean droplet.
  SQLite lives on a host volume with WAL enabled. Litestream replication is
  optional, and ingress is through the configured private boundary.

## Cutover Rules

- The first wave may remove active rejected surfaces while historical tables and
  columns remain readable. A later migration owns dormant-storage cleanup.
- Preserve typed run history, elicitation/response activity, attribution,
  proof, parent edges, links, comments, work logs, claim expiry, event order,
  FTS, ready snapshots, idempotency, and the opaque `repo` value.
- Never replace typed fields with generic JSON blobs. Do not add fallbacks,
  adapters, replacement frameworks, or a second agent face.
- Do not add repository ingestion, shaping, session forensics, telemetry,
  analytics, attachments, field-note generation, portfolio rollups, webhook
  delivery, or marketing/static product surfaces.

## Gates

Run before claiming completion:

```sh
test -z "$(find . -type d -name backlog.d -not -path './.git/*' -print -quit)"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The `master` branch protection rule requires the GitHub Actions
`Rust CI / fmt-clippy-test` status check with strict status checks and admin
enforcement enabled; `master` runs the same gate after merge.

## Red Lines

- Do not add personal/operator backlog data to the repo.
- Do not create a repository-local ticket or Kanban ledger.
- Do not weaken gates or add mocked internal collaborators to get green.
- Do not add a dispatch loop or model call to the core.
- Do not add a registry, telemetry, media, portfolio, webhook-delivery, or
  marketing product beside the narrow ledger.
