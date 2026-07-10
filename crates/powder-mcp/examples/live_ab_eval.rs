//! Live-model A/B pilot entry point (powder-mcp-live-model-ab). Never run
//! from `cargo test`; invoke explicitly:
//!
//! ```text
//! export OPENROUTER_API_KEY=...           # or POWDER_EVAL_MODEL_API_KEY
//! export POWDER_EVAL_OLD_BINARY=/path/to/pre-epic/powder-mcp  # optional
//! cargo run --example live_ab_eval -p powder-mcp
//! ```
//!
//! See `crates/powder-mcp/src/live_eval.rs` module docs for how to build the
//! pre-epic binary and for the full env var surface
//! (`POWDER_EVAL_MODEL_CLAUDE`, `POWDER_EVAL_MODEL_OPEN`, `POWDER_EVAL_TRIALS`,
//! `POWDER_EVAL_MAX_TOOL_CALLS`). Writes the same markdown table it prints to
//! stdout at `AB_RESULTS.md` in the current directory.

use powder_mcp::live_eval::{run_pilot, LiveEvalConfig};

fn main() {
    let Some(config) = LiveEvalConfig::from_env() else {
        println!(
            "live-model A/B pilot skipped: set POWDER_EVAL_MODEL_API_KEY or \
             OPENROUTER_API_KEY to run it. No live calls were made."
        );
        return;
    };

    eprintln!(
        "running live-model A/B pilot: {} scenarios x {} models x {} surfaces x {} trials",
        3,
        config.models.len(),
        if config.old_binary.is_some() { 2 } else { 1 },
        config.trials
    );

    let report = run_pilot(&config);
    let table = report.table();
    print!("{table}");

    if let Err(err) = std::fs::write("AB_RESULTS.md", &table) {
        eprintln!("warning: could not write AB_RESULTS.md: {err}");
    }
}
