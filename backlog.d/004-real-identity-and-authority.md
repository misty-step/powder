# Real identity and authority

Priority: P1 | Status: backlog | Type: Epic

## Goal
Replace caller-supplied identity strings with real actors. API keys, tailnet
headers, and future user sessions must resolve to agent or user entities, and
mutations must be authorized against claim ownership and explicit scope instead
of trusting an `agent` field in the request body.

## Oracle
- [ ] API keys are bound to durable identities and scopes, and each mutation records the resolved actor.
- [ ] A non-holder cannot mutate or complete another agent's active claim.
- [ ] Admin-only operations are enforced by scope, with tests proving agent keys are rejected.
- [ ] Cross-agent impersonation attempts return `403` across HTTP and equivalent CLI/MCP errors.
- [ ] Existing bootstrap/key-create flows migrate without exposing secrets or breaking first-run setup.

## Children
- Add agent/user identity records or an equivalent durable actor model.
- Thread actor context through store transactions and activity writes.
- Enforce claim-holder ownership for status, progress, input, and completion.
- Add key-management/admin routes only if they preserve the one-deployable shape.
