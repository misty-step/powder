# Stop bcrypt-per-request for API key verification

Priority: P2 | Status: done | Type: Bug

## Goal
The original groom teardown flagged this and it was never fixed: every
authenticated request runs `verify_api_key`, which bcrypt-verifies (cost 12,
~200-300ms of CPU) while holding the server's global `Arc<Mutex<Store>>`
synchronously on a tokio worker. Unknown-prefix requests still burn a dummy
verify. Ceiling is roughly single-digit requests/sec for the whole instance,
and a trickle of garbage-token requests degrades `/readyz` since it shares
the same lock. Worse: bcrypt is the wrong primitive here — it's a
deliberately slow KDF built to blunt brute-forcing *low-entropy human
passwords*. A Powder API key is a 32-character string drawn from a 64-symbol
alphabet (`sk_powder_...`), already far higher entropy than bcrypt's own
72-byte input limit can even represent meaningfully. The slow hash buys
nothing here and only taxes every request.

## Oracle
- [x] Every newly created API key is hashed with SHA-256, not bcrypt.
- [x] Every already-issued (bcrypt-hashed) key keeps authenticating after the
      migration — no deployed instance's existing keys break.
- [x] `verify_api_key` fails closed (never authenticates) for a row with an
      unrecognized `hash_algorithm` value, rather than guessing an algorithm.
- [x] `Store::migrate()` steps through every intermediate schema version in
      sequence instead of jumping straight from any `current` version to
      `SCHEMA_VERSION`, so a database several versions behind can never skip
      a migration's schema change while still being marked fully current.

## Progress
- 2026-07-02 slice (overnight autonomous): confirmed live via grep that
  `bcrypt::verify`/`bcrypt::hash` were still the only hashing path for API
  keys, exactly as the groom report described. Added `hash_algorithm` column
  (schema v3; `MIGRATE_2_TO_3` defaults existing rows to `'bcrypt'`, the
  fresh-init schema defaults new installs to `'sha256'`). New keys hash with
  SHA-256 + a constant-time comparison on verify; legacy bcrypt-hashed keys
  keep verifying via `bcrypt::verify` exactly as before — `verify_secret`
  branches on the row's own `hash_algorithm`, so no existing key (including
  whatever is live on the deployed Fly instance today) ever breaks. A row
  with any other `hash_algorithm` value fails closed.
  Fixed `Store::migrate()` in the same slice: it previously jumped straight
  from any `current` version to `SCHEMA_VERSION` in one step (`match
  current { 0 => full SCHEMA, 1 => MIGRATE_1_TO_2, ...}`, each arm setting
  `user_version = SCHEMA_VERSION` directly) — harmless with only two
  versions ever existing, but adding a third would have silently skipped a
  migration step for any database still behind by more than one version.
  Rewrote it as a loop that applies one migration per iteration and bumps
  `user_version` by exactly one step at a time, verified by extending the
  existing "migrate a v1 database" test to assert it now lands on v3 (not
  v2) and still authenticates its legacy key correctly after both
  migrations run.
  Proof: 2 new tests (a v2-schema database with a pre-existing bcrypt key
  migrates to v3 without breaking that key, and a newly created key after
  migration is stored with `hash_algorithm = 'sha256'` and verifies), 1 new
  fail-closed test (an unrecognized `hash_algorithm` never authenticates),
  and the existing v1-migration test extended to assert the full v1->v3
  jump. 104 workspace tests green (fmt/clippy/test). Residual: this doesn't
  address the still-synchronous nature of the store mutex itself (the other
  perf-floor items — `spawn_blocking`, connection pooling, SQL-native
  `list_ready` — remain open follow-ups), but removing the ~200-300ms
  bcrypt tax from every new-key request is the single highest-leverage fix
  from that list.
