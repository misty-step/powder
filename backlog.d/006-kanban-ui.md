# Kanban UI

Priority: P2 | Status: backlog | Type: Epic

## Goal
Give humans a gorgeous, useful face on the same state agents use. Powder remains
a dumb coordination ledger, but it should provide a Misty Step aesthetic-kit
Kanban board that makes cards, claims, blockers, timelines, awaiting input, and
proof legible without becoming a full project-management clone.

## Oracle
- [ ] A deployed human can view backlog, ready, claimed/running, awaiting-input, review, blocked, and done lanes from the same API state agents use.
- [ ] Claim ownership, expiry, blockers, and proof links are visually legible without reading raw JSON.
- [ ] Awaiting-input cards can be answered from the UI once the answer-loop epic has landed.
- [ ] The first viewport is the board itself, not a marketing page or decorative dashboard.
- [ ] Responsive screenshots prove the board is usable on desktop and mobile.

## Children
- Choose the thin UI host shape that preserves the one-deployable Rust service.
- Build board read views before mutation controls.
- Add answer/release/renew controls only after their APIs exist.
- Verify visual fit with Misty Step aesthetic-kit conventions.
