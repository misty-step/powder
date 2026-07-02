# Migration imports

Priority: P2 | Status: backlog | Type: Epic

## Goal
Import backlog files and GitHub issues from all Factory repos without erasing
live state. Migration is a sync into an instance database, not a destructive
rewrite of claims, statuses, runs, or proof.

## Oracle
- [x] Re-importing a backlog file over a claimed or running card preserves claim and runtime state.
- [x] Digest-aware import reports content drift without clobbering live lifecycle fields.
- [x] GitHub issue import maps source URL, title, body, labels, and state without creating personal/operator data in the product repo.
- [x] A dry-run report shows create/update/skip/conflict counts before mutation.
- [x] A synthetic multi-repo fixture proves import ordering and duplicate handling.

## Children
- Fix the import-clobber bug before importing real fleet work.
- Add digest-aware update semantics.
- Namespace card ids per repo so eight independently numbered backlog.d directories can share one instance (done — `load_backlog_dir_for_repo` / `powder import-repo`).
- Add a body-content import request shape so a remote client can push parsed cards to a flycast-only deployed instance instead of only reading a server-local path (done — `POST /api/v1/cards/import` accepts `files`/`repo`).
- Add GitHub issue source adapter with dry-run mode — done, `powder_shell::github` + `powder import-github-issues`.
- Add a synthetic multi-repo fixture (id collision, claimed card, in-directory duplicate) — done, `crates/powder-cli/tests/multi_repo_import.rs`.
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
- 2026-07-02 slice (overnight autonomous): added the synthetic multi-repo
  fixture from §6 below. `crates/powder-cli/tests/fixtures/multi_repo/{repo-a,repo-b}`
  ships two repos that each number their own tickets from `001`, plus an
  in-directory duplicate in repo-a (`001-first.md` and
  `001-first-duplicate.md`, both parsing to bare id `001`). The new
  integration test `crates/powder-cli/tests/multi_repo_import.rs` drives
  `import-repo` on both repos through real SQLite and proves: the id
  collision never happens (`repo-a-001` and `repo-b-001` coexist), the
  in-directory duplicate resolves deterministically (alphabetical
  processing order, canonical file persisted last, its content wins), an
  ordinary second card imports fine alongside the collision cases, and --
  reusing the reimport-safety fix from this epic's first slice -- claiming
  `repo-a-001` and then reimporting repo-a again reports both source files
  mapping to it as `preserved` (never `updated`), proving the claim/status
  survives a stale reimport even when a duplicate file is present in the
  same batch. Only the GitHub issue adapter (needs its own scoping pass,
  possibly a follow-up session) and the CLI `--api-base-url` remote-push
  convenience noted in the slice above remain open in this epic.
  90 workspace tests green (fmt/clippy/test).
- 2026-07-02 slice (overnight autonomous): closed the GitHub issue adapter
  oracle item — see §5 below for the design. New `powder_shell::github`
  module (`github_issue_to_card`, `load_github_issues_file`) maps an
  already-fetched-by-the-operator JSON array of GitHub issues into
  namespaced, digest-tracked `Card`s; new CLI command
  `powder import-github-issues <file.json> --repo org/repo --db X
  [--dry-run]` wires it to the existing `Store::import_cards`/
  `preview_import`, so it gets reimport-safety, dry-run reporting, and
  digest-aware drift detection for free -- no new store or HTTP code
  needed, only a new source of `Vec<Card>`. Deliberately does not talk to
  the GitHub API itself: no token ever needs to reach powder, and the
  operator's own `gh issue list --json ... > issues.json` step is the only
  place credentials or rate limits are a concern. Acceptance is
  deliberately left empty on import rather than fabricated, so an imported
  issue stays unclaimable until a real oracle exists for it -- proved live
  via a CLI test that shows moving an acceptance-less card to `ready`
  doesn't make it show up in `list-ready`.
  Proof: 6 new `powder_shell::github` unit tests (open→Backlog with no
  fabricated acceptance, closed→Done, digest changes on body/state edits,
  different repos never collide on the same issue number, a real JSON
  array maps correctly, an invalid repo slug is rejected) and 1 new CLI
  integration test
  (`cli_import_github_issues_maps_open_and_closed_issues_and_survives_reimport`)
  proving: no fabricated acceptance, status-alone doesn't make a card
  claimable, and reopening a closed issue on GitHub can't revert Powder's
  `Done` status on reimport (while content like title/body still refreshes,
  identically to backlog.d reimport). 99 workspace tests green
  (fmt/clippy/test).
  This closes all 5 backlog.d/007 oracle items -- the sync contract (fixed
  reimport, digest-aware drift, dry-run reporting, multi-repo namespacing +
  fixture, GitHub issues) is now provably safe. What's still open is the
  epic's last Children item: actually running this against the other eight
  Factory repos' real backlog.d/issues and importing into the deployed
  instance -- a separate, larger undertaking (needs live checkouts of
  those repos, GitHub read access, and likely coordination with the other
  repos' own lanes) that stays out of scope for an unattended overnight
  session. Status stays `backlog` until that migration actually runs.

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

**5. Closed: GitHub issue adapter.** `powder_shell::github` maps an
already-fetched GitHub issue to a `Card`: id `{repo-slug}-{issue-number}`,
`source` set to the issue's `html_url` (not a local file path — the digest
is computed over title+body+labels+state instead of file bytes, so it still
tracks drift), `labels` from GitHub labels, `status` open→`Backlog` /
closed→`Done`, routed through the existing `Card::merge_reimport` so a
reopened issue can't revert Powder's `Done` record. Deliberately
file-based, not a live GitHub API client: an operator runs
`gh issue list --json number,title,body,labels,state,url --repo org/repo > issues.json`
themselves (their own credentials, their own rate limits), and
`powder import-github-issues issues.json --repo org/repo --db X
[--dry-run]` only maps and imports what's already on disk — no GitHub
token ever needs to reach powder, no personal/operator issue data is ever
committed to this repo (the JSON is a local, gitignored-by-convention
scratch file, same as any `--db` path). Acceptance is deliberately left
empty on import (GitHub issues don't carry a backlog.d-style Oracle
section) — an imported issue stays unclaimable until a real oracle is
added later, rather than fabricating one.

**6. Closed: synthetic multi-repo fixture.**
`crates/powder-cli/tests/fixtures/multi_repo/{repo-a,repo-b}` +
`crates/powder-cli/tests/multi_repo_import.rs` exercise the full
`import-repo` → `import_cards` path end-to-end against real SQLite: an id
collision across repos, an in-directory duplicate resolving
deterministically, and a claimed card surviving a stale reimport of the
same repo.
