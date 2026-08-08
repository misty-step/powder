# Where production Powder runs

Production Powder runs as a private `powder-server` app on an operator-owned
DigitalOcean droplet. It is reached through the tailnet, never a public URL.
This document records the production path for operators who need to deploy a
merged Powder change or restore its SQLite database.

The live origin comes from `POWDER_API_BASE_URL`; this repository does not carry
the private hostname. The process binds a loopback port inside the supervisor,
the database lives on the droplet's `/data` volume, and Litestream replicates it
to the operator's S3-compatible storage.

Verify the live instance before relying on this document:

```sh
curl -s "$POWDER_API_BASE_URL/healthz"
tailscale ssh root@<box-hostname>
```

## The real production instance

Powder is supervised as a private app on a
[Sanctum](https://github.com/misty-step/sanctum) box -- a separate,
operator-owned **DigitalOcean droplet** that supervises several small apps
privately over Tailscale since the 2026-07-09 migration. It is reached only
over Tailscale, never a public URL:

- **Origin:** the box's own private tailnet hostname on port `10001` -- the
  operator's `POWDER_API_BASE_URL` env var is the live source of truth for
  the exact value; this repo does not carry it (powder-951: no operator
  topology literals in tracked source).
- **Process:** `powder-server`, bound to a loopback port inside the Sanctum
  box, launched by the Sanctum supervisor (systemd `sanctum.service` running
  `sanctum --config /etc/sanctum/sanctum.toml run`; an `[[app]]` block named
  `"powder"` in that config). Binaries live at `/usr/local/bin/` on the box;
  `powder-serve` is the launch wrapper that sets the env below.
- **Data:** a SQLite path under the box's `/data` volume (WAL mode), streamed
  to DigitalOcean Spaces via Litestream
- **Runtime env** (set in Sanctum's own supervisor config, in that `[[app]]`
  block's env section) -- **`none`, by operator ruling 2026-08-03.** The box
  is reachable only over Tailscale (see above): powder binds loopback, the
  only route in is the tailnet-only `tailscale serve` origin, and tailnet
  reachability plus host custody is the entire authorization boundary (the
  same doctrine as Mint). No browser tab, agent, or integration needs a key;
  admin routes are open because `none` mode does not enforce identity.

  ```
  POWDER_DB_PATH=<path under Sanctum's /data volume>
  POWDER_BIND_ADDR=127.0.0.1:<port>
  POWDER_AUTH_MODE=none
  ```

  **Do not "fix" this back to `api-key` or `tailscale-header`.** A 2026-08-03
  session found the instance half-configured in `tailscale-header` mode
  (no `POWDER_TAILNET_PROXY_SECRET`, so identity headers were never trusted
  and every browser read 401'd into a paste-a-key card); the operator ruled
  that anything on the tailnet does everything with zero authentication.
  `none` is only accepted on a loopback bind, which this deployment satisfies.

  For deployments that DO want per-request identity: `tailscale-header` mode
  trusts the identity header a trusted ingress injects only after that
  ingress strips client-supplied values, and requires
  `POWDER_TAILNET_PROXY_SECRET` + the matching `X-Powder-Proxy-Secret`
  header (plain `tailscale serve` cannot inject it). Admin scope comes from
  `POWDER_TAILNET_ADMIN_PRINCIPALS`. Same-box callers without an identity
  header may use a valid bearer key through the explicit fallback. API keys
  still exist as a first-class feature for such deployments; on this
  instance bearer headers are simply ignored.

  Key minting on this instance is optional (nothing requires a key), but the
  lifecycle still works for integrations that want one. For keyed
  deployments generally: `POWDER_BOOTSTRAP_KEY_FILE=/data/powder-bootstrap.key`
  is required on a new database. The server writes the first admin key
  exactly once to this 0600 file while holding the SQLite seed lock and never
  writes raw key bytes to stdout, stderr, or service logs. Read it over the
  operator channel, store it in a secret manager, and remove the one-shot
  file. A stale file from an interrupted first seed is replaced inside the
  locked transaction; a restart against an already seeded database does not
  generate another key. Mint further keys with `powder key-create --db <path>
  --name <consumer> --scope <scope> --show-secret` over SSH and store them
  per the durable key-drop convention in
  [`docs/operations.md`](operations.md#api-key-lifecycle-minting-storage-and-whats-recoverable-powder-918)
  -- hand-out-at-mint-only, into the consumer's own secret store, never
  parked on the box.

**Verify before trusting this document over live state** -- Sanctum's own
`README.md` "powder — the agent work board" section, in the Sanctum repo, is
the canonical, detailed, and current source; this is a pointer for agents who
never clone Sanctum, not a mirror of its content:

```sh
curl -s "$POWDER_API_BASE_URL/healthz"
tailscale ssh root@<box-hostname>   # the droplet is on the tailnet; ssh works from operator machines
```

## Deploying a code change to production

The box runs plain host binaries; there is no container-image step in this
production path. Shipping a merged Powder change to the live instance (verified
2026-07-09):

1. **Cross-compile from a checkout at the merged `master` SHA** (the box
   carries no toolchain, deliberately):

   ```sh
   cargo zigbuild --release --target x86_64-unknown-linux-gnu -p powder-server -p powder-cli
   ```

2. **Snapshot the live database before touching a binary.** The swap in
   step 3 respawns the process against the *same* database file; a bad
   migration or a schema-version regression in the new binary should never
   also cost the last-known-good data. A WAL-safe live snapshot via
   `sqlite3 .backup` (works against a database `powder-server` still has
   open, unlike `cp`, which can copy a torn read mid-write):

   ```sh
   ssh root@<box> 'sqlite3 <path-under-/data> ".backup <path-under-/data>/powder.pre-deploy-$(date +%Y%m%d%H%M%S).db"'
   ```

   Litestream is already replicating continuously in the background
   (sanctum-owned config on the box; see "Backup, restore drill, and
   rollback" below) -- this local `.backup` snapshot is a *second*,
   deploy-scoped safety net you control the exact timing of, not a
   replacement for that replication.

3. **Swap binaries atomically, keep the prior binary, and let the
   supervisor respawn** (do NOT restart `sanctum.service` -- that bounces
   every app on the box):

   ```sh
   scp target/x86_64-unknown-linux-gnu/release/powder-server root@<box>:/usr/local/bin/powder-server.new
   scp target/x86_64-unknown-linux-gnu/release/powder root@<box>:/usr/local/bin/powder.new
   ssh root@<box> 'cp /usr/local/bin/powder-server /usr/local/bin/powder-server.prev \
     && cp /usr/local/bin/powder /usr/local/bin/powder.prev \
     && mv /usr/local/bin/powder-server.new /usr/local/bin/powder-server \
     && mv /usr/local/bin/powder.new /usr/local/bin/powder \
     && chmod +x /usr/local/bin/powder-server /usr/local/bin/powder \
     && pkill -x powder-server'   # supervisor respawns it on the new binary
   curl -s "$POWDER_API_BASE_URL/healthz"   # verify it came back
   curl -s "$POWDER_API_BASE_URL/readyz"    # confirm schema/writable/poison gates are green
   ```

   `powder-server.prev`/`powder.prev` are the binaries this deploy just
   replaced -- kept in place (overwritten by the *next* deploy's own
   `.prev` copy, not retained indefinitely) specifically for the rollback
   command below.

4. **Rollback**, if `/readyz` or `/healthz` comes back unhealthy and the new
   binary itself (not just data) is the suspect: swap the `.prev` binaries
   back in and respawn, the same way step 3 swapped them forward.

   ```sh
   ssh root@<box> 'mv /usr/local/bin/powder-server /usr/local/bin/powder-server.rolled-back \
     && mv /usr/local/bin/powder /usr/local/bin/powder.rolled-back \
     && mv /usr/local/bin/powder-server.prev /usr/local/bin/powder-server \
     && mv /usr/local/bin/powder.prev /usr/local/bin/powder \
     && pkill -x powder-server'
   curl -s "$POWDER_API_BASE_URL/healthz"
   ```

   Rollback restores the *binary*, not the database -- if the new binary
   already wrote schema-incompatible data before you rolled back, restore
   from the step-2 snapshot (or a Litestream generation) instead; see
   "Backup, restore drill, and rollback" below.

5. **Record the deploy**: note the deployed `master` SHA and date on the
   Powder card that drove the change (work log or completion proof). The
   Sanctum repo's `vendor/powder` pin was the durable record until
   sanctum#83 ("reduce Sanctum to host infrastructure") deleted `vendor/`
   entirely — do not try to bump it; there is currently no Sanctum-side
   record of the deployed SHA (verified 2026-07-13). The running instance's
   own startup log line (`powder-server starting`, `journalctl -u
   sanctum`) now carries `version`/`git_sha` for exactly this purpose
   (`powder-deploy-provenance`) -- read it back over SSH as a second,
   independent confirmation of what actually booted, rather than trusting
   the deploy script alone.
6. **One-time after the reciprocal-relations deploy (PR #136): repair
   pre-existing asymmetric relation edges.** New relations writes are
   mirrored onto both cards atomically from this build onward, but edges
   written *before* this deploy stay one-sided until repaired — and with
   `blocked_by` now the sole source of blocking truth, an un-mirrored edge
   silently mis-orders `list_ready` on the side that never heard about it.
   Over SSH, against the live database:

   ```sh
   ssh root@<box> 'powder relations-doctor --db /data/apps/powder/powder.db'          # inspect the report first
   ssh root@<box> 'powder relations-doctor --db /data/apps/powder/powder.db --repair --actor operator'
   ```

   Run the report *before* repairing: normal doctor runs are read-only and
   include deterministic `parent_issues` findings for dangling, self, cycle,
   and invalid persisted parent edges, plus typed `issues` findings when relation
   JSON is malformed or contains noncanonical IDs. Repair uses union semantics for
   relation mirrors (it adds the missing mirror edge, never deletes the one-sided edge),
   so it cannot distinguish a missing mirror-add from a half-applied removal —
   if the report shows an edge you know was meant to be deleted, delete it via
   `update-relations` instead of letting repair resurrect it. Parent repair
   is refused with `parent_repair_refusal`: the doctor never invents a parent
   from corrupted raw state. The relation repair is idempotent and audited per
   touched card; a clean second run reports zero issues. This step is a one-time
   backfill for relation mirrors, not a recurring deploy step — it can be
   dropped from this runbook once the live board reports clean.
7. **Post-deploy checklist item:** verify `/healthz` and `/readyz` after the
   supervisor respawns the process. The checks must report the expected schema,
   writable database, and clean readiness state.

## Backup, restore drill, and rollback

The generic Litestream plus S3 restore procedure is documented once,
provider-agnostically, in
[`docs/self-hosting.md#backup-and-restore-litestream--s3`](self-hosting.md#backup-and-restore-litestream--s3).

This section gives the production-box commands an operator runs over SSH. It
requires the live box and is not part of a local checkout gate.
- **Litestream itself is supervisor-owned**, not this repository's standalone
  configuration. The live supervisor replicates the production SQLite path to
  S3-compatible storage. Read that configuration on the box before running a
  drill so the command uses the active config and database path.

> **Poison counter is cleared only by a restart.** If `/readyz` reports
> `poison_count` > 0 (a request handler panicked and the store mutex was
> recovered), the process keeps serving but stays not-ready until it
> restarts -- the counter is monotonic within a process lifetime by design,
> so a transient panic can't self-clear and hide itself. Investigate the
> panic (check `journalctl -u sanctum` for the `store mutex was poisoned`
> warn line and whatever panicked before it), then clear the counter the
> only way there is: `pkill -x powder-server` and let the supervisor respawn
> it (the same respawn step the deploy uses). This is intended
> human-in-the-loop behavior, not a bug -- do not add an auto-restart that
> would paper over recurring panics.

A merged PR on `misty-step/powder` alone changes nothing in production until
the steps above happen. `powder version` on a locally installed CLI reports
the commit *your local build* came from; it says nothing about what commit
the deployed instance is running.

