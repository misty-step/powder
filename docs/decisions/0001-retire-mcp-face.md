# 0001. Retire the MCP face

## Status

Accepted

## Context

Powder exposed three agent-facing adapters: HTTP API, `powder` CLI, and
`powder-mcp`. MCP duplicated the CLI contract with a second tool catalog,
second launch path, and second remote client. Agents already shell out. A
second face raised drift risk and skill bloat.

## Decision

1. Delete `powder-mcp` as a product face.
2. Agents use the `powder` CLI plus `SKILL.md` only.
3. HTTP remains the server contract for UI and integrations.
4. Before deletion, every agent workflow that MCP offered remotely must work
   on the CLI with `POWDER_API_BASE_URL` / `POWDER_API_KEY` (no `--db`).

## Consequences

- One command surface to document and test.
- Skill text stays short: workflow plus `powder <cmd> --help`, not a tool dump.
- Harness entries that launched `powder-mcp` must switch to `powder`.
- Release, Docker, and install scripts ship `powder` and `powder-server` only.

## Non-goals

- No new agent RPC layer.
- No forced HTTP-from-agent path; CLI is the agent face.
