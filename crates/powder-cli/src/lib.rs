#![forbid(unsafe_code)]

use powder_api::{parse_list_page, urlencode, RemoteClient};
use powder_core::{
    normalize_acceptance, normalize_csv_relations, normalize_labels, parse_priority, parse_status,
    Authority, Card, CardField, CardFieldError, CardId, CardStatus, DetailLevel, DomainError,
    Priority, ReadyCursor, ReadyQuery, RunId,
};
use powder_store::{
    ApiKeyScope, CardFilter, CardPatch, KeyedOperationContext, SearchQuery, Store, StoreError,
};
use serde_json::{json, Value};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum ShellError {
    NotFound(String),
    Conflict(String),
    Invalid(String),
    Store(String),
    Forbidden(String),
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Conflict(message)
            | Self::Invalid(message)
            | Self::Store(message)
            | Self::Forbidden(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ShellError {}

impl From<DomainError> for ShellError {
    fn from(value: DomainError) -> Self {
        match value {
            DomainError::NotFound { .. } => Self::NotFound(value.to_string()),
            DomainError::Conflict(_) | DomainError::ClaimExpired(_) => {
                Self::Conflict(value.to_string())
            }
            DomainError::Validation { .. } | DomainError::EventData { .. } => {
                Self::Invalid(value.to_string())
            }
            DomainError::Forbidden(_) | DomainError::AuthorityDenied { .. } => {
                Self::Forbidden(value.to_string())
            }
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

static NEXT_IDEMPOTENCY_KEY: AtomicU64 = AtomicU64::new(0);

pub const COMMANDS: &[&str] = &[
    "help",
    "version",
    "init-db",
    "key-create",
    "key-list",
    "key-revoke",
    "create-card",
    "update-card",
    "update-relations",
    "relations-doctor",
    "set-parent",
    "list-ready",
    "list-cards",
    "search",
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
    "event-tail",
];
// `--actor` is a deliberately accepted semantic audit label on local mutation
// commands. It never supplies the trusted authority and may be unused by a
// command whose audit record is fully determined by that authority.
const INIT_DB_FLAGS: &[&str] = &["--db", "--show-secret"];
const KEY_CREATE_FLAGS: &[&str] = &["--db", "--name", "--scope", "--show-secret", "--redacted"];
const KEY_LIST_FLAGS: &[&str] = &["--db"];
const KEY_REVOKE_FLAGS: &[&str] = &["--db", "--idempotency-key"];
const CREATE_CARD_FLAGS: &[&str] = &[
    "--db",
    "--id",
    "--title",
    "--body",
    "--acceptance",
    "--proof-plan",
    "--status",
    "--priority",
    "--related",
    "--blocks",
    "--blocked-by",
    "--parent",
    "--repo",
    "--actor",
    "--idempotency-key",
];
const UPDATE_CARD_FLAGS: &[&str] = &[
    "--db",
    "--title",
    "--body",
    "--acceptance",
    "--proof-plan",
    "--status",
    "--priority",
    "--labels",
    "--repo",
    "--clear-repo",
    "--actor",
    "--idempotency-key",
];
const UPDATE_RELATIONS_FLAGS: &[&str] = &[
    "--db",
    "--related",
    "--blocks",
    "--blocked-by",
    "--actor",
    "--idempotency-key",
];
const RELATIONS_DOCTOR_FLAGS: &[&str] = &["--db", "--repair"];
const SET_PARENT_FLAGS: &[&str] = &[
    "--db",
    "--parent",
    "--clear",
    "--actor",
    "--idempotency-key",
];
const LIST_READY_FLAGS: &[&str] = &[
    "--db",
    "--limit",
    "--json",
    "--repo",
    "--priority",
    "--after",
];
const LIST_CARDS_FLAGS: &[&str] = &[
    "--db",
    "--limit",
    "--status",
    "--repo",
    "--label",
    "--updated-after",
    "--updated-before",
    "--after",
];
const SEARCH_FLAGS: &[&str] = &[
    "--db",
    "--q",
    "--limit",
    "--json",
    "--status",
    "--priority",
    "--repo",
    "--label",
    "--created-after",
    "--created-before",
    "--updated-after",
    "--updated-before",
    "--after",
];
const CLAIM_FLAGS: &[&str] = &["--db", "--agent", "--ttl"];
const RELEASE_CLAIM_FLAGS: &[&str] = &["--db", "--run", "--idempotency-key"];
const RENEW_CLAIM_FLAGS: &[&str] = &["--db", "--run", "--ttl", "--idempotency-key"];
const TRANSFER_CLAIM_FLAGS: &[&str] =
    &["--db", "--run", "--to-agent", "--ttl", "--idempotency-key"];
const HEARTBEAT_FLAGS: &[&str] = &["--db", "--run", "--actor", "--idempotency-key"];
const GET_CARD_FLAGS: &[&str] = &["--db"];
const GET_RUN_FLAGS: &[&str] = &["--db"];
const LIST_AWAITING_INPUT_FLAGS: &[&str] = &["--db", "--limit"];
const ANSWER_INPUT_FLAGS: &[&str] = &["--db", "--actor", "--answer", "--idempotency-key"];
const UPDATE_STATUS_FLAGS: &[&str] = &["--db", "--status", "--actor", "--idempotency-key"];
const CHECK_CRITERION_FLAGS: &[&str] = &[
    "--db",
    "--criterion",
    "--actor",
    "--unchecked",
    "--idempotency-key",
];
const ADD_LINK_FLAGS: &[&str] = &["--db", "--label", "--url", "--idempotency-key"];
const ADD_COMMENT_FLAGS: &[&str] = &["--db", "--author", "--body", "--idempotency-key"];
const APPEND_WORK_LOG_FLAGS: &[&str] =
    &["--db", "--agent", "--body", "--run-id", "--idempotency-key"];
const REQUEST_INPUT_FLAGS: &[&str] = &["--db", "--question", "--actor", "--idempotency-key"];
const COMPLETE_CARD_FLAGS: &[&str] = &[
    "--db",
    "--proof",
    "--criterion-proof",
    "--actor",
    "--idempotency-key",
];
const EVENT_TAIL_FLAGS: &[&str] = &["--db", "--after", "--limit"];

fn known_flags(command: &str) -> &'static [&'static str] {
    match command {
        "init-db" => INIT_DB_FLAGS,
        "key-create" => KEY_CREATE_FLAGS,
        "key-list" => KEY_LIST_FLAGS,
        "key-revoke" => KEY_REVOKE_FLAGS,
        "create-card" => CREATE_CARD_FLAGS,
        "update-card" => UPDATE_CARD_FLAGS,
        "update-relations" => UPDATE_RELATIONS_FLAGS,
        "relations-doctor" => RELATIONS_DOCTOR_FLAGS,
        "set-parent" => SET_PARENT_FLAGS,
        "list-ready" => LIST_READY_FLAGS,
        "list-cards" => LIST_CARDS_FLAGS,
        "search" => SEARCH_FLAGS,
        "claim" => CLAIM_FLAGS,
        "release-claim" => RELEASE_CLAIM_FLAGS,
        "renew-claim" => RENEW_CLAIM_FLAGS,
        "transfer-claim" => TRANSFER_CLAIM_FLAGS,
        "heartbeat" => HEARTBEAT_FLAGS,
        "get-card" => GET_CARD_FLAGS,
        "get-run" => GET_RUN_FLAGS,
        "list-awaiting-input" => LIST_AWAITING_INPUT_FLAGS,
        "answer-input" => ANSWER_INPUT_FLAGS,
        "update-status" => UPDATE_STATUS_FLAGS,
        "check-criterion" => CHECK_CRITERION_FLAGS,
        "add-link" => ADD_LINK_FLAGS,
        "add-comment" => ADD_COMMENT_FLAGS,
        "append-work-log" => APPEND_WORK_LOG_FLAGS,
        "request-input" => REQUEST_INPUT_FLAGS,
        "complete-card" => COMPLETE_CARD_FLAGS,
        "event-tail" => EVENT_TAIL_FLAGS,
        _ => &[],
    }
}

fn validate_flags(command: &str, args: &[String]) -> Result<(), ShellError> {
    let known = known_flags(command);
    let mut index = 0;
    let mut options = true;
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
            index += 1;
            continue;
        }
        if !options || !arg.starts_with('-') {
            index += 1;
            continue;
        }

        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        if !known.contains(&flag) {
            let is_hyphenated_positional = index == 0 && command == "search" && arg.len() > 1;
            if is_hyphenated_positional {
                index += 1;
                continue;
            }
            let suggestion = suggest_flag(flag, known)
                .map(|candidate| format!("; did you mean {candidate}?"))
                .unwrap_or_default();
            return Err(ShellError::Invalid(format!(
                "unknown flag {flag} for {command}{suggestion}"
            )));
        }
        index += if !arg.contains('=') && flag_takes_value(flag) {
            2
        } else {
            1
        };
    }
    Ok(())
}

fn suggest_flag<'a>(flag: &str, known: &'a [&'a str]) -> Option<&'a str> {
    if flag == "--criteria" && known.contains(&"--acceptance") {
        return Some("--acceptance");
    }
    known.iter().copied().find(|candidate| {
        candidate.starts_with(flag)
            || flag.starts_with(candidate)
            || edit_distance(flag, candidate) <= 2
    })
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right = right.as_bytes();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_byte) in left.as_bytes().iter().enumerate() {
        let mut current = vec![row + 1; right.len() + 1];
        for (column, right_byte) in right.iter().enumerate() {
            current[column + 1] = (current[column] + 1)
                .min(previous[column + 1] + 1)
                .min(previous[column] + usize::from(left_byte != right_byte));
        }
        previous = current;
    }
    previous[right.len()]
}

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
    if args.is_empty() {
        return Ok(help());
    }
    if args[0] == "help" {
        match args.get(1).map(String::as_str) {
            None | Some("--help") | Some("-h") => return Ok(help()),
            Some(command) if COMMANDS.contains(&command) => return command_help(command),
            Some(command) => {
                return Err(ShellError::Invalid(format!("unknown command: {command}")))
            }
        }
    }
    if let Some(command) = args
        .first()
        .map(String::as_str)
        .filter(|command| COMMANDS.contains(command))
    {
        if help_requested(command, &args[1..]) {
            return command_help(command);
        }
    }

    reject_admin_flag(args)?;
    if let Some(command) = args
        .first()
        .map(String::as_str)
        .filter(|command| COMMANDS.contains(command))
    {
        validate_flags(command, &args[1..])?;
    }
    match args {
        [] => Ok(help()),
        [command] if command == "--help" || command == "-h" => Ok(help()),
        [command] if command == "version" || command == "--version" || command == "-v" => {
            Ok(version_with_remote_env(remote_env))
        }
        [command, rest @ ..] if command == "init-db" => init_db(rest),
        [command, rest @ ..] if command == "key-create" => key_create(rest),
        [command, rest @ ..] if command == "key-list" => key_list(rest),
        [command, rest @ ..] if command == "key-revoke" => key_revoke(rest),
        [command, rest @ ..] if command == "create-card" => create_card(rest, remote_env),
        [command, rest @ ..] if command == "update-card" => update_card(rest, remote_env),
        [command, rest @ ..] if command == "update-relations" => update_relations(rest, remote_env),
        [command, rest @ ..] if command == "relations-doctor" => relations_doctor(rest),
        [command, rest @ ..] if command == "set-parent" => set_parent(rest, remote_env),
        [command, rest @ ..] if command == "list-ready" => list_ready(rest, remote_env),
        [command, rest @ ..] if command == "list-cards" => list_cards(rest, remote_env),
        [command, rest @ ..] if command == "search" => search(rest, remote_env),
        [command, rest @ ..] if command == "claim" => claim(rest, remote_env),
        [command, rest @ ..] if command == "release-claim" => release_claim(rest, remote_env),
        [command, rest @ ..] if command == "renew-claim" => renew_claim(rest, remote_env),
        [command, rest @ ..] if command == "transfer-claim" => transfer_claim(rest, remote_env),
        [command, rest @ ..] if command == "heartbeat" => heartbeat(rest, remote_env),
        [command, rest @ ..] if command == "get-card" => get_card(rest, remote_env),
        [command, rest @ ..] if command == "get-run" => get_run(rest, remote_env),
        [command, rest @ ..] if command == "list-awaiting-input" => {
            list_awaiting_input(rest, remote_env)
        }
        [command, rest @ ..] if command == "answer-input" => answer_input(rest, remote_env),
        [command, rest @ ..] if command == "update-status" => update_status(rest, remote_env),
        [command, rest @ ..] if command == "check-criterion" => check_criterion(rest, remote_env),
        [command, rest @ ..] if command == "add-link" => add_link(rest, remote_env),
        [command, rest @ ..] if command == "add-comment" => add_comment(rest, remote_env),
        [command, rest @ ..] if command == "append-work-log" => append_work_log(rest, remote_env),
        [command, rest @ ..] if command == "request-input" => request_input(rest, remote_env),
        [command, rest @ ..] if command == "complete-card" => complete_card(rest, remote_env),
        [command, rest @ ..] if command == "event-tail" => event_tail(rest, remote_env),
        [command, ..] => Err(ShellError::Invalid(format!("unknown command: {command}"))),
    }
}

fn help_requested(command: &str, args: &[String]) -> bool {
    let known = known_flags(command);
    let mut index = 0;
    let options = true;
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            break;
        }
        if options && (arg == "--help" || arg == "-h") {
            return true;
        }
        if options && arg.starts_with('-') {
            let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
            if known.contains(&flag) && !arg.contains('=') && flag_takes_value(flag) {
                index += 2;
            } else {
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    false
}

fn command_help(command: &str) -> Result<String, ShellError> {
    if !COMMANDS.contains(&command) {
        return Err(ShellError::Invalid(format!("unknown command: {command}")));
    }
    let flags = known_flags(command);
    let flags = if flags.is_empty() {
        "(none)".to_string()
    } else {
        flags.join(", ")
    };
    Ok(format!(
        "usage: powder {command} [arguments] [options]\n\nRun the {command} command.\nflags: {flags}\n"
    ))
}

/// Reports the installed binary's build provenance so a lane can catch a
/// stale `~/.cargo/bin/powder` (built from an old commit that predates a
/// command's API-mode support) before it starts a claim, instead of hitting
/// a bare `missing --db` on a command the checkout has long since covered.
/// Compare against `git -C <checkout> rev-parse --short=12 HEAD`; a mismatch
/// means `scripts/install-workstation.sh` (or `cargo install --path
/// crates/powder-cli`) is due. Uses the process environment for any remote
/// drift check below; `version_with_remote_env` is the version under test.
pub fn version() -> String {
    version_with_remote_env(&RemoteEnv::from_pairs(std::env::vars()))
}

/// powder-workstation-cli-convergence: a stale local binary was the whole
/// incident (0.1.0 git 1d1ded8 vs. a checkout at 414ac7f, silently missing
/// the repeated-`--acceptance` fix -- a live card lost four criteria) and
/// the server side was never the problem. `powder version` alone can only
/// ever prove what the *binary* was built from; it cannot prove that
/// matches what the operator is actually talking to. When
/// `POWDER_API_BASE_URL` is configured, also fetch the unauthenticated
/// `/readyz` (added `version`/`git_sha` fields, additive) and compare the
/// server's git sha against this binary's -- a mismatch prints a DRIFT line
/// instead of staying silent. A server unreachable, or one predating these
/// fields (older deploy), degrades to a plain note, never an error: `powder
/// version` must never fail just because the network is down.
fn version_with_remote_env(remote_env: &RemoteEnv) -> String {
    let dirty = env!("POWDER_CLI_GIT_DIRTY") == "true";
    let local_sha = env!("POWDER_CLI_GIT_SHA");
    let mut out = format!(
        "powder {} (git {}{})\n",
        env!("CARGO_PKG_VERSION"),
        local_sha,
        if dirty { ", dirty" } else { "" }
    );

    if let Some(client) = remote_env.client() {
        match client.get("/readyz") {
            Ok(body) => {
                let server_version = body.get("version").and_then(Value::as_str);
                let server_sha = body.get("git_sha").and_then(Value::as_str);
                match (server_version, server_sha) {
                    (Some(server_version), Some(server_sha)) => {
                        out.push_str(&format!(
                            "server {server_version} (git {server_sha}) at {}\n",
                            client.base_url()
                        ));
                        if server_sha != local_sha {
                            out.push_str(&format!(
                                "DRIFT: installed binary (git {local_sha}) != server (git \
                                 {server_sha}) -- run scripts/install-workstation.sh to converge\n"
                            ));
                        }
                    }
                    _ => out.push_str(&format!(
                        "server: reachable at {} but /readyz has no version/git_sha \
                         (deploy predates powder-workstation-cli-convergence)\n",
                        client.base_url()
                    )),
                }
            }
            Err(err) => {
                out.push_str(&format!(
                    "server: unreachable at {} ({err})\n",
                    client.base_url()
                ));
            }
        }
    }

    out
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
    help.push_str(
        "  powder key-create --db ./data/powder.db --name codex --scope agent --show-secret  \
         (prints the raw secret once — store it immediately, it cannot be recovered)\n",
    );
    help.push_str(
        "  powder key-create --db ./data/powder.db --name codex --scope agent --redacted  \
         (mints the key but discards the secret; only use this if you don't need the raw value)\n",
    );
    help.push_str(
        "  key-create requires exactly one of --show-secret or --redacted; it refuses to mint \
         without an explicit choice\n",
    );
    help.push_str("  powder key-list --db ./data/powder.db\n");
    help.push_str("  powder key-revoke key-id --db ./data/powder.db --idempotency-key revoke-1\n");
    help.push_str("  powder list-ready --db ./data/powder.db --limit 10\n");
    help.push_str(
        "  powder create-card --db ./data/powder.db --id canary-001 --title \"Canary task\" --repo misty-step/canary [--proof-plan \"CI + PR\"]\n",
    );
    help.push_str(
        "  powder create-card ... --acceptance \"first criterion\" --acceptance \"second criterion\"  (repeatable; every occurrence is one criterion, in order)\n",
    );
    help.push_str(
        "  powder update-card canary-001 --db ./data/powder.db --acceptance \"a\" --acceptance \"b\"  (repeatable; replaces the full criteria list)\n",
    );
    help.push_str(
        "  powder list-cards --db ./data/powder.db --status ready --repo misty-step/example --after cursor\n",
    );
    help.push_str(
        "  powder update-relations 001 --db ./data/powder.db --related 002,003 --blocks 004 --blocked-by 000  (mirrors reciprocally onto 002, 003, and 004 atomically)\n",
    );
    help.push_str(
        "  powder relations-doctor --db ./data/powder.db  (report-only: relation asymmetry, malformed relation values, plus dangling/self/cycle/invalid parent edges; nested parents remain valid)\n",
    );
    help.push_str(
        "  powder relations-doctor --db ./data/powder.db --repair  (audited relation mirror repair; malformed relation values stay unchanged; parent repair refuses with evidence)\n",
    );
    help.push_str(
        "    (--repair always ADDS missing relation mirrors, never invents parents; parent findings are refused with evidence because raw state has no unambiguous audited correction.)\n",
    );
    help.push_str("  powder set-parent 002 --db ./data/powder.db --parent 001\n");
    help.push_str("  powder set-parent 002 --db ./data/powder.db --clear\n");
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
    help.push_str(
        "  powder update-status 001 --db ./data/powder.db --status in_progress --actor codex\n",
    );
    help.push_str(
        "  powder check-criterion 001 --db ./data/powder.db --criterion 0 --actor operator [--unchecked]\n",
    );
    help.push_str(
        "  powder add-comment 001 --db ./data/powder.db --author operator --body \"looks good\"\n",
    );
    help.push_str(
        "  powder append-work-log 001 --db ./data/powder.db --agent codex --body \"tracing the claim expiry bug\" [--run-id run-id]\n",
    );
    help.push_str(
        "  powder complete-card 001 --db ./data/powder.db [--proof https://example.test/proof]\n",
    );
    help.push_str("  powder event-tail --db ./data/powder.db --after 0 --limit 20\n");
    help.push_str(
        "authority:\n  local mutations use POWDER_PRINCIPAL (or the fixed trusted local-cli admin principal). \
         --actor, --author, and --agent are semantic audit labels only; they never grant authority. \
         --admin is not accepted.\n\n",
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
    let redacted = has_flag(args, "--redacted");

    // The raw secret exists only in the instant `create_api_key` returns; the
    // store persists nothing but a sha256 hash, so a key minted without
    // capturing that value is gone forever (the dogfood incident: minting
    // without --show-secret printed "redacted" and the secret was
    // unrecoverable). Require the caller to make an explicit choice before
    // we mint, rather than defaulting to either a silent discard or an
    // unsolicited secret print.
    match (show_secret, redacted) {
        (true, true) => {
            return Err(ShellError::Invalid(
                "key-create: pass exactly one of --show-secret or --redacted, not both".to_string(),
            ))
        }
        (false, false) => {
            return Err(ShellError::Invalid(
                "key-create: refusing to mint a key without an explicit secret-handling choice; \
                 pass --show-secret to print the raw key once (store it immediately, it cannot \
                 be recovered) or --redacted to acknowledge the secret will be discarded"
                    .to_string(),
            ))
        }
        _ => {}
    }

    let name = flag_value(args, "--name").unwrap_or("agent");
    let scope = flag_value(args, "--scope")
        .and_then(ApiKeyScope::parse)
        .unwrap_or(ApiKeyScope::Agent);
    let now = unix_now();
    let mut store = open_store(required_flag(args, "--db")?)?;
    let key = store
        .create_api_key_with_authority(name, scope, now, &admin_authority(args))
        .map_err(store_err)?;

    if show_secret {
        // The warning is for the human; stdout stays machine-readable so
        // `... | cut -f4` captures exactly the secret (the lease-race demo
        // broke when this warning shared stdout with the key line).
        eprintln!(
            "WARNING: this is the only time this secret is shown. Store it now \
             (consumer secret store: keychain, 1Password, etc.) — it cannot be recovered."
        );
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
    let idempotency_key = required_idempotency_key(args)?;
    let mut store = open_store(required_flag(args, "--db")?)?;
    store
        .revoke_api_key_keyed(key_id, now, &idempotency_key, &admin_authority(args))
        .map_err(store_err)?;
    Ok(format!("revoked\t{key_id}\n"))
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
    // --acceptance is repeatable: every occurrence contributes one criterion,
    // in order. A single occurrence keeps its historical behavior.
    let acceptance = normalize_acceptance(
        flag_values(args, "--acceptance")
            .into_iter()
            .map(str::to_string),
    );
    let proof_plan: Vec<String> = flag_value(args, "--proof-plan")
        .map(|value| vec![value.to_string()])
        .unwrap_or_default();
    let status = flag_value(args, "--status")
        .map(parse_status_flag)
        .transpose()?
        .unwrap_or_else(|| CardStatus::default_for_acceptance(&acceptance));
    let priority = flag_value(args, "--priority")
        .map(parse_priority_flag)
        .transpose()?
        .unwrap_or_default();

    let related = card_ids_flag(args, "--related")?;
    let blocks = card_ids_flag(args, "--blocks")?;
    let blocked_by = card_ids_flag(args, "--blocked-by")?;
    let parent = flag_value(args, "--parent")
        .map(|value| CardId::new(value).map_err(|error| ShellError::Invalid(error.to_string())))
        .transpose()?;
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
        card.parent = parent;
        card.repo = repo;
        keyed_json(
            store
                .create_card_with_events_as_keyed(
                    card,
                    &idempotency_key(args)?,
                    &authority(args),
                    now,
                )
                .map_err(store_err)?,
        )?
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
        if let Some(parent) = parent {
            payload["parent"] = json!(parent.as_str());
        }
        client
            .post_with_key("/api/v1/cards", payload, &idempotency_key(args)?)
            .map_err(remote_err)?
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

fn update_card(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-card")?;
    let repo = flag_value(args, "--repo").map(str::to_string);
    let clear_repo = has_flag(args, "--clear-repo");
    if repo.is_some() && clear_repo {
        return Err(ShellError::Invalid(
            "--repo and --clear-repo are mutually exclusive".to_string(),
        ));
    }
    let patch = CardPatch {
        title: flag_value(args, "--title").map(str::to_string),
        body: flag_value(args, "--body").map(str::to_string),
        acceptance: {
            let values = flag_values(args, "--acceptance");
            (!values.is_empty())
                .then(|| normalize_acceptance(values.into_iter().map(str::to_string)))
        },
        proof_plan: flag_value(args, "--proof-plan").map(|value| vec![value.to_string()]),
        status: flag_value(args, "--status")
            .map(parse_status_flag)
            .transpose()?,
        priority: flag_value(args, "--priority")
            .map(parse_priority_flag)
            .transpose()?,
        labels: flag_value(args, "--labels").map(|raw| normalize_labels(split_csv(raw))),
        repo: if clear_repo {
            Some(None)
        } else {
            repo.map(Some)
        },
    };
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        keyed_json(
            store
                .patch_card_as_keyed(
                    &card_id,
                    patch,
                    &idempotency_key(args)?,
                    &authority(args),
                    now,
                )
                .map_err(store_err)?,
        )?
    } else if let Some(client) = remote_env.client() {
        let mut payload = json!({});
        if let Some(title) = patch.title {
            payload["title"] = json!(title);
        }
        if let Some(body) = patch.body {
            payload["body"] = json!(body);
        }
        if let Some(acceptance) = patch.acceptance {
            payload["acceptance"] = json!(acceptance);
        }
        if let Some(proof_plan) = patch.proof_plan {
            payload["proof_plan"] = json!(proof_plan);
        }
        if let Some(status) = patch.status {
            payload["status"] = json!(status.as_str());
        }
        if let Some(priority) = patch.priority {
            payload["priority"] = json!(priority.as_str());
        }
        if let Some(repo) = patch.repo {
            payload["repo"] = json!(repo);
        }
        if let Some(labels) = patch.labels {
            payload["labels"] = json!(labels);
        }
        client
            .patch_with_key(
                &format!("/api/v1/cards/{card_id}"),
                payload,
                &idempotency_key(args)?,
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("update-card"));
    };

    Ok(format!(
        "updated\t{}\t{}\t{}\n",
        json_string(&card, "id")?,
        json_priority(&card)?,
        json_string(&card, "status")?
    ))
}

fn update_relations(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-relations")?;
    let related = card_ids_flag(args, "--related")?;
    let blocks = card_ids_flag(args, "--blocks")?;
    let blocked_by = card_ids_flag(args, "--blocked-by")?;
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let card = store
            .update_relations_keyed(
                &card_id,
                related,
                blocks,
                blocked_by,
                KeyedOperationContext::new(now, &idempotency_key(args)?, &authority(args)),
            )
            .map_err(store_err)?
            .value;
        json!({ "id": card.id })
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/relations"),
                json!({
                    "related": card_id_values(&related),
                    "blocks": card_id_values(&blocks),
                    "blocked_by": card_id_values(&blocked_by),
                }),
                &idempotency_key(args)?,
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("update-relations"));
    };
    Ok(format!("relations\t{}\n", json_string(&card, "id")?))
}

/// Report (or, with `--repair`, fix) cards whose `blocks`/`blocked_by`/
/// `related` edges disagree with a peer that names them back -- since
/// `update-relations`/`create-card` mirror reciprocally now, this should
/// only ever find drift from data written before that guarantee existed or
/// written directly against the database. `--repair` uses union semantics
/// (adds the missing mirror edge, never deletes the one-sided edge), so it
/// resurrects a half-applied removal rather than finishing it -- inspect
/// the report before repairing.
fn relations_doctor(args: &[String]) -> Result<String, ShellError> {
    let now = unix_now();
    let repair = has_flag(args, "--repair");
    let mut store = open_store(required_flag(args, "--db")?)?;
    let report = store
        .relations_doctor_with_authority(&admin_authority(args), now, repair)
        .map_err(store_err)?;
    to_pretty_json(&report)
}

fn set_parent(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "set-parent")?;
    let parent = match (flag_value(args, "--parent"), has_flag(args, "--clear")) {
        (Some(parent), false) => {
            Some(CardId::new(parent).map_err(|error| ShellError::Invalid(error.to_string()))?)
        }
        (None, true) => None,
        _ => {
            return Err(ShellError::Invalid(
                "set-parent requires exactly one of --parent <card-id> or --clear".to_string(),
            ))
        }
    };
    let parent_wire = parent.as_ref().map(|parent| parent.as_str().to_string());
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        let card = store
            .set_parent_keyed(
                &card_id,
                parent,
                now,
                &idempotency_key(args)?,
                &authority(args),
            )
            .map_err(store_err)?
            .value;
        json!({
            "id": card.id,
            "parent": card.parent.map(|parent| parent.as_str().to_string()),
        })
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/parent"),
                json!({ "parent": parent_wire }),
                &idempotency_key(args)?,
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("set-parent"));
    };
    Ok(format!(
        "parent\t{}\t{}\n",
        json_string(&card, "id")?,
        card.get("parent").and_then(Value::as_str).unwrap_or("none")
    ))
}

fn list_ready(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let json_output = has_flag(args, "--json");
    let now = unix_now();
    let repo = flag_value(args, "--repo")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .map(|value| {
                    if value.is_empty() {
                        Err(ShellError::Invalid(
                            "--repo must not contain a blank repository".to_string(),
                        ))
                    } else {
                        Ok(value.to_string())
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let priority = flag_value(args, "--priority")
        .map(parse_priority_flag)
        .transpose()?;
    let query = ReadyQuery::new(now, limit)
        .with_repositories(repo.clone())
        .with_priority(priority);
    let raw_after = flag_value(args, "--after");
    let payload = if let Some(db) = flag_value(args, "--db") {
        let after = raw_after
            .map(|raw| ReadyCursor::decode_for_query(raw, &query))
            .transpose()
            .map_err(|err| ShellError::Invalid(err.to_string()))?;
        let store = open_store(db)?;
        let page = store
            .list_ready_page_after(query.clone(), after.as_ref())
            .map_err(store_err)?;
        ready_page_json(&page)
    } else if let Some(client) = remote_env.client() {
        let mut url = format!("/api/v1/cards/ready?limit={limit}");
        if !repo.is_empty() {
            url.push_str(&format!("&repo={}", urlencode(&repo.join(","))));
        }
        if let Some(priority) = priority {
            url.push_str(&format!("&priority={}", priority.as_str()));
        }
        if let Some(after) = raw_after {
            url.push_str(&format!("&after={}", urlencode(after)));
        }
        client.get(&url).map_err(remote_err)?
    } else {
        return Err(ShellError::Invalid(
            "list-ready requires --db or POWDER_API_BASE_URL; set POWDER_API_KEY too for api-key deployments".to_string(),
        ));
    };
    if json_output {
        return serde_json::to_string(&payload).map_err(|err| ShellError::Store(err.to_string()));
    }
    let mut out = String::new();
    for card in json_array(
        payload
            .get("cards")
            .ok_or_else(|| ShellError::Store("ready response missing cards array".to_string()))?,
    )? {
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

/// Search cards and indexed comments/work logs. The JSON envelope is shared with
/// the HTTP surface; --json is accepted explicitly for scripts.
fn search(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let positionals = positional_preserving_leading_hyphen(args, known_flags("search"));
    if positionals.len() > 1 {
        return Err(ShellError::Invalid(
            "search accepts one positional query; quote multi-word queries or use --q".to_string(),
        ));
    }
    let q = flag_value(args, "--q")
        .or_else(|| positionals.first().copied())
        .ok_or_else(|| ShellError::Invalid("search requires --q <text>".to_string()))?;
    let limit = parse_limit(args).unwrap_or(20).max(1);
    let status = flag_value(args, "--status")
        .map(parse_status_flag)
        .transpose()?;
    let priority = flag_value(args, "--priority")
        .map(parse_priority_flag)
        .transpose()?;
    let parse_time = |flag: &'static str| parse_unix_time_flag(args, flag);
    let query = SearchQuery {
        q: q.to_string(),
        status,
        repo: flag_value(args, "--repo").map(str::to_string),
        label: flag_value(args, "--label").map(str::to_string),
        priority,
        created_after: parse_time("--created-after")?,
        created_before: parse_time("--created-before")?,
        updated_after: parse_time("--updated-after")?,
        updated_before: parse_time("--updated-before")?,
        limit,
        after: flag_value(args, "--after").map(str::to_string),
    };
    let payload = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        json!(store.search_page(&query).map_err(store_err)?)
    } else if let Some(client) = remote_env.client() {
        let mut parts = vec![("q", q.to_string()), ("limit", limit.to_string())];
        let mut add = |key: &'static str, value: Option<String>| {
            if let Some(value) = value {
                parts.push((key, value));
            }
        };
        add("status", status.map(|v| v.as_str().to_string()));
        add("repo", query.repo.clone());
        add("label", query.label.clone());
        add("priority", priority.map(|v| v.as_str().to_string()));
        add("created_after", query.created_after.map(|v| v.to_string()));
        add(
            "created_before",
            query.created_before.map(|v| v.to_string()),
        );
        add("updated_after", query.updated_after.map(|v| v.to_string()));
        add(
            "updated_before",
            query.updated_before.map(|v| v.to_string()),
        );
        add("after", query.after.clone());
        let query_string = parts
            .into_iter()
            .map(|(key, value)| format!("{key}={}", urlencode(&value)))
            .collect::<Vec<_>>()
            .join("&");
        client
            .get(&format!("/api/v1/cards/search?{query_string}"))
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("search"));
    };
    serde_json::to_string(&payload).map_err(|err| ShellError::Store(err.to_string()))
}

fn ready_page_json(page: &powder_store::CardListPage) -> Value {
    let mut payload = json!({
        "cards": page.cards,
        "total_count": page.total_count,
        "has_more": page.next_after.is_some(),
    });
    if !page.cycle_card_ids.is_empty() {
        payload["cycle_card_ids"] = json!(page.cycle_card_ids);
    }
    if let Some(next_after) = page.ready_cursor.as_ref() {
        payload["next_after"] = json!(next_after);
    }
    payload
}

/// Enumerate cards by status/repo, not just ready-eligible ones -- a card
/// with an unresolved blocker, and `done` cards, are otherwise invisible
/// without opening the database file directly.
fn list_cards(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let status = flag_value(args, "--status")
        .map(parse_status_flag)
        .transpose()?;
    let repo = flag_value(args, "--repo").map(str::to_string);
    let label = flag_value(args, "--label").map(str::to_string);
    let raw_after = flag_value(args, "--after");
    let after = raw_after
        .map(CardId::new)
        .transpose()
        .map_err(ShellError::from)?;
    let updated_after = parse_unix_time_flag(args, "--updated-after")?;
    let updated_before = parse_unix_time_flag(args, "--updated-before")?;
    if let (Some(updated_after), Some(updated_before)) = (updated_after, updated_before) {
        if updated_after > updated_before {
            return Err(ShellError::Invalid(
                "--updated-after must be less than or equal to --updated-before".to_string(),
            ));
        }
    }
    let cards = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        let filter = CardFilter {
            status,
            repo: repo.clone(),
            label: label.clone(),
            include_terminal: true,
        };
        let page = store
            .list_cards_page_after(&filter, limit, after.as_ref())
            .map_err(store_err)?;
        let mut cards = json!(page.cards);
        filter_cards_by_updated_at(&mut cards, updated_after, updated_before);
        cards
    } else if let Some(client) = remote_env.client() {
        let mut query = format!("limit={limit}");
        if let Some(status) = status {
            query.push_str(&format!("&status={}", status.as_str()));
        }
        if let Some(repo) = &repo {
            query.push_str(&format!("&repo={}", urlencode(repo)));
        }
        if let Some(label) = &label {
            query.push_str(&format!("&label={}", urlencode(label)));
        }
        if let Some(after) = raw_after {
            query.push_str(&format!("&after={}", urlencode(after)));
        }
        let mut cards = list_page_cards(
            client
                .get(&format!("/api/v1/cards?{query}"))
                .map_err(remote_err)?,
        )?;
        filter_cards_by_updated_at(&mut cards, updated_after, updated_before);
        cards
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
            .release_claim_keyed(
                &card_id,
                &run_id,
                now,
                &idempotency_key(args)?,
                &authority(args),
            )
            .map_err(store_err)?
            .value;
        (claim.card_id.to_string(), claim.run_id.to_string())
    } else if let Some(client) = remote_env.client() {
        let released = client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/release"),
                json!({"run_id": run_id.as_str()}),
                &idempotency_key(args)?,
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
            .renew_claim_keyed(
                &card_id,
                &run_id,
                now,
                ttl_seconds,
                &idempotency_key(args)?,
                &authority(args),
            )
            .map_err(store_err)?
            .value;
        (
            claim.card_id.to_string(),
            claim.run_id.to_string(),
            claim.expires_at,
        )
    } else if let Some(client) = remote_env.client() {
        let renewed = client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/renew"),
                json!({"run_id": run_id.as_str(), "ttl_seconds": ttl_seconds}),
                &idempotency_key(args)?,
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
            .transfer_claim_keyed(
                &card_id,
                &run_id,
                to_agent,
                ttl_seconds,
                KeyedOperationContext::new(now, &idempotency_key(args)?, &authority(args)),
            )
            .map_err(store_err)?
            .value;
        (
            claim.card_id.to_string(),
            claim.run_id.to_string(),
            claim.agent,
            claim.expires_at,
        )
    } else if let Some(client) = remote_env.client() {
        let transferred = client
                .post_with_key(
                    &format!("/api/v1/cards/{card_id}/transfer"),
                    json!({"run_id": run_id.as_str(), "to_agent": to_agent, "ttl_seconds": ttl_seconds}),
                    &idempotency_key(args)?,
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
            .heartbeat_claim_keyed(
                &card_id,
                &run_id,
                now,
                &idempotency_key(args)?,
                &authority(args),
            )
            .map_err(store_err)?
            .value;
        (
            claim.card_id.to_string(),
            claim.run_id.to_string(),
            claim.expires_at,
        )
    } else if let Some(client) = remote_env.client() {
        let beat = client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/heartbeat"),
                json!({"run_id": run_id.as_str()}),
                &idempotency_key(args)?,
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
            .get_card_detail(&card_id, DetailLevel::Detailed, unix_now())
            .map_err(store_err)?
            .ok_or_else(|| ShellError::NotFound(format!("card not found: {card_id}")))?;
        to_pretty_json(&detail)
    } else if let Some(client) = remote_env.client() {
        let detail = client
            .get(&format!("/api/v1/cards/{card_id}?detail=detailed"))
            .map_err(remote_err)?;
        to_pretty_json(&detail)
    } else {
        Err(missing_transport("get-card"))
    }
}
fn get_run(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let run_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("get-run requires a run id".to_string()))
        .and_then(|id| RunId::new(id).map_err(ShellError::from))?;
    let detail = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        serde_json::to_value(
            store
                .get_run_detail(&run_id, DetailLevel::Detailed)
                .map_err(store_err)?
                .ok_or_else(|| ShellError::NotFound(format!("run not found: {run_id}")))?,
        )
        .map_err(|error| ShellError::Store(error.to_string()))?
    } else if let Some(client) = remote_env.client() {
        client
            .get(&format!("/api/v1/runs/{run_id}?detail=detailed"))
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("get-run"));
    };
    to_pretty_json(&detail)
}

fn list_awaiting_input(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let limit = parse_limit(args).unwrap_or(20);
    let value = if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        json!({
            "awaiting": store.list_awaiting_input(limit).map_err(store_err)?
        })
    } else if let Some(client) = remote_env.client() {
        client
            .get(&format!("/api/v1/runs/awaiting-input?limit={limit}"))
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("list-awaiting-input"));
    };
    to_pretty_json(&value)
}

fn answer_input(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let run_id = positional(args)
        .first()
        .copied()
        .ok_or_else(|| ShellError::Invalid("answer-input requires a run id".to_string()))
        .and_then(|id| RunId::new(id).map_err(ShellError::from))?;
    let actor = required_flag(args, "--actor")?;
    let answer = required_flag(args, "--answer")?;
    let run = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        keyed_json(
            store
                .answer_input_keyed(
                    &run_id,
                    actor,
                    answer,
                    now,
                    &idempotency_key(args)?,
                    &authority(args),
                )
                .map_err(store_err)?,
        )?
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/runs/{run_id}/answer"),
                json!({"actor": actor, "answer": answer}),
                &idempotency_key(args)?,
            )
            .map_err(remote_err)?
    } else {
        return Err(missing_transport("answer-input"));
    };
    Ok(format!(
        "answered-input\t{}\t{}\n",
        json_string(&run, "id")?,
        json_string(&run, "card_id")?
    ))
}

fn update_status(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let now = unix_now();
    let card_id = positional_card_id(args, "update-status")?;
    let status = match flag_value(args, "--status") {
        Some(raw) => parse_status_flag(raw)?,
        None => {
            return Err(ShellError::Invalid(
                "update-status requires --status".to_string(),
            ))
        }
    };
    let card = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        keyed_json(
            store
                .update_status_keyed(
                    &card_id,
                    status,
                    now,
                    &idempotency_key(args)?,
                    &authority(args),
                )
                .map_err(store_err)?,
        )?
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/status"),
                json!({"status": status.as_str()}),
                &idempotency_key(args)?,
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
        keyed_json(
            store
                .check_criterion_as_keyed(
                    &card_id,
                    criterion,
                    actor,
                    checked,
                    KeyedOperationContext::new(now, &idempotency_key(args)?, &authority(args)),
                )
                .map_err(store_err)?,
        )?
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/criteria/check"),
                json!({"criterion": criterion, "actor": actor, "checked": checked}),
                &idempotency_key(args)?,
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
            .add_link_as_keyed(
                &card_id,
                label,
                url,
                now,
                &idempotency_key(args)?,
                &authority(args),
            )
            .map_err(store_err)?
            .value;
        (link.card_id.to_string(), link.id.to_string())
    } else if let Some(client) = remote_env.client() {
        let link = client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/links"),
                json!({"label": label, "url": url}),
                &idempotency_key(args)?,
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
        keyed_json(
            store
                .add_comment_as_keyed(
                    &card_id,
                    author,
                    body,
                    now,
                    &idempotency_key(args)?,
                    &authority(args),
                )
                .map_err(store_err)?,
        )?
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/comments"),
                json!({"author": author, "body": body}),
                &idempotency_key(args)?,
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
    let run_id = flag_value(args, "--run-id");
    let entry = if let Some(db) = flag_value(args, "--db") {
        let mut store = open_store(db)?;
        keyed_json(
            store
                .append_work_log_as_keyed(
                    &card_id,
                    agent,
                    run_id,
                    body,
                    KeyedOperationContext::new(now, &idempotency_key(args)?, &authority(args)),
                )
                .map_err(store_err)?,
        )?
    } else if let Some(client) = remote_env.client() {
        client
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/work-log"),
                json!({"agent": agent, "body": body, "run_id": run_id}),
                &idempotency_key(args)?,
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
            .request_input_keyed(
                &run_id,
                question,
                now,
                &idempotency_key(args)?,
                &authority(args),
            )
            .map_err(store_err)?
            .value;
        (run.id.to_string(), run.card_id.to_string())
    } else if let Some(client) = remote_env.client() {
        let run = client
            .post_with_key(
                &format!("/api/v1/runs/{run_id}/input"),
                json!({"question": question}),
                &idempotency_key(args)?,
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
        keyed_json(
            store
                .complete_card_keyed(
                    &card_id,
                    proof,
                    criterion_proofs,
                    now,
                    &idempotency_key(args)?,
                    &authority(args),
                )
                .map_err(store_err)?,
        )?
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
            .post_with_key(
                &format!("/api/v1/cards/{card_id}/complete"),
                body,
                &idempotency_key(args)?,
            )
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

fn event_tail(args: &[String], remote_env: &RemoteEnv) -> Result<String, ShellError> {
    let after = flag_value(args, "--after")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let limit = parse_limit(args).unwrap_or(20);
    if let Some(db) = flag_value(args, "--db") {
        let store = open_store(db)?;
        return to_pretty_json(&serde_json::json!({
            "events": store.list_event_tail(after, limit).map_err(store_err)?
        }));
    }
    if let Some(client) = remote_env.client() {
        return client
            .get_text(&format!("/api/v1/events/tail?after={after}&limit={limit}"))
            .map_err(remote_err);
    }
    Err(missing_transport("event-tail"))
}

fn idempotency_key(args: &[String]) -> Result<String, ShellError> {
    let mut values = Vec::new();
    let mut supplied = false;
    for (index, arg) in args.iter().enumerate() {
        if arg == "--idempotency-key" {
            supplied = true;
            let value = args.get(index + 1).ok_or_else(|| {
                ShellError::Invalid("--idempotency-key requires a value".to_string())
            })?;
            let value = value.trim();
            if value.is_empty() || value.starts_with("--") {
                return Err(ShellError::Invalid(
                    "--idempotency-key requires a non-empty value".to_string(),
                ));
            }
            values.push(value);
        } else if let Some(value) = arg.strip_prefix("--idempotency-key=") {
            supplied = true;
            let value = value.trim();
            if value.is_empty() {
                return Err(ShellError::Invalid(
                    "--idempotency-key requires a non-empty value".to_string(),
                ));
            }
            values.push(value);
        }
    }
    if !supplied {
        let sequence = NEXT_IDEMPOTENCY_KEY.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        return Ok(format!(
            "powder-cli-{}-{nanos}-{sequence}",
            std::process::id()
        ));
    }
    let first = values[0];
    if values.iter().any(|value| *value != first) {
        return Err(ShellError::Invalid(
            "conflicting --idempotency-key values are not accepted".to_string(),
        ));
    }
    Ok(first.to_string())
}
fn required_idempotency_key(args: &[String]) -> Result<String, ShellError> {
    if !args
        .iter()
        .any(|arg| arg == "--idempotency-key" || arg.starts_with("--idempotency-key="))
    {
        return Err(ShellError::Invalid(
            "key-revoke requires --idempotency-key".to_string(),
        ));
    }
    idempotency_key(args)
}

fn keyed_json<T: serde::Serialize>(
    outcome: powder_store::IdempotencyOutcome<T>,
) -> Result<Value, ShellError> {
    let mut value = serde_json::to_value(outcome.value)
        .map_err(|error| ShellError::Store(error.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.insert("replayed".to_string(), json!(outcome.replayed));
    }
    Ok(value)
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
    let field = match flag {
        "--related" => CardField::Related,
        "--blocks" => CardField::Blocks,
        "--blocked-by" => CardField::BlockedBy,
        _ => unreachable!("card relation parser called with {flag}"),
    };
    let values = flag_value(args, flag)
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned);
    normalize_csv_relations(field, values).map_err(field_error)
}

fn field_error(error: CardFieldError) -> ShellError {
    ShellError::Invalid(error.to_string())
}

fn cli_field_error(error: CardFieldError, flag: &str) -> ShellError {
    let message = error.to_string();
    let canonical = format!("invalid {}", error.field().as_str());
    let cli = format!("invalid --{flag}");
    ShellError::Invalid(message.replacen(&canonical, &cli, 1))
}

/// powder-status-vocabulary: every `--status` call site uses the shared
/// parser, so retired names are rejected with the current vocabulary.
fn parse_status_flag(raw: &str) -> Result<CardStatus, ShellError> {
    parse_status(raw).map_err(|error| cli_field_error(error, "status"))
}

fn parse_priority_flag(raw: &str) -> Result<Priority, ShellError> {
    parse_priority(raw).map_err(|error| cli_field_error(error, "priority"))
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
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

/// Build the trusted process authority for a local SQLite mutation.
///
/// `--actor`, `--author`, and `--agent` are semantic audit inputs only; they
/// never construct or elevate the authenticated principal. A deployment may set
/// `POWDER_PRINCIPAL` in the trusted process environment. Otherwise the
/// single-operator local CLI uses its fixed `local-cli` admin principal.
fn local_authority() -> Authority {
    let principal = std::env::var("POWDER_PRINCIPAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local-cli".to_string());
    Authority::principal(principal, true)
}

fn authority(_args: &[String]) -> Authority {
    local_authority()
}

fn admin_authority(_args: &[String]) -> Authority {
    local_authority()
}

fn reject_admin_flag(args: &[String]) -> Result<(), ShellError> {
    if args
        .iter()
        .any(|arg| arg == "--admin" || arg.starts_with("--admin="))
    {
        return Err(ShellError::Invalid(
            "--admin is not accepted; authority comes from trusted process configuration"
                .to_string(),
        ));
    }
    Ok(())
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

fn list_page_cards(value: Value) -> Result<Value, ShellError> {
    let page = parse_list_page(value).map_err(ShellError::Store)?;
    Ok(Value::Array(page.cards))
}

fn filter_cards_by_updated_at(
    cards: &mut Value,
    updated_after: Option<i64>,
    updated_before: Option<i64>,
) {
    if updated_after.is_none() && updated_before.is_none() {
        return;
    }
    let Some(cards) = cards.as_array_mut() else {
        return;
    };
    cards.retain(|card| {
        let Some(updated_at) = card.get("updated_at").and_then(Value::as_i64) else {
            return false;
        };
        updated_after.is_none_or(|threshold| updated_at >= threshold)
            && updated_before.is_none_or(|threshold| updated_at <= threshold)
    });
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
    parse_priority(raw)
        .map(|priority| priority.as_str())
        .map_err(|error| ShellError::Store(format!("remote response {error}")))
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

fn parse_unix_time_flag(args: &[String], flag: &'static str) -> Result<Option<i64>, ShellError> {
    if !has_flag(args, flag) {
        return Ok(None);
    }
    let raw =
        flag_value(args, flag).ok_or_else(|| ShellError::Invalid(format!("missing {flag}")))?;
    raw.parse::<i64>()
        .map(Some)
        .map_err(|err| ShellError::Invalid(format!("invalid {flag}: {err}")))
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

/// Every value of a repeatable flag, in argument order. `flag_value` takes
/// only the first occurrence, which silently discards later repeatable values.
fn flag_values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == flag)
        .filter_map(|(index, _)| args.get(index + 1))
        .map(String::as_str)
        .collect()
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    let mut options = true;
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
            index += 1;
        } else if options && arg.starts_with("--") {
            index += if flag_takes_value(arg) { 2 } else { 1 };
        } else {
            values.push(arg.as_str());
            index += 1;
        }
    }
    values
}

fn positional_preserving_leading_hyphen<'a>(args: &'a [String], known: &[&str]) -> Vec<&'a str> {
    if args.first().is_some_and(|arg| {
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        arg.starts_with("--") && arg != "--" && !known.contains(&flag)
    }) {
        let mut values = vec![args[0].as_str()];
        values.extend(positional(&args[1..]));
        values
    } else {
        positional(args)
    }
}

fn flag_takes_value(flag: &str) -> bool {
    !matches!(
        flag,
        "--clear"
            | "--clear-repo"
            | "--show-secret"
            | "--redacted"
            | "--unchecked"
            | "--repair"
            | "--json"
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
        assert!(!COMMANDS.contains(&"import"));
        assert!(!COMMANDS.contains(&"import-repo"));
        assert!(COMMANDS.contains(&"update-card"));
        assert!(COMMANDS.contains(&"list-ready"));
        assert!(COMMANDS.contains(&"list-cards"));
        assert!(COMMANDS.contains(&"update-relations"));
        assert!(COMMANDS.contains(&"relations-doctor"));
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

        for retired in [
            "import",
            "import-repo",
            "import-github-issues",
            "repair-criteria",
            "papercut",
            "list-approvals",
        ] {
            let err = run(&[retired.to_string()]).unwrap_err();
            assert!(
                matches!(err, ShellError::Invalid(message) if message == format!("unknown command: {retired}"))
            );
        }
        assert!(COMMANDS.contains(&"event-tail"));
        assert!(COMMANDS.contains(&"help"));
    }

    #[test]
    fn help_command_alias_describes_commands() {
        let output = run_with_env(&args(["help", "event-tail"]), &remote_env(None, None)).unwrap();
        assert!(output.contains("usage: powder event-tail"), "{output}");
        let list_cards =
            run_with_env(&args(["help", "list-cards"]), &remote_env(None, None)).unwrap();
        assert!(list_cards.contains("--updated-after"), "{list_cards}");
        assert!(list_cards.contains("--updated-before"), "{list_cards}");
        assert!(run_with_env(&args(["help"]), &remote_env(None, None))
            .unwrap()
            .contains("  help\n"));
    }

    #[test]
    fn help_complete_card_without_db_prints_usage() {
        let output =
            run_with_env(&args(["complete-card", "--help"]), &remote_env(None, None)).unwrap();
        assert!(output.contains("usage: powder complete-card"), "{output}");
        assert!(output.contains("complete-card"), "{output}");
    }

    #[test]
    fn help_complete_card_with_card_does_not_mutate() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-help-complete-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();
        let env = remote_env(None, None);
        run_with_env(&args(["init-db", "--db", &db]), &env).unwrap();
        run_with_env(
            &args([
                "create-card",
                "--db",
                &db,
                "--id",
                "help-card",
                "--title",
                "Help card",
                "--acceptance",
                "proof exists",
                "--status",
                "in_progress",
            ]),
            &env,
        )
        .unwrap();

        let output = run_with_env(
            &args(["complete-card", "help-card", "--db", &db, "--help"]),
            &env,
        )
        .unwrap();
        assert!(output.contains("usage: powder complete-card"), "{output}");

        let card = run_with_env(&args(["get-card", "help-card", "--db", &db]), &env).unwrap();
        let detail: Value = serde_json::from_str(&card).unwrap();
        assert_eq!(detail["card"]["status"], "in_progress");
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn cli_rejects_unknown_create_card_flags_with_command_and_flag() {
        let error = run_with_env(
            &args([
                "create-card",
                "--id",
                "x",
                "--title",
                "t",
                "--body",
                "b",
                "--mystery",
                "value",
            ]),
            &remote_env(None, None),
        )
        .expect_err("unknown flags must fail before transport selection");

        assert!(matches!(
            error,
            ShellError::Invalid(message)
                if message == "unknown flag --mystery for create-card"
        ));
    }

    #[test]
    fn cli_suggests_acceptance_for_criteria_near_miss() {
        let error = run_with_env(
            &args([
                "create-card",
                "--id",
                "x",
                "--title",
                "t",
                "--body",
                "b",
                "--criteria",
                "criterion",
            ]),
            &remote_env(None, None),
        )
        .expect_err("the retired near-miss flag must fail");

        assert!(matches!(
            error,
            ShellError::Invalid(message)
                if message == "unknown flag --criteria for create-card; did you mean --acceptance?"
        ));
    }

    #[test]
    fn cli_preserves_hyphenated_positionals_and_supports_end_of_options() {
        let hyphenated_query = args(["--retry the failed search"]);
        assert_eq!(
            positional_preserving_leading_hyphen(&hyphenated_query, known_flags("search")),
            vec!["--retry the failed search"]
        );
        validate_flags("search", &hyphenated_query).unwrap();

        let after_options = args(["--", "--id"]);
        assert_eq!(positional(&after_options), vec!["--id"]);
        validate_flags("create-card", &after_options).unwrap();
    }

    #[test]
    fn update_card_clear_repo_accepts_flag_before_or_after_card_id() {
        for arguments in [
            args(["update-card", "--clear-repo", "card-id"]),
            args(["update-card", "card-id", "--clear-repo"]),
        ] {
            let (command, arguments) = arguments.split_first().unwrap();
            validate_flags(command, arguments).unwrap();
            let card_id = positional_card_id(arguments, command).unwrap();
            assert_eq!(card_id.as_str(), "card-id");
        }
    }

    #[test]
    fn cli_help_examples_only_advertise_current_statuses() {
        let help = help();
        let status_values = help
            .lines()
            .filter_map(|line| {
                let words = line.split_whitespace().collect::<Vec<_>>();
                words
                    .windows(2)
                    .find(|pair| pair[0] == "--status")
                    .map(|pair| pair[1])
            })
            .collect::<Vec<_>>();

        assert!(
            !status_values.is_empty(),
            "the help should keep at least one copy-pasteable --status example"
        );
        for status in status_values {
            assert!(
                CardStatus::parse(status).is_some(),
                "help advertises retired or invalid --status value {status}"
            );
        }
    }

    /// The whole point of `version` is catching a stale installed binary
    /// before a lane starts (powder-924): it must report the exact commit
    /// this build compiled from, not just an unchanging crate version that
    /// has sat at 0.1.0 since inception.
    ///
    /// Pins remote env explicitly (powder-cli-version-test-hermeticity):
    /// `run()` reads real process env, so on a workstation with
    /// `POWDER_API_BASE_URL` set this would make live /readyz calls and
    /// flake on a server restart. Using `run_with_env` + `remote_env(None,
    /// None)` keeps this assertion hermetic regardless of workstation env.
    #[test]
    fn cli_version_reports_the_build_commit() {
        let env = remote_env(None, None);
        let output = run_with_env(&args(["version"]), &env).unwrap();
        assert!(output.starts_with("powder 0.1.0 (git "));
        assert!(!output.contains("(git )"), "must not embed an empty sha");

        assert_eq!(run_with_env(&args(["--version"]), &env).unwrap(), output);
        assert_eq!(run_with_env(&args(["-v"]), &env).unwrap(), output);
    }

    /// Alias equivalence must also hold when a remote is configured and
    /// `version` queries /readyz for drift comparison, not just on the
    /// offline path above -- exercised against a local test server so it
    /// stays hermetic (powder-cli-version-test-hermeticity).
    #[test]
    fn cli_version_alias_equivalence_with_configured_remote() {
        let local_sha = env!("POWDER_CLI_GIT_SHA");
        let (base_url, _recorded) = spawn_test_server(vec![
            (
                200,
                json!({"ok": true, "version": "0.1.0", "git_sha": local_sha}),
            ),
            (
                200,
                json!({"ok": true, "version": "0.1.0", "git_sha": local_sha}),
            ),
            (
                200,
                json!({"ok": true, "version": "0.1.0", "git_sha": local_sha}),
            ),
        ]);
        let env = remote_env(Some(&base_url), None);

        let output = run_with_env(&args(["version"]), &env).unwrap();
        assert_eq!(run_with_env(&args(["--version"]), &env).unwrap(), output);
        assert_eq!(run_with_env(&args(["-v"]), &env).unwrap(), output);
    }

    /// powder-workstation-cli-convergence: no `POWDER_API_BASE_URL`
    /// configured must reproduce the exact prior output byte-for-byte --
    /// the drift check is additive, never a default-on behavior change.
    #[test]
    fn cli_version_adds_no_server_line_without_remote_env() {
        let output = run_with_env(&args(["version"]), &remote_env(None, None)).unwrap();
        assert!(output.starts_with("powder 0.1.0 (git "));
        assert!(!output.contains("server"));
        assert!(!output.contains("DRIFT"));
    }

    /// The whole point of the operator incident this card fixes: a stale
    /// workstation binary and a fine server, with nothing surfacing the
    /// drift. `version` must name both shas and steer toward the fix.
    #[test]
    fn cli_version_warns_on_drift_when_server_git_sha_differs() {
        let local_sha = env!("POWDER_CLI_GIT_SHA");
        let (base_url, _recorded) = spawn_test_server(vec![(
            200,
            json!({"ok": true, "version": "0.1.0", "git_sha": "deadbeefcafe"}),
        )]);

        let output = run_with_env(&args(["version"]), &remote_env(Some(&base_url), None)).unwrap();

        assert!(output.contains("DRIFT"), "{output}");
        assert!(output.contains(local_sha), "{output}");
        assert!(output.contains("deadbeefcafe"), "{output}");
        assert!(
            output.contains("scripts/install-workstation.sh"),
            "{output}"
        );
    }

    #[test]
    fn cli_version_reports_no_drift_when_server_git_sha_matches() {
        let local_sha = env!("POWDER_CLI_GIT_SHA");
        let (base_url, _recorded) = spawn_test_server(vec![(
            200,
            json!({"ok": true, "version": "0.1.0", "git_sha": local_sha}),
        )]);

        let output = run_with_env(&args(["version"]), &remote_env(Some(&base_url), None)).unwrap();

        assert!(
            output.contains(&format!("server 0.1.0 (git {local_sha})")),
            "{output}"
        );
        assert!(!output.contains("DRIFT"), "{output}");
    }

    /// A deploy that predates this card's `/readyz` fields must degrade to
    /// a plain note, not a false DRIFT (there is nothing to compare).
    #[test]
    fn cli_version_degrades_gracefully_when_server_readyz_predates_version_fields() {
        let (base_url, _recorded) = spawn_test_server(vec![(200, json!({"ok": true}))]);

        let output = run_with_env(&args(["version"]), &remote_env(Some(&base_url), None)).unwrap();

        assert!(
            output.contains("predates powder-workstation-cli-convergence"),
            "{output}"
        );
        assert!(!output.contains("DRIFT"), "{output}");
    }

    /// `powder version` must never fail just because the network is down --
    /// it degrades to a plain note instead of an error.
    #[test]
    fn cli_version_reports_unreachable_server_without_failing() {
        let output = run_with_env(
            &args(["version"]),
            &remote_env(Some("http://127.0.0.1:1"), None),
        )
        .unwrap();

        assert!(output.starts_with("powder 0.1.0 (git "), "{output}");
        assert!(output.contains("server: unreachable"), "{output}");
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
    fn cli_search_json_uses_store_contract() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-search-{}.db",
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
            "cli-search",
            "--title",
            "Needle CLI",
            "--acceptance",
            "proof exists",
            "--status",
            "backlog",
        ]))
        .unwrap();
        let output = run(&args([
            "search",
            "--json",
            "--db",
            &db,
            "--q",
            "needle",
            "--status",
            "backlog",
            "--created-after",
            "0",
        ]))
        .unwrap();
        let payload: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(payload["matches"][0]["card"]["id"], "cli-search");
        assert_eq!(payload["matches"][0]["source_kind"], "cards");
        assert_eq!(payload["total_count"], 1);

        // A positional query remains safe after value-taking flags; parser
        // must not mistake the database path or filter value for q.
        let positional_output = run(&args([
            "search", "--json", "--db", &db, "needle", "--status", "backlog", "--limit", "1",
        ]))
        .unwrap();
        let positional_payload: Value = serde_json::from_str(&positional_output).unwrap();
        assert_eq!(positional_payload["matches"][0]["card"]["id"], "cli-search");
        assert_eq!(positional_payload["total_count"], 1);

        let unquoted =
            run(&args(["search", "--json", "--db", &db, "needle", "second"])).unwrap_err();
        assert!(matches!(
            unquoted,
            ShellError::Invalid(message) if message.contains("one positional query")
        ));
        let _ = std::fs::remove_file(db);
    }

    #[test]
    fn cli_remote_search_forwards_filters_and_auth() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({
                "matches": [{
                    "card": {"id": "remote-search", "title": "Needle remote"},
                    "source_kind": "cards", "source_field": "title", "source_created_at": 10,
                    "snippet": "Needle remote", "rank": -1.0
                }],
                "total_count": 1, "has_more": false
            }),
        )]);
        let output = run_with_env(
            &args([
                "search",
                "--json",
                "--q",
                "needle",
                "--status",
                "backlog",
                "--created-after",
                "10",
                "--limit",
                "1",
            ]),
            &remote_env(Some(&base_url), Some("sk_powder_search")),
        )
        .unwrap();
        let payload: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(payload["matches"][0]["card"]["id"], "remote-search");
        let requests = recorded.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].path.contains("/api/v1/cards/search?"));
        assert!(requests[0].path.contains("q=needle"));
        assert!(requests[0].path.contains("created_after=10"));
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer sk_powder_search")
        );
    }

    #[test]
    fn cli_rejects_retired_search_source_filter() {
        let error = run(&args(["search", "--q", "needle", "--source", "cards"])).unwrap_err();
        assert!(error.to_string().contains("unknown flag --source"));
    }

    #[test]
    fn cli_remote_http_error_surfaces_body_and_stays_an_error() {
        let (base_url, _recorded) = spawn_test_server(vec![(
            403,
            json!({"error": "status changes require an operator policy approval"}),
        )]);

        let error = run_with_env(
            &args(["update-card", "card-1", "--status", "ready"]),
            &remote_env(Some(&base_url), Some("sk_powder_test")),
        )
        .expect_err("a non-2xx response must remain a CLI error");

        assert!(matches!(
            error,
            ShellError::Forbidden(message)
                if message == "status changes require an operator policy approval"
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
            "in_progress",
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
            "in-progress-1",
            "--title",
            "In progress ticket",
            "--status",
            "in_progress",
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
        assert!(all.contains("in-progress-1"));
        assert!(all.contains("ready-1"));

        let in_progress_only = run(&args([
            "list-cards",
            "--db",
            &db,
            "--status",
            "in_progress",
        ]))
        .unwrap();
        assert!(in_progress_only.contains("in-progress-1"));
        assert!(!in_progress_only.contains("ready-1"));

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
    fn cli_list_cards_filters_by_updated_at_with_ordered_unix_bounds() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-list-cards-updated-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        let mut store = open_store(&db).unwrap();
        for (id, updated_at) in [("stale-1", 100), ("fresh-1", 200)] {
            let mut card = Card::new(CardId::new(id).unwrap(), id, "").unwrap();
            card.created_at = updated_at;
            card.updated_at = updated_at;
            store
                .create_card_with_events(card, "test", updated_at)
                .unwrap();
        }

        let stale = run(&args([
            "list-cards",
            "--db",
            &db,
            "--updated-before",
            "100",
        ]))
        .unwrap();
        assert!(stale.contains("stale-1"));
        assert!(!stale.contains("fresh-1"));

        let fresh = run(&args(["list-cards", "--db", &db, "--updated-after", "200"])).unwrap();
        assert!(!fresh.contains("stale-1"));
        assert!(fresh.contains("fresh-1"));

        let bounded = run(&args([
            "list-cards",
            "--db",
            &db,
            "--updated-after",
            "100",
            "--updated-before",
            "200",
        ]))
        .unwrap();
        assert!(bounded.contains("stale-1"));
        assert!(bounded.contains("fresh-1"));

        let invalid = run(&args([
            "list-cards",
            "--db",
            &db,
            "--updated-before",
            "not-a-time",
        ]))
        .unwrap_err();
        assert!(matches!(
            invalid,
            ShellError::Invalid(message) if message.starts_with("invalid --updated-before:")
        ));

        let reversed = run(&args([
            "list-cards",
            "--db",
            &db,
            "--updated-after",
            "201",
            "--updated-before",
            "200",
        ]))
        .unwrap_err();
        assert!(matches!(
            reversed,
            ShellError::Invalid(message)
                if message == "--updated-after must be less than or equal to --updated-before"
        ));
    }

    /// powder-dogfood-2026-07-14-nonreciprocal-relations: `update-relations`
    /// mirrors onto the peer reciprocally, so `relations-doctor` finds no
    /// issues over a graph built entirely through the CLI's own public
    /// commands -- and `get-card` on the peer already shows the mirrored
    /// edge with no second `update-relations` call.
    #[test]
    fn cli_update_relations_mirrors_and_relations_doctor_reports_clean() {
        assert!(COMMANDS.contains(&"relations-doctor"));

        let db = std::env::temp_dir().join(format!(
            "powder-cli-relations-doctor-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();

        run(&args(["init-db", "--db", &db])).unwrap();
        for id in ["cli-rel-a", "cli-rel-b"] {
            run(&args([
                "create-card",
                "--db",
                &db,
                "--id",
                id,
                "--title",
                id,
                "--acceptance",
                "proof exists",
            ]))
            .unwrap();
        }

        run(&args([
            "update-relations",
            "cli-rel-a",
            "--db",
            &db,
            "--blocked-by",
            "cli-rel-b",
        ]))
        .unwrap();

        let peer = run(&args(["get-card", "cli-rel-b", "--db", &db])).unwrap();
        assert!(
            peer.contains("\"blocks\"") && peer.contains("cli-rel-a"),
            "peer output was: {peer}"
        );

        let report = run(&args(["relations-doctor", "--db", &db])).unwrap();
        assert!(report.contains("\"scanned\": 2"), "report was: {report}");
        assert!(report.contains("\"issues\": []"), "report was: {report}");
        assert!(
            report.contains("\"parent_issues\": []"),
            "report was: {report}"
        );
        assert!(
            report.contains("\"parent_repair_refusal\": null"),
            "report was: {report}"
        );
        assert!(
            report.contains("\"repaired\": false"),
            "report was: {report}"
        );

        let repaired = run(&args(["relations-doctor", "--db", &db, "--repair"])).unwrap();
        assert!(
            repaired.contains("\"issues\": []"),
            "report was: {repaired}"
        );
        assert!(
            repaired.contains("\"parent_issues\": []")
                && repaired.contains("\"parent_repair_refusal\": null"),
            "report was: {repaired}"
        );
        assert!(
            repaired.contains("\"repaired\": true"),
            "report was: {repaired}"
        );
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
    fn cli_rejects_removed_admin_flag_before_mutation() {
        let err = run(&args([
            "update-status",
            "card",
            "--status",
            "ready",
            "--admin",
        ]))
        .unwrap_err();
        assert!(matches!(
            err,
            ShellError::Invalid(message)
                if message == "--admin is not accepted; authority comes from trusted process configuration"
        ));
    }

    #[test]
    fn annotation_commands_reject_a_caller_supplied_principal_flag() {
        let env = remote_env(None, None);
        for command in [
            args([
                "check-criterion",
                "card",
                "--criterion",
                "0",
                "--actor",
                "operator",
                "--principal",
                "forged",
            ]),
            args([
                "add-link",
                "card",
                "--label",
                "proof",
                "--url",
                "https://example.test/proof",
                "--principal",
                "forged",
            ]),
            args([
                "add-comment",
                "card",
                "--author",
                "operator",
                "--body",
                "note",
                "--principal",
                "forged",
            ]),
            args([
                "append-work-log",
                "card",
                "--agent",
                "worker-a",
                "--body",
                "log",
                "--principal",
                "forged",
            ]),
        ] {
            let error = run_with_env(&command, &env).expect_err("principal flag rejected");
            assert!(error.to_string().contains("unknown flag --principal"));
        }
    }

    #[test]
    fn cli_tails_events() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-events-{}.db",
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
    }

    #[test]
    fn cli_remote_event_tail_returns_finite_sse_with_auth_and_cursor() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind SSE test server");
        let address = listener.local_addr().expect("SSE test server address");
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let body = "id: 4\nevent: card-created\ndata: {\"id\":\"remote-tail\"}\n\n";
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SSE request");
            let mut reader = BufReader::new(stream.try_clone().expect("clone SSE stream"));
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read SSE request line");
            let mut authorization = None;
            loop {
                let mut line = String::new();
                reader
                    .read_line(&mut line)
                    .expect("read SSE request header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("Authorization:") {
                    authorization = Some(value.trim().to_string());
                }
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .expect("SSE request path")
                .to_string();
            request_tx
                .send((path, authorization))
                .expect("record SSE request");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write SSE response");
        });

        let output = run_with_env(
            &args(["event-tail", "--after", "3", "--limit", "7"]),
            &remote_env(Some(&format!("http://{address}")), Some("sk_powder_tail")),
        )
        .unwrap();
        server.join().expect("SSE server must finish");

        assert_eq!(output, body);
        let (path, authorization) = request_rx.recv().expect("recorded request");
        assert_eq!(path, "/api/v1/events/tail?after=3&limit=7");
        assert_eq!(authorization.as_deref(), Some("Bearer sk_powder_tail"));
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

        let revoked = run(&args([
            "key-revoke",
            &key_id,
            "--db",
            &db,
            "--idempotency-key",
            "revoke-1",
        ]))
        .unwrap();
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

        // Same-key retry replays the keyed receipt without error.
        run(&args([
            "key-revoke",
            &key_id,
            "--db",
            &db,
            "--idempotency-key",
            "revoke-1",
        ]))
        .unwrap();

        let missing_key = run(&args(["key-revoke", &key_id, "--db", &db])).unwrap_err();
        assert!(missing_key
            .to_string()
            .contains("requires --idempotency-key"));

        let missing = run(&args([
            "key-revoke",
            "key-does-not-exist",
            "--db",
            &db,
            "--idempotency-key",
            "revoke-missing",
        ]))
        .unwrap_err();
        assert!(matches!(missing, ShellError::NotFound(_)));
    }

    /// powder-918: minting a key without saying what to do with the secret
    /// used to silently print "redacted" and throw the only copy away
    /// forever (the store never persists the raw value). `key-create` must
    /// refuse rather than guess, and the refusal must name both flags so an
    /// agent hitting this cold learns what to pass without reading source.
    #[test]
    fn cli_key_create_refuses_without_an_explicit_secret_choice() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-key-create-refusal-{}.db",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = db.to_string_lossy().to_string();
        run(&args(["init-db", "--db", &db])).unwrap();

        let err = run(&args(["key-create", "--db", &db, "--name", "codex"])).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("--show-secret") && message.contains("--redacted"),
            "refusal must name both flags: {message}"
        );

        let err_both = run(&args([
            "key-create",
            "--db",
            &db,
            "--name",
            "codex",
            "--show-secret",
            "--redacted",
        ]))
        .unwrap_err();
        assert!(
            err_both.to_string().contains("--show-secret")
                && err_both.to_string().contains("--redacted"),
            "conflicting flags must also be refused: {err_both}"
        );

        // no "codex" key was minted by either refused call (init-db already
        // seeds an unrelated bootstrap key, so the list is not itself empty).
        let listed = run(&args(["key-list", "--db", &db])).unwrap();
        assert!(
            !listed.contains("codex"),
            "a refused key-create must not persist a key: {listed}"
        );

        let redacted = run(&args([
            "key-create",
            "--db",
            &db,
            "--name",
            "codex",
            "--redacted",
        ]))
        .unwrap();
        assert!(redacted.contains("redacted"));
        assert!(
            !redacted.to_lowercase().contains("sk_"),
            "redacted output must never carry the raw secret: {redacted}"
        );

        let shown = run(&args([
            "key-create",
            "--db",
            &db,
            "--name",
            "codex2",
            "--show-secret",
        ]))
        .unwrap();
        // The store-it-now warning moved to stderr so stdout stays
        // machine-readable (`cut -f4` captures exactly the secret); stdout
        // must be the single tab-separated key line and nothing else.
        assert!(
            !shown.contains("WARNING"),
            "warning must not pollute machine-readable stdout: {shown}"
        );
        assert_eq!(
            shown.lines().count(),
            1,
            "stdout must be exactly one tab-separated line: {shown}"
        );
        assert!(shown.starts_with("api-key\t"));
        assert_eq!(shown.trim_end().split('\t').count(), 4);
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
    fn cli_actor_flag_is_semantic_for_local_admin_corrections() {
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
        ]))
        .unwrap();

        let status = run(&args([
            "update-status",
            "holder-test",
            "--db",
            &db,
            "--status",
            "in_progress",
            "--actor",
            "intruder",
        ]))
        .unwrap();
        assert!(status.contains("status\tholder-test\tin_progress"));

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
        assert!(card.contains("\"actor\": \"local-cli\""));
        assert!(card.contains("\"previous\": \"in_progress\""));
        assert!(card.contains("\"current\": \"done\""));
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
        assert!(card.contains("\"actor\": \"local-cli\""));
    }

    #[test]
    fn repeated_acceptance_flags_preserve_every_criterion_in_order() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-repeated-acceptance-{}.db",
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
            "multi-oracle",
            "--title",
            "Multi oracle",
            "--acceptance",
            "first criterion",
            "--acceptance",
            "second criterion",
        ]))
        .unwrap();

        let card = run(&args(["get-card", "multi-oracle", "--db", &db])).unwrap();
        let detail: serde_json::Value = serde_json::from_str(&card).unwrap();
        let criteria = detail["card"]["criteria"]
            .as_array()
            .unwrap()
            .iter()
            .map(|criterion| criterion["text"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(criteria, vec!["first criterion", "second criterion"]);

        // update-card: same no-silent-discard behavior, replacing the list.
        run(&args([
            "update-card",
            "multi-oracle",
            "--db",
            &db,
            "--acceptance",
            "updated first",
            "--acceptance",
            "updated second",
            "--acceptance",
            "updated third",
        ]))
        .unwrap();
        let card = run(&args(["get-card", "multi-oracle", "--db", &db])).unwrap();
        let detail: serde_json::Value = serde_json::from_str(&card).unwrap();
        let criteria = detail["card"]["criteria"]
            .as_array()
            .unwrap()
            .iter()
            .map(|criterion| criterion["text"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            criteria,
            vec!["updated first", "updated second", "updated third"]
        );

        // Single-value compatibility unchanged.
        run(&args([
            "update-card",
            "multi-oracle",
            "--db",
            &db,
            "--acceptance",
            "only one",
        ]))
        .unwrap();
        let card = run(&args(["get-card", "multi-oracle", "--db", &db])).unwrap();
        assert!(card.contains("only one"));
        assert!(!card.contains("updated second"));
    }

    #[test]
    fn cli_set_parent_links_and_get_card_shows_children() {
        let db = std::env::temp_dir().join(format!(
            "powder-cli-parent-{}.db",
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
            "parent-cli",
            "--title",
            "Parent",
            "--acceptance",
            "children land",
        ]))
        .unwrap();
        let born = run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "child-cli-a",
            "--title",
            "Child A",
            "--acceptance",
            "proof",
            "--parent",
            "parent-cli",
        ]))
        .unwrap();
        assert!(born.contains("created\tchild-cli-a"));
        run(&args([
            "create-card",
            "--db",
            &db,
            "--id",
            "child-cli-b",
            "--title",
            "Child B",
            "--acceptance",
            "proof",
        ]))
        .unwrap();

        let linked = run(&args([
            "set-parent",
            "child-cli-b",
            "--db",
            &db,
            "--parent",
            "parent-cli",
            "--actor",
            "operator",
        ]))
        .unwrap();
        assert_eq!(linked, "parent\tchild-cli-b\tparent-cli\n");

        let card = run(&args(["get-card", "parent-cli", "--db", &db])).unwrap();
        assert!(card.contains("\"children_total\": 2"));
        assert!(card.contains("\"child-cli-a\""));

        let cleared = run(&args(["set-parent", "child-cli-b", "--db", &db, "--clear"])).unwrap();
        assert_eq!(cleared, "parent\tchild-cli-b\tnone\n");

        let both = run(&args([
            "set-parent",
            "child-cli-b",
            "--db",
            &db,
            "--parent",
            "parent-cli",
            "--clear",
        ]));
        assert!(both.is_err(), "exactly one of --parent/--clear");
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
            "in_progress",
        ]))
        .unwrap();
        run(&args([
            "add-link",
            "answer-test",
            "--db",
            &db,
            "--label",
            "approval/packet",
            "--url",
            "https://example.test/packet",
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
    fn cli_idempotency_key_override_is_stable_and_conflicts_fail() {
        let explicit = args([
            "update-status",
            "card-1",
            "--status",
            "done",
            "--idempotency-key",
            "replay-1",
        ]);
        assert_eq!(idempotency_key(&explicit).unwrap(), "replay-1");
        let repeated = args([
            "update-status",
            "card-1",
            "--status",
            "done",
            "--idempotency-key",
            "replay-1",
            "--idempotency-key",
            "replay-1",
        ]);
        assert_eq!(idempotency_key(&repeated).unwrap(), "replay-1");
        let conflicting = args([
            "update-status",
            "card-1",
            "--status",
            "done",
            "--idempotency-key",
            "replay-1",
            "--idempotency-key",
            "replay-2",
        ]);
        let error = idempotency_key(&conflicting).unwrap_err().to_string();
        assert!(error.contains("conflicting --idempotency-key"));
        let empty = args([
            "update-status",
            "card-1",
            "--status",
            "done",
            "--idempotency-key",
            "",
        ]);
        assert!(idempotency_key(&empty)
            .unwrap_err()
            .to_string()
            .contains("requires a non-empty"));
        let missing = args([
            "update-status",
            "card-1",
            "--status",
            "done",
            "--idempotency-key",
        ]);
        assert!(idempotency_key(&missing)
            .unwrap_err()
            .to_string()
            .contains("requires a value"));
        let generated = args(["update-status", "card-1", "--status", "done"]);
        let first = idempotency_key(&generated).unwrap();
        let second = idempotency_key(&generated).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("powder-cli-"));
    }

    #[test]
    fn cli_list_cards_remote_filters_by_updated_at_on_fetched_page() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({
                "cards": [
                    {"id": "stale-remote", "priority": "p1", "status": "backlog", "title": "Stale remote", "updated_at": 100},
                    {"id": "fresh-remote", "priority": "p2", "status": "backlog", "title": "Fresh remote", "updated_at": 200}
                ],
                "total_count": 2,
                "has_more": false
            }),
        )]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));
        let output = run_with_env(
            &args(["list-cards", "--limit", "2", "--updated-before", "100"]),
            &env,
        )
        .unwrap();
        assert_eq!(output, "stale-remote\tP1\tbacklog\tStale remote\n");

        let requests = recorded.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/cards?limit=2");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer sk_powder_test")
        );
    }

    #[test]
    fn cli_remote_mode_uses_http_for_the_accepted_card_commands() {
        let (base_url, recorded) = spawn_test_server(vec![
            (
                200,
                json!({
                    "cards": [{"id": "remote-1", "priority": "p0", "title": "Remote ready"}],
                    "total_count": 1,
                    "has_more": false
                }),
            ),
            (
                200,
                json!({
                    "cards": [{"id": "in-progress-1", "priority": "p2", "status": "in_progress", "title": "In progress"}],
                    "total_count": 1,
                    "has_more": false
                }),
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
                json!({"id": "remote-created", "priority": "p1", "status": "in_progress", "title": "Remote created"}),
            ),
            (
                200,
                json!({"id": "remote-created", "priority": "p1", "status": "in_progress", "title": "Remote created"}),
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
                "in_progress",
                "--repo",
                "misty-step/powder",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(cards, "in-progress-1\tP2\tin_progress\tIn progress\n");

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
                "--idempotency-key",
                "create-replay",
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
            &args(["update-status", "remote-created", "--status", "in_progress"]),
            &env,
        )
        .unwrap();
        assert_eq!(status, "status\tremote-created\tin_progress\n");

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
                "GET /api/v1/cards?limit=2&status=in_progress&repo=misty-step%2Fpowder",
                "GET /api/v1/cards/remote-1?detail=detailed",
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
            requests[3].idempotency_key.as_deref(),
            Some("create-replay")
        );
        assert!(requests[4].idempotency_key.is_none());
        for index in [5usize, 6, 7] {
            assert!(requests[index].idempotency_key.is_some());
        }
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
        assert_eq!(requests[5].body, Some(json!({"status": "in_progress"})));
        assert_eq!(
            requests[6].body,
            Some(json!({"criterion": 0, "actor": "operator", "checked": true}))
        );
        assert_eq!(
            requests[7].body,
            Some(json!({"author": "operator", "body": "looks good"}))
        );
    }

    /// Client read paths must degrade per-card on an unknown status value;
    /// the CLI remote path keeps responses as `serde_json::Value` and simply
    /// prints the raw status string, so a future vocabulary addition never
    /// aborts the whole listing.
    #[test]
    fn cli_remote_listings_tolerate_unknown_status_values() {
        let (base_url, _recorded) = spawn_test_server(vec![
            (
                200,
                json!({
                    "cards": [
                        {"id": "known-1", "priority": "p1", "title": "Known card"},
                        {"id": "future-1", "priority": "p2", "title": "Future status card"},
                    ],
                    "total_count": 2,
                    "has_more": false,
                }),
            ),
            (
                200,
                json!({
                    "cards": [
                        {"id": "known-1", "priority": "p1", "status": "ready", "title": "Known card"},
                        {"id": "future-1", "priority": "p2", "status": "paused", "title": "Future status card"},
                    ],
                    "total_count": 2,
                    "has_more": false,
                }),
            ),
        ]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));

        let ready = run_with_env(&args(["list-ready", "--limit", "5"]), &env).unwrap();
        assert_eq!(
            ready,
            "known-1\tP1\tKnown card\nfuture-1\tP2\tFuture status card\n"
        );

        let cards = run_with_env(&args(["list-cards", "--limit", "5"]), &env).unwrap();
        assert_eq!(
            cards,
            "known-1\tP1\tready\tKnown card\nfuture-1\tP2\tpaused\tFuture status card\n"
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
    fn cli_remote_mode_answers_input_without_db() {
        let (base_url, recorded) = spawn_test_server(vec![(
            200,
            json!({"id": "run-approval", "card_id": "approval-1", "state": "active"}),
        )]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));

        let answered = run_with_env(
            &args([
                "answer-input",
                "run-approval",
                "--actor",
                "operator",
                "--answer",
                "Approved",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(answered, "answered-input\trun-approval\tapproval-1\n");

        let requests = recorded.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            format!("{} {}", requests[0].method, requests[0].path),
            "POST /api/v1/runs/run-approval/answer"
        );
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer sk_powder_test")
        );
        assert_eq!(
            requests[0].body,
            Some(json!({"actor": "operator", "answer": "Approved"}))
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
    fn cli_remote_mode_uses_http_for_relations_parent_and_runs() {
        let (base_url, recorded) = spawn_test_server(vec![
            (200, json!({"id": "rel-a", "status": "ready"})),
            (
                200,
                json!({"id": "child-a", "parent": "rel-a", "status": "ready"}),
            ),
            (
                200,
                json!({"state": "active", "card_id": "rel-a", "answer": "Approved"}),
            ),
            (200, json!({"awaiting": [{"id": "run-a"}]})),
        ]);
        let env = remote_env(Some(&base_url), Some("sk_powder_test"));

        let relations = run_with_env(
            &args([
                "update-relations",
                "rel-a",
                "--related",
                "rel-b",
                "--blocks",
                "rel-c",
                "--idempotency-key",
                "rel-key",
            ]),
            &env,
        )
        .unwrap();
        assert_eq!(relations, "relations\trel-a\n");

        let parent =
            run_with_env(&args(["set-parent", "child-a", "--parent", "rel-a"]), &env).unwrap();
        assert_eq!(parent, "parent\tchild-a\trel-a\n");

        let run = run_with_env(&args(["get-run", "run-a"]), &env).unwrap();
        assert!(run.contains("\"state\": \"active\""));

        let awaiting = run_with_env(&args(["list-awaiting-input"]), &env).unwrap();
        assert!(awaiting.contains("\"awaiting\""));
        assert!(awaiting.contains("run-a"));

        let requests = recorded.lock().unwrap();
        let paths = requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "POST /api/v1/cards/rel-a/relations",
                "POST /api/v1/cards/child-a/parent",
                "GET /api/v1/runs/run-a?detail=detailed",
                "GET /api/v1/runs/awaiting-input?limit=20",
            ]
        );
        assert_eq!(
            requests[0].body,
            Some(json!({"related": ["rel-b"], "blocks": ["rel-c"], "blocked_by": []}))
        );
        assert_eq!(requests[0].idempotency_key.as_deref(), Some("rel-key"));
        assert_eq!(requests[1].body, Some(json!({"parent": "rel-a"})));
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
        idempotency_key: Option<String>,
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
                let mut idempotency_key = None;
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
                    if let Some(value) = header_line.strip_prefix("Idempotency-Key:") {
                        idempotency_key = Some(value.trim().to_string());
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
                    idempotency_key,
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
