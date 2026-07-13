use powder_mcp::eval_harness::{run_eval, McpCommand};

#[test]
fn tool_use_eval_runs_all_scenarios_over_stdio() {
    let report = run_eval(McpCommand::binary(env!("CARGO_BIN_EXE_powder-mcp")));
    assert!(
        report.all_passed(),
        "tool-use eval failures: {:?}\n{}",
        report.failures(),
        report.table()
    );
    assert_eq!(report.scenarios.len(), 3);
}
