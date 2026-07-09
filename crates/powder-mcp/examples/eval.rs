use powder_mcp::eval_harness::{run_eval, McpCommand};

fn main() {
    let report = run_eval(McpCommand::from_env_or_default());
    print!("{}", report.table());
    if !report.all_passed() {
        for failure in report.failures() {
            eprintln!("failure: {failure}");
        }
        std::process::exit(1);
    }
}
