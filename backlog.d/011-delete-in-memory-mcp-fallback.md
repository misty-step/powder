# Delete the in-memory MCP fallback and its duplicate dispatch layer

Priority: P2 | Status: done | Type: Bug

## Goal
The original groom teardown flagged this and it was never fixed: `powder-mcp`
fell back to an ephemeral in-memory `Board` whenever neither
`POWDER_API_BASE_URL` nor `POWDER_DB_PATH` was set. An agent using that mode
believed its claims and completions persisted; nothing did — the state
evaporated on process exit. The fallback was backed by a hand-duplicated
JSON-RPC dispatch layer (`call_tool`/`handle_json_rpc` against `Board`,
parallel to `call_tool_store`/`handle_json_rpc_store` against `Store`) with
zero claim-holder authority enforcement (backlog.d/004 never reached it),
making it not just redundant but a straight security regression relative to
the real path.

## Oracle
- [x] `powder-mcp` with neither `POWDER_API_BASE_URL` nor `POWDER_DB_PATH` set
      fails loudly (non-zero exit, clear stderr message) instead of silently
      starting an ephemeral in-memory instance.
- [x] The duplicate Board-based dispatch layer (`call_tool`, `handle_json_rpc`)
      is deleted; only the `Store`-backed and remote-HTTP dispatch paths
      remain.
- [x] `powder_core::Board` itself is untouched — `powder-cli`'s
      `list-ready <backlog.d-path>` (no `--db`) still uses it for a
      legitimate read-only preview of a directory's readiness, unrelated to
      MCP's dangerous stateful fallback.
- [x] Docs (`SKILL.md`) no longer describe the removed fallback mode.

## Progress
- 2026-07-02 slice (overnight autonomous): confirmed live (not stale) via
  grep that both the dangerous fallback and the duplicate dispatch layer
  were still present, exactly as the original groom report described.
  Deleted `powder_mcp::call_tool`/`handle_json_rpc` (the `Board`-based
  JSON-RPC dispatch, ~130 LOC) and their superseded test
  (`mcp_claim_request_input_and_complete_flow`, a structural duplicate of
  `mcp_tools_can_operate_against_sqlite_store`). `powder-mcp/src/main.rs` no
  longer falls back silently: with neither `POWDER_API_BASE_URL` nor
  `POWDER_DB_PATH` set, it now prints a clear error naming both valid modes
  and exits 1. `powder_core::Board` itself is untouched -- `powder-cli`'s
  `list-ready <path>` (no `--db`) still legitimately uses it for a read-only
  directory-readiness preview, a different, safe use case from MCP's
  stateful claim/completion path. Updated `SKILL.md`'s stale description of
  the fallback mode.
  Proof: new integration test
  `crates/powder-mcp/tests/no_ephemeral_fallback.rs` spawns the actual
  compiled binary with both env vars unset and asserts non-zero exit, a
  stderr message naming both valid modes, and no JSON-RPC output on stdout.
  101 workspace tests green (fmt/clippy/test); powder-mcp's unit test count
  dropped from 7 to 6 (duplicate removed) while gaining 1 new integration
  test.
