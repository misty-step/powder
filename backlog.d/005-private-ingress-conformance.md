# Private ingress conformance

Priority: P1 | Status: backlog | Type: Epic

## Goal
Make the deployment match Powder's private-instance promise. Product code stays
public-able, but real backlog data lives in a deployment that should be reached
through private ingress by default; MCP and HTTP should operate against the
deployed instance rather than local-only database files.

## Oracle
- [x] The deployment has a conformance check that proves whether public IPs are absent or intentionally declared.
- [x] A private ingress smoke test reaches health/readiness and an authenticated agent route from the operator network.
- [ ] Unauthenticated onboarding/health exposure is reviewed and either minimized or explicitly documented as safe.
- [x] MCP works against the deployed instance through an HTTP or hosted MCP transport; it no longer silently falls back to evaporating in-memory state for real work.
- [ ] Litestream restore and required-backup behavior are documented and tested for the deployment profile.

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
  instance (agent + admin scope, clearly named `mcp-verification-*`);
  unauthenticated-onboarding review and the Litestream restore-drill writeup
  remain open for a follow-up slice.
