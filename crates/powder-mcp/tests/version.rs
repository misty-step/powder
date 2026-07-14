use std::process::{Command, Stdio};

/// powder-workstation-cli-convergence: `powder-mcp` previously had no
/// version signal at all -- a stale, long-lived MCP subprocess was
/// indistinguishable from a freshly built one short of reading its source.
/// This mirrors `powder-cli`'s `cli_version_reports_the_build_commit` test:
/// the report must name the exact commit this build compiled from, and must
/// work with no `POWDER_API_BASE_URL`/`POWDER_DB_PATH` configured at all
/// (it exits before either is read).
#[test]
fn mcp_version_reports_the_build_commit_without_any_persistence_mode_configured() {
    let binary = env!("CARGO_BIN_EXE_powder-mcp");

    for flag in ["version", "--version", "-v"] {
        let output = Command::new(binary)
            .arg(flag)
            .env_remove("POWDER_API_BASE_URL")
            .env_remove("POWDER_DB_PATH")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("spawn powder-mcp");

        assert!(
            output.status.success(),
            "powder-mcp {flag} must exit 0 with no persistence mode configured"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with("powder-mcp 0.1.0 (git "),
            "unexpected version output for {flag}: {stdout}"
        );
        assert!(
            !stdout.contains("(git )"),
            "must not embed an empty sha for {flag}: {stdout}"
        );
        assert!(
            output.stderr.is_empty(),
            "version must not warn about a missing persistence mode: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
