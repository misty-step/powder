# Private ingress conformance

Priority: P1 | Status: backlog | Type: Epic

## Goal
Make the deployment match Powder's private-instance promise. Product code stays
public-able, but real backlog data lives in a deployment that should be reached
through private ingress by default; MCP and HTTP should operate against the
deployed instance rather than local-only database files.

## Oracle
- [ ] The deployment has a conformance check that proves whether public IPs are absent or intentionally declared.
- [ ] A private ingress smoke test reaches health/readiness and an authenticated agent route from the operator network.
- [ ] Unauthenticated onboarding/health exposure is reviewed and either minimized or explicitly documented as safe.
- [ ] MCP works against the deployed instance through an HTTP or hosted MCP transport; it no longer silently falls back to evaporating in-memory state for real work.
- [ ] Litestream restore and required-backup behavior are documented and tested for the deployment profile.

## Children
- Add private-ingress checks around the deploy profile.
- Decide and implement MCP remote access shape.
- Remove or quarantine in-memory MCP fallback for demos/tests only.
- Add restore drill documentation and a proof command.
