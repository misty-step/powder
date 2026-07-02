# Private ingress conformance

Priority: P1 | Status: done | Type: Epic

## Goal
Make the deployment match Powder's private-instance promise. Product code stays
public-able, but real backlog data lives in a deployment that should be reached
through private ingress by default; MCP and HTTP should operate against the
deployed instance rather than local-only database files.

## Oracle
- [x] The deployment has a conformance check that proves whether public IPs are absent or intentionally declared.
- [x] A private ingress smoke test reaches health/readiness and an authenticated agent route from the operator network.
- [x] Unauthenticated onboarding/health exposure is reviewed and either minimized or explicitly documented as safe.
- [x] MCP works against the deployed instance through an HTTP or hosted MCP transport; it no longer silently falls back to evaporating in-memory state for real work.
- [x] Litestream restore and required-backup behavior are documented and tested for the deployment profile.

## Children
- Add private-ingress checks around the deploy profile.
- Decide and implement MCP remote access shape.
- Remove or quarantine in-memory MCP fallback for demos/tests only.
- Add restore drill documentation and a proof command.

## Progress
- 2026-07-01 slice: `fly.toml` rewritten flycast-native — `[[services]]` (plain
  `http` port handler, no `tls`) instead of `[http_service]`, `force_https`
  dropped, so a bare future `fly deploy` has no config-level trigger asking
  flyctl to re-provision a public IP. Public IPs were already released; added
  a private Flycast IPv6 address (`fly ips allocate-v6 --private`) so the app
  always "has an address" other than a public one. `bin/check-private-ingress.sh`
  (fly ips list conformance check) plus a `deploy_contract` test lock the shape
  so it can't silently regress.
  Live-debugged and fixed a real reachability bug in the process: Fly's guest
  kernel does not dual-stack a `0.0.0.0` bind, and this app's private-network
  path (Flycast/`.internal`) is IPv6-only — `POWDER_BIND_ADDR` must stay
  `[::]:4000` (dual-stack here, confirmed by both a literal IPv4-loopback
  curl and a `fly proxy` tunnel succeeding), not `0.0.0.0` (`fly proxy`
  resets every request against a `0.0.0.0` deploy). `fly deploy`'s "not
  listening on 0.0.0.0" warning for the `[::]` bind is a confirmed cosmetic
  false positive — its scanner only checks the IPv4 socket table. Live proof:
  `fly proxy 14030:4000 --app powder` reached `/healthz` (200) and `/readyz`
  (200) with zero public IPs allocated.
  Shipped MCP-over-HTTP: `powder-mcp` gained a `POWDER_API_BASE_URL` +
  `POWDER_API_KEY` remote mode (new `remote.rs`, `ureq`-based, no async
  runtime needed) that translates JSON-RPC tool calls into the same REST
  calls HTTP/CLI use, so claim-holder/admin authority from backlog.d/004 is
  enforced identically for MCP callers hitting the deployed instance — no
  `actor`/`admin` tool arguments needed, identity comes from the bearer key.
  Live-proved end to end over the `fly proxy` tunnel: minted a real
  agent-scoped API key on the deployed instance, ran the release
  `powder-mcp` binary in remote mode, and drove a full
  `initialize` → `tools/list` → `list_ready` → `claim_card` → `update_status`
  → `complete_card` → `get_card` JSON-RPC session against
  `powder.internal:4000`, ending with the card `done` and its proof/activity
  trail visible via `get_card`. Gates green (61 workspace tests, including 3
  new `remote::tests::*` covering auth header, GET query params, and
  Forbidden-error passthrough). Residual: no API-key revoke path exists yet,
  so the two verification keys minted for this proof persist in the deployed
  instance (agent + admin scope, clearly named `mcp-verification-*` --
  now revocable via backlog.d/009's `key-revoke`); unauthenticated-onboarding
  review and the Litestream restore-drill writeup remain open for a
  follow-up slice.
- 2026-07-02 slice (overnight autonomous): reviewed the three
  unauthenticated routes (`GET /healthz`, `GET /readyz`,
  `GET /api/v1/onboarding`). All three are *intentionally* unauthenticated
  by design, not an oversight: Fly's own health checker probes `/healthz`
  and `/readyz` without a bearer token, and `/api/v1/onboarding` must be
  reachable before any API key exists so a fresh deploy can tell an
  operator it needs first-run setup. That contract stands. What was
  reviewed and fixed: both `Ready` and `Onboarding` response bodies
  included `db_path`, the server's local database file path -- a pure
  implementation detail with no operational value to a caller (a client
  doesn't need to know the deployed instance's filesystem layout to decide
  whether it's healthy or needs onboarding) and no reason to be legible to
  an unauthenticated caller. Removed from both response shapes;
  `schema_version` alone already proves the database is open and migrated,
  which is all `/readyz` needs to demonstrate. `auth_mode` and
  `public_base_url` stay: an operator deciding how to authenticate against
  a fresh instance needs to know the configured auth mode, and neither
  value is sensitive (the auth mode is a public deployment fact, not a
  secret; `public_base_url` is meant to be public by name). This closes
  the review now that the deployment is flycast-only (backlog.d/005's
  first slice) -- these routes are reachable only over the private network
  in the first place, so the residual exposure the original groom report
  flagged (public internet) no longer applies; the `db_path` fix is
  defense-in-depth on top of that, not a response to an active public leak.
  Proof: new `healthz_readyz_and_onboarding_are_unauthenticated_and_never_leak_the_db_path`
  test asserts all three routes stay reachable with no bearer token
  (proving the by-design contract didn't regress) and that none of their
  response bodies contain `db_path` or the actual configured path. 92
  workspace tests green (fmt/clippy/test).
- 2026-07-02 slice (overnight autonomous): closed the Litestream oracle
  item. Added `POWDER_REQUIRE_LITESTREAM = "1"` to `fly.toml` (the required
  secrets -- `BUCKET_NAME`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` --
  are already deployed): a mis-secreted future deploy now refuses to boot
  instead of silently running unreplicated with only a warning on stderr,
  the gap the original groom teardown flagged. `docs/litestream-restore-drill.md`
  documents what's replicated, the required-backup enforcement, the
  automatic on-boot restore path, and a non-destructive drill procedure
  (`litestream restore` to a scratch path on the live machine, verified by
  reading a real card through the restored file with the CLI binary already
  on the machine, then removed). Ran the drill live against the deployed
  instance as part of writing the doc: the restored replica contained
  `mcp-live-proof` (the card created during backlog.d/005's earlier MCP
  verification session) with its real status and proof intact, confirming
  the S3 replica is a genuine, current, restorable copy -- not just a file
  that exists. Locked the config with a new `deploy_contract` assertion
  that `fly.toml` sets the flag. This closes the last open backlog.d/005
  oracle item; the epic's oracle is now fully checked.
  Proof: 92 workspace tests green (fmt/clippy/test), plus the live restore
  drill transcript in the new doc.
