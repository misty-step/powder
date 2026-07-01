# CI gate and license

Priority: P0 | Status: done | Type: Epic

## Goal
Make Powder's quality floor external to the agent running a local command. The
repository already declares MIT in Cargo metadata and now carries a root
license file; the remaining work is to make the Rust gate run on every PR and
to keep release-intelligence automation from being the only GitHub workflow.

## Oracle
- [x] GitHub Actions runs `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` on pull requests.
- [x] A deliberately failing test or formatting error produces a red PR check in a disposable branch.
- [x] The Landmark release workflow remains intact and does not replace the Rust gate.
- [x] The root `LICENSE` remains MIT for Misty Step LLC and package metadata stays consistent.
- [x] Branch protection or an equivalent repository rule is documented or enabled so the gate is not honor-system only.

## Children
- Add the CI workflow.
- Run a red-check proof against an injected failure and then remove the failure.
- Record the expected PR gate in README or AGENTS if the repo contract changes.
- Keep release-intelligence integration as a separate workflow concern.

## Evidence
- Disposable red proof: PR #6 (`proof: intentional rustfmt failure`) closed
  after `Rust CI / fmt-clippy-test` failed on the injected formatting error.
- Branch protection: `main` requires strict status check `fmt-clippy-test` with
  admin enforcement enabled; force pushes and branch deletion are disabled.
