# Ship the v0 MCP tools

Priority: P0 | Status: ready | Estimate: L

## Goal
Expose Powder's agent workflow through intent-shaped MCP tools.

## Oracle
- [ ] `powder-mcp` advertises exactly the v0 tools: `list_ready`, `claim_card`, `update_status`, `request_input`, and `complete_card`.
- [ ] Each tool description states when to use it and the required argument shape.
- [ ] Validation errors are returned as tool errors so the model can self-correct.
- [ ] No destructive action is bundled into a read-only tool.

## Verification System
- Claim: Agents can operate Powder through MCP without shelling out to ad hoc commands.
- Falsifier: A v0 workflow step requires an unmodeled command or a tool mixes read and write risk.
- Driver: MCP stdio smoke transcript covering list, claim, input request, and completion.
- Grader: Tool list, schemas, error envelopes, and resulting card/run state.
- Evidence packet: MCP transcript and matching store snapshot.
- Cadence: Every MCP surface change.

## Notes
**Why:** The factory report names MCP plus SDK as the repeated fleet-wide gap; Powder should not repeat it.
