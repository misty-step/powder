# Where production Powder actually runs (powder-937)

Two independent lanes (powder-921, misty-step-906) found the checked-in
`fly.toml` in this repo names a Fly app (`powder`) that is **not** production:
`fly status --app powder` shows zero running machines, and the fleet-wide
board every agent actually reads and writes lives behind a companion box.
Nobody working from this repo alone could find the real deploy path. This
document is that path.

## The real production instance

Powder is supervised as a private app on a
[Bastion](https://github.com/misty-step/bastion) box -- a separate,
operator-owned Fly machine that supervises several small apps privately over
Tailscale, canonical as of the 2026-07-04 cutover. It is reached only over
Tailscale, never a public Fly URL:

- **Origin:** the box's own private tailnet hostname on port `10001` -- the
  operator's `POWDER_API_BASE_URL` env var is the live source of truth for
  the exact value; this repo does not carry it (powder-951: no operator
  topology literals in tracked source).
- **Process:** `powder-server`, bound to a loopback port inside the Bastion
  machine, launched by Bastion's own supervisor config (an `[[app]]` block
  named `"powder"` in Bastion's own repo)
- **Data:** a SQLite path under Bastion's own `/data` volume (WAL mode) --
  **not** the volume this repo's `fly.toml` provisions
- **Runtime env** (set in Bastion's own supervisor config, in that `[[app]]`
  block's env section):

  ```
  POWDER_DB_PATH=<path under Bastion's /data volume>
  POWDER_BIND_ADDR=127.0.0.1:<port>
  POWDER_AUTH_MODE=api-key
  POWDER_PUBLIC_BASE_URL=<the box's tailnet origin, see above>
  POWDER_DISCLOSE_BOOTSTRAP_KEY=false
  ```

**Verify before trusting this document over live state** -- Bastion's own
`README.md` "powder — the agent work board" section, in the Bastion repo, is
the canonical, detailed, and current source; this is a pointer for agents who
never clone Bastion, not a mirror of its content:

```sh
fly machine list --app powder      # confirm 0 running -- the checked-in app is inert
curl -s "$POWDER_API_BASE_URL/healthz"
```

## Deploying a code change to production

The Bastion image bakes in a **pinned, git-archived snapshot** of this repo
under `vendor/powder` -- Fly builds never need GitHub credentials, and the
pin is an explicit, reviewable commit in Bastion's own git history. Shipping
a merged powder PR to the live instance is a two-step, cross-repo process,
not a single command:

1. **From the Bastion checkout**, bump the vendored snapshot to the new
   `main` commit (this repo's own `main` is never deployed directly):

   ```sh
   rm -rf vendor/powder
   mkdir -p vendor/powder
   git -C ../powder archive --format=tar <new-commit-sha> | tar -x -C vendor/powder
   $EDITOR vendor/powder/SOURCE   # record the new pin
   cargo test --release --locked -p powder-server -p powder-cli   # the exact Docker build step
   ```

   Commit this as a `chore: bump sanctum powder pin for <reason>` -- see
   Bastion's own git history for the established shape and message
   convention (several such commits already exist there).

2. **Deploy Bastion itself.** This restarts every app Bastion supervises,
   not just Powder -- treat it as a Bastion-repo production deploy, with
   Bastion's own review and rollback discipline, not a Powder-repo action.

A merged PR on `misty-step/powder` alone changes nothing in production until
both steps above happen. `powder version` on the CLI installed locally
reports the commit *your local build* came from; it says nothing about what
commit the deployed instance is running.

## The checked-in Fly app: disposition

The `powder` Fly app this repo's `fly.toml`/README describe (`fly apps
create powder`, `fly deploy --app powder`) was **destroyed 2026-07-07**,
after its data was verified fully migrated to the Bastion-hosted instance.
`fly.toml`'s header comment records this and explains why the file is kept
only as a config reference -- `fly deploy` against it would recreate a decoy,
not restore production.

- It remains useful only as the reference implementation for anyone
  self-hosting Powder standalone under their own Fly org (the product's own
  README instructions are written against that use case and are correct for
  it).
- It must **never** be assumed live. Every agent and every doc in this repo
  that references "the deployed instance" means the Bastion-fronted one
  above, not a `powder` Fly app, unless `POWDER_API_BASE_URL` is explicitly
  pointed at one.

## Field-note generator env target (powder-921 residual)

The field-note draft generator (`Store::with_field_note_config`,
`crates/powder-server/src/main.rs`) is opt-in and reads:

- `POWDER_FIELD_NOTE_REPOS` (comma-separated allowlist; unset/empty = fully
  inert, the default)
- `POWDER_FIELD_NOTE_PROOF_MIN_CHARS` (optional override)
- `POWDER_FIELD_NOTE_WEEKLY_BUDGET` (optional override)

Enabling it in production means adding these to the **same** `[[app]]` env
block in Bastion's own supervisor config documented above, then redeploying
Bastion per the steps above -- there is no separate config surface for this
repo's own `fly.toml` to carry, since that app is not what serves production
traffic.

## Home affordance (powder-942)

`POWDER_HOME_URL` (unset by default) makes the board render a plain text
link back to that URL in its always-visible chrome (footer on the board
view, header on the card-detail view) -- built for exactly this deployment
shape, where Powder is its own tailnet origin and a separate portal root
lives at a different one. Setting it in production is the same env-target
pattern as the field-note generator above: add `POWDER_HOME_URL=<the box's
portal root>` to the same `[[app]]` env block, then redeploy Bastion.
