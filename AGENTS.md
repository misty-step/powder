# Powder Repo Contract

Powder is a Rust-first, public, self-hostable agent work application. It is the
tool people deploy to host their own backlog data; it is not a repository that
stores the operator's backlog.

Read `VISION.md` before changing product scope, the card/run model, the runner
boundary, or the self-hosting/deployment shape.

## Architecture

- `powder-core` owns domain rules and imports no adapter, shell, runtime, DB,
  network, filesystem, or process-launching crates.
- `powder-shell` owns effect traits and ports. Concrete stores and clients
  implement those traits outside the core.
- `powder-api`, `powder-cli`, and `powder-mcp` are thin faces over the same
  domain and shell contracts. No business rule may live only in an adapter.
- The board store is separate from the runner. A dispatch daemon may consume
  `ready` cards later, but it is not in the core.
- MCP tools are designed around agent intent, not one-to-one REST wrappers.
- No real backlog/card/run data belongs in this repo. Use synthetic fixtures
  under tests only. Instance data lives in the deployed SQLite database.
- Follow the Canary-style deployment shape: one deployable Rust service, SQLite
  path from env, Fly volume at `/data`, Litestream optional replication, health
  and readiness routes, and tailnet-friendly auth configuration.

## Gates

Run before claiming completion:

```sh
cargo test --workspace
```

## Red Lines

- Do not add personal/operator backlog data to the repo.
- Do not lower gates or add mocked internal collaborators to get green.
- Do not add a dispatch loop to the core.
