# Claim lifecycle correctness

Priority: P0 | Status: done | Type: Epic

## Goal
Make Powder's claim lifecycle boring enough that heterogeneous runners can
share one work pool without duplicate work, leaked locks, or advertised cards
that cannot actually be claimed. The board is the lock manager; its lease
semantics must be implemented once and exercised through the SQLite-backed
surface every adapter uses.

## Oracle
- [x] The live-proven reclaim repro passes: a running card with an expired claim appears ready and can be claimed by a different agent without a `running -> claimed` transition failure.
- [x] Voluntary release clears claim ownership immediately and the card is visible to another agent before TTL expiry.
- [x] Claim renewal and heartbeat/update operations extend liveness without requiring huge initial TTLs.
- [x] Same-agent retry is idempotent for a still-valid claim, while competing agents receive a conflict.
- [x] `rg` and tests show one lifecycle implementation owns claim/release/renew/heartbeat semantics; adapters do not maintain divergent domain rules.
- [x] `cargo test --workspace` passes with regression coverage for the prior livelock and claim-leak bugs.

## Children
- Reclaim expired `claimed` and `running` cards atomically in the store.
- Add explicit `release_claim`, `renew_claim`, and heartbeat/progress operations.
- Collapse duplicated lifecycle behavior between `powder-core::Board`, `powder-store::Store`, and MCP helper paths.
- Add contention coverage: many agents, one card, exactly one successful active lease.
- Update CLI, HTTP, MCP, and skill docs only after the shared semantics exist.
