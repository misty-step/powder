# Where production Powder actually runs (powder-937)

Two independent lanes (powder-921, misty-step-906) found the checked-in
`fly.toml` in this repo names a Fly app (`powder`) that is **not** production:
`fly status --app powder` shows zero running machines, and the fleet-wide
board every agent actually reads and writes lives behind
`bastion.tail5f5eb4.ts.net:10001`. Nobody working from this repo alone could
find the real deploy path. This document is that path.

## The real production instance

Powder is supervised as a private app on the `phrazzld-bastion` Fly machine
(the [Bastion](https://github.com/misty-step/bastion) box), canonical as of
the 2026-07-04 cutover. It is reached only over Tailscale, never a public
Fly URL:

- **Origin:** `https://bastion.tail5f5eb4.ts.net:10001` (this is the value
  every agent's `POWDER_API_BASE_URL` should point at)
- **Board UI:** `https://bastion.tail5f5eb4.ts.net:10001/board`
- **Process:** `powder-server`, bound to `127.0.0.1:4175` inside the Bastion
  machine, launched by Bastion's supervisor (`platform/bastion.toml`,
  `[[app]] name = "powder"`)
- **Data:** `/data/apps/powder/powder.db` on Bastion's own Fly volume (WAL
  SQLite) -- **not** the volume this repo's `fly.toml` provisions
- **Runtime env** (set in Bastion's `platform/bastion.toml`, `[app.env]`
  block for the `powder` app):

  ```
  POWDER_DB_PATH=/data/apps/powder/powder.db
  POWDER_BIND_ADDR=127.0.0.1:4175
  POWDER_AUTH_MODE=api-key
  POWDER_PUBLIC_BASE_URL=https://bastion.tail5f5eb4.ts.net:10001
  POWDER_DISCLOSE_BOOTSTRAP_KEY=false
  ```

**Verify before trusting this document over live state** (Bastion's own
`README.md` "powder — the agent work board" section is the canonical, more
detailed source; this is a pointer into this repo for agents who never clone
Bastion):

```sh
fly machine list --app powder      # confirm 0 running -- the checked-in app is inert
curl -s https://bastion.tail5f5eb4.ts.net:10001/healthz
```

## Deploying a code change to production

The Bastion image bakes in a **pinned, git-archived snapshot** of this repo
under `vendor/powder` -- Fly builds never need GitHub credentials, and the
pin is an explicit, reviewable commit in Bastion's own git history. Shipping
a merged powder PR to the live instance is a two-step, cross-repo process,
not a single command:

1. **From `~/Development/bastion`**, bump the vendored snapshot to the new
   `main` commit (this repo's own `main` is never deployed directly):

   ```sh
   rm -rf vendor/powder
   mkdir -p vendor/powder
   git -C ../powder archive --format=tar <new-commit-sha> | tar -x -C vendor/powder
   $EDITOR vendor/powder/SOURCE   # record the new pin
   cargo test --release --locked -p powder-server -p powder-cli   # the exact Docker build step
   ```

   Commit this as a `chore: bump sanctum powder pin for <reason>` -- see
   Bastion's git history (e.g. `463226d`, `8dd6ce0`, `ba735d2`) for the
   established shape and message convention.

2. **Deploy Bastion itself** (`fly deploy --app phrazzld-bastion` from
   `~/Development/bastion`). This restarts every app Bastion supervises, not
   just Powder (Bastion also runs `cairn`, `crucible`, and others as of this
   writing) -- treat it as a Bastion-repo production deploy, with Bastion's
   own review and rollback discipline, not a Powder-repo action.

A merged PR on `misty-step/powder` alone changes nothing in production until
both steps above happen. `powder version` on the CLI installed locally
reports the commit *your local build* came from; it says nothing about what
commit the deployed instance is running.

## The suspended checked-in Fly app: disposition

The `powder` Fly app this repo's `fly.toml`/README describe (`fly apps
create powder`, `fly deploy --app powder`) is **kept, suspended, not
deleted** -- its volume and image are retained for rollback, but it has zero
running machines and is not production. Disposition, made explicit here:

- It remains the reference implementation for anyone self-hosting Powder
  standalone (the product's own README instructions are written against it
  and are correct for that use case).
- It must **never** be assumed live. Every agent and every doc in this repo
  that references "the deployed instance" means the Bastion-fronted one
  above, not this app, unless `POWDER_API_BASE_URL` is explicitly pointed at
  `powder.internal` or the app's own Fly hostname.
- Reviving it to run production traffic again, or deleting it outright, is
  an operator decision (it is real, if currently unused, infrastructure);
  this document does not make that call. The standing default is: leave it
  suspended, do not deploy to it expecting it to be live.

## Field-note generator env target (powder-921 residual)

The field-note draft generator (`Store::with_field_note_config`,
`crates/powder-server/src/main.rs`) is opt-in and reads:

- `POWDER_FIELD_NOTE_REPOS` (comma-separated allowlist; unset/empty = fully
  inert, the default)
- `POWDER_FIELD_NOTE_PROOF_MIN_CHARS` (optional override)
- `POWDER_FIELD_NOTE_WEEKLY_BUDGET` (optional override)

Enabling it in production means adding these to the **same** `[app.env]`
block in `~/Development/bastion/platform/bastion.toml` documented above
(`[[app]] name = "powder"`), then redeploying Bastion per the steps above --
there is no separate config surface for this repo's own `fly.toml` to carry,
since that app is not what serves production traffic.
