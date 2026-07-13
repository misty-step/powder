# Vendored: @misty-step/aesthetic law gate

`index.ts` and `invariants.ts` are byte-identical copies of
`misty-step/aesthetic`'s `law/` directory at tag **v0.25.0**. Both files are
byte-identical to that tag, and the served CSS is pinned to the same release.

### Why this refresh (v2.16.0 → v2.17.1)

v2.16.0's `index.ts` had `assertLaw`'s failure branch call
`expect.fail(...)`, importing `expect` from `@playwright/test` — but
Playwright's `expect` has no `.fail()` method (that's a Jest/Vitest API).
Every real law violation threw a raw `TypeError: expect.fail is not a
function` instead of the intended named-offender `Error`. Fixed upstream in
`misty-step/aesthetic` v2.17.1 (commit 9dd7ac0, fix at 00fb5b9):
`assertLaw` now does `throw new Error(...)` directly and no longer imports
`expect` at all. `invariants.ts` was not touched by this fix.

## Why vendored instead of a package dependency

The law gate is designed to be installed as `@misty-step/aesthetic` and
imported straight from `node_modules` (see the package's own
`law/README.md`), since it ships as `.ts` with no build step — Playwright's
test runner is supposed to transform it at require-time.

That works inside aesthetic's own repo (its `package.json` has no
`"type": "module"`, so Node treats it as CommonJS and Playwright's
require-time transform can patch it). It does **not** work in an ESM
consumer package (`"type": "module"`, as this `law/` package is): Node's
native loader raises `Unknown file extension ".ts"` for the `node_modules`
copy, and its `--experimental-strip-types` flag explicitly refuses to strip
types for anything under `node_modules` ("Stripping types is currently
unsupported for files under node_modules"). Playwright's own TS transform
does not reach into `node_modules` either. Confirmed locally 2026-07-03
against `@misty-step/aesthetic@v2.16.0` (same finding reported from the
`curb` adoption PR — this is a general consumer-ergonomics gap, not
powder-specific).

Vendoring the two files sidesteps the node_modules restriction: the files
live in the consumer's own source tree, so Playwright's transform applies
normally.

## Upgrading

Replace both files with the `law/` directory contents from the desired
`misty-step/aesthetic` tag, verbatim (no edits) — diff against upstream to
confirm, and bump `crates/powder-server/static/assets/aesthetic.css` in
the same change so the law and the CSS it checks stay pinned together.

## Reported upstream

Candidate fix for `@misty-step/aesthetic` backlog 015: ship a form of the
law gate that actually works when installed into an ESM consumer's
`node_modules` (e.g. pre-compiled `.js` output alongside the `.ts` source),
or document vendoring as the supported path for ESM consumers.
