# Repo A first ticket

Priority: P0 | Status: ready

## Goal
The canonical version of repo A's ticket 001. `import-repo` sorts files
alphabetically before parsing, and `001-first-duplicate.md` sorts before
this file, so this is the last one persisted for id `001` -- proving
last-write-wins for an in-directory duplicate.

## Oracle
- [ ] repo-a-001 ends up titled "Repo A first ticket", not the duplicate's title
