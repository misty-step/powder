# Powder Repo Contract

Powder is a self-hosted exclusive-work ledger. Agents take a known job.
They do not ask the system what is next. One Go binary, one SQLite file.

The Rust Card/Claim/Run/Event service is retired. Its git history remains
in this repository. Do not resurrect those crates.

## Faces

- Agent: `powder` CLI plus `SKILL.md`.
- HTTP: same contract as the CLI. Peek UI is SSR HTML in-process.
- Auth: `api-key` by default. `none` is loopback-only (Sanctum tailnet).

`SKILL.md` is generated-truth: `go test -run TestSkillDocumentsEveryCommand`
fails if a CLI verb is missing from the skill. Update the skill in the
same commit as any new verb.

## Production

One `powder serve` process on the Sanctum host, loopback bind, tailnet
origin. SQLite is `/data/apps/powder/ledger.db`. The retired rust
database remains on disk as an archive and is not served.

## Gates

```sh
go test ./...
go vet ./...
```

## Red lines

- Do not add a dispatch loop or model call.
- Do not add a second agent face beside `powder` plus `SKILL.md`.
- Do not create a repository-local ticket ledger.
- Do not commit instance data, API keys, or the `powder` binary.
- Do not change the take predicate without updating VISION.md and SKILL.md.
