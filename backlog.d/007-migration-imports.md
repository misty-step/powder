# Migration imports

Priority: P2 | Status: backlog | Type: Epic

## Goal
Import backlog files and GitHub issues from all Factory repos without erasing
live state. Migration is a sync into an instance database, not a destructive
rewrite of claims, statuses, runs, or proof.

## Oracle
- [ ] Re-importing a backlog file over a claimed or running card preserves claim and runtime state.
- [ ] Digest-aware import reports content drift without clobbering live lifecycle fields.
- [ ] GitHub issue import maps source URL, title, body, labels, and state without creating personal/operator data in the product repo.
- [ ] A dry-run report shows create/update/skip/conflict counts before mutation.
- [ ] A synthetic multi-repo fixture proves import ordering and duplicate handling.

## Children
- Fix the import-clobber bug before importing real fleet work.
- Add digest-aware update semantics.
- Add GitHub issue source adapter with dry-run mode.
- Import Factory backlog into the deployed Powder instance after the sync contract is safe.
