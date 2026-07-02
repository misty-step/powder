# Stop fabricating acceptance criteria on card creation

Priority: P2 | Status: done | Type: Bug

## Goal
VISION.md is explicit: "Ready is a query, not vibes. Eligibility must be
explainable from card status, blockers, acceptance, priority, age, and claim
expiry." CLI `create-card` violates this today: when `--acceptance` is
omitted, it silently fills in the literal string `"proof exists"` and
defaults `--status` to `ready` regardless — manufacturing a fake oracle so
the card looks claimable even though nobody wrote a real completion
criterion. This was flagged in the original groom teardown and never fixed.

## Oracle
- [x] `powder create-card` with no `--acceptance` creates a card with empty
      acceptance, not a fabricated placeholder string.
- [x] A card created with empty acceptance defaults to `backlog` status, not
      `ready` — it cannot silently appear claimable with a fake oracle.
- [x] An explicit `--status` (even `ready`) is still honored regardless of
      acceptance, matching existing status/readiness semantics elsewhere
      (status is a label; `is_ready_at` is the independent gate).
- [x] HTTP `create_card`'s status default follows the same rule (empty
      acceptance -> `backlog` default, not `ready`) for consistency, even
      though HTTP already requires an explicit (possibly empty) `acceptance`
      field.

## Children
- Fix CLI `create_card`'s acceptance/status defaults.
- Fix HTTP `create_card`'s status default for consistency.
- Update tests that exercised the old fabricated-default behavior.

## Progress
- 2026-07-02 slice (overnight autonomous): re-read the original groom
  teardown's §7 deletion-candidates table for leftover findings not yet
  captured as their own backlog.d ticket, since the OVERNIGHT contract
  covers "tech debt, tests, refactors, docs, ergonomics" beyond just the
  existing queue. Confirmed live (still true in current code, not stale):
  CLI `create_card` defaulted `--acceptance` to the literal string
  `"proof exists"` and `--status` to `ready` unconditionally when the flags
  were omitted -- a real, still-present violation of VISION.md's "ready is
  a query, not vibes," confirmed by grep and by the fact every existing
  test that exercises `create-card` already passes both flags explicitly
  (so the fabricated default was live-and-dangerous for any real omitted
  invocation, but invisible to the test suite).
  Fixed both surfaces: CLI now defaults acceptance to empty (never
  fabricated) and derives the default status from whether acceptance is
  non-empty (`backlog` if empty, `ready` if not) rather than blindly
  defaulting to `ready`; HTTP's `create_card` (which already required an
  explicit, possibly-empty `acceptance` field, so it never fabricated
  criteria) gets the same conditional-default-status fix for consistency,
  since a caller submitting `"acceptance": []` with no `"status"` was still
  getting a `ready`-labeled card that could never actually satisfy
  `is_ready_at`. An explicit `--status`/`"status"` is honored regardless of
  acceptance either way -- status is a label, `is_ready_at` is the
  independent gate, matching the same pattern already proven for GitHub
  issue import (backlog.d/007) and reimport-safety generally.
  Proof: 1 new CLI test (no acceptance -> empty + backlog + absent from
  list-ready; explicit `--status ready` with empty acceptance still
  honored; real acceptance still defaults to ready) and 1 new HTTP test
  (empty acceptance never defaults to `ready`, absent from `/cards/ready`).
  101 workspace tests green (fmt/clippy/test).
