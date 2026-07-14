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

2. **Swap binaries atomically and let the supervisor respawn** (do NOT
   restart `sanctum.service` -- that bounces every app on the box):

   ```sh
   scp target/x86_64-unknown-linux-gnu/release/powder-server root@<box>:/usr/local/bin/powder-server.new
   scp target/x86_64-unknown-linux-gnu/release/powder root@<box>:/usr/local/bin/powder.new
   ssh root@<box> 'mv /usr/local/bin/powder-server.new /usr/local/bin/powder-server \
     && mv /usr/local/bin/powder.new /usr/local/bin/powder \
     && chmod +x /usr/local/bin/powder-server /usr/local/bin/powder \
     && pkill -x powder-server'   # supervisor respawns it on the new binary
   curl -s "$POWDER_API_BASE_URL/healthz"   # verify it came back
   ```

3. **Record the deploy**: note the deployed `master` SHA and date on the
   Powder card that drove the change (work log or completion proof). The
   Sanctum repo's `vendor/powder` pin was the durable record until
   sanctum#83 ("reduce Sanctum to host infrastructure") deleted `vendor/`
   entirely — do not try to bump it; there is currently no Sanctum-side
   record of the deployed SHA (verified 2026-07-13).

A merged PR on `misty-step/powder` alone changes nothing in production until
the steps above happen. `powder version` on a locally installed CLI reports
the commit *your local build* came from; it says nothing about what commit
the deployed instance is running.

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
