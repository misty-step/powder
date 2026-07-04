# EPIC: Powder is mission-critical — never down, never lose data, always usable

Priority: P0 | Status: running

## Goal
Operator directive 2026-07-03: powder manages ALL project backlogs. Harden it: no downtime, no data loss, always usable. The sanctum migration (bastion PR #34) is the foundation; this epic tracks the drills and remaining gaps.

## Oracle
- [ ] Continuous off-box replication RUNNING — Litestream PID confirmed, S3 (DO Spaces bastion-backups/powder), 2026-07-04 hardening pass
- [ ] Restore rehearsal PERFORMED — phoenix-drill exit 0, ~1s, integrity_check ok, 405 cards matched live
- [ ] Crash recovery EXERCISED — kill -9 → supervisor auto-restart, healthz 200 within ~1s, count unchanged
- [ ] Split-brain closed — old fly app at 0 machines (volume + 2 snapshots retained for rollback), fly-proxy launchd disabled, proxy script tombstoned, consumers audited (bridge/poll/relay/portal/fleet-doc all on canonical :10001)
- [ ] Canary watch DURABLE — TTL monitor exists (MON-4uhhh40u445u) + ingest key in 1P (CANARY_INGEST_KEY__powder), but no durable recurring pusher: canary-obs can't reach tailnet-only origins, and bastion supervisor supports only one box-wide heartbeat. Fix = per-app heartbeat in bastion supervisor (preferred) or canary-obs tailnet join.
- [x] Heartbeat ingest key rotation DONE 2026-07-04: new key KEY-re2pn6e2ez6p live (bastion-router check-ins verified), old KEY-0x8xdy8gphyq revoked, stored in 1P Agents vault.
- [ ] Backup-verification cadence: schedule periodic phoenix-drill with alert on failure (a restore that only ran once decays)
