# Audit and fix server-local path exposure on Card.source

Priority: P2 · Status: done · Estimate: S

## Goal
Tonight's fix (backlog.d/005) removed the server's local `db_path` from
unauthenticated `/healthz`/`/readyz`/`/api/v1/onboarding` responses on the
grounds that it is "a pure implementation detail with no operational value
to a caller." `Card.source.path` is an unaudited second instance of the same
class: a plain, always-serialized field carrying the server's local
filesystem path at import time, returned to any caller — including
agent-scoped keys — who can read a card.

## Oracle
- [x] Every field on `Card` (and its nested `CardSource`) returned by
      `GET /api/v1/cards/{id}`, `/api/v1/cards/ready`, the new list endpoint
      if backlog.d/010 has landed, and MCP `get_card` is reviewed against the
      same standard 005 applied to `db_path`: does an agent/operator caller
      need this to act, or is it a server-filesystem detail?
- [x] `Card.source.path` is either redacted/relativized for non-admin
      callers, replaced with something that carries no server filesystem
      layout, or explicitly justified in a code comment the way 005
      justified `auth_mode`/`public_base_url` staying public.
- [x] A test locks in whichever behavior is chosen so it can't silently
      regress (mirroring 005's
      `healthz_readyz_and_onboarding_are_unauthenticated_and_never_leak_the_db_path`
      pattern).
- [x] `cargo test --workspace` stays green.

## Progress
- 2026-07-02 slice (overnight autonomous): reviewed every `Card`/`CardSource`
  field against the same standard 005 applied to `db_path`. Every route
  (`GET /api/v1/cards/{id}`, `/api/v1/cards/ready`, the new
  `GET /api/v1/cards` list endpoint from backlog.d/014, and MCP `get_card`)
  serializes the same `Card` struct directly, so the review is one pass over
  the type, not per-route:
  - `id`/`title`/`body`/`acceptance`/`status`/`priority`/`labels`/
    `blocked_by`/`repo`/`claim`/`created_at`/`updated_at` -- all genuinely
    needed for a caller to act; no server-filesystem content.
  - `assignee`/`workspace_path`/`branch_name` -- confirmed still dead (no
    surface writes a real value to any of them, same finding the original
    groom report made; only round-tripped via the store's `ON CONFLICT
    ... = excluded.*` upsert). Not a practical leak today since they're
    always null, but they carry the identical risk class as `source.path`
    the moment anything ever writes a real value -- flagged for whoever
    eventually wires them (backlog.d/018 tracks the still-dead-field
    cleanup question generally; this note is the leak-specific flag for
    `workspace_path`/`branch_name` specifically, since that name in
    particular reads like exactly the kind of server-local-checkout-path
    field this ticket is about).
  - `source.digest` -- a hash, not sensitive.
  - `source.path` -- confirmed live: for backlog.d-imported cards,
    `load_backlog_dir` passed the caller's full argument (potentially an
    absolute local path, e.g. `powder import-repo
    /Users/operator/dev/bitterblossom/backlog.d ...`) straight into
    `parse_backlog_card`, which stored it verbatim in `CardSource.path` --
    persisted forever, visible to any caller (including agent-scoped keys)
    who can read that card. GitHub-issue-imported cards already use the
    issue's `html_url` (not sensitive) and were untouched.
  Fixed at the source: `load_backlog_dir` now passes only the file's
  basename to `parse_backlog_card`, never the full path. `id_from_path`
  already extracts just the basename regardless (`path.rsplit('/')`), so id
  derivation is unaffected; `source.path` is never used as a comparison or
  lookup key anywhere in the reimport-safety logic (only `source.digest`
  is), so truncating it changes no behavior beyond removing the leak.
  Chose truncation over admin-only redaction (matching 005's approach of
  removing `db_path` outright rather than gating it by caller): the
  basename still carries real, useful context (which file a card came
  from), while the directory-structure portion has no operational value to
  any caller, admin or agent.
  Proof: new `powder-shell` test imports a card from a directory under
  `std::env::temp_dir()` (an absolute path on every platform this runs on)
  and asserts `source.path` is exactly the basename, not the directory.
  111 workspace tests green (fmt/clippy/test).

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
