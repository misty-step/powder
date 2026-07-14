use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("powder-server lives two levels below repo root")
        .to_path_buf()
}

/// powder-doctor-socket-smoke: `test/powder-remote-doctor.sh` exercised
/// `bin/powder-remote-doctor.sh`'s failure-classification tree (config
/// missing, endpoint drift, service outage, credential bootstrap) but
/// nothing ran it -- neither a cargo target nor a CI step -- so it could
/// silently break with no signal. It already had: the doctor's fail-closed
/// behavior when `POWDER_EXPECTED_API_BASE_URL`/`POWDER_SANCTUM_ROOT_URL`
/// changed from a baked-in default (powder-ci-leak-gate) to a required
/// caller-supplied value went untested by anything that runs, and a bug
/// (identical drift-test fixture and success-test fixture) shipped
/// unnoticed. This wires it in the same way `deploy_contract.rs` already
/// wires `test/bin/entrypoint_test.sh` in: shell out, assert success.
#[test]
fn remote_doctor_classifies_failures_without_exposing_credentials() {
    let root = repo_root();
    let script = root.join("test/powder-remote-doctor.sh");
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .output()
        .expect("run test/powder-remote-doctor.sh");

    assert!(
        output.status.success(),
        "powder-remote-doctor test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
