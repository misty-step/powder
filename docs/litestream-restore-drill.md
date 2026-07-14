# Litestream restore drill (tombstoned)

This document recorded a Litestream restore drill against the standalone
`powder` Fly app. That app was destroyed 2026-07-07 after its data verified
migrated, and the fleet moved off Fly entirely on 2026-07-09 (see
[`docs/production-deploy.md`](production-deploy.md)) — the `fly ssh console
--app powder ...` commands this document once walked through no longer have
a live target.

- **The backup/restore procedure itself** (what `litestream.yml` replicates,
  how `bin/entrypoint.sh` enforces and auto-restores, how to run a
  non-destructive restore drill against Litestream + any S3-compatible
  bucket) is documented, generically, in
  [`docs/self-hosting.md#backup-and-restore-litestream--s3`](self-hosting.md#backup-and-restore-litestream--s3).
- **Where production actually runs today**, and how its own backups are
  configured, is in [`docs/production-deploy.md`](production-deploy.md).

This file is kept as a pointer rather than deleted so old links (cards,
commit messages, other repos) don't 404. Its previously recorded "live
proof" drill run against the Fly app is historical only — do not cite it as
current evidence for any deployment.
