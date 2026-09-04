---
name: powder
description: >
  Powder is the exclusive-work ledger. Use when listing takeable jobs,
  taking a job, asking the operator, or completing work with proof.
---

# Powder

Powder stores jobs. Take one. Finish it. Write proof.

## When

Use Powder when work must be exclusive across agents: the next takeable
job, a known job id, an operator question, or a completion.

## Client

The CLI is an HTTP client of one explicitly configured instance. Run
`powder use <url>` once to write `~/.config/powder/config`, or set
`POWDER_URL`. Environment wins over the config file. Remote origins require
HTTPS; HTTP is loopback-only. There is no default origin or local-ledger
fallback.

The optional audit label is `--agent`, then `POWDER_AGENT`, then config
`agent`; when none is supplied, the CLI may use its local default only as
audit metadata. `POWDER_AGENT` never grants ownership or authorization.
`POWDER_API_KEY` authenticates transport. Managed workers pass the canonical
label `forest-misty-step/powder`, but the label is not a capability.

Each successful `take` creates a per-job claim. The server returns a flat JSON
Job plus a `claim_token` made from 32 random bytes encoded as base64url; only
the SHA-256 hash is stored in `jobs.lease_token_hash`. A live-job take returns
`held` unless it presents that job's matching claim token, which resumes the
existing claim. An audit label never grants resume, and distinct jobs may be
held under one label.

The CLI stores claims privately under XDG state by validated origin and job id,
resumes by job id, and sends the claim automatically. It never prints claim
tokens, and list/show/logs/notes never contain them. `release`, `renew`, `ask`,
`done`, live-job field edits, and live `abandon` require the claim: missing is
`claim_required`; mismatched or expired is `invalid_claim`. `note` stays
report-authorized and claim-independent. Free-job patching or abandoning uses
`promote` authority without a claim. The CLI deletes the claim after release,
ask, done, or abandon.

`powder doctor` prints the resolved origin and audit-label sources, key
presence, and live health/readiness without printing key material or claims.

`powder <command> --help` is flag truth. JSON on stdout. `list --plain`
and `show --plain` print text. Errors are JSON on stderr with `code`.

## Loop

1. `powder list --takeable --plain`
   Done when the list is on screen or empty.
2. `powder show <id>`
   Done when you can state the goal and the proof the spec asks for.
3. `powder take <id>`
   Done when the job is yours and the CLI has stored its private claim, or
   when the `code` names why not. A live job reports `held`, including for the
   same audit label.
4. Do the work the spec names.
5. `powder done <id> --proof <url-or-text>`
   Done when the job is terminal and its private claim has been deleted.

## Ask

A valid claim permits `powder ask <id> --question '...'`; the command parks
the job, releases its lease, and deletes the private claim. Any principal may
`powder answer <id> --text '...'`. Then someone takes it again.

## Take

`take` succeeds when the job is takeable, or resumes a live job when its saved
claim matches. Takeable means: not terminal, not waiting, spec nonempty, no live
lease, every **direct** `blocked_by` exists and is terminal. A take of a live
job without its matching claim returns `held`, regardless of audit label.

The `code` is the reason: `empty_spec` `blocked` `waiting` `held` `terminal`
`not_found` `no_origin` `claim_required` `invalid_claim`.

## Authority

API keys carry repository-scoped capabilities. `report` may create an
empty-spec draft and add notes in its repository. `promote` may create or
set a nonempty spec and perform lifecycle work in its repository. A key with
no repository scope may act on every repository. `show` prints `created_by`,
`promoted_by`, and `promoted_at` when they are set; it never prints key
material or claim tokens. Authorization failures return `missing_capability`
or `repo_scope`.

## Verbs

```
powder serve
powder version
powder use <url>
powder doctor
powder skill
powder list --takeable
powder show <id>
powder take <id>
powder release <id>
powder renew <id>
powder note <id> --text '...'
powder ask <id> --question '...'
powder answer <id> --text '...'
powder done <id> --proof <url-or-text>
powder abandon <id>
powder reopen <id>
powder create --id <slug> --title '...'
powder set-title <id> --title '...'
powder set-spec <id> --spec '...'
powder set-repo <id>
powder set-blockers <id>
```

`list` filters: `--takeable --waiting --repo --mine --query/-q`,
`--state`, `--summary`, `--limit`, `--cursor`. `--state` takes one of
`draft blocked waiting live takeable open terminal abandoned done`;
it cannot be combined with `--takeable`/`--waiting`. `--summary` returns
bounded rows without spec, notes, ask text, or proof body. `--limit` and
`--cursor` page the stable `created_at,id` order and return `next_cursor`
in the machine envelope. `--query` matches a case-insensitive title
substring. Order is `created_at` ascending (scan order, not rank).
