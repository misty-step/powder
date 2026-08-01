#![forbid(unsafe_code)]

use powder_core::{Authority, Board, Card, CardId, CardStatus, Priority, ReadyQuery, RunId};
use powder_shell::{
    load_backlog_dir, load_backlog_dir_for_repo, load_github_issues_file, unix_now, ShellError,
};
use powder_store::{ApiKeyScope, CardFilter, Store, StoreError};

pub const COMMANDS: &[&str] = &[
    "init-db",
    "key-create",
    "key-list",
    "key-revoke",
    "import",
    "import-repo",
    "import-github-issues",
    "create-card",
    "update-relations",
    "list-ready",
    "list-cards",
    "claim",
    "release-claim",
    "renew-claim",
    "heartbeat",
    "get-card",
    "get-run",
    "list-awaiting-input",
    "answer-input",
    "update-status",
    "add-link",
    "add-comment",
    "request-input",
    "complete-card",
];

pub fn run(args: &[String]) -> Result<String, ShellError> {
    match args {
        [] => Ok(help()),
        [command] if command == "help" || command == "--help" || command == "-h" => Ok(help()),
        [command, rest @ ..] if command == "init-db" => init_db(rest),
        [command, rest @ ..] if command == "key-create" => key_create(rest),
        [command, rest @ ..] if command == "key-list" => key_list(rest),
        [command, rest @ ..] if command == "key-revoke" => key_revoke(rest),
        [command, rest @ ..] if command == "import" => import(rest),
        [command, rest @ ..] if command == "import-repo" => import_repo(rest),
        [command, rest @ ..] if command == "import-github-issues" => import_github_issues(rest),
        [command, rest @ ..] if command == "create-card" => create_card(rest),
        [command, rest @ ..] if command == "update-relations" => update_relations(rest),
        [command, rest @ ..] if command == "list-ready" => list_ready(rest),
        [command, rest @ ..] if command == "list-cards" => list_cards(rest),
        [command, rest @ ..] if command == "claim" => claim(rest),
        [command, rest @ ..] if command == "release-claim" => release_claim(rest),
        [command, rest @ ..] if command == "renew-claim" => renew_claim(rest),
        [command, rest @ ..] if command == "heartbeat" => heartbeat(rest),
        [command, rest @ ..] if command == "get-card" => get_card(rest),
        [command, rest @ ..] if command == "get-run" => get_run(rest),
        [command, rest @ ..] if command == "list-awaiting-input" => list_awaiting_input(rest),
        [command, rest @ ..] if command == "answer-input" => answer_input(rest),
        [command, rest @ ..] if command == "update-status" => update_status(rest),
        [command, rest @ ..] if command == "add-link" => add_link(rest),
        [command, rest @ ..] if command == "add-comment" => add_comment(rest),
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
    help.push_str("  powder key-create --db ./data/powder.db --name codex --scope agent\n");
    help.push_str("  powder key-list --db ./data/powder.db\n");
    help.push_str("  powder key-revoke key-id --db ./data/powder.db\n");
    help.push_str("  powder import backlog.d --db ./data/powder.db\n");
    help.push_str(
        "  powder import-repo ../bitterblossom/backlog.d --repo misty-step/bitterblossom --db ./data/powder.db\n",
    );
    help.push_str(
        "  gh issue list --json number,title,body,labels,state,url --repo misty-step/bitterblossom > issues.json\n",
    );
    help.push_str(
        "  powder import-github-issues issues.json --repo misty-step/bitterblossom --db ./data/powder.db\n",
    );
    help.push_str("  powder list-ready --db ./data/powder.db --limit 10\n");
    help.push_str(
        "  powder list-cards --db ./data/powder.db --status blocked --repo misty-step/example\n",
    );
    help.push_str(
        "  powder update-relations 001 --db ./data/powder.db --related 002,003 --blocks 004 --blocked-by 000\n",
    );
    help.push_str("  powder claim 001 --db ./data/powder.db --agent codex\n");
    help.push_str("  powder heartbeat 001 --db ./data/powder.db --run run-id\n");
    help.push_str("  powder renew-claim 001 --db ./data/powder.db --run run-id --ttl 3600\n");
    help.push_str("  powder release-claim 001 --db ./data/powder.db --run run-id\n");
    help.push_str("  powder get-card 001 --db ./data/powder.db\n");
    help.push_str("  powder list-awaiting-input --db ./data/powder.db\n");
    help.push_str(
        "  powder answer-input run-id --db ./data/powder.db --actor operator --answer approved\n",
    );
    help.push_str("  powder update-status 001 --db ./data/powder.db --status running\n");
    help.push_str(
        "  powder add-comment 001 --db ./data/powder.db --author operator --body \"looks good\"\n",
    );
    help.push_str(
        "  powder complete-card 001 --db ./data/powder.db [--proof https://example.test/proof]\n",
    );
    help.push_str(
        "  powder update-status 001 --db ./data/powder.db --status running --actor codex\n\n",
    );
    help.push_str(
        "authority:\n  add --actor <name> to audit status, relation, and completion changes. \
         Claim impersonation and lease mutations (release/renew/heartbeat/request-input) still \
         check the caller against the claim holder unless --admin is supplied. Omitting --actor \
         keeps direct-DB-access trust and records unchecked audit events.\n\n",
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

fn key_list(args: &[String]) -> Result<String, ShellError> {
    let store = open_store(required_flag(args, "--db")?)?;
    let keys = store.list_api_keys().map_err(store_err)?;
    let mut out = String::new();
    for key in keys {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            key.id,
            key.name,
            key.scope.as_str(),
            key.created_at,
            key.revoked_at
                .map(|at| at.to_string())
                .unwrap_or_else(|| "active".to_string())
        ));
    }
    Ok(out)
}

fn key_revoke(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let key_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("key-revoke requires a key id".to_string()))?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    store.revoke_api_key(key_id, now).map_err(store_err)?;
    Ok(format!("revoked\t{key_id}\n"))
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

    match (dry_run, flag_value(args, "--db")) {
        (true, None) => {
            out.push_str(&format!("dry-run\t{}\n", cards.len()));
        }
        (true, Some(db)) => {
            let store = open_store(db)?;
            let outcome = store.preview_import(&cards).map_err(store_err)?;
            out.push_str(&format!("dry-run\t{}\n", outcome_line(&outcome)));
        }
        (false, _) => {
            let mut store = open_store(required_flag(args, "--db")?)?;
            let outcome = store.import_cards(cards.clone()).map_err(store_err)?;
            out.push_str(&format!("imported\t{}\n", outcome_line(&outcome)));
        }
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

fn outcome_line(outcome: &powder_store::ImportOutcome) -> String {
    format!(
        "total={}\tcreated={}\tupdated={}\tpreserved={}\tunchanged={}",
        outcome.total(),
        outcome.created,
        outcome.updated,
        outcome.preserved,
        outcome.unchanged
    )
}

/// Import one Factory repo's backlog.d into a shared instance database: card
/// ids are namespaced `{repo-slug}-{original-id}` and tagged with `--repo`
/// so cards from independently numbered repos (every repo's backlog.d
/// starts its own `001-*.md`) can coexist without id collisions. Run once
/// per repo to migrate the fleet's backlog into one Powder instance.
fn import_repo(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let path = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("import-repo requires a backlog.d path".to_string()))?;
    let repo = required_flag(args, "--repo")?;
    let cards = load_backlog_dir_for_repo(path, repo, now)?;
    let mut out = String::new();

    if has_flag(args, "--dry-run") {
        let store = open_store(required_flag(args, "--db")?)?;
        let outcome = store.preview_import(&cards).map_err(store_err)?;
        out.push_str(&format!("dry-run\t{}\n", outcome_line(&outcome)));
    } else {
        let mut store = open_store(required_flag(args, "--db")?)?;
        let outcome = store.import_cards(cards.clone()).map_err(store_err)?;
        out.push_str(&format!("imported\t{}\n", outcome_line(&outcome)));
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

/// Import a GitHub repo's issues from a local JSON file (the shape produced
/// by `gh issue list --json number,title,body,labels,state,url`). Powder
/// never talks to the GitHub API itself; fetching is the operator's own
/// step, this only maps and imports what's already on disk.
fn import_github_issues(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let path = positional(args).first().copied().ok_or_else(|| {
        ShellError::Invalid("import-github-issues requires a JSON file path".to_string())
    })?;
    let repo = required_flag(args, "--repo")?;
    let cards = load_github_issues_file(path, repo, now)?;
    let mut out = String::new();

    if has_flag(args, "--dry-run") {
        let store = open_store(required_flag(args, "--db")?)?;
        let outcome = store.preview_import(&cards).map_err(store_err)?;
        out.push_str(&format!("dry-run\t{}\n", outcome_line(&outcome)));
    } else {
        let mut store = open_store(required_flag(args, "--db")?)?;
        let outcome = store.import_cards(cards.clone()).map_err(store_err)?;
        out.push_str(&format!("imported\t{}\n", outcome_line(&outcome)));
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
    // No fabricated acceptance: an omitted --acceptance means empty, not a
    // placeholder oracle that would falsely make the card look claimable
    // ("ready is a query, not vibes", VISION.md). An explicit --status is
    // still honored regardless -- status is a label, is_ready_at is the
    // independent gate -- but the *default* status must reflect whether a
    // real oracle exists.
    let acceptance: Vec<String> = flag_value(args, "--acceptance")
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    let status = flag_value(args, "--status")
        .and_then(CardStatus::parse)
        .unwrap_or(if acceptance.is_empty() {
            CardStatus::Backlog
        } else {
            CardStatus::Ready
        });
    let priority = flag_value(args, "--priority")
        .and_then(Priority::parse)
        .unwrap_or_default();
    let mut store = open_store(required_flag(args, "--db")?)?;
    let mut card = Card::new(CardId::new(id).map_err(ShellError::from)?, title, body)
        .map_err(ShellError::from)?
        .with_status(status)
        .with_priority(priority)
        .with_acceptance(acceptance)
        .with_created_at(now);
    card.related = card_ids_flag(args, "--related")?;
    card.blocks = card_ids_flag(args, "--blocks")?;
    card.blocked_by = card_ids_flag(args, "--blocked-by")?;
    let card = store.upsert_card(card).map_err(store_err)?;
    store
        .record_card_event(
            &card.id,
            "create",
            &authority(args).actor_label(),
            "created card",
            now,
        )
        .map_err(store_err)?;
    Ok(format!(
        "created\t{}\t{}\t{}\n",
        card.id,
        card.priority.as_str(),
        card.status.as_str()
    ))
}

fn update_relations(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-relations")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let card = store
        .update_relations(
            &card_id,
            card_ids_flag(args, "--related")?,
            card_ids_flag(args, "--blocks")?,
            card_ids_flag(args, "--blocked-by")?,
            now,
            &authority(args),
        )
        .map_err(store_err)?;
    Ok(format!("relations\t{}\n", card.id))
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

/// Enumerate cards by status/repo, not just ready-eligible ones -- `blocked`,
/// `review`, and `done` cards are otherwise invisible without opening the
/// database file directly.
fn list_cards(args: &[String]) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let store = open_store(required_flag(args, "--db")?)?;
    let status = flag_value(args, "--status")
        .map(|raw| {
            CardStatus::parse(raw)
                .ok_or_else(|| ShellError::Invalid(format!("invalid status: {raw}")))
        })
        .transpose()?;
    let filter = CardFilter {
        status,
        repo: flag_value(args, "--repo").map(str::to_string),
    };
    let cards = store.list_cards(&filter, limit).map_err(store_err)?;

    let mut out = String::new();
    for card in cards {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            card.id,
            card.priority.as_str(),
            card.status.as_str(),
            card.title
        ));
    }
    if out.is_empty() {
        out.push_str("no-cards\n");
    }
    Ok(out)
}

fn claim(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "claim")?;
    let agent = required_flag(args, "--agent")?;
    let ttl_seconds = optional_ttl(args)?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let claim = store
        .claim_card(&card_id, agent, now, ttl_seconds, &authority(args))
        .map_err(store_err)?;
    Ok(format!(
        "claimed\t{}\t{}\t{}\n",
        claim.card_id, claim.run_id, claim.expires_at
    ))
}

fn release_claim(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "release-claim")?;
    let run_id = required_run_flag(args)?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let claim = store
        .release_claim(&card_id, &run_id, now, &authority(args))
        .map_err(store_err)?;
    Ok(format!("released\t{}\t{}\n", claim.card_id, claim.run_id))
}

fn renew_claim(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "renew-claim")?;
    let run_id = required_run_flag(args)?;
    let ttl_seconds = optional_ttl(args)?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let claim = store
        .renew_claim(&card_id, &run_id, now, ttl_seconds, &authority(args))
        .map_err(store_err)?;
    Ok(format!(
        "renewed\t{}\t{}\t{}\n",
        claim.card_id, claim.run_id, claim.expires_at
    ))
}

fn heartbeat(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "heartbeat")?;
    let run_id = required_run_flag(args)?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let claim = store
        .heartbeat_claim(&card_id, &run_id, now, &authority(args))
        .map_err(store_err)?;
    Ok(format!(
        "heartbeat\t{}\t{}\t{}\n",
        claim.card_id, claim.run_id, claim.expires_at
    ))
}

fn get_card(args: &[String]) -> Result<String, ShellError> {
    let card_id = positional_card_id(args, "get-card")?;
    let store = open_store(required_flag(args, "--db")?)?;
    let detail = store
        .get_card_detail(&card_id)
        .map_err(store_err)?
        .ok_or_else(|| ShellError::NotFound(format!("card not found: {card_id}")))?;
    to_pretty_json(&detail)
}

fn get_run(args: &[String]) -> Result<String, ShellError> {
    let run_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("get-run requires a run id".to_string()))
        .and_then(|id| RunId::new(id).map_err(ShellError::from))?;
    let store = open_store(required_flag(args, "--db")?)?;
    let detail = store
        .get_run_detail(&run_id)
        .map_err(store_err)?
        .ok_or_else(|| ShellError::NotFound(format!("run not found: {run_id}")))?;
    to_pretty_json(&detail)
}

fn list_awaiting_input(args: &[String]) -> Result<String, ShellError> {
    let store = open_store(required_flag(args, "--db")?)?;
    let awaiting = store
        .list_awaiting_input(parse_limit(args).unwrap_or(20))
        .map_err(store_err)?;
    to_pretty_json(&serde_json::json!({ "awaiting": awaiting }))
}

fn answer_input(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let run_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("answer-input requires a run id".to_string()))
        .and_then(|id| RunId::new(id).map_err(ShellError::from))?;
    let actor = required_flag(args, "--actor")?;
    let answer = required_flag(args, "--answer")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let run = store
        .answer_input(&run_id, actor, answer, now, &authority(args))
        .map_err(store_err)?;
    Ok(format!("answered-input\t{}\t{}\n", run.id, run.card_id))
}

fn update_status(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-status")?;
    let status = flag_value(args, "--status")
        .and_then(CardStatus::parse)
        .ok_or_else(|| ShellError::Invalid("update-status requires --status".to_string()))?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let card = store
        .update_status(&card_id, status, now, &authority(args))
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

fn add_comment(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "add-comment")?;
    let author = required_flag(args, "--author")?;
    let body = required_flag(args, "--body")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let comment = store
        .add_comment(&card_id, author, body, now)
        .map_err(store_err)?;
    Ok(format!(
        "comment\t{}\t{}\t{}\n",
        comment.card_id, comment.author, comment.body
    ))
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
        .request_input(&run_id, question, now, &authority(args))
        .map_err(store_err)?;
    Ok(format!("awaiting-input\t{}\t{}\n", run.id, run.card_id))
}

fn complete_card(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "complete-card")?;
    let proof = flag_value(args, "--proof");
    let mut store = open_store(required_flag(args, "--db")?)?;
    let card = store
        .complete_card(&card_id, proof, now, &authority(args))
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

fn required_run_flag(args: &[String]) -> Result<RunId, ShellError> {
    required_flag(args, "--run").and_then(|id| RunId::new(id).map_err(ShellError::from))
}

fn card_ids_flag(args: &[String], flag: &'static str) -> Result<Vec<CardId>, ShellError> {
    flag_value(args, flag)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| CardId::new(value).map_err(ShellError::from))
        .collect()
}

/// Build the `Authority` a mutation is checked against from `--actor` (and
/// `--admin`). Omitting `--actor` preserves prior CLI behavior exactly: a
/// direct-DB-access operator is trusted and no ownership check runs.
fn authority(args: &[String]) -> Authority {
    match flag_value(args, "--actor") {
        Some(name) => Authority::actor(name, has_flag(args, "--admin")),
        None => Authority::unchecked(),
    }
}

fn optional_ttl(args: &[String]) -> Result<u64, ShellError> {
    flag_value(args, "--ttl")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| ShellError::Invalid(format!("invalid --ttl: {value}")))
        })
        .transpose()
        .map(|ttl| ttl.unwrap_or(3600))
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
    !matches!(flag, "--dry-run" | "--show-secret" | "--admin")
}

fn store_err(err: StoreError) -> ShellError {
    match err {
        StoreError::Domain(domain_err) => ShellError::from(domain_err),
        other => ShellError::Store(other.to_string()),
    }
}

fn to_pretty_json(value: &impl serde::Serialize) -> Result<String, ShellError> {
    serde_json::to_string_pretty(value)
        .map(|json| format!("{json}\n"))
        .map_err(|err| ShellError::Store(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_names_the_instance_workflow() {
        assert!(COMMANDS.contains(&"init-db"));
        assert!(COMMANDS.contains(&"key-list"));
        assert!(COMMANDS.contains(&"key-revoke"));
        assert!(COMMANDS.contains(&"import"));
        assert!(COMMANDS.contains(&"import-repo"));
        assert!(COMMANDS.contains(&"import-github-issues"));
        assert!(COMMANDS.contains(&"list-ready"));
        assert!(COMMANDS.contains(&"list-cards"));
        assert!(COMMANDS.contains(&"update-relations"));
        assert!(COMMANDS.contains(&"claim"));
        assert!(COMMANDS.contains(&"release-claim"));
        assert!(COMMANDS.contains(&"renew-claim"));
        assert!(COMMANDS.contains(&"heartbeat"));
        assert!(COMMANDS.contains(&"get-card"));
        assert!(COMMANDS.contains(&"get-run"));
        assert!(COMMANDS.contains(&"list-awaiting-input"));
        assert!(COMMANDS.contains(&"answer-input"));
        assert!(COMMANDS.contains(&"add-comment"));
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
    fn cli_rejects_invalid_ttl_values() {
        let claim_err = run(&args([
            "claim",
            "ttl-test",
            "--db",
            "/tmp/not-opened.db",
            "--agent",
            "codex",
            "--ttl",
            "not-a-number",
        ]))
        .unwrap_err();
        assert!(matches!(
            claim_err,
            ShellError::Invalid(message) if message == "invalid --ttl: not-a-number"
        ));

        let renew_err = run(&args([
            "renew-claim",
            "ttl-test",
            "--db",
            "/tmp/not-opened.db",
            "--run",
            "run-1",
            "--ttl",
            "not-a-number",
        ]))
        .unwrap_err();
        assert!(matches!(
            renew_err,
            ShellError::Invalid(message) if message == "invalid --ttl: not-a-number"
        ));
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
        let heartbeat = run(&args([
            "heartbeat",
            "cli-test",
            "--db",
            &db,
            "--run",
            &run_id,
        ]))
        .unwrap();
        assert!(heartbeat.contains("heartbeat\tcli-test"));
        let renewed = run(&args([
            "renew-claim",
            "cli-test",
            "--db",
            &db,
            "--run",
            &run_id,
            "--ttl",
            "3600",
        ]))
        .unwrap();
        assert!(renewed.contains("renewed\tcli-test"));
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

    #[test]
    fn cli_reimport_over_a_claimed_card_preserves_the_claim() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!("powder-cli-import-{nanos}.db"));
        let db = db.to_string_lossy().to_string();
        let backlog_dir = std::env::temp_dir().join(format!("powder-cli-import-backlog-{nanos}"));
        std::fs::create_dir_all(&backlog_dir).unwrap();
        let ticket_path = backlog_dir.join("001-reimport-test.md");
        let ticket = "# Reimport test\n\nPriority: P0 | Status: ready\n\n## Goal\nProve reimport safety.\n\n## Oracle\n- [ ] reimport preserves an active claim\n";
        std::fs::write(&ticket_path, ticket).unwrap();
        let backlog_dir = backlog_dir.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();

        let first_import = run(&args(["import", &backlog_dir, "--db", &db])).unwrap();
        assert!(first_import
            .contains("imported\ttotal=1\tcreated=1\tupdated=0\tpreserved=0\tunchanged=0"));

        run(&args(["claim", "001", "--db", &db, "--agent", "codex"])).unwrap();
        run(&args([
            "update-status",
            "001",
            "--db",
            &db,
            "--status",
            "running",
        ]))
        .unwrap();

        // re-importing the exact same, unedited backlog.d file must not
        // clobber the active claim/status.
        let second_import = run(&args(["import", &backlog_dir, "--db", &db])).unwrap();
        assert!(second_import
            .contains("imported\ttotal=1\tcreated=0\tupdated=0\tpreserved=1\tunchanged=0"));

        let card = run(&args(["get-card", "001", "--db", &db])).unwrap();
        assert!(card.contains("\"status\": \"running\""));
        assert!(card.contains("\"agent\": \"codex\""));

        // a dry-run against the same --db reports what would happen without
        // mutating anything.
        let dry_run = run(&args(["import", &backlog_dir, "--db", &db, "--dry-run"])).unwrap();
        assert!(
            dry_run.contains("dry-run\ttotal=1\tcreated=0\tupdated=0\tpreserved=1\tunchanged=0")
        );
        let card_after_dry_run = run(&args(["get-card", "001", "--db", &db])).unwrap();
        assert_eq!(
            card, card_after_dry_run,
            "dry-run must not mutate the store"
        );
    }

    #[test]
    fn cli_import_repo_namespaces_ids_so_two_repos_never_collide() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!("powder-cli-import-repo-{nanos}.db"));
        let db = db.to_string_lossy().to_string();

        let repo_a = std::env::temp_dir().join(format!("powder-cli-repo-a-{nanos}"));
        std::fs::create_dir_all(&repo_a).unwrap();
        std::fs::write(
            repo_a.join("001-first.md"),
            "# Repo A ticket one\n\nPriority: P0 | Status: ready\n\n## Goal\nA.\n\n## Oracle\n- [ ] a\n",
        )
        .unwrap();

        let repo_b = std::env::temp_dir().join(format!("powder-cli-repo-b-{nanos}"));
        std::fs::create_dir_all(&repo_b).unwrap();
        std::fs::write(
            repo_b.join("001-first.md"),
            "# Repo B ticket one\n\nPriority: P0 | Status: ready\n\n## Goal\nB.\n\n## Oracle\n- [ ] b\n",
        )
        .unwrap();

        run(&args(["init-db", "--db", &db])).unwrap();
        let import_a = run(&args([
            "import-repo",
            repo_a.to_str().unwrap(),
            "--repo",
            "misty-step/repo-a",
            "--db",
            &db,
        ]))
        .unwrap();
        assert!(import_a.contains("repo-a-001"));
        let import_b = run(&args([
            "import-repo",
            repo_b.to_str().unwrap(),
            "--repo",
            "misty-step/repo-b",
            "--db",
            &db,
        ]))
        .unwrap();
        assert!(import_b.contains("repo-b-001"));

        // both survive independently: no id collision even though both
        // repos number their tickets starting from 001.
        let card_a = run(&args(["get-card", "repo-a-001", "--db", &db])).unwrap();
        assert!(card_a.contains("Repo A ticket one"));
        let card_b = run(&args(["get-card", "repo-b-001", "--db", &db])).unwrap();
        assert!(card_b.contains("Repo B ticket one"));
    }

    #[test]
    fn create_card_with_no_acceptance_never_fabricates_one_and_defaults_to_backlog() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-no-fabricated-acceptance-{}.db",
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
            "no-acceptance",
            "--title",
            "Untriaged",
        ]))
        .unwrap();

        let card = run(&args(["get-card", "no-acceptance", "--db", &db])).unwrap();
        assert!(
            card.contains("\"acceptance\": []"),
            "an omitted --acceptance must never fabricate a placeholder oracle: {card}"
        );
        assert!(
            card.contains("\"status\": \"backlog\""),
            "empty acceptance must not default to a claimable status: {card}"
        );
        let ready = run(&args(["list-ready", "--db", &db])).unwrap();
        assert!(!ready.contains("no-acceptance"));

        // an explicit --status is still honored even with empty acceptance
        // (status is a label; is_ready_at is the independent gate).
        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "forced-ready",
            "--title",
            "Explicitly forced ready",
            "--status",
            "ready",
        ]))
        .unwrap();
        let forced = run(&args(["get-card", "forced-ready", "--db", &db])).unwrap();
        assert!(forced.contains("\"status\": \"ready\""));
        assert!(forced.contains("\"acceptance\": []"));

        // real acceptance still makes the default status ready, matching
        // prior behavior for callers who do provide a criterion.
        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "with-acceptance",
            "--title",
            "Has a real oracle",
            "--acceptance",
            "the tests pass",
        ]))
        .unwrap();
        let with_acceptance = run(&args(["get-card", "with-acceptance", "--db", &db])).unwrap();
        assert!(with_acceptance.contains("\"acceptance\": [\n      \"the tests pass\"\n    ]"));
        assert!(with_acceptance.contains("\"status\": \"ready\""));
    }

    #[test]
    fn cli_list_cards_filters_by_status_and_repo() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-list-cards-{}.db",
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
            "blocked-1",
            "--title",
            "Blocked ticket",
            "--status",
            "blocked",
        ]))
        .unwrap();
        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "ready-1",
            "--title",
            "Ready ticket",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
        ]))
        .unwrap();

        let all = run(&args(["list-cards", "--db", &db])).unwrap();
        assert!(all.contains("blocked-1"));
        assert!(all.contains("ready-1"));

        let blocked_only = run(&args(["list-cards", "--db", &db, "--status", "blocked"])).unwrap();
        assert!(blocked_only.contains("blocked-1"));
        assert!(!blocked_only.contains("ready-1"));

        let err = run(&args([
            "list-cards",
            "--db",
            &db,
            "--status",
            "not-a-status",
        ]))
        .unwrap_err();
        assert!(matches!(err, ShellError::Invalid(_)));
    }

    #[test]
    fn cli_add_comment_appears_in_get_card() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-add-comment-{}.db",
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
            "commented",
            "--title",
            "Has a comment",
        ]))
        .unwrap();

        let output = run(&args([
            "add-comment",
            "commented",
            "--db",
            &db,
            "--author",
            "operator",
            "--body",
            "looks good",
        ]))
        .unwrap();
        assert!(output.contains("commented"));
        assert!(output.contains("operator"));
        assert!(output.contains("looks good"));

        let card = run(&args(["get-card", "commented", "--db", &db])).unwrap();
        assert!(card.contains("\"author\": \"operator\""));
        assert!(card.contains("\"body\": \"looks good\""));
    }

    #[test]
    fn cli_import_github_issues_maps_open_and_closed_issues_and_survives_reimport() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!("powder-cli-gh-issues-{nanos}.db"));
        let db = db.to_string_lossy().to_string();
        let issues_file = std::env::temp_dir().join(format!("powder-cli-gh-issues-{nanos}.json"));
        std::fs::write(
            &issues_file,
            r#"[
              {"number": 1, "title": "Open issue", "body": "needs work", "labels": [{"name": "bug"}], "state": "OPEN", "url": "https://github.com/misty-step/example/issues/1"},
              {"number": 2, "title": "Closed issue", "body": "done", "labels": [], "state": "CLOSED", "url": "https://github.com/misty-step/example/issues/2"}
            ]"#,
        )
        .unwrap();
        let issues_file = issues_file.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        let imported = run(&args([
            "import-github-issues",
            &issues_file,
            "--repo",
            "misty-step/example",
            "--db",
            &db,
        ]))
        .unwrap();
        assert!(imported.contains("imported\ttotal=2\tcreated=2"));

        let open_card = run(&args(["get-card", "example-1", "--db", &db])).unwrap();
        assert!(open_card.contains("\"status\": \"backlog\""));
        assert!(
            open_card.contains("\"acceptance\": []"),
            "no fabricated acceptance"
        );

        let closed_card = run(&args(["get-card", "example-2", "--db", &db])).unwrap();
        assert!(closed_card.contains("\"status\": \"done\""));

        // status alone doesn't make it claimable: moving Backlog -> Ready is
        // a legal transition, but with no real acceptance criteria the card
        // still never shows up as ready ("ready is a query, not vibes").
        run(&args([
            "update-status",
            "example-1",
            "--db",
            &db,
            "--status",
            "ready",
        ]))
        .unwrap();
        let ready = run(&args(["list-ready", "--db", &db])).unwrap();
        assert!(
            !ready.contains("example-1"),
            "no acceptance criteria means never claimable, regardless of status: {ready}"
        );

        // reimport-safety carries through the GitHub path too: the closed
        // issue is a terminal (Done) card, so its *status* is protected from
        // a stale reimport the same way a claim is -- even if the issue is
        // reopened on GitHub afterward (content like title/body still
        // refreshes on reimport, same as backlog.d; only status/claim are
        // frozen, per Card::merge_reimport -- reopening can't silently
        // revert a card Powder already recorded as done).
        std::fs::write(
            &issues_file,
            r#"[
              {"number": 1, "title": "Open issue", "body": "needs work", "labels": [{"name": "bug"}], "state": "OPEN", "url": "https://github.com/misty-step/example/issues/1"},
              {"number": 2, "title": "Reopened issue", "body": "done", "labels": [], "state": "OPEN", "url": "https://github.com/misty-step/example/issues/2"}
            ]"#,
        )
        .unwrap();
        let reimport = run(&args([
            "import-github-issues",
            &issues_file,
            "--repo",
            "misty-step/example",
            "--db",
            &db,
        ]))
        .unwrap();
        assert!(
            reimport.contains("preserved=1"),
            "the terminal (closed) issue must be reported preserved: {reimport}"
        );

        let closed_card_after = run(&args(["get-card", "example-2", "--db", &db])).unwrap();
        assert!(
            closed_card_after.contains("\"status\": \"done\""),
            "reopening on GitHub must not revert Powder's done status: {closed_card_after}"
        );
        assert!(
            closed_card_after.contains("\"title\": \"Reopened issue\""),
            "content still refreshes on reimport, same as backlog.d: {closed_card_after}"
        );
    }

    #[test]
    fn cli_key_lifecycle_lists_and_revokes() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-key-lifecycle-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        let created = run(&args([
            "key-create",
            "--db",
            &db,
            "--name",
            "codex",
            "--scope",
            "agent",
            "--show-secret",
        ]))
        .unwrap();
        let key_id = created.split('\t').nth(1).expect("key id").to_owned();

        let listed = run(&args(["key-list", "--db", &db])).unwrap();
        assert!(listed.contains(&key_id));
        assert!(listed.contains("codex"));
        assert!(listed.contains("active"));
        assert!(
            !listed.contains("sk_powder_"),
            "key-list must never print raw secrets"
        );

        let revoked = run(&args(["key-revoke", &key_id, "--db", &db])).unwrap();
        assert_eq!(revoked, format!("revoked\t{key_id}\n"));

        let listed_after = run(&args(["key-list", "--db", &db])).unwrap();
        let revoked_line = listed_after
            .lines()
            .find(|line| line.contains(&key_id))
            .expect("revoked key still listed");
        assert!(
            !revoked_line.ends_with("active"),
            "revoked key must not report active: {revoked_line}"
        );

        // idempotent: revoking again does not error.
        run(&args(["key-revoke", &key_id, "--db", &db])).unwrap();

        let missing = run(&args(["key-revoke", "key-does-not-exist", "--db", &db])).unwrap_err();
        assert!(matches!(missing, ShellError::NotFound(_)));
    }

    #[test]
    fn cli_release_claim_makes_the_card_ready_again() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-release-{}.db",
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
            "release-test",
            "--title",
            "Release test",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
        ]))
        .unwrap();
        let claimed = run(&args([
            "claim",
            "release-test",
            "--db",
            &db,
            "--agent",
            "codex",
            "--ttl",
            "3600",
        ]))
        .unwrap();
        let run_id = claimed.split('\t').nth(2).expect("run id").to_owned();
        let released = run(&args([
            "release-claim",
            "release-test",
            "--db",
            &db,
            "--run",
            &run_id,
        ]))
        .unwrap();

        assert!(released.contains("released\trelease-test"));
        let ready = run(&args(["list-ready", "--db", &db])).unwrap();
        assert!(ready.contains("release-test"));
    }

    #[test]
    fn cli_actor_flag_enforces_claim_holder_like_http_and_mcp() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-holder-{}.db",
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
            "holder-test",
            "--title",
            "Holder test",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
        ]))
        .unwrap();
        run(&args([
            "claim",
            "holder-test",
            "--db",
            &db,
            "--agent",
            "codex",
            "--actor",
            "codex",
        ]))
        .unwrap();

        let status = run(&args([
            "update-status",
            "holder-test",
            "--db",
            &db,
            "--status",
            "running",
            "--actor",
            "intruder",
        ]))
        .unwrap();
        assert!(status.contains("status\tholder-test\trunning"));

        let completed = run(&args([
            "complete-card",
            "holder-test",
            "--db",
            &db,
            "--actor",
            "intruder",
        ]))
        .unwrap();
        assert!(completed.contains("completed\tholder-test\tdone"));
        let card = run(&args(["get-card", "holder-test", "--db", &db])).unwrap();
        assert!(card.contains("\"actor\": \"intruder\""));
        assert!(card.contains("running -> done"));
    }

    #[test]
    fn cli_updates_relations_and_get_card_shows_them() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-relations-{}.db",
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
            "relation-test",
            "--title",
            "Relation test",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
            "--related",
            "peer-a,peer-b",
            "--blocks",
            "child-a",
            "--blocked-by",
            "parent-a",
        ]))
        .unwrap();
        let updated = run(&args([
            "update-relations",
            "relation-test",
            "--db",
            &db,
            "--related",
            "peer-c",
            "--blocks",
            "",
            "--blocked-by",
            "parent-a,parent-b",
            "--actor",
            "operator",
        ]))
        .unwrap();
        assert!(updated.contains("relations\trelation-test"));

        let card = run(&args(["get-card", "relation-test", "--db", &db])).unwrap();
        assert!(card.contains("\"related\": [\n      \"peer-c\""));
        assert!(card.contains("\"blocked_by\": [\n      \"parent-a\""));
        assert!(card.contains("\"actor\": \"operator\""));
    }

    #[test]
    fn cli_exposes_answer_loop_details() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-answer-{}.db",
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
            "answer-test",
            "--title",
            "Answer test",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
        ]))
        .unwrap();
        let claimed = run(&args([
            "claim",
            "answer-test",
            "--db",
            &db,
            "--agent",
            "codex",
        ]))
        .unwrap();
        let run_id = claimed.split('\t').nth(2).expect("run id").to_owned();
        run(&args([
            "update-status",
            "answer-test",
            "--db",
            &db,
            "--status",
            "running",
        ]))
        .unwrap();
        run(&args([
            "request-input",
            &run_id,
            "--db",
            &db,
            "--question",
            "Approve?\nwith\ttab",
        ]))
        .unwrap();

        let awaiting = run(&args(["list-awaiting-input", "--db", &db])).unwrap();
        assert!(awaiting.contains("\"awaiting\""));
        assert!(awaiting.contains("answer-test"));
        assert!(awaiting.contains("Approve?\\nwith\\ttab"));

        let card = run(&args(["get-card", "answer-test", "--db", &db])).unwrap();
        assert!(card.contains("\"activities\""));
        assert!(card.contains("Approve?\\nwith\\ttab"));

        let answered = run(&args([
            "answer-input",
            &run_id,
            "--db",
            &db,
            "--actor",
            "operator",
            "--answer",
            "Approved",
        ]))
        .unwrap();
        assert!(answered.contains("answered-input"));

        let run_detail = run(&args(["get-run", &run_id, "--db", &db])).unwrap();
        assert!(run_detail.contains("\"state\": \"active\""));
        assert!(run_detail.contains("operator"));
        assert!(run_detail.contains("Approved"));
    }

    fn args<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(ToOwned::to_owned).collect()
    }
}
