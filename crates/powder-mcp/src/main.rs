use std::io::{self, BufRead, Write};

use powder_core::Board;
use powder_shell::{load_backlog_dir, unix_now};
use powder_store::Store;
use serde_json::Value;

fn main() {
    if let Ok(db_path) = std::env::var("POWDER_DB_PATH") {
        match run_persistent(&db_path) {
            Ok(()) => return,
            Err(err) => {
                eprintln!("powder-mcp: persistent mode failed: {err}");
                std::process::exit(1);
            }
        }
    }

    let mut board = Board::default();
    if let Ok(path) = std::env::var("POWDER_BACKLOG_DIR") {
        match load_backlog_dir(path, unix_now()) {
            Ok(cards) => {
                board.import_cards(cards);
            }
            Err(err) => eprintln!("powder-mcp: could not load backlog: {err}"),
        }
    }

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

        if let Some(response) = powder_mcp::handle_json_rpc(&mut board, &request, unix_now()) {
            if let Ok(line) = serde_json::to_string(&response) {
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            }
        }
    }
}

fn run_persistent(db_path: &str) -> Result<(), Box<dyn std::error::Error>> {
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

        if let Some(response) = powder_mcp::handle_json_rpc_store(&mut store, &request, unix_now())
        {
            if let Ok(line) = serde_json::to_string(&response) {
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            }
        }
    }
    Ok(())
}
