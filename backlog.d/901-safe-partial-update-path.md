# Cards need a safe partial-update path

Priority: P1 | Status: ready

## Goal
Found 2026-07-04 while updating the powder-powder epic's acceptance lines after a completed heartbeat-token rotation. The only by-id mutation route for an existing card is POST /api/v1/cards (create_card, crates/powder-server/src/main.rs:697-741). It builds a fresh Card via Card::new(...).with_created_at(now) unconditionally -- there is no created_at field in CreateCardRequest to preserve it -- and the request schema has no fields at all for source, labels, claim, branch_name, or workspace_path. The store call is Board::upsert_card (crates/powder-core/src/board.rs:56-58), a raw HashMap::insert -- full replace, not a merge. Using this route to tweak one field on an existing card silently resets created_at to the call time and drops source/labels/claim/branch_name/workspace_path, since none of those survive the round trip through CreateCardRequest. The only safe existing path is re-running /api/v1/cards/import with the full reconstructed backlog markdown (merge_reimport in crates/powder-core/src/model.rs:503-511 correctly preserves created_at and protected lifecycle fields), but that requires having the original source file on disk and re-deriving the exact acceptance/body text, which is heavyweight for a one-line edit. On the product whose thesis is never down, never lose data, always usable, the one card-mutation surface that touches an existing record by id is a data-loss footgun.

## Oracle
- [ ] A route exists to patch specific card fields (acceptance, body, title, priority, status, labels) on an existing card id without touching created_at, source, claim, branch_name, or workspace_path unless explicitly included in the request
- [ ] create_card (POST /api/v1/cards) either rejects requests whose id already exists, or is documented and tested as an explicit full-replace with a warning, so it can no longer be reached for accidental partial edits
- [ ] A regression test asserts that patching one field of an existing card preserves created_at and source across the call
