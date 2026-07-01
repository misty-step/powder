# Powder

Powder is a greenfield, agent-first work-management app for the Factory: a
durable board for backlog cards, claims, runs, activity, links, comments, and
human-in-loop pauses.

The first milestone is intentionally small:

- `powder-core`: pure domain vocabulary and scheduling rules.
- `powder-shell`: effect ports for storage, time, and ids.
- `powder-api`: HTTP/API contract surface.
- `powder-cli`: human and agent command-line face.
- `powder-mcp`: MCP tool contract for agents.
- `SKILL.md`: shipped agent-facing usage contract.
- `backlog.d/`: v0 build queue.

The dispatch daemon is not part of the core. It will consume the board through
the API/MCP/CLI surfaces and run agents elsewhere.

## Gate

```sh
cargo test --workspace
```
