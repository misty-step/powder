# `powder-qa` eval

The one claim this generated skill must earn: **a cold agent uses this skill
to run a real card-lifecycle smoke against a throwaway local DB (init-db →
import → list-ready) with the exact flags from the skill, on the first try —
where the bare repo (README's "Current local smoke paths" is present but
un-flagged as the QA path, and the root `SKILL.md` describes the deployed
*product* contract, not a build/verify workflow) leaves an agent unsure
whether to treat `cargo test --workspace` alone as sufficient QA for a
claim/lease change.**

## Fixtures

| # | Task given to the cold agent | Forbidden edits | What it stresses |
|---|---|---|---|
| 1 | "Use this skill to verify Powder's card claim lifecycle actually works end to end, without touching any real instance data." | No writes outside `/tmp`; no edits to fixture backlog under `crates/powder-core/tests/fixtures/`; no commits | Distinguishing the deterministic gate (insufficient alone) from the live CLI lifecycle smoke, and using a throwaway DB rather than a real/committed one |

## Objective checks

- [x] The agent runs `cargo fmt`/`clippy`/`test` AND the CLI lifecycle smoke,
      not just the deterministic gate alone (the skill states the gate is
      necessary but not sufficient for a lifecycle change).
- [x] The DB path used is a throwaway path (e.g. under `/tmp`), never a path
      that would leave instance data in the repo tree.
- [x] The agent uses the repo's own fixture backlog
      (`crates/powder-core/tests/fixtures/backlog.d`) as import source, not
      invented or real operator backlog content.
- [x] `claim` → `heartbeat`/`update-status` → `get-card` round-trips
      correctly (the returned JSON reflects the claimed/running state).
- [x] The agent reports a verdict in the skill's Report contract shape.

## Pass condition

The cold agent completes fixture 1 using only the skill + repo, all objective
checks passing. A no-op "skill" fails because a cold agent working from the
bare repo alone is likely to stop at `cargo test --workspace` (green, but
exercising only canned fixtures) rather than also running the live claim
round-trip the skill calls out as the thing tests can't prove.

## Cadence

Re-smoke when the CLI subcommand surface, DB env var name, or fixture backlog
path changes; re-check after any `.github/workflows/ci.yml` gate change.

## Run log

2026-07-01 — Self-validation run (generation author, not evidence, but
confirms every command is real before committing): ran `cargo fmt --all --
--check` (exit 0) and the full CLI lifecycle
(`init-db → import → list-ready → claim → heartbeat → update-status →
get-card`) against `/tmp/powder-smoke/powder.db` on a clean worktree at
`origin/main@f948307` — every command matched the skill verbatim and returned
the expected output (imported card `001`, claim/heartbeat/status round-tripped
correctly in `get-card`'s JSON). Cleaned up the throwaway DB afterward.

2026-07-02 — **Cold-agent fixture 1 run: PASS.** Fresh-context subagent
(general-purpose, Sonnet 5), given only `powder-qa/SKILL.md` + normal repo
read access, no session memory of this generation. Task: "verify Powder's
card claim lifecycle actually works end to end, without touching any real
instance data." Ran the full 12-step CLI lifecycle verbatim from the skill
(`init-db → import → list-ready → claim → heartbeat → update-status →
request-input → list-awaiting-input → answer-input → get-card →
complete-card → get-card`) against a throwaway `/tmp` DB — overall exit 0.
Final `get-card` JSON showed `status: done`, run `state: complete`, the
recorded proof URL, and a full activity trail (claimed → heartbeat →
elicitation → operator response → completed). Agent reported explicit
self-sufficiency: "No guessing, no inventing, no digging outside the skill
required to succeed." All objective checks passed; cleaned up its own
throwaway DB; left the repo tree unmodified. One non-blocking note folded
back into the skill's Gotchas: `init-db --show-secret` prints the bootstrap
key to stdout — now called out explicitly.
