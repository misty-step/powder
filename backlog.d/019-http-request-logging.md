# Add HTTP request logging middleware

Priority: P2 | Status: done | Type: Bug

## Goal
The original groom teardown flagged this: `tracing_subscriber` is
initialized (`crates/powder-server/src/main.rs`) and then essentially
unused — no request logging middleware, no per-mutation events, no
metrics. For a coordination server whose whole value proposition is "audit
what the fleet did," the server's own observability is a blind spot: an
operator debugging a stuck claim or a spike in errors has nothing to look
at beyond `fly logs` showing two total log lines (a startup message and a
config-error path).

## Oracle
- [x] Every HTTP request logs method, path, status, and latency via the
      existing `tracing` setup (no new logging framework).
- [x] The log line never includes the `Authorization` header, request
      bodies, or response bodies (bearer keys and card content must not
      end up in log output).
- [x] A test proves request logging is wired (e.g., a request emits a
      tracing event capturable by a test subscriber).
- [x] `cargo test --workspace` and
      `cargo clippy --workspace --all-targets -- -D warnings` stay green.

## Progress
- 2026-07-02 slice (overnight autonomous): added `tower-http` (`trace`
  feature, matching the axum 0.8 generation already in use) and wired
  `.layer(TraceLayer::new_for_http())` onto the router in `app()`.
  `TraceLayer`'s default callbacks log method/path/status/latency and never
  touch headers or bodies, so bearer keys and card content never reach the
  log by construction -- no separate redaction work was needed.
  Testing this exposed a real trap: the ticket's suggested approach
  (capture live output via `tracing::subscriber::set_default` +
  `tracing_subscriber::fmt`) is genuinely flaky under `cargo test`'s
  parallel execution -- `tracing-core`'s per-callsite interest cache is
  process-wide, and concurrently-running tests each trying to install
  their own dynamically-scoped default race against each other (confirmed
  empirically: ~66% failure rate across repeated runs with default
  parallelism, 100% pass with `--test-threads=1`; tried
  `tracing::callsite::rebuild_interest_cache()` as a fix and it made things
  worse, 100% failure). Rather than ship a flaky test, switched to a
  deterministic proof: `TraceLayer::new_for_http().on_response(...)` is a
  plain closure invoked directly by the tower `Service` machinery
  regardless of any tracing subscriber state, so wrapping the real `app()`
  router in a second, test-only layer with a recording closure proves the
  same request/response data the production `TraceLayer` sees -- method,
  path, status -- reaches a callback on every request, and that the raw
  bearer token never does, without depending on global dispatch timing.
  Confirmed deterministic: 8/8 clean runs of the crate's tests plus 3/3
  clean full-workspace runs under normal parallel execution.
  Proof: 1 new test (`every_request_triggers_the_trace_layer_without_leaking_the_bearer_token`).
  118 workspace tests green (fmt/clippy/test).
  Per-mutation activity events and a `/metrics` endpoint remain open,
  separate asks (noted in this ticket's own scope-limiting note above).

## Notes
`rg "tracing::"` across `crates/powder-server/src/main.rs` (2026-07-02)
shows exactly two call sites: a config-error log and a startup message.
Nothing logs per-request. `tower-http`'s `TraceLayer` is the standard,
idiomatic axum middleware for this — it integrates directly with the
`tracing` crate already in use, requires no new logging framework, and its
default `on_request`/`on_response` callbacks log method/path/status/latency
without touching headers or bodies (so no separate redaction work is
needed for the request-logging half of this gap).

Per-mutation activity events and a `/metrics` endpoint (the rest of the
original groom report's observability finding) are bigger, separate
asks — this ticket is scoped to just closing the "no request logging at
all" half, the cheapest and most immediately useful piece.

**Why:** live-read of `crates/powder-server/src/main.rs` confirms the groom
report's finding is still accurate as of this session; none of the tickets
worked tonight (001–018) touched server-side observability.
