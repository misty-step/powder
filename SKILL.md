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
`POWDER_URL`. Environment wins over the config file. There is no default
origin or local-ledger fallback.

The holder identity is `--agent`, then the `POWDER_AGENT` workload identity,
then config `agent`, then `user@host`. `POWDER_AGENT` is distinct from the
shared `POWDER_API_KEY` transport credential. The default permits one live
lease across all repositories for that user and host. Parallel workers use
distinct holders. Subagents inherit the parent's holder.

`powder doctor` prints the resolved origin and holder sources, key presence,
and live health/readiness without printing key material.

`powder <command> --help` is flag truth. JSON on stdout. `list --plain`
and `show --plain` print text. Errors are JSON on stderr with `code`.

## Loop

1. `powder list --takeable --plain`
   Done when the list is on screen or empty.
2. `powder show <id>`
   Done when you can state the goal and the proof the spec asks for.
3. `powder take <id>`
   Done when the job is yours, or the `code` names why not.
4. Do the work the spec names.
5. `powder done <id> --proof <url-or-text>`
   Done when the job is terminal and you hold no lease.

One live lease per agent. `already_holding` means finish, ask, or
release first.

## Ask

Holder only. `powder ask <id> --question '...'` parks the job and
releases you. Any principal may `powder answer <id> --text '...'`.
Then someone takes it again.

## Take

`take` succeeds when the job is takeable and you hold no other live
lease. Takeable means: not terminal, not waiting, spec nonempty, no
live lease, every **direct** `blocked_by` exists and is terminal.

The `code` is the reason: `empty_spec` `blocked` `waiting` `held`
`already_holding` `terminal` `not_found` `no_origin`.

If you already hold `id`, `take` returns it.

## Authority

API keys carry repository-scoped capabilities. `report` may create an
empty-spec draft and add notes in its repository. `promote` may create or
set a nonempty spec and perform lifecycle work in its repository. A key with
no repository scope may act on every repository. `show` prints `created_by`,
`promoted_by`, and `promoted_at` when they are set; it never prints key
material. Authorization failures return `missing_capability` (the key lacks
the capability) or `repo_scope` (the key is scoped to another repository).

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
