# Create the GitHub remote after approval

Priority: P1 | Status: blocked | Estimate: S

## Goal
Create `misty-step/powder` only after the operator approves the outward-facing step.

## Oracle
- [ ] Operator explicitly approves remote creation and visibility.
- [ ] `gh repo create misty-step/powder --source=. --remote=origin --private` or the approved public variant succeeds.
- [ ] `git push -u origin factory/scaffold-vision-backlog` pushes the milestone branch.
- [ ] `git remote -v` shows only the intended `misty-step/powder` remote.

## Verification System
- Claim: Powder's GitHub remote exists and local branch sync is configured only after approval.
- Falsifier: A remote is created, pushed, or renamed before approval.
- Driver: `gh repo create`, `git push`, `git remote -v`, and remote branch check.
- Grader: GitHub repo URL and `git rev-list --left-right --count` against upstream.
- Evidence packet: Command transcript with secret-free output.
- Cadence: Once, when the operator approves.

## Notes
**Why:** The lane explicitly says to plan the `misty-step/powder` remote and pause before creating it.
