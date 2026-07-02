# Audit and fix server-local path exposure on Card.source

Priority: P2 · Status: ready · Estimate: S

## Goal
Tonight's fix (backlog.d/005) removed the server's local `db_path` from
unauthenticated `/healthz`/`/readyz`/`/api/v1/onboarding` responses on the
grounds that it is "a pure implementation detail with no operational value
to a caller." `Card.source.path` is an unaudited second instance of the same
class: a plain, always-serialized field carrying the server's local
filesystem path at import time, returned to any caller — including
agent-scoped keys — who can read a card.

## Oracle
- [ ] Every field on `Card` (and its nested `CardSource`) returned by
      `GET /api/v1/cards/{id}`, `/api/v1/cards/ready`, the new list endpoint
      if backlog.d/010 has landed, and MCP `get_card` is reviewed against the
      same standard 005 applied to `db_path`: does an agent/operator caller
      need this to act, or is it a server-filesystem detail?
- [ ] `Card.source.path` is either redacted/relativized for non-admin
      callers, replaced with something that carries no server filesystem
      layout, or explicitly justified in a code comment the way 005
      justified `auth_mode`/`public_base_url` staying public.
- [ ] A test locks in whichever behavior is chosen so it can't silently
      regress (mirroring 005's
      `healthz_readyz_and_onboarding_are_unauthenticated_and_never_leak_the_db_path`
      pattern).
- [ ] `cargo test --workspace` stays green.

## Notes
`crates/powder-core/src/model.rs:365-368` (`CardSource{path, digest}`) is a
plain serialized field on every `Card` (`model.rs:398`). For backlog.d-
imported cards, `path` is the operator's local filesystem path at import
time (an absolute or relative checkout path on whatever machine ran
`powder import`), visible to any agent-scoped key via `GET
/api/v1/cards/{id}` today. GitHub-issue-imported cards use the issue's
`html_url` as `source.path` instead, which is not sensitive — this ticket is
specifically about the backlog.d-import case.

**Why:** live-read of `crates/powder-core/src/model.rs:365-402` and the
card-read routes (2026-07-01) shows this field was never reviewed; 005's own
progress note explicitly scoped its audit to
`healthz`/`readyz`/`onboarding` only, not card responses, leaving this the
same leak class as tonight's `db_path` fix but on a different route.
