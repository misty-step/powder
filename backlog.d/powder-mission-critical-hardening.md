# EPIC: Powder is mission-critical — never down, never lose data, always usable

Priority: P0 | Status: running

## Goal
Operator directive 2026-07-03: powder manages ALL project backlogs. Harden it: no downtime, no data loss, always usable. The sanctum migration (bastion PR #34) is the foundation; this epic tracks the drills and remaining gaps.

## Oracle
- [x] Continuous off-box replication RUNNING — Litestream PID confirmed, S3 (DO Spaces bastion-backups/powder), 2026-07-04 hardening pass
- [x] Restore rehearsal PERFORMED — phoenix-drill exit 0, ~1s, integrity_check ok, 405 cards matched live
- [x] Crash recovery EXERCISED — kill -9 → supervisor auto-restart, healthz 200 within ~1s, count unchanged
- [x] Split-brain closed — old fly app at 0 machines (volume + 2 snapshots retained for rollback), fly-proxy launchd disabled, proxy script tombstoned, consumers audited (bridge/poll/relay/portal/fleet-doc all on canonical :10001)
- [x] Canary watch DURABLE — DONE 2026-07-04: bastion supervisor per-app heartbeat (bastion PR #39) beats monitor powder (MON-4uhhh40u445u) from the box every 60s, gated on local /readyz (fail-toward-alarm); deployed + "canary app heartbeat accepted for powder" in logs; interim Mac launchd pusher retired.
- [x] Heartbeat ingest key rotation DONE 2026-07-04: new key KEY-re2pn6e2ez6p live (bastion-router check-ins verified), old KEY-0x8xdy8gphyq revoked, stored in 1P Agents vault.
- [x] Backup-verification cadence — DONE 2026-07-04: supervisor spawns phoenix_drill_loop at startup (weekly default, BASTION_PHOENIX_DRILL_INTERVAL_SECS tunable), scratch-space restores only, drill failure emits canary status=down for powder (test-covered in bastion PR #39).
