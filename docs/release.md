# Powder Release Path

Powder ships versioned binaries and a container image from git tags. Humans, or
a lead workflow, cut the tag after the merge commit passes the repository gate.

## Version Identity

1. Git tag `vX.Y.Z` is the release identity and source of truth.
2. Cargo workspace crates stay at their stable floor version (`0.1.0` today).
   Install and runtime drift use the embedded git SHA. See
   [`docs/operations.md`](operations.md).
3. Do not require Cargo workspace version to equal the tag.
4. Cut tags on the intended merge commit after CI is green.

## Pipeline

| Step | Owner | Workflow |
|---|---|---|
| Build binaries and image, publish release assets | Powder | `.github/workflows/release.yml` on `v*.*.*` tags |
| Write release notes | Release owner | `docs/releases/{version}.md` in the release PR |

Release notes are ordinary repository documents maintained in the release PR.
Do not add a second publishing surface or release-note service.

## Operator Commands

```sh
# after master is green at the intended commit
git tag vX.Y.Z
git push origin vX.Y.Z
```

Release assets must identify the exact tag and commit. The running server reports
its build SHA so an operator can compare a deployed binary with the source that
produced it.
