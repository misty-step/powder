# Migration imports

Priority: P2 | Status: backlog | Type: Epic

## Goal
Import backlog files and GitHub issues from all Factory repos without erasing
live state. Migration is a sync into an instance database, not a destructive
rewrite of claims, statuses, runs, or proof.

## Oracle
- [x] Re-importing a backlog file over a claimed or running card preserves claim and runtime state.
- [x] Digest-aware import reports content drift without clobbering live lifecycle fields.
- [ ] GitHub issue import maps source URL, title, body, labels, and state without creating personal/operator data in the product repo.
- [x] A dry-run report shows create/update/skip/conflict counts before mutation.
- [ ] A synthetic multi-repo fixture proves import ordering and duplicate handling.

## Children
- Fix the import-clobber bug before importing real fleet work.
- Add digest-aware update semantics.
- Namespace card ids per repo so eight independently numbered backlog.d directories can share one instance (done — `load_backlog_dir_for_repo` / `powder import-repo`).
- Add a body-content import request shape so a remote client can push parsed cards to a flycast-only deployed instance instead of only reading a server-local path (done — `POST /api/v1/cards/import` accepts `files`/`repo`).
- Add GitHub issue source adapter with dry-run mode.
- Add a synthetic multi-repo fixture (id collision, claimed card, in-directory duplicate).
- Import Factory backlog into the deployed Powder instance after the sync contract is safe.

## Progress
- 2026-07-01 slice: fixed the import-clobber bug. `Store::import_cards` used to
  blind-`UPSERT` every parsed card, unconditionally overwriting `status` and
  `claim_*` columns with whatever the backlog.d file's front matter said
  (always claim-less, usually `status: ready`) — so re-running import over a
  claimed, running, or already-shipped card silently reverted it. Fixed via
  `Card::protects_lifecycle_on_reimport`/`Card::merge_reimport` in
  powder-core (a domain rule, not an adapter-only patch): a card that is
  claimed, running, awaiting input, or already at a terminal outcome
  (done/shipped/abandoned) keeps its live `status`/`claim` across reimport,
  while everything else (title, body, acceptance, labels, priority,
  blocked_by, repo/workspace/branch, source path+digest) still refreshes
  from the file, and `created_at` is never reset. A quiescent card
  (backlog/ready/blocked, no active claim) is free to have its status
  refreshed by the file, since no one owns it.
  `Store::import_cards` now returns an `ImportOutcome{created, updated,
  preserved, unchanged}` (digest-compared against the stored `source.digest`)
  instead of a bare count, and a new `Store::preview_import` computes the
  same breakdown read-only for dry-run reporting. Wired through HTTP
  (`POST /api/v1/cards/import` gains `dry_run`, returns the outcome struct)
  and CLI (`powder import <dir> --db X [--dry-run]` prints
  `total=/created=/updated=/preserved=/unchanged=`).
  Proof: 6 new powder-store tests (reimport over claimed/terminal/quiescent
  cards, mixed-outcome counts, preview-doesn't-mutate), 4 new
  `Card::merge_reimport`/`protects_lifecycle_on_reimport` unit tests in
  powder-core, and a CLI integration test
  (`cli_reimport_over_a_claimed_card_preserves_the_claim`) driving a real
  temp backlog.d directory through claim → reimport → dry-run. 72 workspace
  tests green (fmt/clippy/test). GitHub issue adapter and the multi-repo
  fixture remain open — see the design note below for the multi-repo
  import shape.
- 2026-07-01 slice 2: shipped the id-namespacing piece of the multi-repo
  design and wrote up the rest. `powder_shell::load_backlog_dir_for_repo`
  loads one repo's backlog.d, tags every card `repo = Some("org/repo")`,
  and rewrites its id to `{repo-slug}-{original-id}` (e.g. `bitterblossom-001`)
  so cards from independently numbered repos never collide even though
  every repo's backlog.d starts its own `001-*.md`. New CLI command
  `powder import-repo <path> --repo org/repo --db X [--dry-run]` wires it
  to the existing (now lifecycle-safe) `Store::import_cards`/
  `preview_import`. Proof: 2 new powder-shell tests, 1 new CLI test
  (`cli_import_repo_namespaces_ids_so_two_repos_never_collide`, two repos
  each shipping a `001-first.md`, both survive under distinct ids). 76
  workspace tests green.
- 2026-07-02 slice (overnight autonomous): closed the HTTP body-content gap
  from §3 below. `ImportRequest` now takes `path: Option<String>` (server-
  local, unchanged) *or* `files: Option<Vec<{path, contents}>>` (raw
  markdown, parsed server-side via the same `parse_backlog_card` the
  local-path route already uses, so digest computation stays in one
  place), plus an optional `repo` that runs the result through
  `namespace_cards_for_repo` -- the same id-namespacing `import-repo`
  already does locally, now reachable over HTTP. Sending both `path` and
  `files`, or neither, is a 400. Extracted `powder_shell::namespace_cards_for_repo`
  out of `load_backlog_dir_for_repo` (which now validates the repo slug
  before touching the filesystem, fixing an ordering bug the refactor's
  own test caught) so the HTTP route and the CLI share one namespacing
  implementation instead of two. A remote client (an operator's machine,
  a CI job) can now push a repo's backlog.d to a flycast-only deployed
  instance's `/api/v1/cards/import` without the instance ever reading a
  local path. Still open: a `powder import-repo --api-base-url/--api-key`
  CLI variant that builds this request automatically (today a caller
  drives the new shape directly, e.g. with curl or the existing MCP-remote
  bearer-key pattern from backlog.d/005) -- deferred to keep this slice
  focused; noted as a follow-up, not silently dropped.
  Proof: 3 new powder-shell tests (direct `namespace_cards_for_repo`
  coverage + the ordering-bug regression), 4 new HTTP tests (`files` body
  creates a card, `files`+`repo` namespaces the id over HTTP, `path`+`files`
  together is 400, neither is 400). 86 workspace tests green
  (fmt/clippy/test).

## Design: importing all nine Factory repos' backlog.d

The nine repos are bitterblossom, weave, powder, crucible, harness-kit,
bastion, cerberus, canary, landmark (per `~/.factory-lanes/SUPERVISOR.md`).
Powder is itself one of the nine, but its own root `backlog.d/` holds
Powder's *product-development* epics (this file is one) — per `AGENTS.md`
those are committed source, never imported instance data. "Import all nine"
means importing the *other eight* repos' backlog.d into the deployed
instance's database; Powder's own epics stay exactly as they are.

**1. Id collisions are the first hazard, now solved.** Every repo numbers
its own backlog.d independently, so a flat shared card-id space guarantees
collisions (repo A's `001` clashes with repo B's `001`). `load_backlog_dir_for_repo`
+ `import-repo` (this slice) solve it: namespace every imported id
`{repo-slug}-{original-id}` and tag `card.repo`. Local, single-repo usage
(`powder import backlog.d` for Powder's own epics) is untouched — it keeps
bare ids, since it never mixes with another repo's cards.

**2. The reimport-safety fix (this epic's first slice) is exactly the
right foundation for this** — repeatedly running the fleet import job
(e.g. nightly, or on every push to a source repo's backlog.d) must not
revert cards that agents are actively working through Powder. That's
already true today: `import_cards`/`preview_import` compare digests and
protect claimed/running/awaiting-input/terminal cards regardless of which
repo tagged the card.

**3. Closed: the HTTP import route no longer requires a server-side path.**
`POST /api/v1/cards/import` now takes `{"path": "..."}` (server-local,
unchanged) *or* `{"files": [{"path", "contents"}], "repo": "..."}` (raw
markdown parsed server-side, then optionally namespaced) — a remote client
(an operator's machine, a CI job) can push a repo's backlog.d to a
flycast-only deployed instance without the instance ever reading a local
path. Still open: a `powder import-repo --api-base-url/--api-key` CLI
variant that builds this request automatically instead of a caller driving
it directly (curl, or a small script) — small, deferred, not blocking.

**4. Ordering and duplicate handling.** Import repos in a fixed order
(alphabetical by slug is simplest and reviewable in a log) so a partial
run is resumable and diffable. Within a repo, `load_backlog_dir` already
sorts files by name before parsing. Duplicates across a re-run of the same
repo are exactly the reimport case the digest-aware merge already handles;
duplicates *within* one `import_cards` call for the same id (e.g. a
malformed backlog.d with two `001-*.md` files) resolve last-write-wins
within the transaction, same as today — worth a fixture case, not a code
change.

**5. GitHub issue adapter (separate child, still unscoped).** Out of
scope for this slice. When picked up: map `source` to the issue URL (not a
local file path — `CardSource.digest` still works over the issue body's
content hash), `labels` from GitHub labels, `status` from open/closed (+
maybe a label convention for `ready`/`blocked`), and route through the
same `Card::merge_reimport` so a closed-then-reopened issue can't clobber
an in-flight Powder claim either. Needs its own dry-run mode and must not
create personal/operator data in this repo (per `AGENTS.md`) — the adapter
lives in powder-shell/powder-store, not a data file committed here.

**6. Synthetic multi-repo fixture (test-only, still open).** A
`tests/fixtures/` tree with 2-3 fake repos' backlog.d (mirroring
`crates/powder-core/tests/fixtures/backlog.d/001-example.md`'s existing
single-repo fixture), including: an id collision across repos (both ship
`001-*.md`), a card claimed in one repo, and a stale duplicate within one
repo's directory — to exercise the full `import-repo` → `import_cards`
path end-to-end the way `cli_import_repo_namespaces_ids_so_two_repos_never_collide`
does today, but as a reusable fixture rather than inline `tempdir` writes.
