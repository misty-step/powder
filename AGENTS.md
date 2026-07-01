# Powder Repo Contract

Powder is a Rust-first, agent-first work board. It is greenfield: do not reuse
Gradient or the Hermes `kanban.db`. The only planned migration input is
repository `backlog.d/` markdown.

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

## Gates

Run before claiming completion:

```sh
cargo test --workspace
```

## Red Lines

- Do not create, push to, or mutate the GitHub remote until the operator
  explicitly approves the `misty-step/powder` remote plan.
- Do not lower gates or add mocked internal collaborators to get green.
- Do not add a UI, DB schema, dispatch loop, or real MCP runtime in scaffold
  work unless the backlog item explicitly scopes it.
