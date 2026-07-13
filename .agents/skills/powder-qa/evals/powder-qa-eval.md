# `powder-qa` eval

The one claim this generated skill must earn: **a cold agent uses this skill
to run a real card-lifecycle smoke against a throwaway local DB (init-db →
create-card → list-ready) with the exact flags from the skill, on the first try —
where the bare repo (README's "Current local smoke paths" is present but
un-flagged as the QA path, and the root `SKILL.md` describes the deployed
*product* contract, not a build/verify workflow) leaves an agent unsure
whether to treat `cargo test --workspace` alone as sufficient QA for a
claim/lease change.**

## Fixtures

| # | Task given to the cold agent | Forbidden edits | What it stresses |
|---|---|---|---|
| 1 | "Use this skill to verify Powder's card claim lifecycle actually works end to end, without touching any real instance data." | No writes outside `/tmp`; no commits | Distinguishing the deterministic gate (insufficient alone) from the live CLI lifecycle smoke, and using a throwaway DB rather than a real/committed one |

## Objective checks

- [x] The agent runs `cargo fmt`/`clippy`/`test` AND the CLI lifecycle smoke,
      not just the deterministic gate alone (the skill states the gate is
      necessary but not sufficient for a lifecycle change).
- [x] The DB path used is a throwaway path (e.g. under `/tmp`), never a path
      that would leave instance data in the repo tree.
- [x] The agent creates one synthetic card through the supported CLI, without
      reading or inventing real operator backlog content.
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

Re-smoke when the CLI subcommand surface or DB env var name changes; re-check
after any `.github/workflows/ci.yml` gate change.

## Run log

2026-07-13 — Self-validation after repository-ingestion retirement: ran the
full documented lifecycle (`init-db → create-card → list-ready → claim →
heartbeat → update-status → request-input → list-awaiting-input → answer-input
→ complete-card → get-card`) against a fresh throwaway `/tmp` DB. The final
readback showed card `001` done, its run complete, and the proof URL persisted;
the temporary directory was removed. A new cold-agent comparison is still due
before claiming this eval itself has passed.
