---
name: powder
description: Exclusive work ledger. Take a known job. Do not ask the system what is next.
---

# Powder

One noun: a Job. One take policy. CLI talks HTTP only.

```
export POWDER_URL=http://127.0.0.1:4000
export POWDER_API_KEY=<key>
export POWDER_AGENT=<label>
```

`powder <command> --help` is flag truth. JSON on stdout. `list --plain` and
`show --plain` print text. Errors are JSON on stderr with `code`.
`--agent` wins over `$POWDER_AGENT`.

## Take predicate

`take <id>` succeeds iff all of:

- not terminal (`proof` set or `abandoned`)
- not waiting (`ask` set)
- `spec` nonempty
- no live lease (`lease.until` > now)
- every **direct** `blocked_by` id exists and is terminal
- this agent holds no other live lease

If you already hold `id`, `take` returns it. Failure names the clause: `empty_spec`, `blocked`, `waiting`, `held`, `already_holding`, `terminal`, `not_found`.

`done` and `abandon` clear the lease and the ask. Do not `release` after them.

## Verbs

```
powder serve
powder version
powder list --takeable
powder list --takeable --plain
powder show <id>
powder take <id> [--agent <label>]
powder ask <id> --question '...' [--agent <label>]
powder answer <id> --text '...'
powder done <id> --proof <url-or-text> [--agent <label>]
powder abandon <id> [--agent <label>]
powder release <id>
powder renew <id> [--agent <label>]
powder note <id> --text '...' [--agent <label>]
powder create --id <slug> --title '...' [--spec '...'] [--repo <exact>] [--blocked-by a,b]
powder set-spec <id> --spec '...' [--agent <label>]
powder set-title <id> --title '...' [--agent <label>]
powder set-repo <id> [--repo <exact>|--clear] [--agent <label>]
powder set-blockers <id> [--blocked-by a,b|--clear] [--agent <label>]
powder reopen <id>
```

`list` filters: `--takeable --waiting --repo --mine`. Order is `created_at` ascending. That is scan order, not rank. `powder version` prints `powder-next <sha>`.

## Rules

- Claim one job. `already_holding` means finish, abandon, ask, or release first.
- Holder-only: `ask`, `done`, `renew`. Field edits: holder, or anyone if not live.
- Anyone may `release` or `answer`.
- After TTL the lease dies. `done` then fails `not_holder`; `take` again.
- Do not call a model from inside Powder.
