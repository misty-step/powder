use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("powder-server lives two levels below repo root")
        .to_path_buf()
}

/// powder-workstation-cli-convergence: `scripts/install-workstation.sh` is
/// the repo-owned convergence path for the operator's workstation
/// `~/.cargo/bin/powder{,-mcp,-server}` -- wired in here the same way
/// `doctor_contract.rs` already wires `test/powder-remote-doctor.sh` in, so
/// it runs on every `cargo test --workspace` instead of only when someone
/// remembers to invoke it by hand. `test/install-workstation.sh` fakes
/// `git`/`cargo`/`curl` (a real build here would take minutes and the
/// release-tarball path needs network -- neither is affordable per PR); it
/// never touches this checkout's real git state or `~/.cargo/bin`.
#[test]
fn workstation_install_script_installs_verifies_and_falls_back_correctly() {
    let root = repo_root();
    let script = root.join("test/install-workstation.sh");
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .output()
        .expect("run test/install-workstation.sh");

    assert!(
        output.status.success(),
        "install-workstation test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
