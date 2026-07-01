#![forbid(unsafe_code)]

use powder_core::{Board, ReadyQuery};
use powder_shell::{load_backlog_dir, unix_now, ShellError};

pub const COMMANDS: &[&str] = &[
    "import",
    "list-ready",
    "claim",
    "update-status",
    "request-input",
    "complete-card",
];

pub fn run(args: &[String]) -> Result<String, ShellError> {
    match args {
        [] => Ok(help()),
        [command] if command == "help" || command == "--help" || command == "-h" => Ok(help()),
        [command, path, rest @ ..] if command == "import" => import(path, rest),
        [command, path, rest @ ..] if command == "list-ready" => list_ready(path, rest),
        [command, ..] => Err(ShellError::Invalid(format!("unknown command: {command}"))),
    }
}

pub fn help() -> String {
    let mut help = String::from("powder - agent-first work board\n\ncommands:\n");
    for command in COMMANDS {
        help.push_str("  ");
        help.push_str(command);
        help.push('\n');
    }
    help.push_str("\nexamples:\n");
    help.push_str("  powder import backlog.d --dry-run\n");
    help.push_str("  powder list-ready backlog.d --limit 10\n\n");
    help.push_str("api contract:\n");
    help.push_str(&powder_api::route_summary());
    help
}

fn import(path: &str, args: &[String]) -> Result<String, ShellError> {
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let now = unix_now();
    let cards = load_backlog_dir(path, now)?;
    let mut out = String::new();

    if dry_run {
        out.push_str(&format!("dry-run: parsed {} cards\n", cards.len()));
    } else {
        let mut board = Board::default();
        let count = board.import_cards(cards.clone());
        out.push_str(&format!("imported {count} cards into in-memory board\n"));
    }

    for card in cards {
        out.push_str(&format!(
            "{}\t{:?}\t{:?}\t{}\n",
            card.id, card.priority, card.status, card.title
        ));
    }
    Ok(out)
}

fn list_ready(path: &str, args: &[String]) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let now = unix_now();
    let cards = load_backlog_dir(path, now)?;
    let mut board = Board::default();
    board.import_cards(cards);

    let ready = board.list_ready(ReadyQuery::new(now, limit));
    let mut out = String::new();
    for card in ready {
        out.push_str(&format!(
            "{}\t{:?}\t{}\n",
            card.id, card.priority, card.title
        ));
    }
    if out.is_empty() {
        out.push_str("no ready cards\n");
    }
    Ok(out)
}

fn parse_limit(args: &[String]) -> Option<usize> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--limit" {
            return iter.next().and_then(|value| value.parse::<usize>().ok());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_names_the_v0_workflow() {
        assert!(COMMANDS.contains(&"list-ready"));
        assert!(COMMANDS.contains(&"claim"));
        assert!(COMMANDS.contains(&"request-input"));
        assert!(COMMANDS.contains(&"complete-card"));
    }

    #[test]
    fn parses_limit_flag() {
        assert_eq!(
            parse_limit(&["--limit".to_string(), "7".to_string()]),
            Some(7)
        );
    }
}
