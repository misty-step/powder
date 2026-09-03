---
name: powder
description: >
  Powder is the exclusive-work ledger. Use when listing takeable jobs
  for this repository, taking a job, asking the operator, or completing
  work with proof after an approve Gate.
---

# Powder

Powder stores jobs. Take one. Finish it. Write proof.

## Client contract

Origin resolves from `POWDER_URL`, then `~/.config/powder/config`; there is no
default. The approved runtime environment must set `POWDER_AGENT` to the exact
canonical holder `forest-misty-step/powder`. This workload identity is distinct
from the shared `POWDER_API_KEY` transport credential.

Require `POWDER_AGENT=forest-misty-step/powder`, then run
`powder doctor --agent "$POWDER_AGENT"` before selection. If either check fails,
do not call Powder. GitHub Issues remain the Tracker.

## Factory loop

1. `powder list --mine "$POWDER_AGENT" --repo <forest.yaml repo>`
   Continue a held job for this repository that has no
   `forest/<id>/*` branch.
2. `powder list --takeable --repo <forest.yaml repo>`
3. `powder show <id>`
   The spec is the work. Empty spec is not takeable.
4. `powder take <id> --agent "$POWDER_AGENT"`
   Do this before creating a branch. `already_holding` means finish or ask;
   release only a failed or unpublished Builder attempt. Keep a published
   Subject held for the Kernel completion loop.
5. A Fixer confirms or re-takes that same Subject before branch mutation.
6. Publish with schema v2, `tracker` set to the source actually selected
   (`github` or `powder`), and branch `forest/<id>/<slug>`. Every Subject uses
   that shape, including GitHub Issue numbers.
7. Agents do not call `powder done`. The Kernel completes the current
   Git-landed Subject only when request evidence has `tracker: powder`, using
   the approved Revision as proof.

One live lease per holder. Every managed worker and the Kernel inherit the
approved `POWDER_AGENT`; they do not mint task identities.

## Verbs

```
powder list --takeable --repo REPO
powder list --mine AGENT --repo REPO
powder show ID
powder take ID
powder release ID
powder ask ID --question '...'
```
