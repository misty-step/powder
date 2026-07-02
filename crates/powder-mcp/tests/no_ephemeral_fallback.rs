use std::process::{Command, Stdio};

/// There used to be a third, unconfigured mode here: an in-memory `Board`
/// that silently accepted claims/completions and evaporated on process
/// exit. An agent believed its work persisted; nothing did. Prove instead
/// that running the binary with neither `POWDER_API_BASE_URL` nor
/// `POWDER_DB_PATH` set fails loudly, rather than falling back to it.
#[test]
fn refuses_to_start_without_a_persistence_mode() {
    let binary = env!("CARGO_BIN_EXE_powder-mcp");

    let output = Command::new(binary)
        .env_remove("POWDER_API_BASE_URL")
        .env_remove("POWDER_DB_PATH")
        .env_remove("POWDER_BACKLOG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn powder-mcp");

    assert!(
        !output.status.success(),
        "must exit non-zero with no persistence mode configured"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("POWDER_API_BASE_URL") && stderr.contains("POWDER_DB_PATH"),
        "error must name both valid modes: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "must not emit any JSON-RPC output"
    );
}
