//! Embeds the checkout's git commit at compile time so `powder version` can
//! prove which commit a given binary was built from. This is the concrete
//! signal a campaign lane needs to catch a stale `~/.cargo/bin/powder`
//! before it starts a lane and hits `missing --db` on commands API mode has
//! long since covered (powder-924's root cause: nobody could tell the
//! installed binary predated a feature without reading its source).

use std::process::Command;

fn main() {
    let sha = git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git_output(&["status", "--porcelain"])
        .map(|out| !out.is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=POWDER_CLI_GIT_SHA={sha}");
    println!("cargo:rustc-env=POWDER_CLI_GIT_DIRTY={dirty}");

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
