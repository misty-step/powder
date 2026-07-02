# Repo B first ticket

Priority: P0 | Status: ready

## Goal
Repo B numbers its own tickets independently of repo A -- this file also
parses to bare card id `001`, the same as repo A's ticket. Without
per-repo id namespacing the two would collide; the fixture proves they
don't.

## Oracle
- [ ] repo-b-001 and repo-a-001 both exist as distinct cards
