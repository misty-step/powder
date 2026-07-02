# API key lifecycle: list and revoke

Priority: P1 | Status: done | Type: Epic

## Goal
Close the gap identity work left open: API keys can be created and verified,
but there is no way to see what keys exist or revoke one. Every key minted
for testing, a departed agent, or a leaked secret lives forever. Give the
store a real key lifecycle: list metadata (never secrets), and revoke.

## Oracle
- [x] `Store::list_api_keys` returns id, name, scope, actor, created_at, and
      revoked_at for every key, never the hash or raw secret.
- [x] `Store::revoke_api_key` sets `revoked_at` once; a revoked key immediately
      fails `verify_api_key` and re-revoking is idempotent (no error, no
      double-write).
- [x] CLI `key-list` and `key-revoke <id>` exist and are covered by tests.
- [x] HTTP admin-only routes exist for both operations; an agent-scoped key
      gets 403 on both.
- [x] The bootstrap key can be revoked like any other (no special-cased
      immortal key), proven by a test.

## Children
- Add `revoked_at` write path and a metadata-only read model in
  `powder-store::identity`.
- Wire CLI `key-list`/`key-revoke`.
- Wire HTTP `GET /api/v1/keys` and `POST /api/v1/keys/{id}/revoke`, admin-gated.

## Progress
- 2026-07-02 slice (overnight autonomous): `powder_store::identity` gains
  `ApiKeySummary` (id, actor, name, scope, created_at, revoked_at -- never
  the hash or raw secret) plus `Store::list_api_keys`/`Store::revoke_api_key`.
  Revoke is a single `UPDATE ... WHERE revoked_at IS NULL`, so re-revoking a
  key is a no-op that never moves the original timestamp, and revoking an
  unknown id returns `DomainError::NotFound`. CLI gains `key-list` and
  `key-revoke <id>`; HTTP gains admin-gated `GET /api/v1/keys` and
  `POST /api/v1/keys/{id}/revoke` (agent-scoped keys get 403, matching the
  existing `require_admin` pattern from backlog.d/004). Proved the bootstrap
  key is not special-cased: it revokes and immediately fails
  `verify_api_key` like any other key. Two verification keys minted during
  the identity/MCP-remote proof session are the first real use of this path
  (revoked live once this ships and deploys).
  Proof: 5 new powder-store tests (list metadata never leaks secrets,
  revoke fails auth immediately, idempotent re-revoke keeps the original
  timestamp, unknown id errors, bootstrap key revokes like any other), 1 new
  CLI integration test, 3 new HTTP tests (agent 403 on both routes, admin
  list+revoke+immediate-auth-loss, unknown id -> 404). 89 workspace tests
  green (fmt/clippy/test).
