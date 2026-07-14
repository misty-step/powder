//! Embeds the checkout's git commit at compile time so `powder-server` can
//! log which commit a running instance was actually built from (powder-epic-
//! truthful-ops): a `curl /healthz` or a log line otherwise cannot tell you
//! whether the deployed binary matches a given merged PR -- see
//! `docs/production-deploy.md`'s "A merged PR ... changes nothing in
//! production until the steps above happen" note. Mirrors
//! `crates/powder-cli/build.rs` exactly, one crate over.

use std::process::Command;

fn main() {
    let sha = git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git_output(&["status", "--porcelain"])
        .map(|out| !out.is_empty())
        .unwrap_or(false);

    println!("cargo:rustc-env=POWDER_SERVER_GIT_SHA={sha}");
    println!("cargo:rustc-env=POWDER_SERVER_GIT_DIRTY={dirty}");

    // Re-run when HEAD moves (commit, checkout, merge) so a rebuild right
    // after pulling actually picks up the new SHA instead of reusing a
    // cached build artifact stamped with the old one.
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
