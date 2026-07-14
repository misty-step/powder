//! Embeds the checkout's git commit at compile time, mirroring
//! `crates/powder-cli/build.rs` and `crates/powder-server/build.rs` exactly,
//! two crates over. `powder-mcp` had no version signal at all before
//! powder-workstation-cli-convergence: a long-lived MCP subprocess could be
//! running a build several merges behind with nothing short of reading its
//! source to tell -- the same blind spot that let a stale `~/.cargo/bin/
//! powder` silently drop repeated `--acceptance` criteria on a live card.

use std::process::Command;

fn main() {
    let sha = git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git_output(&["status", "--porcelain"])
        .map(|out| !out.is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=POWDER_MCP_GIT_SHA={sha}");
    println!("cargo:rustc-env=POWDER_MCP_GIT_DIRTY={dirty}");

    // Re-run when HEAD moves (commit, checkout, merge) so a `cargo install`
    // right after pulling actually picks up the new SHA instead of reusing
    // a cached build artifact stamped with the old one.
    println!("cargo:rerun-if-changed=../../.git/logs/HEAD");
    println!("cargo:rerun-if-changed=build.rs");
}

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_string())
}
