use std::io::{self, BufRead, Write};

use powder_mcp::{RemoteClient, Toolset};
use powder_shell::{load_backlog_dir, unix_now};
use powder_store::Store;
use serde_json::Value;

fn main() {
    let toolset = match Toolset::from_env() {
        Ok(toolset) => toolset,
        Err(err) => {
            eprintln!("powder-mcp: {err}");
            std::process::exit(1);
        }
    };

    if let Ok(base_url) = std::env::var("POWDER_API_BASE_URL") {
        run_remote(base_url, std::env::var("POWDER_API_KEY").ok(), toolset);
        return;
    }

    if let Ok(db_path) = std::env::var("POWDER_DB_PATH") {
        match run_persistent(&db_path, toolset) {
            Ok(()) => return,
            Err(err) => {
                eprintln!("powder-mcp: persistent mode failed: {err}");
                std::process::exit(1);
            }
        }
    }

    // No ephemeral in-memory fallback: one used to exist here, silently
    // accepting claims/completions into a Board that evaporated on process
    // exit -- an agent believed its work persisted and nothing did. Fail
    // loudly instead; there is no safe mode that isn't one of the two above.
    eprintln!(
        "powder-mcp: set POWDER_API_BASE_URL (remote mode, against a deployed instance) or \
         POWDER_DB_PATH (persistent mode, against a local SQLite file). There is no in-memory \
         fallback: claims and completions must not silently evaporate on process exit."
    );
    std::process::exit(1);
}

fn run_persistent(db_path: &str, toolset: Toolset) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = Store::open(db_path)?;
    store.migrate()?;
    if let Ok(path) = std::env::var("POWDER_BACKLOG_DIR") {
        let cards = load_backlog_dir(path, unix_now())?;
        store.import_cards(cards)?;
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("powder-mcp: invalid json: {err}");
                continue;
            }
        };

        if let Some(response) = powder_mcp::handle_json_rpc_store_with_toolset(
            &mut store,
            &request,
            unix_now(),
            toolset,
        ) {
            if let Ok(line) = serde_json::to_string(&response) {
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            }
        }
    }
    Ok(())
}

/// Work against a deployed Powder instance's HTTP API instead of a local
/// SQLite file, so MCP tool calls carry the identity, lease ownership, and
/// admin authority of `POWDER_API_KEY` all the way to the deployed instance.
fn run_remote(base_url: String, api_key: Option<String>, toolset: Toolset) {
    let client = RemoteClient::new(base_url, api_key);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("powder-mcp: invalid json: {err}");
                continue;
            }
        };

        if let Some(response) =
            powder_mcp::handle_json_rpc_remote_with_toolset(&client, &request, toolset)
        {
            if let Ok(line) = serde_json::to_string(&response) {
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            }
        }
    }
}
