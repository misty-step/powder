# Import backlog.d markdown into cards

Priority: P0 | Status: ready | Estimate: M

## Goal
Turn git-native `backlog.d/*.md` tickets into Powder cards without losing the source oracle text.

## Oracle
- [ ] Given a fixture repo with `backlog.d/001-example.md`, `powder import backlog.d --dry-run` reports one card with identifier `001`, title, priority, status, estimate, goal, and oracle.
- [ ] Invalid tickets are reported with file paths and do not partially import.
- [ ] The importer records the source path and content digest for traceability.

## Verification System
- Claim: Powder can ingest the fleet's markdown backlog shape into durable cards.
- Falsifier: A valid backlog ticket loses its oracle, status, or source path.
- Driver: Fixture directory plus CLI dry-run and import commands.
- Grader: Snapshot of parsed card fields and source digest.
- Evidence packet: CLI transcript, fixture files, and parsed-card JSON.
- Cadence: Every importer change.

## Notes
**Why:** The powder lane says the only migration input is the fleet's `backlog.d/` dirs.
