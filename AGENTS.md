# Powder Repo Contract

Powder is a Rust-first, public, self-hostable agent work application. It is the
tool people deploy to host their own backlog data; it is not a repository that
stores the operator's backlog.

Read `VISION.md` before changing product scope, the card/claim model, the runner
boundary, or the self-hosting/deployment shape.

## Architecture

- `powder-core` owns domain rules and imports no adapter, shell, runtime, DB,
  network, filesystem, or process-launching crates.
- `powder-shell` owns filesystem-facing import/parsing helpers (backlog.d
  loading, repo id-namespacing, the GitHub issue adapter) and the shared
  adapter error type. `powder-store::Store` is called concretely by every
  face; there is no effect-trait/port indirection layer, so do not
  reintroduce one without a concrete second implementation that needs it.
- `powder-store` owns SQLite schema, migrations, WAL pragmas, API keys, and
  transactional persistence. Adapters do not assemble lifecycle SQL directly.
- `powder-api`, `powder-cli`, and `powder-mcp` are thin faces over the same
  domain and shell contracts. No business rule may live only in an adapter.
- The board store is separate from the runner. A dispatch daemon may consume
  `ready` cards later, but it is not in the core.
- MCP tools are designed around agent intent, not one-to-one REST wrappers.
- Root `backlog.d/` may contain Powder product-development epics in the house
  Goal/Oracle format. Do not commit imported/operator/customer card, claim,
  event, work-log, or instance export data; fixtures belong under tests only.
  Instance data lives in the deployed SQLite database.
- Follow the Canary-style deployment shape: one deployable Rust service, SQLite
  path from env, WAL, Fly volume at `/data`, Litestream optional replication,
  health and readiness routes, first-run bootstrap key, and tailnet-friendly
  auth configuration.

## Gates

Run before claiming completion:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The `master` branch protection rule requires the GitHub Actions
`Rust CI / fmt-clippy-test` status check with strict status checks and admin
enforcement enabled; `master` runs the same gate after merge.

## Red Lines

- Do not add personal/operator backlog data to the repo.
- Do not lower gates or add mocked internal collaborators to get green.
- Do not add a dispatch loop to the core.
