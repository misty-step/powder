# Litestream restore drill

Powder's only durable state is `/data/powder.db` (SQLite, WAL mode) on a Fly
volume. [Litestream](https://litestream.io) continuously replicates it to a
Tigris S3 bucket so a lost volume or a bad deploy doesn't lose the board.
This document is the restore proof and the drill an operator runs to prove
that proof still holds.

## What's replicated

`litestream.yml`:

```yaml
dbs:
  - path: /data/powder.db
    replicas:
      - type: s3
        bucket: ${BUCKET_NAME}
        path: powder.db
        endpoint: https://fly.storage.tigris.dev
        region: auto
        force-path-style: true
        snapshot-interval: 1h
```

`bin/entrypoint.sh` runs `litestream replicate -exec <powder-server>` as the
container's main process whenever `BUCKET_NAME`, `AWS_ACCESS_KEY_ID`, and
`AWS_SECRET_ACCESS_KEY` are all present — replication and the app share one
process tree, so a crashed replicator takes the app down with it rather than
silently stopping backups.

## Required-backup enforcement

`fly.toml` sets `POWDER_REQUIRE_LITESTREAM=1`. With that set,
`bin/entrypoint.sh` refuses to boot at all if any of the three Litestream
secrets is missing:

```
ERROR: Litestream replication required but backup configuration is incomplete
```

Without it, a mis-secreted deploy would run with only a WARNING on stderr
(easy to miss in `fly logs`) and no backups at all — the gap the original
groom teardown flagged. The logic-level contract (both branches: warn-only
when unset, hard-fail when set) is tested in `test/bin/entrypoint_test.sh`,
run via `cargo test` (`entrypoint_restore_and_replication_paths_are_locked`
in `crates/powder-server/tests/deploy_contract.rs`).

## Automatic restore on boot

`bin/entrypoint.sh` restores from the replica whenever the local DB file is
absent (a fresh volume, or after a lost volume) and Litestream is
configured:

```sh
if [ ! -f "$DB_PATH" ] && [ "$LITESTREAM_READY" = "1" ]; then
  litestream restore -if-replica-exists -o "$DB_PATH" -config /etc/litestream.yml "$DB_PATH"
fi
```

This is the path a real volume-loss recovery takes: replace the volume,
redeploy, the entrypoint notices no local file and restores before
`powder-server` ever starts.

## Restore drill (live proof, non-destructive)

Run this periodically (and after any Litestream/Tigris configuration
change) to prove the replica is actually restorable, without touching the
live database:

```sh
fly ssh console --app powder --command \
  "litestream restore -if-replica-exists -o /tmp/restore-drill.db -config /etc/litestream.yml /data/powder.db"
```

Then verify the restored file is a real, openable Powder database (not just
a nonzero-size blob) by reading a known card through it with the CLI binary
already on the machine:

```sh
fly ssh console --app powder --command \
  "/app/bin/powder get-card <some-card-id> --db /tmp/restore-drill.db"
```

Clean up the scratch file afterward:

```sh
fly ssh console --app powder --command "rm -f /tmp/restore-drill.db"
```

### Drill run, 2026-07-02

Executed live against the deployed `powder` app:

```
$ fly ssh console --app powder --command \
    "litestream restore -if-replica-exists -o /tmp/restore-drill.db -config /etc/litestream.yml /data/powder.db; echo EXIT:$?; ls -la /tmp/restore-drill.db"
EXIT:0
-rw-r--r-- 1 root root 94208 Jul  2 03:34 /tmp/restore-drill.db

$ fly ssh console --app powder --command \
    "/app/bin/powder get-card mcp-live-proof --db /tmp/restore-drill.db"
{
  "card": {
    "id": "mcp-live-proof",
    "title": "MCP live proof",
    ...
    "status": "done",
    ...
  },
  ...
}
```

The restored replica contained the real card data (including its
proof/activity history), confirming the S3 replica is a genuine, current,
restorable copy of the live database — not just a file that exists.
Scratch file removed after the drill.

## When to re-run this drill

- After any change to `litestream.yml`, Tigris bucket/credentials, or
  `POWDER_REQUIRE_LITESTREAM`.
- After a Litestream version bump in the `Dockerfile`.
- Periodically as a standing operational check (monthly is reasonable for
  an instance this size).
