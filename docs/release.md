# Powder release path

Powder ships versioned binaries and a container image from git tags. Landmark
owns user-facing release-note synthesis. Humans (or a lead workflow) still cut
the tag.

## Version identity

1. Git tag `vX.Y.Z` is the release identity and source of truth.
2. Cargo workspace crates intentionally stay at a stable floor version
   (`0.1.0` today). Install and runtime drift use **git SHA**, not Cargo
   semver bumps between tags. See `docs/operations.md`.
3. Do not require Cargo workspace version to equal the tag.
4. Cut tags on the intended merge commit after CI is green.

## Pipeline

| Step | Owner | Workflow |
|---|---|---|
| Build binaries + image, publish GitHub Release assets | Powder | `.github/workflows/release.yml` on `v*.*.*` tags |
| Synthesize user-facing notes (Landmark) | Landmark | `.github/workflows/landmark-release-notes.yml` on `release` published |
| Public changelog page | `docs/releases/` + `scripts/render-site-changelog.py` | regenerate in a PR; Pages deploys `site/` |

Landmark does **not** push commits to `master`. Under branch protection a
silent `git push || true` would drop notes. Land updated `docs/releases/*` and
regenerated `site/changelog.html` through a normal PR when notes change.

## Landmark artifacts

Configured in `.landmark.yml` and kept in git:

- `docs/releases/{version}.md`
- `docs/releases/releases.json`

`site/changelog.html` is generated from those artifacts:

```sh
python3 scripts/render-site-changelog.py
```

Do not hand-maintain the changelog page as the source of truth.

## Operator commands

```sh
# after master is green at the intended commit
git tag vX.Y.Z
git push origin vX.Y.Z

# re-run Landmark synthesis for an existing release (does not commit)
gh workflow run landmark-release-notes.yml -f release-tag=vX.Y.Z
```
