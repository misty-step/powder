# Where production Powder actually runs (powder-937)

Two independent lanes (powder-921, misty-step-906) found the checked-in
`fly.toml` in this repo names a Fly app (`powder`) that is **not** production:
the fleet-wide board every agent actually reads and writes lives behind a
companion box. Nobody working from this repo alone could find the real deploy
path. This document is that path.

> **Hosting ruling (operator, 2026-07-09): the fleet is off Fly and on
> DigitalOcean.** The supervising box is a DigitalOcean droplet. Fly remains
> only for explicitly retained exceptions (Fly Sprites); nothing in this
> repo's deploy path touches Fly anymore. Any Fly-shaped instruction you find
> in older docs, cards, or the checked-in `fly.toml` is historical reference
> for standalone self-hosters, not the operator's production path.

## The real production instance

Powder is supervised as a private app on a
[Sanctum](https://github.com/misty-step/sanctum) box -- a separate,
operator-owned **DigitalOcean droplet** that supervises several small apps
privately over Tailscale (Fly-hosted 2026-07-04 to 2026-07-09, DigitalOcean
canonical since the 2026-07-09 migration). It is reached only over Tailscale,
never a public URL:

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
  block's env section):

  ```
  POWDER_DB_PATH=<path under Sanctum's /data volume>
  POWDER_BIND_ADDR=127.0.0.1:<port>
  POWDER_AUTH_MODE=api-key
  POWDER_PUBLIC_BASE_URL=<the box's tailnet origin, see above>
  POWDER_DISCLOSE_BOOTSTRAP_KEY=false
  ```

  `POWDER_DISCLOSE_BOOTSTRAP_KEY=false` means the very first admin key
  `powder-server` seeds on an empty database is created **redacted** --
  nothing but `"Powder bootstrap API key created and redacted."` reaches
  stderr, so the raw key never lands in `journald` for the box's lifetime.
  This is a deliberate production-only posture; the code's own default
  (`true`, unset) stays unchanged so a self-hoster running the binary with
  zero config still sees their first key.

  The seed only ever runs once (it's guarded by a `seed_runs` row) --
  flipping the env var back to `true` and redeploying **after** the first
  boot does nothing; the seed has already applied and there is no raw value
  left to print. Get a usable admin key on a freshly bootstrapped production
  box one of two ways, decided *before* or *at* that first boot:

  - **`init-db --show-secret` on the box (preferred: never touches logs).**
    SSH to the box and run `powder init-db --db <path> --show-secret`
    yourself, once, before `powder-server` ever starts against that
    database. This applies the one-time seed and prints the raw key
    directly to your SSH session. Then start (or redeploy) `powder-server`
    normally with `POWDER_DISCLOSE_BOOTSTRAP_KEY=false` already set --
    its own call to the same seed finds it already applied and no-ops.
  - **Disclose once, then rotate.** If `powder-server` already auto-seeded
    the database (the common case), the raw bootstrap value is gone for
    good -- there is no "re-disclose" path. Mint a fresh admin key instead
    via the operator-key flow already documented in
    [`docs/operations.md`](operations.md#self-hosting) (`powder key-create
    --db <path> --name operator --scope admin --show-secret` over SSH),
    confirm it authenticates, then `powder key-revoke <bootstrap-key-id>`
    (its id is visible via `key-list`, which never needs the secret) to
    retire the now-permanently-unrecoverable original.

  Either way, store the captured key per the durable key-drop convention in
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

The box runs plain host binaries -- there is no image build and no Fly step.
Shipping a merged powder PR to the live instance (verified 2026-07-09):

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
   curl -s "$POWDER_API_BASE_URL/readyz"    # confirm schema/writable/dead-letter/poison gates are all green
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
   (powder-epic-truthful-ops) -- read it back over SSH as a second,
   independent confirmation of what actually booted, rather than trusting
   the deploy script alone.
6. **Post-deploy checklist item (lead, not this task):** re-verify the
   Canary heartbeat against the live box after the swap. This is a manual
   step for whoever drove the deploy to do against the real instance --
   nothing in this repo can exercise it.

A merged PR on `misty-step/powder` alone changes nothing in production until
the steps above happen. `powder version` on a locally installed CLI reports
the commit *your local build* came from; it says nothing about what commit
the deployed instance is running.

## Backup, restore drill, and rollback (powder-epic-truthful-ops)

The generic Litestream + S3 restore procedure -- what gets replicated, how
`bin/entrypoint.sh` auto-restores on boot, how to run a non-destructive
drill -- is documented once, provider-agnostically, in
[`docs/self-hosting.md#backup-and-restore-litestream--s3`](self-hosting.md#backup-and-restore-litestream--s3).
[`docs/litestream-restore-drill.md`](litestream-restore-drill.md) is a
tombstone: it recorded a real drill run against the now-destroyed Fly app
and is not current evidence for anything running today.

**This section is the DO-box-specific version of that drill** -- the
commands an operator actually runs over `ssh` against the Sanctum box, not
against a local checkout. It requires the box; nothing here can be exercised
from this repo alone, and it is not part of this PR's own gate.

- **Litestream itself is Sanctum-owned**, not this repo's `litestream.yml`
  (that file, like `fly.toml`, is the standalone self-hoster's reference
  config -- see "The checked-in Fly config: disposition" below). The box
  runs its own Litestream config, replicating the production SQLite path to
  DigitalOcean Spaces continuously. Read that config on the box (its exact
  path is Sanctum's own concern, not tracked in this repo per powder-951)
  before running the drill below, so `-config` points at what's actually
  running.

- **Non-destructive restore drill**, run over `ssh root@<box>` (or via
  `tailscale ssh root@<box>` from an operator machine, per "Verify before
  trusting this document" above):

  ```sh
  # 1. Restore the latest replicated generation to a scratch path -- never
  #    the live DB path -- so the drill cannot touch what's currently
  #    serving traffic.
  litestream restore -if-replica-exists \
    -o /tmp/powder-restore-drill.db \
    -config <the box's own litestream config path> \
    <the box's live POWDER_DB_PATH>

  # 2. Prove the restored file is a real, current Powder database with a
  #    readback, not just "the file exists" -- pick any card id known to
  #    exist on the live board.
  powder get-card <a-known-card-id> --db /tmp/powder-restore-drill.db

  # 3. Clean up -- this was a drill, not a real restore.
  rm -f /tmp/powder-restore-drill.db
  ```

  A successful step 2 (the card's real title/status/acceptance come back,
  not an error) is the drill's pass condition. If it fails, Litestream's
  replication itself is broken and needs attention before the next real
  incident needs it.

- **Restoring for real** (not a drill) replaces the live `POWDER_DB_PATH`
  file with a restored generation and requires stopping `powder-server`
  first (`pkill -x powder-server`; the supervisor will respawn it against
  whatever file is at that path when it comes back) -- run the same
  `litestream restore` command from step 1 above but with `-o` pointed at
  the real `POWDER_DB_PATH` instead of a scratch path, after moving the
  current (corrupt/lost) file aside rather than deleting it outright.

## The checked-in Fly config: disposition

The `powder` Fly app that `fly.toml`/README once described was **destroyed
2026-07-07** after its data migrated to the Sanctum-hosted instance, and the
fleet left Fly entirely on 2026-07-09. `fly.toml` is kept only as a reference
implementation for anyone self-hosting Powder standalone on Fly under their
own org -- the operator's production never touches it.

- It must **never** be assumed live. Every agent and every doc in this repo
  that references "the deployed instance" means the Sanctum-hosted
  DigitalOcean box above, unless `POWDER_API_BASE_URL` is explicitly pointed
  elsewhere.
- **Stale-client warning** (observed 2026-07-09): long-lived MCP
  subprocesses resolve `POWDER_API_BASE_URL` once at startup. When the box's
  tailnet hostname changes (as it did in the Fly→DO cutover), running
  sessions keep calling the dead origin and fail with opaque 404s until
  restarted. If the MCP face 404s while direct `curl` against the current
  env var succeeds, restart the MCP client. powder-944 tracks the durable
  fix.

## Field-note generator env target (powder-921 residual)

The field-note draft generator (`Store::with_field_note_config`,
`crates/powder-server/src/main.rs`) is opt-in and reads:

- `POWDER_FIELD_NOTE_REPOS` (comma-separated allowlist; unset/empty = fully
  inert, the default)
- `POWDER_FIELD_NOTE_PROOF_MIN_CHARS` (optional override)
- `POWDER_FIELD_NOTE_WEEKLY_BUDGET` (optional override)

Enabling it in production means adding these to the **same** `[[app]]` env
block in Sanctum's own supervisor config documented above, then redeploying
Sanctum per the steps above -- there is no separate config surface for this
repo's own `fly.toml` to carry, since that app is not what serves production
traffic.

## Home affordance (powder-942)

`POWDER_HOME_URL` (unset by default) makes the board render a plain text
link back to that URL in its always-visible chrome (footer on the board
view, header on the card-detail view) -- built for exactly this deployment
shape, where Powder is its own tailnet origin and a separate portal root
lives at a different one. Setting it in production is the same env-target
pattern as the field-note generator above: add `POWDER_HOME_URL=<the box's
portal root>` to the same `[[app]]` env block, then redeploy Sanctum.
