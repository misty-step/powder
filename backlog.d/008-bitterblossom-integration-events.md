# Bitterblossom integration events

Priority: P2 | Status: backlog | Type: Epic

## Goal
Make Powder the pile and state machine half of the Factory loop while keeping
intelligence in Bitterblossom workloads. Powder emits deterministic events for
ticket-added, moved-to-ready, awaiting-input, claim-expired, completed, and
similar state changes; Bitterblossom subscribes and decides what agent work to
run.

## Oracle
- [ ] Rules can be configured per repository and label without embedding model calls or budget policy in Powder.
- [ ] A webhook or SSE consumer receives ticket-added and moved-to-ready events with enough card context to act.
- [ ] Event delivery is durable enough to retry or detect missed notifications.
- [ ] A Bitterblossom demo consumes a Powder event and writes back via Powder API/MCP without Powder calling a model.
- [ ] Ramp-up/ramp-down and spend policy remain outside Powder.

## Children
- Define the event vocabulary and rule configuration shape.
- Add a deterministic webhook/SSE delivery path.
- Record delivery attempts as activities or operational logs.
- Build the first BB subscriber demo after lifecycle and identity are trustworthy.
