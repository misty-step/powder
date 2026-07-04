# API-filed cards must land their source markdown on disk

Priority: P2 | Status: ready

## Goal
Found 2026-07-04: the powder-powder epic (and bitterblossom-901) were created via POST /api/v1/cards/import using the files (inline content) variant of ImportRequest, never the path variant. card.source records a path (powder-mission-critical-hardening.md) and a sha256 digest, but the file itself never touched any filesystem this instance can read -- it existed only in the request body of a one-off import call from a prior session. The first time anyone needs to edit that card's acceptance/body (this session did, for the heartbeat-rotation update), there is no file to open: it has to be reconstructed from the live card JSON, hoping the reconstruction round-trips through the same parser (id_from_path in crates/powder-core/src/backlog.rs:65-74, title_from_contents, parse_field, oracle_items) closely enough to match. That reconstruction worked this time only because the id-from-filename convention (stem before first '-') and repo-slug namespacing (crates/powder-shell/src/lib.rs:113-123) were reverse-engineered from source, and because the card's fields were simple enough to reproduce faithfully. Any card imported via files with no on-disk counterpart is one edit away from this same trap.

## Oracle
- [ ] The import handler (crates/powder-server/src/main.rs import_cards / files branch) writes each inline file to the repo's own backlog.d (or a configured durable location) as part of a successful (non-dry-run) import, so source.path always resolves to a real file after import
- [ ] Alternatively: reject or flag files-based imports for repos the server cannot also see on disk, and surface that flag on GET card responses (e.g. source.on_disk: false) so editors know reconstruction is needed before any edit
- [ ] Docs/README note the edit-file-then-reimport workflow as the sanctioned way to change an existing card's content
