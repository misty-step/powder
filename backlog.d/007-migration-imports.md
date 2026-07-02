# Migration imports

Priority: P2 | Status: backlog | Type: Epic

## Goal
Import backlog files and GitHub issues from all Factory repos without erasing
live state. Migration is a sync into an instance database, not a destructive
rewrite of claims, statuses, runs, or proof.

## Oracle
- [x] Re-importing a backlog file over a claimed or running card preserves claim and runtime state.
- [x] Digest-aware import reports content drift without clobbering live lifecycle fields.
- [ ] GitHub issue import maps source URL, title, body, labels, and state without creating personal/operator data in the product repo.
- [x] A dry-run report shows create/update/skip/conflict counts before mutation.
- [ ] A synthetic multi-repo fixture proves import ordering and duplicate handling.

## Children
- Fix the import-clobber bug before importing real fleet work.
- Add digest-aware update semantics.
- Add GitHub issue source adapter with dry-run mode.
- Import Factory backlog into the deployed Powder instance after the sync contract is safe.

## Progress
- 2026-07-01 slice: fixed the import-clobber bug. `Store::import_cards` used to
  blind-`UPSERT` every parsed card, unconditionally overwriting `status` and
  `claim_*` columns with whatever the backlog.d file's front matter said
  (always claim-less, usually `status: ready`) — so re-running import over a
  claimed, running, or already-shipped card silently reverted it. Fixed via
  `Card::protects_lifecycle_on_reimport`/`Card::merge_reimport` in
  powder-core (a domain rule, not an adapter-only patch): a card that is
  claimed, running, awaiting input, or already at a terminal outcome
  (done/shipped/abandoned) keeps its live `status`/`claim` across reimport,
  while everything else (title, body, acceptance, labels, priority,
  blocked_by, repo/workspace/branch, source path+digest) still refreshes
  from the file, and `created_at` is never reset. A quiescent card
  (backlog/ready/blocked, no active claim) is free to have its status
  refreshed by the file, since no one owns it.
  `Store::import_cards` now returns an `ImportOutcome{created, updated,
  preserved, unchanged}` (digest-compared against the stored `source.digest`)
  instead of a bare count, and a new `Store::preview_import` computes the
  same breakdown read-only for dry-run reporting. Wired through HTTP
  (`POST /api/v1/cards/import` gains `dry_run`, returns the outcome struct)
  and CLI (`powder import <dir> --db X [--dry-run]` prints
  `total=/created=/updated=/preserved=/unchanged=`).
  Proof: 6 new powder-store tests (reimport over claimed/terminal/quiescent
  cards, mixed-outcome counts, preview-doesn't-mutate), 4 new
  `Card::merge_reimport`/`protects_lifecycle_on_reimport` unit tests in
  powder-core, and a CLI integration test
  (`cli_reimport_over_a_claimed_card_preserves_the_claim`) driving a real
  temp backlog.d directory through claim → reimport → dry-run. 72 workspace
  tests green (fmt/clippy/test). GitHub issue adapter and the multi-repo
  fixture remain open — see the design note this epic's next slice adds for
  the multi-repo import shape.
