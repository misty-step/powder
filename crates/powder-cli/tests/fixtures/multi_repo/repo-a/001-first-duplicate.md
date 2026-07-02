# Repo A stale duplicate of ticket 001

Priority: P2 | Status: ready

## Goal
An in-directory duplicate of ticket 001 (both files parse to card id `001`
within repo A). This file sorts *before* `001-first.md` alphabetically, so
it is persisted first and then overwritten within the same import -- the
fixture proves that outcome is deterministic, not a race.

## Oracle
- [ ] this title never wins
