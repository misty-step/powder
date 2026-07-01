#![forbid(unsafe_code)]

use powder_core::{Board, Card, CardId, CardStatus, Priority, ReadyQuery, RunId};
use powder_shell::{load_backlog_dir, unix_now, ShellError};
use powder_store::{ApiKeyScope, Store, StoreError};

pub const COMMANDS: &[&str] = &[
    "init-db",
    "key-create",
    "import",
    "create-card",
    "list-ready",
    "claim",
    "update-status",
    "add-link",
    "request-input",
    "complete-card",
];

pub fn run(args: &[String]) -> Result<String, ShellError> {
    match args {
        [] => Ok(help()),
        [command] if command == "help" || command == "--help" || command == "-h" => Ok(help()),
        [command, rest @ ..] if command == "init-db" => init_db(rest),
        [command, rest @ ..] if command == "key-create" => key_create(rest),
        [command, rest @ ..] if command == "import" => import(rest),
        [command, rest @ ..] if command == "create-card" => create_card(rest),
        [command, rest @ ..] if command == "list-ready" => list_ready(rest),
        [command, rest @ ..] if command == "claim" => claim(rest),
        [command, rest @ ..] if command == "update-status" => update_status(rest),
        [command, rest @ ..] if command == "add-link" => add_link(rest),
        [command, rest @ ..] if command == "request-input" => request_input(rest),
        [command, rest @ ..] if command == "complete-card" => complete_card(rest),
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
    help.push_str("  powder init-db --db ./data/powder.db --show-secret\n");
    help.push_str("  powder import backlog.d --db ./data/powder.db\n");
    help.push_str("  powder list-ready --db ./data/powder.db --limit 10\n");
    help.push_str("  powder claim 001 --db ./data/powder.db --agent codex\n");
    help.push_str("  powder update-status 001 --db ./data/powder.db --status running\n");
    help.push_str(
        "  powder complete-card 001 --db ./data/powder.db --proof https://example.test/proof\n\n",
    );
    help.push_str("api contract:\n");
    help.push_str(&powder_api::route_summary());
    help
}

fn init_db(args: &[String]) -> Result<String, ShellError> {
    let show_secret = has_flag(args, "--show-secret");
    let now = unix_now();
    let mut store = open_store(required_flag(args, "--db")?)?;
    let seed = store.apply_initial_seed(now).map_err(store_err)?;

    match seed {
        Some(key) if show_secret => Ok(format!(
            "bootstrap-key\t{}\t{}\t{}\n",
            key.id,
            key.scope.as_str(),
            key.raw_key
        )),
        Some(key) => Ok(format!(
            "bootstrap-key\t{}\t{}\tredacted\n",
            key.id,
            key.scope.as_str()
        )),
        None => Ok("already-initialized\n".to_string()),
    }
}

fn key_create(args: &[String]) -> Result<String, ShellError> {
    let show_secret = has_flag(args, "--show-secret");
    let name = flag_value(args, "--name").unwrap_or("agent");
    let scope = flag_value(args, "--scope")
        .and_then(ApiKeyScope::parse)
        .unwrap_or(ApiKeyScope::Agent);
    let now = unix_now();
    let mut store = open_store(required_flag(args, "--db")?)?;
    let key = store.create_api_key(name, scope, now).map_err(store_err)?;

    if show_secret {
        Ok(format!(
            "api-key\t{}\t{}\t{}\n",
            key.id,
            key.scope.as_str(),
            key.raw_key
        ))
    } else {
        Ok(format!(
            "api-key\t{}\t{}\tredacted\n",
            key.id,
            key.scope.as_str()
        ))
    }
}

fn import(args: &[String]) -> Result<String, ShellError> {
    let dry_run = has_flag(args, "--dry-run");
    let now = unix_now();
    let path = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("import requires a backlog.d path".to_string()))?;
    let cards = load_backlog_dir(path, now)?;
    let mut out = String::new();

    if dry_run {
        out.push_str(&format!("dry-run\t{}\n", cards.len()));
    } else {
        let mut store = open_store(required_flag(args, "--db")?)?;
        let count = store.import_cards(cards.clone()).map_err(store_err)?;
        out.push_str(&format!(
            "imported\t{count}\t{}\n",
            required_flag(args, "--db")?
        ));
    }

    for card in cards {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            card.id,
            card.priority.as_str(),
            card.status.as_str(),
            card.title
        ));
    }
    Ok(out)
}

fn create_card(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let id = required_flag(args, "--id")?;
    let title = required_flag(args, "--title")?;
    let body = flag_value(args, "--body").unwrap_or_default();
    let status = flag_value(args, "--status")
        .and_then(CardStatus::parse)
        .unwrap_or(CardStatus::Ready);
    let priority = flag_value(args, "--priority")
        .and_then(Priority::parse)
        .unwrap_or_default();
    let acceptance = flag_value(args, "--acceptance").unwrap_or("proof exists");
    let mut store = open_store(required_flag(args, "--db")?)?;
    let card = Card::new(CardId::new(id).map_err(ShellError::from)?, title, body)
        .map_err(ShellError::from)?
        .with_status(status)
        .with_priority(priority)
        .with_acceptance([acceptance.to_string()])
        .with_created_at(now);
    let card = store.upsert_card(card).map_err(store_err)?;
    Ok(format!(
        "created\t{}\t{}\t{}\n",
        card.id,
        card.priority.as_str(),
        card.status.as_str()
    ))
}

fn list_ready(args: &[String]) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let now = unix_now();
    let ready = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        store
            .list_ready(ReadyQuery::new(now, limit))
            .map_err(store_err)?
    } else {
        let path = positional(args).first().copied().ok_or_else(|| {
            ShellError::Invalid("list-ready requires --db or a backlog.d path".to_string())
        })?;
        let cards = load_backlog_dir(path, now)?;
        let mut board = Board::default();
        board.import_cards(cards);
        board.list_ready(ReadyQuery::new(now, limit))
    };

    let mut out = String::new();
    for card in ready {
        out.push_str(&format!(
            "{}\t{}\t{}\n",
            card.id,
            card.priority.as_str(),
            card.title
        ));
    }
    if out.is_empty() {
        out.push_str("no-ready-cards\n");
    }
    Ok(out)
}

fn claim(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "claim")?;
    let agent = required_flag(args, "--agent")?;
    let ttl_seconds = flag_value(args, "--ttl")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3600);
    let mut store = open_store(required_flag(args, "--db")?)?;
    let claim = store
        .claim_card(&card_id, agent, now, ttl_seconds)
        .map_err(store_err)?;
    Ok(format!(
        "claimed\t{}\t{}\t{}\n",
        claim.card_id, claim.run_id, claim.expires_at
    ))
}

fn update_status(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-status")?;
    let status = flag_value(args, "--status")
        .and_then(CardStatus::parse)
        .ok_or_else(|| ShellError::Invalid("update-status requires --status".to_string()))?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let card = store
        .update_status(&card_id, status, now)
        .map_err(store_err)?;
    Ok(format!("status\t{}\t{}\n", card.id, card.status.as_str()))
}

fn add_link(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "add-link")?;
    let label = required_flag(args, "--label")?;
    let url = required_flag(args, "--url")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let link = store
        .add_link(&card_id, label, url, now)
        .map_err(store_err)?;
    Ok(format!("link\t{}\t{}\n", link.card_id, link.id))
}

fn request_input(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let run_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("request-input requires a run id".to_string()))
        .and_then(|id| RunId::new(id).map_err(ShellError::from))?;
    let question = required_flag(args, "--question")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let run = store
        .request_input(&run_id, question, now)
        .map_err(store_err)?;
    Ok(format!("awaiting-input\t{}\t{}\n", run.id, run.card_id))
}

fn complete_card(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "complete-card")?;
    let proof = required_flag(args, "--proof")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let card = store
        .complete_card(&card_id, proof, now)
        .map_err(store_err)?;
    Ok(format!(
        "completed\t{}\t{}\n",
        card.id,
        card.status.as_str()
    ))
}

fn open_store(path: &str) -> Result<Store, ShellError> {
    let mut store = Store::open(path).map_err(store_err)?;
    store.migrate().map_err(store_err)?;
    Ok(store)
}

fn positional_card_id(args: &[String], command: &str) -> Result<CardId, ShellError> {
    positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid(format!("{command} requires a card id")))
        .and_then(|id| CardId::new(id).map_err(ShellError::from))
}

fn parse_limit(args: &[String]) -> Option<usize> {
    flag_value(args, "--limit").and_then(|value| value.parse::<usize>().ok())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn required_flag<'a>(args: &'a [String], flag: &'static str) -> Result<&'a str, ShellError> {
    flag_value(args, flag).ok_or_else(|| ShellError::Invalid(format!("missing {flag}")))
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg.starts_with("--") {
            index += if flag_takes_value(arg) { 2 } else { 1 };
        } else {
            values.push(arg.as_str());
            index += 1;
        }
    }
    values
}

fn flag_takes_value(flag: &str) -> bool {
    !matches!(flag, "--dry-run" | "--show-secret")
}

fn store_err(err: StoreError) -> ShellError {
    ShellError::Store(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_names_the_instance_workflow() {
        assert!(COMMANDS.contains(&"init-db"));
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

    #[test]
    fn positional_args_skip_flags_with_values() {
        assert_eq!(
            positional(&[
                "001".to_string(),
                "--db".to_string(),
                "powder.db".to_string(),
                "--show-secret".to_string(),
            ]),
            vec!["001"]
        );
    }

    #[test]
    fn cli_runs_persisted_card_lifecycle() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "cli-test",
            "--title",
            "CLI test",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
        ]))
        .unwrap();
        let ready = run(&args(["list-ready", "--db", &db])).unwrap();
        assert!(ready.contains("cli-test"));

        let claimed = run(&args([
            "claim", "cli-test", "--db", &db, "--agent", "codex", "--ttl", "3600",
        ]))
        .unwrap();
        let run_id = claimed.split('\t').nth(2).expect("run id").to_owned();
        run(&args([
            "update-status",
            "cli-test",
            "--db",
            &db,
            "--status",
            "running",
        ]))
        .unwrap();
        run(&args([
            "add-link",
            "cli-test",
            "--db",
            &db,
            "--label",
            "proof",
            "--url",
            "https://example.test/proof",
        ]))
        .unwrap();
        run(&args([
            "request-input",
            &run_id,
            "--db",
            &db,
            "--question",
            "Approve completion?",
        ]))
        .unwrap();
        let completed = run(&args([
            "complete-card",
            "cli-test",
            "--db",
            &db,
            "--proof",
            "https://example.test/proof",
        ]))
        .unwrap();

        assert!(completed.contains("completed\tcli-test\tdone"));
    }

    fn args<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(ToOwned::to_owned).collect()
    }
}
