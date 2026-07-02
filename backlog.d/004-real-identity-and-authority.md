# Real identity and authority

Priority: P1 | Status: done | Type: Epic

## Goal
Replace caller-supplied identity strings with real actors. API keys, tailnet
headers, and future user sessions must resolve to agent or user entities, and
mutations must be authorized against claim ownership and explicit scope instead
of trusting an `agent` field in the request body.

## Oracle
- [x] API keys are bound to durable identities and scopes, and each mutation records the resolved actor.
- [x] A non-holder cannot mutate or complete another agent's active claim.
- [x] Admin-only operations are enforced by scope, with tests proving agent keys are rejected.
- [x] Cross-agent impersonation attempts return `403` across HTTP and equivalent CLI/MCP errors.
- [x] Existing bootstrap/key-create flows migrate without exposing secrets or breaking first-run setup.

## Children
- Add agent/user identity records or an equivalent durable actor model.
- Thread actor context through store transactions and activity writes.
- Enforce claim-holder ownership for status, progress, input, and completion.
- Add key-management/admin routes only if they preserve the one-deployable shape.

## Progress
- 2026-07-01 slice: API keys create and verify durable `Actor` records, v1 keys migrate to actor-bound keys without regenerating secrets, and HTTP claim rejects a request-body `agent` that does not match the authenticated API-key actor. Remaining oracle surface: ownership checks for non-claim mutations, admin-only operations, and equivalent CLI/MCP authority errors.
- 2026-07-01 slice 2: added `powder_core::Authority` (`Unchecked` / `Actor{display_name,is_admin}`) and `DomainError::Forbidden`, the single domain-level rule `Store` mutation methods check claim ownership against. `Store::claim_card/release_claim/renew_claim/heartbeat_claim/update_status/request_input/complete_card/answer_input` all take `&Authority` and enforce it directly (not just in the HTTP adapter), so CLI (`--actor`/`--admin` flags), MCP (`actor`/`admin` tool arguments), and HTTP (bearer key scope, or trusted tailnet identity) all produce the identical `DomainError::Forbidden` message when a non-holder or non-admin tries to act on someone else's claim. HTTP admin-only gate (`require_admin`) now protects `POST /api/v1/cards` and `/api/v1/cards/import` — agent-scoped keys get 403, admin-scoped and trusted-tailnet callers pass. `answer_input`'s caller-supplied `actor` field is now checked against the authenticated identity (unless admin), closing the same impersonation gap `claim` closed in slice 1. Proof: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green (57 tests, including new `non_holder_actor_is_rejected_from_claim_mutations` / `admin_authority_bypasses_claim_ownership` in powder-store, `agent_scoped_key_cannot_author_or_import_cards` / `non_holder_agent_key_cannot_mutate_anothers_claim` in powder-server, `cli_actor_flag_enforces_claim_holder_like_http_and_mcp` in powder-cli, and `mcp_actor_argument_enforces_claim_holder_like_http_and_cli` in powder-mcp — the last three assert the literal same "does not hold the active claim" / intruder-name text surfaces on all three faces). Oracle fully closed; epic shipped.
