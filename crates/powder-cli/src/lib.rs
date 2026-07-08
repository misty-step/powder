#![forbid(unsafe_code)]

use powder_api::{urlencode, RemoteClient};
use powder_core::{Authority, Board, Card, CardId, CardStatus, Priority, ReadyQuery, RunId};
use powder_shell::{
    load_backlog_dir, load_backlog_dir_for_repo, load_github_issues_file, unix_now, ShellError,
};
use powder_store::{
    ApiKeyScope, CardFilter, RepositoryTier, RepositoryUpsert, RepositoryVisibility, Store,
    StoreError,
};
use serde_json::{json, Value};

pub const COMMANDS: &[&str] = &[
    "version",
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
    "repository-list",
    "repository-get",
    "repository-upsert",
    "repository-merge-alias",
    "repository-delete",
    "claim",
    "release-claim",
    "renew-claim",
    "transfer-claim",
    "heartbeat",
    "get-card",
    "get-run",
    "list-awaiting-input",
    "answer-input",
    "update-status",
    "check-criterion",
    "add-link",
    "add-comment",
    "append-work-log",
    "request-input",
    "complete-card",
    "subscription-create",
    "subscription-list",
    "subscription-disable",
    "dead-letter-list",
    "event-tail",
];

#[derive(Debug, Clone, Default)]
struct RemoteEnv {
    base_url: Option<String>,
    api_key: Option<String>,
}

impl RemoteEnv {
    fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut env = Self::default();
        for (key, value) in pairs {
            let key = key.into();
            let value = value.into();
            if value.trim().is_empty() {
                continue;
            }
            match key.as_str() {
                "POWDER_API_BASE_URL" => env.base_url = Some(value.trim().to_string()),
                "POWDER_API_KEY" => env.api_key = Some(value.trim().to_string()),
                _ => {}
            }
        }
        env
    }

    fn client(&self) -> Option<RemoteClient> {
        self.base_url
            .as_ref()
            .map(|base_url| RemoteClient::new(base_url.clone(), self.api_key.clone()))
    }
}

pub fn run(args: &[String]) -> Result<String, ShellError> {
    let remote_env = RemoteEnv::from_pairs(std::env::vars());
    run_with_remote_env(args, &remote_env)
}

fn run_with_remote_env(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    match args {
        [] => Ok(help()),
        [command] if command == "help" || command == "--help" || command == "-h" => Ok(help()),
        [command] if command == "version" || command == "--version" || command == "-v" => {
            Ok(version())
        }
        [command, rest @ ..] if command == "init-db" => init_db(rest),
        [command, rest @ ..] if command == "key-create" => key_create(rest),
        [command, rest @ ..] if command == "key-list" => key_list(rest),
        [command, rest @ ..] if command == "key-revoke" => key_revoke(rest),
        [command, rest @ ..] if command == "import" => import(rest),
        [command, rest @ ..] if command == "import-repo" => import_repo(rest),
        [command, rest @ ..] if command == "import-github-issues" => import_github_issues(rest),
        [command, rest @ ..] if command == "create-card" => create_card(rest, remote_env),
        [command, rest @ ..] if command == "update-relations" => update_relations(rest),
        [command, rest @ ..] if command == "list-ready" => list_ready(rest, remote_env),
        [command, rest @ ..] if command == "list-cards" => list_cards(rest, remote_env),
        [command, rest @ ..] if command == "repository-list" => repository_list(rest),
        [command, rest @ ..] if command == "repository-get" => repository_get(rest),
        [command, rest @ ..] if command == "repository-upsert" => repository_upsert(rest),
        [command, rest @ ..] if command == "repository-merge-alias" => repository_merge_alias(rest),
        [command, rest @ ..] if command == "repository-delete" => repository_delete(rest),
        [command, rest @ ..] if command == "claim" => claim(rest, remote_env),
        [command, rest @ ..] if command == "release-claim" => release_claim(rest, remote_env),
        [command, rest @ ..] if command == "renew-claim" => renew_claim(rest, remote_env),
        [command, rest @ ..] if command == "transfer-claim" => transfer_claim(rest, remote_env),
        [command, rest @ ..] if command == "heartbeat" => heartbeat(rest, remote_env),
        [command, rest @ ..] if command == "get-card" => get_card(rest, remote_env),
        [command, rest @ ..] if command == "get-run" => get_run(rest),
        [command, rest @ ..] if command == "list-awaiting-input" => list_awaiting_input(rest),
        [command, rest @ ..] if command == "answer-input" => answer_input(rest),
        [command, rest @ ..] if command == "update-status" => update_status(rest, remote_env),
        [command, rest @ ..] if command == "check-criterion" => check_criterion(rest, remote_env),
        [command, rest @ ..] if command == "add-link" => add_link(rest, remote_env),
        [command, rest @ ..] if command == "add-comment" => add_comment(rest, remote_env),
        [command, rest @ ..] if command == "append-work-log" => append_work_log(rest, remote_env),
        [command, rest @ ..] if command == "request-input" => request_input(rest, remote_env),
        [command, rest @ ..] if command == "complete-card" => complete_card(rest, remote_env),
        [command, rest @ ..] if command == "subscription-create" => subscription_create(rest),
        [command, rest @ ..] if command == "subscription-list" => subscription_list(rest),
        [command, rest @ ..] if command == "subscription-disable" => subscription_disable(rest),
        [command, rest @ ..] if command == "dead-letter-list" => dead_letter_list(rest),
        [command, rest @ ..] if command == "event-tail" => event_tail(rest),
        [command, ..] => Err(ShellError::Invalid(format!("unknown command: {command}"))),
    }
}

/// Reports the installed binary's build provenance so a lane can catch a
/// stale `~/.cargo/bin/powder` (built from an old commit that predates a
/// command's API-mode support) before it starts a claim, instead of hitting
/// a bare `missing --db` on a command the checkout has long since covered.
/// Compare against `git -C <checkout> rev-parse --short=12 HEAD`; a mismatch
/// means `cargo install --path crates/powder-cli` is due.
pub fn version() -> String {
    let dirty = env!("POWDER_CLI_GIT_DIRTY") == "true";
    format!(
        "powder {} (git {}{})\n",
        env!("CARGO_PKG_VERSION"),
        env!("POWDER_CLI_GIT_SHA"),
        if dirty { ", dirty" } else { "" }
    )
}

pub fn help() -> String {
    let mut help = String::from("powder - agent-first work board\n\ncommands:\n");
    for command in COMMANDS {
        help.push_str("  ");
        help.push_str(command);
        help.push('\n');
    }
    help.push_str("\nexamples:\n");
    help.push_str(
        "  powder version   # confirm the installed binary's build against `git rev-parse --short=12 HEAD` before starting a lane\n",
    );
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
        "  powder create-card --db ./data/powder.db --id canary-001 --title \"Canary task\" --repo misty-step/canary [--proof-plan \"CI + PR\"]\n",
    );
    help.push_str(
        "  powder list-cards --db ./data/powder.db --status blocked --repo misty-step/example\n",
    );
    help.push_str("  powder repository-list --db ./data/powder.db --include-hidden\n");
    help.push_str(
        "  powder repository-upsert --db ./data/powder.db --name canary --aliases misty-step/canary,legacy-canary --visibility visible --tier active --import-provenance manual\n",
    );
    help.push_str(
        "  powder repository-merge-alias --db ./data/powder.db --alias misty-step/canary --into canary --actor operator\n",
    );
    help.push_str(
        "  powder update-relations 001 --db ./data/powder.db --related 002,003 --blocks 004 --blocked-by 000\n",
    );
    help.push_str("  powder claim 001 --db ./data/powder.db --agent codex\n");
    help.push_str("  powder heartbeat 001 --db ./data/powder.db --run run-id\n");
    help.push_str("  powder renew-claim 001 --db ./data/powder.db --run run-id --ttl 3600\n");
    help.push_str(
        "  powder transfer-claim 001 --db ./data/powder.db --run run-id --to-agent codex --ttl 3600\n",
    );
    help.push_str("  powder release-claim 001 --db ./data/powder.db --run run-id\n");
    help.push_str("  powder get-card 001 --db ./data/powder.db\n");
    help.push_str("  powder list-awaiting-input --db ./data/powder.db\n");
    help.push_str(
        "  powder answer-input run-id --db ./data/powder.db --actor operator --answer approved\n",
    );
    help.push_str("  powder update-status 001 --db ./data/powder.db --status running\n");
    help.push_str(
        "  powder check-criterion 001 --db ./data/powder.db --criterion 0 --actor operator [--unchecked]\n",
    );
    help.push_str(
        "  powder add-comment 001 --db ./data/powder.db --author operator --body \"looks good\"\n",
    );
    help.push_str(
        "  powder append-work-log 001 --db ./data/powder.db --agent codex --body \"tracing the claim expiry bug\" [--model claude-sonnet-5] [--reasoning high] [--harness \"Claude Code\"] [--run-id run-id]\n",
    );
    help.push_str(
        "  powder complete-card 001 --db ./data/powder.db [--proof https://example.test/proof]\n",
    );
    help.push_str(
        "  powder subscription-create --db ./data/powder.db --url http://127.0.0.1:9000/webhook --event-filter moved-to-ready,completed --show-secret\n",
    );
    help.push_str("  powder subscription-list --db ./data/powder.db\n");
    help.push_str("  powder subscription-disable sub-id --db ./data/powder.db\n");
    help.push_str("  powder dead-letter-list --db ./data/powder.db\n");
    help.push_str("  powder event-tail --db ./data/powder.db --after 0 --limit 20\n");
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
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            key.id,
            key.name,
            key.scope.as_str(),
            key.key_prefix,
            key.created_at,
            key.revoked_at
                .map(|at| at.to_string())
                .unwrap_or_else(|| "active".to_string()),
            key.last_used_at
                .map(|at| at.to_string())
                .unwrap_or_else(|| "never".to_string())
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
            let outcome = store
                .import_cards_with_events(cards.clone(), &authority(args).actor_label(), now)
                .map_err(store_err)?;
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
        let outcome = store
            .import_cards_with_events(cards.clone(), &authority(args).actor_label(), now)
            .map_err(store_err)?;
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
        let outcome = store
            .import_cards_with_events(cards.clone(), &authority(args).actor_label(), now)
            .map_err(store_err)?;
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

fn create_card(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let id = required_flag(args, "--id")?;
    let title = required_flag(args, "--title")?;
    let body = flag_value(args, "--body");
    // No fabricated acceptance: an omitted --acceptance means empty, not a
    // placeholder oracle that would falsely make the card look claimable
    // ("ready is a query, not vibes", VISION.md). An explicit --status is
    // still honored regardless -- status is a label, is_ready_at is the
    // independent gate -- but the *default* status must reflect whether a
    // real oracle exists.
    let acceptance: Vec<String> = flag_value(args, "--acceptance")
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    let proof_plan: Vec<String> = flag_value(args, "--proof-plan")
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

    let related = card_ids_flag(args, "--related")?;
    let blocks = card_ids_flag(args, "--blocks")?;
    let blocked_by = card_ids_flag(args, "--blocked-by")?;
    let repo = flag_value(args, "--repo").map(str::to_string);

    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let mut card = Card::new(
            CardId::new(id).map_err(ShellError::from)?,
            title,
            body.unwrap_or_default(),
        )
        .map_err(ShellError::from)?
        .with_status(status)
        .with_priority(priority)
        .with_acceptance(acceptance)
        .with_proof_plan(proof_plan.clone())
        .with_created_at(now);
        card.related = related;
        card.blocks = blocks;
        card.blocked_by = blocked_by;
        card.repo = repo;
        json!(store
            .create_card_with_events(card, &authority(args).actor_label(), now)
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        let mut payload = json!({
            "id": id,
            "title": title,
            "acceptance": acceptance,
            "status": status.as_str(),
            "priority": priority.as_str(),
            "related": card_id_values(&related),
            "blocks": card_id_values(&blocks),
            "blocked_by": card_id_values(&blocked_by),
        });
        if let Some(body) = body {
            payload["body"] = json!(body);
        }
        if !proof_plan.is_empty() {
            payload["proof_plan"] = json!(proof_plan);
        }
        if let Some(repo) = repo {
            payload["repo"] = json!(repo);
        }
        client.post("/api/v1/cards", payload).map_err(remote_err)?
    } else {
        return Err(missing_transport("create-card"));
    };

    Ok(format!(
        "created\t{}\t{}\t{}\n",
        json_string(&card, "id")?,
        json_priority(&card)?,
        json_string(&card, "status")?
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

fn list_ready(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let now = unix_now();
    let ready = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        json!(store
            .list_ready(ReadyQuery::new(now, limit))
            .map_err(store_err)?)
    } else if let Some(path) = positional(args).first().copied() {
        let cards = load_backlog_dir(path, now)?;
        let mut board = Board::default();
        board.import_cards(cards);
        json!(board.list_ready(ReadyQuery::new(now, limit)))
    } else if let Some(client) = remote_env.client() {
        client
            .get(&format!("/api/v1/cards/ready?limit={limit}"))
            .map_err(remote_err)?["cards"]
            .clone()
    } else {
        return Err(ShellError::Invalid(
            "list-ready requires --db, POWDER_API_BASE_URL, or a backlog.d path".to_string(),
        ));
    };

    let mut out = String::new();
    for card in json_array(&ready)? {
        out.push_str(&format!(
            "{}\t{}\t{}\n",
            json_string(card, "id")?,
            json_priority(card)?,
            json_string(card, "title")?
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
fn list_cards(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let status = flag_value(args, "--status")
        .map(|raw| {
            CardStatus::parse(raw)
                .ok_or_else(|| ShellError::Invalid(format!("invalid status: {raw}")))
        })
        .transpose()?;
    let repo = flag_value(args, "--repo").map(str::to_string);
    let cards = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        let filter = CardFilter {
            status,
            repo: repo.clone(),
        };
        json!(store.list_cards(&filter, limit).map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        let mut query = format!("limit={limit}");
        if let Some(status) = status {
            query.push_str(&format!("&status={}", status.as_str()));
        }
        if let Some(repo) = &repo {
            query.push_str(&format!("&repo={}", urlencode(repo)));
        }
        client
            .get(&format!("/api/v1/cards?{query}"))
            .map_err(remote_err)?["cards"]
            .clone()
    } else {
        return Err(missing_transport("list-cards"));
    };

    let mut out = String::new();
    for card in json_array(&cards)? {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            json_string(card, "id")?,
            json_priority(card)?,
            json_string(card, "status")?,
            json_string(card, "title")?
        ));
    }
    if out.is_empty() {
        out.push_str("no-cards\n");
    }
    Ok(out)
}

fn repository_list(args: &[String]) -> Result<String, ShellError> {
    let store = open_store(required_flag(args, "--db")?)?;
    let repositories = if has_flag(args, "--include-hidden") {
        store.list_repositories_with_hidden().map_err(store_err)?
    } else {
        store.list_repositories().map_err(store_err)?
    };
    to_pretty_json(&serde_json::json!({ "repositories": repositories }))
}

fn repository_get(args: &[String]) -> Result<String, ShellError> {
    let name = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("repository-get requires a name".to_string()))?;
    let store = open_store(required_flag(args, "--db")?)?;
    let repository = store
        .get_repository(name)
        .map_err(store_err)?
        .ok_or_else(|| ShellError::NotFound(format!("repository not found: {name}")))?;
    to_pretty_json(&repository)
}

fn repository_upsert(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let name = required_flag(args, "--name")?.to_string();
    let visibility = flag_value(args, "--visibility")
        .map(|raw| {
            RepositoryVisibility::parse(raw)
                .ok_or_else(|| ShellError::Invalid(format!("invalid --visibility: {raw}")))
        })
        .transpose()?;
    let tier = flag_value(args, "--tier")
        .map(|raw| {
            RepositoryTier::parse(raw)
                .ok_or_else(|| ShellError::Invalid(format!("invalid --tier: {raw}")))
        })
        .transpose()?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let repository = store
        .upsert_repository(
            RepositoryUpsert {
                name,
                aliases: aliases_flag(args),
                visibility,
                tier,
                import_provenance: flag_value(args, "--import-provenance").map(str::to_string),
            },
            now,
        )
        .map_err(store_err)?;
    to_pretty_json(&repository)
}

fn repository_merge_alias(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let alias = required_flag(args, "--alias")?;
    let target = required_flag(args, "--into")?;
    let actor = flag_value(args, "--actor").unwrap_or("operator");
    let mut store = open_store(required_flag(args, "--db")?)?;
    let outcome = store
        .merge_repository_alias(alias, target, actor, now)
        .map_err(store_err)?;
    to_pretty_json(&outcome)
}

fn repository_delete(args: &[String]) -> Result<String, ShellError> {
    let name = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("repository-delete requires a name".to_string()))?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    store.delete_repository(name).map_err(store_err)?;
    Ok(format!("deleted\t{name}\n"))
}

fn claim(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "claim")?;
    let agent = required_flag(args, "--agent")?;
    let ttl_seconds = optional_ttl(args)?;
    let claim = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        json!(store
            .claim_card(&card_id, agent, now, ttl_seconds, &authority(args))
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        client
            .post(
                &format!("/api/v1/cards/{card_id}/claim"),
                json!({"agent": agent, "ttl_seconds": ttl_seconds}),
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("claim"));
    };
    Ok(format!(
        "claimed\t{}\t{}\t{}\n",
        json_string(&claim, "card_id")?,
        json_string(&claim, "run_id")?,
        json_i64(&claim, "expires_at")?
    ))
}

fn release_claim(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "release-claim")?;
    let run_id = required_run_flag(args)?;
    let (released_card_id, released_run_id) = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let claim = store
            .release_claim(&card_id, &run_id, now, &authority(args))
            .map_err(store_err)?;
        (claim.card_id.to_string(), claim.run_id.to_string())
    } else if let Some(client) = remote_env.client() {
        let released = client
            .post(
                &format!("/api/v1/cards/{card_id}/release"),
                json!({"run_id": run_id.as_str()}),
            )
            .map_err(remote_err)?;
        (
            json_string(&released, "card_id")?,
            json_string(&released, "run_id")?,
        )
    } else {
        return Err(missing_transport("release-claim"));
    };
    Ok(format!("released\t{released_card_id}\t{released_run_id}\n"))
}

fn renew_claim(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "renew-claim")?;
    let run_id = required_run_flag(args)?;
    let ttl_seconds = optional_ttl(args)?;
    let (renewed_card_id, renewed_run_id, expires_at) = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let claim = store
            .renew_claim(&card_id, &run_id, now, ttl_seconds, &authority(args))
            .map_err(store_err)?;
        (
            claim.card_id.to_string(),
            claim.run_id.to_string(),
            claim.expires_at,
        )
    } else if let Some(client) = remote_env.client() {
        let renewed = client
            .post(
                &format!("/api/v1/cards/{card_id}/renew"),
                json!({"run_id": run_id.as_str(), "ttl_seconds": ttl_seconds}),
            )
            .map_err(remote_err)?;
        (
            json_string(&renewed, "card_id")?,
            json_string(&renewed, "run_id")?,
            json_i64(&renewed, "expires_at")?,
        )
    } else {
        return Err(missing_transport("renew-claim"));
    };
    Ok(format!(
        "renewed\t{renewed_card_id}\t{renewed_run_id}\t{expires_at}\n"
    ))
}

fn transfer_claim(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "transfer-claim")?;
    let run_id = required_run_flag(args)?;
    let to_agent = required_flag(args, "--to-agent")?;
    let ttl_seconds = optional_ttl(args)?;
    let (transferred_card_id, transferred_run_id, transferred_agent, expires_at) = if let Some(db) =
        flag_value(args, "--db")
    {
        let mut store = open_store(db)?;
        let claim = store
            .transfer_claim(
                &card_id,
                &run_id,
                to_agent,
                now,
                ttl_seconds,
                &authority(args),
            )
            .map_err(store_err)?;
        (
            claim.card_id.to_string(),
            claim.run_id.to_string(),
            claim.agent,
            claim.expires_at,
        )
    } else if let Some(client) = remote_env.client() {
        let transferred = client
                .post(
                    &format!("/api/v1/cards/{card_id}/transfer"),
                    json!({"run_id": run_id.as_str(), "to_agent": to_agent, "ttl_seconds": ttl_seconds}),
                )
                .map_err(remote_err)?;
        (
            json_string(&transferred, "card_id")?,
            json_string(&transferred, "run_id")?,
            json_string(&transferred, "agent")?,
            json_i64(&transferred, "expires_at")?,
        )
    } else {
        return Err(missing_transport("transfer-claim"));
    };
    Ok(format!(
        "transferred\t{transferred_card_id}\t{transferred_run_id}\t{transferred_agent}\t{expires_at}\n"
    ))
}

fn heartbeat(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "heartbeat")?;
    let run_id = required_run_flag(args)?;
    let (beat_card_id, beat_run_id, expires_at) = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let claim = store
            .heartbeat_claim(&card_id, &run_id, now, &authority(args))
            .map_err(store_err)?;
        (
            claim.card_id.to_string(),
            claim.run_id.to_string(),
            claim.expires_at,
        )
    } else if let Some(client) = remote_env.client() {
        let beat = client
            .post(
                &format!("/api/v1/cards/{card_id}/heartbeat"),
                json!({"run_id": run_id.as_str()}),
            )
            .map_err(remote_err)?;
        (
            json_string(&beat, "card_id")?,
            json_string(&beat, "run_id")?,
            json_i64(&beat, "expires_at")?,
        )
    } else {
        return Err(missing_transport("heartbeat"));
    };
    Ok(format!(
        "heartbeat\t{beat_card_id}\t{beat_run_id}\t{expires_at}\n"
    ))
}

fn get_card(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let card_id = positional_card_id(args, "get-card")?;
    if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        let detail = store
            .get_card_detail(&card_id)
            .map_err(store_err)?
            .ok_or_else(|| ShellError::NotFound(format!("card not found: {card_id}")))?;
        to_pretty_json(&detail)
    } else if let Some(client) = remote_env.client() {
        let detail = client
            .get(&format!("/api/v1/cards/{card_id}"))
            .map_err(remote_err)?;
        to_pretty_json(&detail)
    } else {
        Err(missing_transport("get-card"))
    }
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

fn update_status(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-status")?;
    let status = flag_value(args, "--status")
        .and_then(CardStatus::parse)
        .ok_or_else(|| ShellError::Invalid("update-status requires --status".to_string()))?;
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        json!(store
            .update_status(&card_id, status, now, &authority(args))
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        client
            .post(
                &format!("/api/v1/cards/{card_id}/status"),
                json!({"status": status.as_str()}),
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("update-status"));
    };
    Ok(format!(
        "status\t{}\t{}\n",
        json_string(&card, "id")?,
        json_string(&card, "status")?
    ))
}

fn check_criterion(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "check-criterion")?;
    let criterion = criterion_flag(args)?;
    let actor = required_flag(args, "--actor")?;
    let checked = !has_flag(args, "--unchecked");
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        json!(store
            .check_criterion(&card_id, criterion, actor, checked, now)
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        client
            .post(
                &format!("/api/v1/cards/{card_id}/criteria/check"),
                json!({"criterion": criterion, "actor": actor, "checked": checked}),
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("check-criterion"));
    };
    Ok(format!(
        "criterion\t{}\t{}\t{}\n",
        json_string(&card, "id")?,
        criterion,
        if checked { "checked" } else { "unchecked" }
    ))
}

fn add_link(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "add-link")?;
    let label = required_flag(args, "--label")?;
    let url = required_flag(args, "--url")?;
    let (link_card_id, link_id) = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let link = store
            .add_link(&card_id, label, url, now)
            .map_err(store_err)?;
        (link.card_id.to_string(), link.id.to_string())
    } else if let Some(client) = remote_env.client() {
        let link = client
            .post(
                &format!("/api/v1/cards/{card_id}/links"),
                json!({"label": label, "url": url}),
            )
            .map_err(remote_err)?;
        (json_string(&link, "card_id")?, json_string(&link, "id")?)
    } else {
        return Err(missing_transport("add-link"));
    };
    Ok(format!("link\t{link_card_id}\t{link_id}\n"))
}

fn add_comment(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "add-comment")?;
    let author = required_flag(args, "--author")?;
    let body = required_flag(args, "--body")?;
    let comment = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        json!(store
            .add_comment(&card_id, author, body, now)
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        client
            .post(
                &format!("/api/v1/cards/{card_id}/comments"),
                json!({"author": author, "body": body}),
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("add-comment"));
    };
    Ok(format!(
        "comment\t{}\t{}\t{}\n",
        json_string(&comment, "card_id")?,
        json_string(&comment, "author")?,
        json_string(&comment, "body")?
    ))
}

fn append_work_log(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "append-work-log")?;
    let agent = required_flag(args, "--agent")?;
    let body = required_flag(args, "--body")?;
    let model = flag_value(args, "--model");
    let reasoning = flag_value(args, "--reasoning");
    let harness = flag_value(args, "--harness");
    let run_id = flag_value(args, "--run-id");
    let attribution = powder_store::WorkLogAttribution {
        model,
        reasoning,
        harness,
        run_id,
    };
    let entry = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        json!(store
            .append_work_log(&card_id, agent, attribution, body, now)
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        client
            .post(
                &format!("/api/v1/cards/{card_id}/work-log"),
                json!({
                    "agent": agent,
                    "body": body,
                    "model": model,
                    "reasoning": reasoning,
                    "harness": harness,
                    "run_id": run_id,
                }),
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("append-work-log"));
    };
    Ok(format!(
        "work-log\t{}\t{}\t{}\n",
        json_string(&entry, "card_id")?,
        json_string(&entry, "agent")?,
        json_string(&entry, "body")?
    ))
}

fn request_input(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let run_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("request-input requires a run id".to_string()))
        .and_then(|id| RunId::new(id).map_err(ShellError::from))?;
    let question = required_flag(args, "--question")?;
    let (awaiting_run_id, awaiting_card_id) = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let run = store
            .request_input(&run_id, question, now, &authority(args))
            .map_err(store_err)?;
        (run.id.to_string(), run.card_id.to_string())
    } else if let Some(client) = remote_env.client() {
        let run = client
            .post(
                &format!("/api/v1/runs/{run_id}/input"),
                json!({"question": question}),
            )
            .map_err(remote_err)?;
        (json_string(&run, "id")?, json_string(&run, "card_id")?)
    } else {
        return Err(missing_transport("request-input"));
    };
    Ok(format!(
        "awaiting-input\t{awaiting_run_id}\t{awaiting_card_id}\n"
    ))
}

fn complete_card(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "complete-card")?;
    let proof = flag_value(args, "--proof");
    let criterion_proofs = criterion_proofs_flag(args)?;
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        json!(store
            .complete_card(&card_id, proof, criterion_proofs, now, &authority(args))
            .map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        let mut body = json!({});
        if let Some(proof) = proof {
            body["proof"] = json!(proof);
        }
        if !criterion_proofs.is_empty() {
            body["criterion_proofs"] = json!(criterion_proofs
                .iter()
                .map(|proof| json!({"criterion": proof.criterion, "url": proof.url}))
                .collect::<Vec<_>>());
        }
        client
            .post(&format!("/api/v1/cards/{card_id}/complete"), body)
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("complete-card"));
    };
    Ok(format!(
        "completed\t{}\t{}\n",
        json_string(&card, "id")?,
        json_string(&card, "status")?
    ))
}

fn subscription_create(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let url = required_flag(args, "--url")?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let created = store
        .create_event_subscription(url, event_filter_flag(args)?, now)
        .map_err(store_err)?;
    if has_flag(args, "--show-secret") {
        Ok(format!(
            "subscription\t{}\t{}\t{}\n",
            created.subscription.id, created.subscription.url, created.signing_secret
        ))
    } else {
        Ok(format!(
            "subscription\t{}\t{}\tredacted\n",
            created.subscription.id, created.subscription.url
        ))
    }
}

fn subscription_list(args: &[String]) -> Result<String, ShellError> {
    let store = open_store(required_flag(args, "--db")?)?;
    to_pretty_json(&serde_json::json!({
        "subscriptions": store.list_event_subscriptions().map_err(store_err)?
    }))
}

fn subscription_disable(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let subscription_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("subscription-disable requires an id".to_string()))?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    let subscription = store
        .disable_event_subscription(subscription_id, now)
        .map_err(store_err)?;
    Ok(format!(
        "disabled\t{}\t{}\n",
        subscription.id,
        subscription
            .disabled_at
            .map(|value| value.to_string())
            .unwrap_or_else(|| "active".to_string())
    ))
}

fn dead_letter_list(args: &[String]) -> Result<String, ShellError> {
    let store = open_store(required_flag(args, "--db")?)?;
    to_pretty_json(&serde_json::json!({
        "dead_letters": store
            .list_dead_letter_deliveries(parse_limit(args).unwrap_or(20))
            .map_err(store_err)?
    }))
}

fn event_tail(args: &[String]) -> Result<String, ShellError> {
    let store = open_store(required_flag(args, "--db")?)?;
    let after = flag_value(args, "--after")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    to_pretty_json(&serde_json::json!({
        "events": store
            .list_event_tail(after, parse_limit(args).unwrap_or(20))
            .map_err(store_err)?
    }))
}

fn open_store(path: &str) -> Result<Store, ShellError> {
    let mut store = Store::open(path).map_err(store_err)?;
    store.migrate().map_err(store_err)?;
    Ok(store)
}

fn missing_transport(command: &str) -> ShellError {
    ShellError::Invalid(format!(
        "{command} requires --db or POWDER_API_BASE_URL; set POWDER_API_KEY too for api-key deployments"
    ))
}

fn remote_err(message: String) -> ShellError {
    if let Some(rest) = message.strip_prefix("http 400: ") {
        ShellError::Invalid(rest.to_string())
    } else if let Some(rest) = message.strip_prefix("http 403: ") {
        ShellError::Forbidden(rest.to_string())
    } else if let Some(rest) = message.strip_prefix("http 404: ") {
        ShellError::NotFound(rest.to_string())
    } else if let Some(rest) = message.strip_prefix("http 409: ") {
        ShellError::Conflict(rest.to_string())
    } else {
        ShellError::Store(message)
    }
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

fn aliases_flag(args: &[String]) -> Option<Vec<String>> {
    flag_value(args, "--aliases").map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn event_filter_flag(args: &[String]) -> Result<Vec<String>, ShellError> {
    Ok(flag_value(args, "--event-filter")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect())
}

fn criterion_proofs_flag(
    args: &[String],
) -> Result<Vec<powder_store::CriterionProofInput>, ShellError> {
    args.iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == "--criterion-proof")
        .filter_map(|(index, _)| args.get(index + 1))
        .map(|raw| {
            let (criterion, url) = raw.split_once('=').ok_or_else(|| {
                ShellError::Invalid(
                    "--criterion-proof must be formatted as <criterion-index>=<url>".to_string(),
                )
            })?;
            let criterion = criterion.parse::<usize>().map_err(|err| {
                ShellError::Invalid(format!("invalid criterion index {criterion}: {err}"))
            })?;
            Ok(powder_store::CriterionProofInput {
                criterion,
                url: url.to_string(),
            })
        })
        .collect()
}

fn criterion_flag(args: &[String]) -> Result<usize, ShellError> {
    let raw = required_flag(args, "--criterion")?;
    raw.parse::<usize>()
        .map_err(|err| ShellError::Invalid(format!("invalid --criterion {raw}: {err}")))
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

fn card_id_values(ids: &[CardId]) -> Vec<String> {
    ids.iter().map(ToString::to_string).collect()
}

fn json_array(value: &Value) -> Result<&[Value], ShellError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| ShellError::Store("remote response expected an array".to_string()))
}

fn json_string(value: &Value, field: &'static str) -> Result<String, ShellError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ShellError::Store(format!("remote response missing string field: {field}")))
}

fn json_priority(value: &Value) -> Result<&'static str, ShellError> {
    let raw = value
        .get("priority")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ShellError::Store("remote response missing string field: priority".to_string())
        })?;
    Priority::parse(raw)
        .map(|priority| priority.as_str())
        .ok_or_else(|| ShellError::Store(format!("remote response invalid priority: {raw}")))
}

fn json_i64(value: &Value, field: &'static str) -> Result<i64, ShellError> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| ShellError::Store(format!("remote response missing integer field: {field}")))
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
    !matches!(
        flag,
        "--dry-run" | "--show-secret" | "--admin" | "--include-hidden" | "--unchecked"
    )
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
    use std::{
        collections::VecDeque,
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };

    #[test]
    fn cli_names_the_instance_workflow() {
        assert!(COMMANDS.contains(&"version"));
        assert!(COMMANDS.contains(&"init-db"));
        assert!(COMMANDS.contains(&"key-list"));
        assert!(COMMANDS.contains(&"key-revoke"));
        assert!(COMMANDS.contains(&"import"));
        assert!(COMMANDS.contains(&"import-repo"));
        assert!(COMMANDS.contains(&"import-github-issues"));
        assert!(COMMANDS.contains(&"list-ready"));
        assert!(COMMANDS.contains(&"list-cards"));
        assert!(COMMANDS.contains(&"repository-list"));
        assert!(COMMANDS.contains(&"repository-get"));
        assert!(COMMANDS.contains(&"repository-upsert"));
        assert!(COMMANDS.contains(&"repository-merge-alias"));
        assert!(COMMANDS.contains(&"repository-delete"));
        assert!(COMMANDS.contains(&"update-relations"));
        assert!(COMMANDS.contains(&"claim"));
        assert!(COMMANDS.contains(&"release-claim"));
        assert!(COMMANDS.contains(&"renew-claim"));
        assert!(COMMANDS.contains(&"transfer-claim"));
        assert!(COMMANDS.contains(&"heartbeat"));
        assert!(COMMANDS.contains(&"get-card"));
        assert!(COMMANDS.contains(&"get-run"));
        assert!(COMMANDS.contains(&"list-awaiting-input"));
        assert!(COMMANDS.contains(&"answer-input"));
        assert!(COMMANDS.contains(&"add-comment"));
        assert!(COMMANDS.contains(&"append-work-log"));
        assert!(COMMANDS.contains(&"check-criterion"));
        assert!(COMMANDS.contains(&"request-input"));
        assert!(COMMANDS.contains(&"complete-card"));
        assert!(COMMANDS.contains(&"subscription-create"));
        assert!(COMMANDS.contains(&"subscription-list"));
        assert!(COMMANDS.contains(&"subscription-disable"));
        assert!(COMMANDS.contains(&"dead-letter-list"));
        assert!(COMMANDS.contains(&"event-tail"));
    }

    /// The whole point of `version` is catching a stale installed binary
    /// before a lane starts (powder-924): it must report the exact commit
    /// this build compiled from, not just an unchanging crate version that
    /// has sat at 0.1.0 since inception.
    #[test]
    fn cli_version_reports_the_build_commit() {
        let output = run(&args(["version"])).unwrap();
        assert!(output.starts_with("powder 0.1.0 (git "));
        assert!(!output.contains("(git )"), "must not embed an empty sha");

        assert_eq!(run(&args(["--version"])).unwrap(), output);
        assert_eq!(run(&args(["-v"])).unwrap(), output);
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
        let detail: Value = serde_json::from_str(&card).unwrap();
        assert!(
            detail["card"].get("acceptance").is_none(),
            "an omitted --acceptance must never fabricate a placeholder oracle: {detail}"
        );
        assert!(
            detail["card"]["status"] == "backlog",
            "empty acceptance must not default to a claimable status: {detail}"
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
        let forced: Value = serde_json::from_str(&forced).unwrap();
        assert_eq!(forced["card"]["status"], "ready");
        assert!(forced["card"].get("acceptance").is_none());

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
        let with_acceptance: Value = serde_json::from_str(&with_acceptance).unwrap();
        assert!(with_acceptance["card"].get("acceptance").is_none());
        assert_eq!(
            with_acceptance["card"]["criteria"][0]["text"],
            "the tests pass"
        );
        assert_eq!(with_acceptance["card"]["status"], "ready");
    }

    #[test]
    fn cli_round_trips_proof_plan_and_criterion_proof_links() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-proof-plan-{}.db",
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
            "proof-plan",
            "--title",
            "Proof plan",
            "--acceptance",
            "HTTP smoke proves the card detail",
            "--proof-plan",
            "PR link plus HTTP smoke transcript",
        ]))
        .unwrap();
        let checked = run(&args([
            "check-criterion",
            "proof-plan",
            "--db",
            &db,
            "--criterion",
            "0",
            "--actor",
            "operator",
        ]))
        .unwrap();
        assert_eq!(checked, "criterion\tproof-plan\t0\tchecked\n");

        run(&args([
            "complete-card",
            "proof-plan",
            "--db",
            &db,
            "--criterion-proof",
            "0=https://example.test/pr",
        ]))
        .unwrap();

        let card = run(&args(["get-card", "proof-plan", "--db", &db])).unwrap();
        let detail: Value = serde_json::from_str(&card).unwrap();
        assert_eq!(
            detail["card"]["proof_plan"][0],
            "PR link plus HTTP smoke transcript"
        );
        assert_eq!(
            detail["card"]["criteria"][0]["text"],
            "HTTP smoke proves the card detail"
        );
        assert_eq!(detail["card"]["criteria"][0]["checked_by"], "operator");
        assert_eq!(
            detail["card"]["criteria"][0]["proof_links"][0]["url"],
            "https://example.test/pr"
        );
        assert!(detail["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| { event["event_type"] == "criterion" && event["actor"] == "operator" }));
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
    fn cli_repository_settings_merge_alias_and_audit_rehomed_card() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-repositories-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        let repository = run(&args([
            "repository-upsert",
            "--db",
            &db,
            "--name",
            "misty-step/canary",
            "--aliases",
            "canary-app,misty-step/canary",
            "--visibility",
            "visible",
            "--import-provenance",
            "manual",
        ]))
        .unwrap();
        assert!(repository.contains("\"name\": \"canary\""));
        assert!(repository.contains("canary-app"));

        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "legacy-canary",
            "--title",
            "Legacy canary",
            "--acceptance",
            "proof exists",
            "--repo",
            "legacy-canary",
        ]))
        .unwrap();

        let merged = run(&args([
            "repository-merge-alias",
            "--db",
            &db,
            "--alias",
            "legacy-canary",
            "--into",
            "canary",
            "--actor",
            "operator",
        ]))
        .unwrap();
        assert!(merged.contains("\"rehomed_cards\": 1"));

        let card = run(&args(["get-card", "legacy-canary", "--db", &db])).unwrap();
        assert!(card.contains("\"repo\": \"canary\""));
        assert!(card.contains("\"event_type\": \"repository\""));
        assert!(card.contains("legacy-canary -> canary"));
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
    fn cli_append_work_log_appears_in_get_card() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-work-log-{}.db",
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
            "worklogged",
            "--title",
            "Has a work log",
        ]))
        .unwrap();

        let output = run(&args([
            "append-work-log",
            "worklogged",
            "--db",
            &db,
            "--agent",
            "codex",
            "--body",
            "tracing the claim expiry bug",
            "--model",
            "claude-sonnet-5",
        ]))
        .unwrap();
        assert!(output.contains("worklogged"));
        assert!(output.contains("codex"));
        assert!(output.contains("tracing the claim expiry bug"));

        let card = run(&args(["get-card", "worklogged", "--db", &db])).unwrap();
        assert!(card.contains("\"agent\": \"codex\""));
        assert!(card.contains("\"model\": \"claude-sonnet-5\""));
        assert!(card.contains("\"body\": \"tracing the claim expiry bug\""));
    }

    #[test]
    fn cli_manages_event_subscriptions_and_tails_events() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-events-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        let created = run(&args([
            "subscription-create",
            "--db",
            &db,
            "--url",
            "http://127.0.0.1:9000/webhook",
            "--event-filter",
            "moved-to-ready,completed",
            "--show-secret",
        ]))
        .unwrap();
        let parts = created.trim().split('\t').collect::<Vec<_>>();
        assert_eq!(parts[0], "subscription");
        assert!(parts[3].starts_with("whsec_powder_"));

        let listed = run(&args(["subscription-list", "--db", &db])).unwrap();
        assert!(listed.contains("moved-to-ready"));
        assert!(
            !listed.contains(parts[3]),
            "subscription-list must not disclose signing secrets"
        );

        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "tail-cli",
            "--title",
            "Tail CLI",
            "--acceptance",
            "proof exists",
            "--status",
            "backlog",
        ]))
        .unwrap();
        run(&args([
            "update-status",
            "tail-cli",
            "--db",
            &db,
            "--status",
            "ready",
        ]))
        .unwrap();
        let events = run(&args(["event-tail", "--db", &db])).unwrap();
        assert!(events.contains("\"event_type\": \"card-created\""));
        assert!(events.contains("\"event_type\": \"moved-to-ready\""));

        let disabled = run(&args(["subscription-disable", parts[1], "--db", &db])).unwrap();
        assert!(disabled.contains(parts[1]));
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
        let open_card: Value = serde_json::from_str(&open_card).unwrap();
        assert_eq!(open_card["card"]["status"], "backlog");
        assert!(
            open_card["card"].get("acceptance").is_none(),
            "no fabricated acceptance"
        );
        run(&args([
            "repository-upsert",
            "--db",
            &db,
            "--name",
            "example",
            "--tier",
            "active",
        ]))
        .unwrap();

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
        let raw_key = created
            .split('\t')
            .nth(3)
            .expect("raw key")
            .trim()
            .to_owned();

        let listed = run(&args(["key-list", "--db", &db])).unwrap();
        assert!(listed.contains(&key_id));
        assert!(listed.contains("codex"));
        assert!(listed.contains("active"));
        assert!(
            !listed.contains(&raw_key),
            "key-list must never print the raw secret"
        );
        let listed_line = listed
            .lines()
            .find(|line| line.contains(&key_id))
            .expect("created key listed");
        let listed_fields: Vec<&str> = listed_line.split('\t').collect();
        assert_eq!(listed_fields[5], "active");
        assert_eq!(
            listed_fields[6], "never",
            "unused key must report never used"
        );
        assert!(
            raw_key.starts_with(listed_fields[3]),
            "key-list's key_prefix must be a real prefix of the raw key"
        );

        let revoked = run(&args(["key-revoke", &key_id, "--db", &db])).unwrap();
        assert_eq!(revoked, format!("revoked\t{key_id}\n"));

        let listed_after = run(&args(["key-list", "--db", &db])).unwrap();
        let revoked_line = listed_after
            .lines()
            .find(|line| line.contains(&key_id))
            .expect("revoked key still listed");
        let revoked_fields: Vec<&str> = revoked_line.split('\t').collect();
        assert_ne!(
            revoked_fields[5], "active",
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
    fn cli_transfer_claim_hands_off_the_lease_and_release_reclaim_still_works() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-transfer-{}.db",
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
            "transfer-test",
            "--title",
            "Transfer test",
            "--acceptance",
            "proof exists",
            "--status",
            "ready",
        ]))
        .unwrap();
        let claimed = run(&args([
            "claim",
            "transfer-test",
            "--db",
            &db,
            "--agent",
            "lane-a",
            "--ttl",
            "3600",
        ]))
        .unwrap();
        let run_id = claimed.split('\t').nth(2).expect("run id").to_owned();

        let transferred = run(&args([
            "transfer-claim",
            "transfer-test",
            "--db",
            &db,
            "--run",
            &run_id,
            "--to-agent",
            "lane-b",
            "--ttl",
            "1800",
        ]))
        .unwrap();
        assert!(transferred.starts_with(&format!("transferred\ttransfer-test\t{run_id}\tlane-b\t")));

        let card = run(&args(["get-card", "transfer-test", "--db", &db])).unwrap();
        assert!(card.contains("\"agent\": \"lane-b\""));

        // Release-then-reclaim still works unchanged after a transfer.
        let released = run(&args([
            "release-claim",
            "transfer-test",
            "--db",
            &db,
            "--run",
            &run_id,
        ]))
        .unwrap();
        assert!(released.contains("released\ttransfer-test"));

        let reclaimed = run(&args([
            "claim",
            "transfer-test",
            "--db",
            &db,
            "--agent",
            "lane-c",
            "--ttl",
            "3600",
        ]))
        .unwrap();
        assert!(reclaimed.starts_with("claimed\ttransfer-test"));
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

    #[test]
    fn cli_remote_mode_uses_http_for_the_accepted_card_commands() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({"cards": [{"id": "remote-1", "priority": "p0", "title": "Remote ready"}]}),
            ),
            (
                200,
                json!({"cards": [{"id": "blocked-1", "priority": "p2", "status": "blocked", "title": "Blocked"}]}),
            ),
            (
                200,
                json!({"card": {"id": "remote-1", "title": "Remote ready"}, "runs": [], "activities": [], "events": [], "links": [], "comments": []}),
            ),
            (
                200,
                json!({"id": "remote-created", "priority": "p1", "status": "ready", "title": "Remote created"}),
            ),
            (
                200,
                json!({"card_id": "remote-created", "run_id": "run-remote", "agent": "codex", "expires_at": 100}),
            ),
            (
                200,
                json!({"id": "remote-created", "priority": "p1", "status": "running", "title": "Remote created"}),
            ),
            (
                200,
                json!({"id": "remote-created", "priority": "p1", "status": "running", "title": "Remote created"}),
            ),
            (
                200,
                json!({"card_id": "remote-created", "author": "operator", "body": "looks good", "created_at": 101}),
            ),
        ]);

        let env = remote_env(Some(&base_url), Some("sk_powder_test"));
        let ready = run_with_env(&args(["list-ready", "--limit", "1"]), &env).unwrap();
        assert_eq!(ready, "remote-1\tP0\tRemote ready\n");

        let cards = run_with_env(
            &args([
                "list-cards",
                "--limit",
                "2",
                "--status",
                "blocked",
                "--repo",
                "misty-step/powder",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(cards, "blocked-1\tP2\tblocked\tBlocked\n");

        let detail = run_with_env(&args(["get-card", "remote-1"]), &env).unwrap();
        assert!(detail.contains("\"id\": \"remote-1\""));

        let created = run_with_env(
            &args([
                "create-card",
                "--id",
                "remote-created",
                "--title",
                "Remote created",
                "--body",
                "body",
                "--acceptance",
                "proof exists",
                "--proof-plan",
                "PR plus smoke",
                "--status",
                "ready",
                "--priority",
                "p1",
                "--repo",
                "misty-step/powder",
                "--related",
                "remote-1",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(created, "created\tremote-created\tP1\tready\n");

        let claimed = run_with_env(
            &args(["claim", "remote-created", "--agent", "codex", "--ttl", "60"]),
            &env,
        )
        .unwrap();
        assert_eq!(claimed, "claimed\tremote-created\trun-remote\t100\n");

        let status = run_with_env(
            &args(["update-status", "remote-created", "--status", "running"]),
            &env,
        )
        .unwrap();
        assert_eq!(status, "status\tremote-created\trunning\n");

        let criterion = run_with_env(
            &args([
                "check-criterion",
                "remote-created",
                "--criterion",
                "0",
                "--actor",
                "operator",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(criterion, "criterion\tremote-created\t0\tchecked\n");

        let comment = run_with_env(
            &args([
                "add-comment",
                "remote-created",
                "--author",
                "operator",
                "--body",
                "looks good",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(comment, "comment\tremote-created\toperator\tlooks good\n");

        let requests = recorded.lock().unwrap();
        let paths = requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "GET /api/v1/cards/ready?limit=1",
                "GET /api/v1/cards?limit=2&status=blocked&repo=misty-step%2Fpowder",
                "GET /api/v1/cards/remote-1",
                "POST /api/v1/cards",
                "POST /api/v1/cards/remote-created/claim",
                "POST /api/v1/cards/remote-created/status",
                "POST /api/v1/cards/remote-created/criteria/check",
                "POST /api/v1/cards/remote-created/comments",
            ]
        );
        assert!(requests
            .iter()
            .all(|request| { request.authorization.as_deref() == Some("Bearer sk_powder_test") }));
        assert_eq!(
            requests[3].body,
            Some(json!({
                "id": "remote-created",
                "title": "Remote created",
                "body": "body",
                "acceptance": ["proof exists"],
                "proof_plan": ["PR plus smoke"],
                "status": "ready",
                "priority": "P1",
                "related": ["remote-1"],
                "blocks": [],
                "blocked_by": [],
                "repo": "misty-step/powder",
            }))
        );
        assert_eq!(
            requests[4].body,
            Some(json!({"agent": "codex", "ttl_seconds": 60}))
        );
        assert_eq!(requests[5].body, Some(json!({"status": "running"})));
        assert_eq!(
            requests[6].body,
            Some(json!({"criterion": 0, "actor": "operator", "checked": true}))
        );
        assert_eq!(
            requests[7].body,
            Some(json!({"author": "operator", "body": "looks good"}))
        );
    }

    /// A lane maintaining a claim lease against a deployed instance (no
    /// local SQLite file at all) must be able to heartbeat, renew, and
    /// release without ever passing `--db` -- the stale-binary friction this
    /// covers was `heartbeat` returning verbatim `missing --db` even with
    /// `POWDER_API_BASE_URL` set (powder-924).
    #[test]
    fn cli_remote_mode_maintains_a_claim_lease_without_db() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({"card_id": "lease-1", "run_id": "run-lease", "expires_at": 200}),
            ),
            (
                200,
                json!({"card_id": "lease-1", "run_id": "run-lease", "expires_at": 260}),
            ),
            (200, json!({"card_id": "lease-1", "run_id": "run-lease"})),
        ]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));

        let beat =
            run_with_env(&args(["heartbeat", "lease-1", "--run", "run-lease"]), &env).unwrap();
        assert_eq!(beat, "heartbeat\tlease-1\trun-lease\t200\n");

        let renewed = run_with_env(
            &args([
                "renew-claim",
                "lease-1",
                "--run",
                "run-lease",
                "--ttl",
                "3600",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(renewed, "renewed\tlease-1\trun-lease\t260\n");

        let released = run_with_env(
            &args(["release-claim", "lease-1", "--run", "run-lease"]),
            &env,
        )
        .unwrap();
        assert_eq!(released, "released\tlease-1\trun-lease\n");

        let requests = recorded.lock().unwrap();
        let paths = requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "POST /api/v1/cards/lease-1/heartbeat",
                "POST /api/v1/cards/lease-1/renew",
                "POST /api/v1/cards/lease-1/release",
            ]
        );
        assert_eq!(requests[0].body, Some(json!({"run_id": "run-lease"})));
        assert_eq!(
            requests[1].body,
            Some(json!({"run_id": "run-lease", "ttl_seconds": 3600}))
        );
        assert_eq!(requests[2].body, Some(json!({"run_id": "run-lease"})));
    }

    /// powder-936: a holder hands its claim to a fresh agent against a
    /// deployed instance with no `--db` -- the same remote-mode requirement
    /// as every other lease-lifecycle command.
    #[test]
    fn cli_remote_mode_transfers_a_claim_without_db() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"card_id": "handoff-1", "run_id": "run-handoff", "agent": "lane-b", "expires_at": 1800}),
        )]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));

        let transferred = run_with_env(
            &args([
                "transfer-claim",
                "handoff-1",
                "--run",
                "run-handoff",
                "--to-agent",
                "lane-b",
                "--ttl",
                "1800",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(
            transferred,
            "transferred\thandoff-1\trun-handoff\tlane-b\t1800\n"
        );

        let requests = recorded.lock().unwrap();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/api/v1/cards/handoff-1/transfer");
        assert_eq!(
            requests[0].body,
            Some(json!({"run_id": "run-handoff", "to_agent": "lane-b", "ttl_seconds": 1800}))
        );
    }

    /// A lane closing out a card against a deployed instance needs
    /// request-input, add-link, and complete-card without `--db` too
    /// (powder-924, powder-926): the closeout path a campaign lane actually
    /// walks -- pause for a question, attach the PR/proof link, mark done.
    #[test]
    fn cli_remote_mode_closes_out_a_card_without_db() {
        let (base_url, recorded) = spawn_test_server(vec![
            (200, json!({"id": "run-closeout", "card_id": "closeout-1"})),
            (
                200,
                json!({"card_id": "closeout-1", "id": "link-1", "label": "pr", "url": "https://example.test/pr"}),
            ),
            (200, json!({"id": "closeout-1", "status": "done"})),
        ]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));

        let awaiting = run_with_env(
            &args([
                "request-input",
                "run-closeout",
                "--question",
                "Approve completion?",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(awaiting, "awaiting-input\trun-closeout\tcloseout-1\n");

        let link = run_with_env(
            &args([
                "add-link",
                "closeout-1",
                "--label",
                "pr",
                "--url",
                "https://example.test/pr",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(link, "link\tcloseout-1\tlink-1\n");

        let completed = run_with_env(
            &args([
                "complete-card",
                "closeout-1",
                "--proof",
                "https://example.test/pr",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(completed, "completed\tcloseout-1\tdone\n");

        let requests = recorded.lock().unwrap();
        let paths = requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "POST /api/v1/runs/run-closeout/input",
                "POST /api/v1/cards/closeout-1/links",
                "POST /api/v1/cards/closeout-1/complete",
            ]
        );
        assert_eq!(
            requests[0].body,
            Some(json!({"question": "Approve completion?"}))
        );
        assert_eq!(
            requests[1].body,
            Some(json!({"label": "pr", "url": "https://example.test/pr"}))
        );
        assert_eq!(
            requests[2].body,
            Some(json!({"proof": "https://example.test/pr"}))
        );
    }

    #[test]
    fn cli_db_flag_wins_over_remote_environment() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"id": "wrong-remote", "priority": "p2", "status": "ready"}),
        )]);
        let db = std::env::temp_dir().join(format!(
            "powder-cli-db-wins-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        let env = remote_env(Some(&base_url), Some("sk_powder_test"));
        run_with_env(&args(["init-db", "--db", &db]), &env).unwrap();
        let output = run_with_env(
            &args([
                "create-card",
                "--db",
                &db,
                "--id",
                "local-card",
                "--title",
                "Local card",
                "--acceptance",
                "proof exists",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(output, "created\tlocal-card\tP2\tready\n");

        assert!(
            recorded.lock().unwrap().is_empty(),
            "--db must use SQLite and must not contact POWDER_API_BASE_URL"
        );
    }

    #[test]
    fn cli_list_ready_path_preview_wins_over_remote_environment() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"cards": [{"id": "wrong-remote", "priority": "p0", "title": "Wrong remote"}]}),
        )]);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let backlog_dir = std::env::temp_dir().join(format!("powder-cli-preview-{nanos}"));
        std::fs::create_dir_all(&backlog_dir).unwrap();
        std::fs::write(
            backlog_dir.join("001-preview.md"),
            "# Preview card\n\nPriority: P0 | Status: ready\n\n## Goal\nPreview locally.\n\n## Oracle\n- [ ] local preview wins\n",
        )
        .unwrap();
        let backlog_dir = backlog_dir.to_string_lossy().to_string();

        let output = run_with_env(
            &args(["list-ready", &backlog_dir]),
            &remote_env(Some(&base_url), Some("sk_powder_test")),
        )
        .unwrap();

        assert_eq!(output, "001\tP0\tPreview card\n");
        assert!(
            recorded.lock().unwrap().is_empty(),
            "positional backlog.d preview must not contact POWDER_API_BASE_URL"
        );
    }

    #[test]
    fn cli_remote_capable_commands_error_clearly_without_db_or_api_env() {
        let err = run_with_env(&args(["list-cards"]), &remote_env(None, None)).unwrap_err();
        assert!(matches!(
            err,
            ShellError::Invalid(message)
                if message == "list-cards requires --db or POWDER_API_BASE_URL; set POWDER_API_KEY too for api-key deployments"
        ));
    }

    fn args<const N: usize>(items: [&str; N]) -> Vec<String> {
        items.into_iter().map(ToOwned::to_owned).collect()
    }

    fn remote_env(base_url: Option<&str>, api_key: Option<&str>) -> RemoteEnv {
        let mut pairs = Vec::new();
        if let Some(base_url) = base_url {
            pairs.push(("POWDER_API_BASE_URL", base_url));
        }
        if let Some(api_key) = api_key {
            pairs.push(("POWDER_API_KEY", api_key));
        }
        RemoteEnv::from_pairs(pairs)
    }

    fn run_with_env(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
        run_with_remote_env(args, remote_env)
    }

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        authorization: Option<String>,
        body: Option<Value>,
    }

    fn spawn_test_server(
        responses: Vec<(u16, Value)>,
    ) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_clone = recorded.clone();
        let mut queue: VecDeque<(u16, Value)> = responses.into();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Some((status, canned_body)) = queue.pop_front() else {
                    break;
                };
                let mut stream = stream.expect("accept connection");
                let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

                let mut request_line = String::new();
                reader
                    .read_line(&mut request_line)
                    .expect("read request line");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();

                let mut content_length = 0usize;
                let mut authorization = None;
                loop {
                    let mut header_line = String::new();
                    reader.read_line(&mut header_line).expect("read header");
                    if header_line == "\r\n" || header_line.is_empty() {
                        break;
                    }
                    if let Some(value) = header_line.strip_prefix("Content-Length:") {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                    if let Some(value) = header_line.strip_prefix("Authorization:") {
                        authorization = Some(value.trim().to_string());
                    }
                }

                let mut body_bytes = vec![0u8; content_length];
                if content_length > 0 {
                    reader.read_exact(&mut body_bytes).expect("read body");
                }
                let request_body = (!body_bytes.is_empty())
                    .then(|| serde_json::from_slice(&body_bytes).expect("parse request body"));

                recorded_clone.lock().unwrap().push(RecordedRequest {
                    method,
                    path,
                    authorization,
                    body: request_body,
                });

                let response_body = serde_json::to_vec(&canned_body).unwrap_or_default();
                let reason = if status == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                );
                stream.write_all(response.as_bytes()).expect("write status");
                stream.write_all(&response_body).expect("write body");
                stream.flush().expect("flush");
            }
        });

        (format!("http://{addr}"), recorded)
    }
}
