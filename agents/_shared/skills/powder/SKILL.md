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
default. Remote origins require HTTPS; HTTP is loopback-only. `POWDER_AGENT` is
optional audit metadata. Managed workers pass the
exact canonical label `forest-misty-step/powder`, but this label never
authorizes a lease or any other operation. `POWDER_API_KEY` is transport
authentication.

Run `powder doctor [--agent "$POWDER_AGENT"]` before selection to verify the
configured origin and service. If the check fails, do not call Powder. GitHub
Issues remain the Tracker.

Each successful `powder take` returns a flat JSON Job plus a per-job
`claim_token` made from 32 random bytes encoded as base64url. Only its SHA-256
hash is stored in `jobs.lease_token_hash`. A live job returns `held` unless
the CLI presents that job's matching stored claim, which resumes it. An audit
label never grants resume. The claim token is capability for only that lease.

The CLI stores claim tokens privately under XDG state by validated origin and
job id, resumes by job id, and sends them automatically. It never prints
claims, and list/show/logs/notes never expose them. `release`, `renew`, `ask`,
`done`, live-job field edits, and live `abandon` require the claim; missing is
`claim_required`, and mismatch or expiry is `invalid_claim`. `note` stays
report-authorized and claim-independent. Free-job patching or abandon uses
`promote` authority without a claim. Tokens are deleted after release, ask,
done, or abandon.

## Factory loop

1. When `POWDER_AGENT` is set, run
   `powder list --mine "$POWDER_AGENT" --repo <forest.yaml repo>`. Treat
   `--mine` as an audit-label filter, never as resumable authority. A live
   candidate is resumable only when `powder take <id> [--agent "$POWDER_AGENT"]`
   succeeds with the private claim stored for this origin and job id, and no
   `forest/<id>/*` branch exists. When `POWDER_AGENT` is unset, attempt
   resumption only for a job id supplied by the managed run or current branch;
   otherwise continue to takeable selection.
2. `powder list --takeable --repo <forest.yaml repo>`
3. `powder show <id>`
   The spec is the work. Empty spec is not takeable. Show never contains a
   claim token.
4. `powder take <id> [--agent "$POWDER_AGENT"]`
   Do this before creating a branch. A successful take stores the private
   claim. A live job returns `held`, even under the same audit label; do not
   interpret `held` as permission from the label. Keep a published Subject
   held for the Kernel completion loop, and release only a failed or
   unpublished Builder attempt.
5. A Fixer uses the same Subject's private claim by job id before branch
   mutation; the audit label is only metadata.
6. Publish with schema v2, `tracker` set to the source actually selected
   (`github` or `powder`), and branch `forest/<id>/<slug>`. Every Subject uses
   that shape, including GitHub Issue numbers.
7. Agents do not call `powder done`. The Kernel completes the current
   Git-landed Subject only when request evidence has `tracker: powder`, using
   the approved Revision as proof. The CLI clears the private claim after
   completion or any release/ask/abandon.

Multiple distinct jobs may be held under the same managed audit label. Every
claim authorizes only its own job; managed workers and the Kernel do not mint
or share authority from the label.


## Verbs

```
powder list --takeable --repo REPO
powder list --mine AGENT --repo REPO
powder show ID
powder take ID
powder release ID
powder ask ID --question '...'
```
