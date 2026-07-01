#![forbid(unsafe_code)]

use std::{fs, path::Path};

use bcrypt::verify;
use powder_core::{
    Activity, ActivityId, ActivityType, Card, CardId, CardSource, CardStatus, Claim, ClaimReceipt,
    DomainError, Link, LinkId, Priority, ReadyQuery, Run, RunId, RunState,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};

mod schema;
#[cfg(test)]
mod tests;

use schema::{
    CARD_COLUMNS, CARD_SELECT_ALL_SQL, CARD_SELECT_SQL, RUN_SELECT_SQL, SCHEMA, SCHEMA_VERSION,
};

pub type Result<T> = std::result::Result<T, StoreError>;

const API_KEY_PREFIX_LEN: usize = 12;
const BOOTSTRAP_SEED: &str = "initial_config_v1";
const DUMMY_BCRYPT_HASH: &str = "$2b$12$C6UzMDM.H6dfI/f/IKcEeO6H9G7Qe0eeDVF2.oTu.2R4z.0/t6j2K";
const API_KEY_ALPHABET: [char; 64] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B',
    'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U',
    'V', 'W', 'X', 'Y', 'Z', '_', '-',
];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("secret hash error: {0}")]
    SecretHash(#[from] bcrypt::BcryptError),
    #[error("{0}")]
    Domain(#[from] DomainError),
    #[error("unsupported schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("stored {field} value is invalid: {value}")]
    InvalidStoredValue { field: &'static str, value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyScope {
    Admin,
    Agent,
}

impl ApiKeyScope {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(Self::Admin),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Agent => "agent",
        }
    }

    pub fn allows_agent(self) -> bool {
        matches!(self, Self::Admin | Self::Agent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyCreated {
    pub id: String,
    pub name: String,
    pub scope: ApiKeyScope,
    pub key_prefix: String,
    pub raw_key: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedApiKey {
    pub id: String,
    pub name: String,
    pub scope: ApiKeyScope,
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(connection: Connection) -> Result<Self> {
        let store = Self { connection };
        store.connection.pragma_update(None, "foreign_keys", "ON")?;
        store.connection.pragma_update(None, "busy_timeout", 5000)?;
        let _mode: String = store
            .connection
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        store
            .connection
            .pragma_update(None, "synchronous", "NORMAL")?;
        Ok(store)
    }

    pub fn migrate(&mut self) -> Result<()> {
        let current = self.schema_version()?;
        if current > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema(current));
        }
        if current == SCHEMA_VERSION {
            return Ok(());
        }

        self.connection.execute_batch(SCHEMA)?;
        self.connection
            .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        Ok(())
    }

    pub fn readiness_check(&self) -> Result<()> {
        self.connection.query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn journal_mode(&self) -> Result<String> {
        Ok(self
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?)
    }

    pub fn apply_initial_seed(&mut self, now: i64) -> Result<Option<ApiKeyCreated>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already_applied = transaction
            .query_row(
                "SELECT 1 FROM seed_runs WHERE seed_name = ?1 LIMIT 1",
                [BOOTSTRAP_SEED],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        if already_applied {
            transaction.commit()?;
            return Ok(None);
        }

        let key = new_api_key("bootstrap", ApiKeyScope::Admin, now)?;
        insert_api_key(&transaction, &key)?;
        transaction.execute(
            "INSERT INTO seed_runs (seed_name, applied_at) VALUES (?1, ?2)",
            params![BOOTSTRAP_SEED, now],
        )?;
        transaction.commit()?;
        Ok(Some(key))
    }

    pub fn create_api_key(
        &mut self,
        name: &str,
        scope: ApiKeyScope,
        now: i64,
    ) -> Result<ApiKeyCreated> {
        let name = non_empty("name", name)?;
        let key = new_api_key(&name, scope, now)?;
        insert_api_key(&self.connection, &key)?;
        Ok(key)
    }

    pub fn verify_api_key(&self, raw_key: &str) -> Result<Option<VerifiedApiKey>> {
        let prefix = key_prefix(raw_key);
        let mut statement = self.connection.prepare(
            "SELECT id, name, scope, key_hash
             FROM api_keys
             WHERE key_prefix = ?1 AND revoked_at IS NULL
             ORDER BY created_at ASC, id ASC",
        )?;
        let candidates = statement
            .query_map([prefix], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        if candidates.is_empty() {
            let _ = verify(raw_key, DUMMY_BCRYPT_HASH);
            return Ok(None);
        }

        for (id, name, scope, key_hash) in candidates {
            if matches!(verify(raw_key, &key_hash), Ok(true)) {
                let scope = ApiKeyScope::parse(&scope).ok_or(StoreError::InvalidStoredValue {
                    field: "api_keys.scope",
                    value: scope,
                })?;
                return Ok(Some(VerifiedApiKey { id, name, scope }));
            }
        }
        Ok(None)
    }

    pub fn active_api_key_count(&self) -> Result<u64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL",
            [],
            |row| row.get::<_, u64>(0),
        )?)
    }

    pub fn import_cards(&mut self, cards: Vec<Card>) -> Result<usize> {
        let count = cards.len();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for card in cards {
            persist_card(&transaction, &card)?;
        }
        transaction.commit()?;
        Ok(count)
    }

    pub fn upsert_card(&mut self, card: Card) -> Result<Card> {
        persist_card(&self.connection, &card)?;
        Ok(card)
    }

    pub fn get_card(&self, card_id: &CardId) -> Result<Option<Card>> {
        let record = self
            .connection
            .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
            .optional()?;
        record.map(CardRecord::into_card).transpose()
    }

    pub fn get_run(&self, run_id: &RunId) -> Result<Option<Run>> {
        let record = self
            .connection
            .query_row(RUN_SELECT_SQL, [run_id.as_str()], RunRecord::from_row)
            .optional()?;
        record.map(RunRecord::into_run).transpose()
    }

    pub fn list_ready(&self, query: ReadyQuery) -> Result<Vec<Card>> {
        let mut statement = self.connection.prepare(CARD_SELECT_ALL_SQL)?;
        let records = statement
            .query_map([], CardRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cards = records
            .into_iter()
            .map(CardRecord::into_card)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|card| card.is_ready_at(query.now))
            .collect::<Vec<_>>();

        cards.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        cards.truncate(query.limit);
        Ok(cards)
    }

    pub fn claim_card(
        &mut self,
        card_id: &CardId,
        agent: &str,
        now: i64,
        ttl_seconds: u64,
    ) -> Result<ClaimReceipt> {
        let agent = non_empty("agent", agent)?;
        if ttl_seconds == 0 {
            return Err(DomainError::validation(
                "ttl_seconds",
                "claim ttl must be greater than zero",
            )
            .into());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;

        if let Some(claim) = &card.claim {
            if !claim.is_expired(now) {
                return Err(DomainError::conflict(format!(
                    "card {card_id} is already claimed by {} until {}",
                    claim.agent, claim.expires_at
                ))
                .into());
            }
        }
        if !card.can_be_claimed_at(now) {
            return Err(
                DomainError::conflict(format!("card {card_id} is not ready to claim")).into(),
            );
        }

        transaction.execute(
            "UPDATE runs
             SET state = 'stale', updated_at = ?2
             WHERE card_id = ?1
               AND state IN ('active', 'pending')
               AND claim_expires_at <= ?2",
            params![card_id.as_str(), now],
        )?;

        let run_id = RunId::new(format!("run-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?;
        let expires_at = now + ttl_seconds as i64;
        card.status.validate_transition(CardStatus::Claimed)?;
        card.status = CardStatus::Claimed;
        card.claim = Some(Claim {
            agent: agent.clone(),
            run_id: run_id.clone(),
            acquired_at: now,
            expires_at,
        });
        card.updated_at = now;
        persist_card(&transaction, &card)?;

        let run = Run {
            id: run_id.clone(),
            card_id: card_id.clone(),
            state: RunState::Active,
            agent: agent.clone(),
            model: None,
            claim_expires_at: expires_at,
            turn_count: 0,
            token_count: 0,
            consecutive_failures: 0,
            last_error: None,
            result: None,
            proof: None,
            created_at: now,
            updated_at: now,
        };
        persist_run(&transaction, &run)?;
        append_activity(
            &transaction,
            &run_id,
            ActivityType::Action,
            &format!("claimed {card_id}"),
            now,
        )?;
        transaction.commit()?;

        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            run_id,
            agent,
            expires_at,
        })
    }

    pub fn update_status(
        &mut self,
        card_id: &CardId,
        status: CardStatus,
        now: i64,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        card.status.validate_transition(status)?;
        if status.is_terminal() {
            card.claim = None;
        }
        card.status = status;
        card.updated_at = now;
        persist_card(&transaction, &card)?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn add_link(&mut self, card_id: &CardId, label: &str, url: &str, now: i64) -> Result<Link> {
        if self.get_card(card_id)?.is_none() {
            return Err(DomainError::not_found("card", card_id.to_string()).into());
        }
        let link = Link {
            id: LinkId::new(format!("link-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?,
            card_id: card_id.clone(),
            label: non_empty("label", label)?,
            url: non_empty("url", url)?,
            created_at: now,
        };
        self.connection.execute(
            "INSERT INTO links (id, card_id, label, url, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                link.id.as_str(),
                link.card_id.as_str(),
                link.label,
                link.url,
                link.created_at
            ],
        )?;
        Ok(link)
    }

    pub fn request_input(&mut self, run_id: &RunId, question: &str, now: i64) -> Result<Run> {
        let question = non_empty("question", question)?;
        let mut run = self
            .get_run(run_id)?
            .ok_or_else(|| DomainError::not_found("run", run_id.to_string()))?;
        let mut card = load_card(&self.connection, &run.card_id)?;

        card.status.validate_transition(CardStatus::AwaitingInput)?;
        card.status = CardStatus::AwaitingInput;
        card.updated_at = now;
        run.state = RunState::AwaitingInput;
        run.updated_at = now;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        persist_card(&transaction, &card)?;
        persist_run(&transaction, &run)?;
        append_activity(
            &transaction,
            run_id,
            ActivityType::Elicitation,
            &question,
            now,
        )?;
        transaction.commit()?;
        Ok(run)
    }

    pub fn complete_card(&mut self, card_id: &CardId, proof: &str, now: i64) -> Result<Card> {
        let proof = non_empty("proof", proof)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;

        if !card.status.can_complete() {
            return Err(DomainError::conflict(format!(
                "card {card_id} cannot complete from {}",
                card.status.as_str()
            ))
            .into());
        }
        if card
            .claim
            .as_ref()
            .is_none_or(|claim| claim.is_expired(now))
        {
            return Err(DomainError::conflict(format!(
                "card {card_id} requires an active claim before completion"
            ))
            .into());
        }

        let run_id = transaction
            .query_row(
                "SELECT id FROM runs
                 WHERE card_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                [card_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| {
                DomainError::conflict(format!("card {card_id} has no run to complete"))
            })?;
        let run_id = RunId::new(run_id)?;

        card.status = CardStatus::Done;
        card.claim = None;
        card.updated_at = now;
        persist_card(&transaction, &card)?;
        transaction.execute(
            "UPDATE runs
             SET state = 'complete', proof = ?2, updated_at = ?3
             WHERE id = ?1",
            params![run_id.as_str(), proof, now],
        )?;
        append_activity(
            &transaction,
            &run_id,
            ActivityType::Response,
            &format!("completed: {proof}"),
            now,
        )?;
        transaction.commit()?;
        Ok(card)
    }
}

fn persist_card(connection: &Connection, card: &Card) -> Result<()> {
    let source_path = card.source.as_ref().map(|source| source.path.as_str());
    let source_digest = card.source.as_ref().map(|source| source.digest.as_str());
    let claim_agent = card.claim.as_ref().map(|claim| claim.agent.as_str());
    let claim_run_id = card.claim.as_ref().map(|claim| claim.run_id.as_str());
    let claim_acquired_at = card.claim.as_ref().map(|claim| claim.acquired_at);
    let claim_expires_at = card.claim.as_ref().map(|claim| claim.expires_at);

    connection.execute(
        &format!(
            "INSERT INTO cards ({CARD_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               body = excluded.body,
               acceptance_json = excluded.acceptance_json,
               status = excluded.status,
               priority = excluded.priority,
               labels_json = excluded.labels_json,
               assignee = excluded.assignee,
               blocked_by_json = excluded.blocked_by_json,
               repo = excluded.repo,
               workspace_path = excluded.workspace_path,
               branch_name = excluded.branch_name,
               source_path = excluded.source_path,
               source_digest = excluded.source_digest,
               claim_agent = excluded.claim_agent,
               claim_run_id = excluded.claim_run_id,
               claim_acquired_at = excluded.claim_acquired_at,
               claim_expires_at = excluded.claim_expires_at,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at"
        ),
        params![
            card.id.as_str(),
            card.title,
            card.body,
            to_json(&card.acceptance)?,
            card.status.as_str(),
            card.priority.as_str(),
            to_json(&card.labels)?,
            card.assignee,
            to_json(&card.blocked_by)?,
            card.repo,
            card.workspace_path,
            card.branch_name,
            source_path,
            source_digest,
            claim_agent,
            claim_run_id,
            claim_acquired_at,
            claim_expires_at,
            card.created_at,
            card.updated_at
        ],
    )?;
    Ok(())
}

fn persist_run(connection: &Connection, run: &Run) -> Result<()> {
    connection.execute(
        "INSERT INTO runs (
            id, card_id, state, agent, model, claim_expires_at, turn_count,
            token_count, consecutive_failures, last_error, result, proof,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
           card_id = excluded.card_id,
           state = excluded.state,
           agent = excluded.agent,
           model = excluded.model,
           claim_expires_at = excluded.claim_expires_at,
           turn_count = excluded.turn_count,
           token_count = excluded.token_count,
           consecutive_failures = excluded.consecutive_failures,
           last_error = excluded.last_error,
           result = excluded.result,
           proof = excluded.proof,
           created_at = excluded.created_at,
           updated_at = excluded.updated_at",
        params![
            run.id.as_str(),
            run.card_id.as_str(),
            run.state.as_str(),
            run.agent,
            run.model,
            run.claim_expires_at,
            run.turn_count,
            run.token_count,
            run.consecutive_failures,
            run.last_error,
            run.result,
            run.proof,
            run.created_at,
            run.updated_at
        ],
    )?;
    Ok(())
}

fn append_activity(
    connection: &Connection,
    run_id: &RunId,
    activity_type: ActivityType,
    payload: &str,
    now: i64,
) -> Result<Activity> {
    let activity = Activity {
        id: ActivityId::new(format!(
            "activity-{}",
            nanoid::nanoid!(12, &API_KEY_ALPHABET)
        ))?,
        run_id: run_id.clone(),
        activity_type,
        payload: payload.to_owned(),
        created_at: now,
    };
    connection.execute(
        "INSERT INTO activities (id, run_id, activity_type, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            activity.id.as_str(),
            activity.run_id.as_str(),
            activity.activity_type.as_str(),
            activity.payload,
            activity.created_at
        ],
    )?;
    Ok(activity)
}

fn load_card(connection: &Connection, card_id: &CardId) -> Result<Card> {
    connection
        .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
        .optional()?
        .ok_or_else(|| DomainError::not_found("card", card_id.to_string()).into())
        .and_then(CardRecord::into_card)
}

fn new_api_key(name: &str, scope: ApiKeyScope, now: i64) -> Result<ApiKeyCreated> {
    let raw_key = format!("sk_powder_{}", nanoid::nanoid!(32, &API_KEY_ALPHABET));
    Ok(ApiKeyCreated {
        id: format!("key-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)),
        name: name.to_owned(),
        scope,
        key_prefix: key_prefix(&raw_key),
        raw_key,
        created_at: now,
    })
}

fn insert_api_key(connection: &Connection, key: &ApiKeyCreated) -> Result<()> {
    let key_hash = bcrypt::hash(&key.raw_key, bcrypt::DEFAULT_COST)?;
    connection.execute(
        "INSERT INTO api_keys (id, name, key_prefix, key_hash, scope, created_at, revoked_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
        params![
            key.id,
            key.name,
            key.key_prefix,
            key_hash,
            key.scope.as_str(),
            key.created_at
        ],
    )?;
    Ok(())
}

fn key_prefix(raw_key: &str) -> String {
    raw_key.chars().take(API_KEY_PREFIX_LEN).collect()
}

fn to_json(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn from_json<T: DeserializeOwned>(field: &'static str, raw: String) -> Result<T> {
    serde_json::from_str(&raw).map_err(|err| StoreError::InvalidStoredValue {
        field,
        value: err.to_string(),
    })
}

fn non_empty(field: &'static str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(DomainError::validation(field, "value cannot be empty").into())
    } else {
        Ok(trimmed.to_owned())
    }
}

struct CardRecord {
    id: String,
    title: String,
    body: String,
    acceptance_json: String,
    status: String,
    priority: String,
    labels_json: String,
    assignee: Option<String>,
    blocked_by_json: String,
    repo: Option<String>,
    workspace_path: Option<String>,
    branch_name: Option<String>,
    source_path: Option<String>,
    source_digest: Option<String>,
    claim_agent: Option<String>,
    claim_run_id: Option<String>,
    claim_acquired_at: Option<i64>,
    claim_expires_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

impl CardRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            title: row.get(1)?,
            body: row.get(2)?,
            acceptance_json: row.get(3)?,
            status: row.get(4)?,
            priority: row.get(5)?,
            labels_json: row.get(6)?,
            assignee: row.get(7)?,
            blocked_by_json: row.get(8)?,
            repo: row.get(9)?,
            workspace_path: row.get(10)?,
            branch_name: row.get(11)?,
            source_path: row.get(12)?,
            source_digest: row.get(13)?,
            claim_agent: row.get(14)?,
            claim_run_id: row.get(15)?,
            claim_acquired_at: row.get(16)?,
            claim_expires_at: row.get(17)?,
            created_at: row.get(18)?,
            updated_at: row.get(19)?,
        })
    }

    fn into_card(self) -> Result<Card> {
        let mut card = Card::new(CardId::new(self.id)?, self.title, self.body)?
            .with_acceptance(from_json::<Vec<String>>(
                "cards.acceptance_json",
                self.acceptance_json,
            )?)
            .with_status(
                CardStatus::parse(&self.status).ok_or(StoreError::InvalidStoredValue {
                    field: "cards.status",
                    value: self.status,
                })?,
            )
            .with_priority(Priority::parse(&self.priority).ok_or(
                StoreError::InvalidStoredValue {
                    field: "cards.priority",
                    value: self.priority,
                },
            )?)
            .with_created_at(self.created_at);
        card.labels = from_json("cards.labels_json", self.labels_json)?;
        card.assignee = self.assignee;
        card.blocked_by = from_json("cards.blocked_by_json", self.blocked_by_json)?;
        card.repo = self.repo;
        card.workspace_path = self.workspace_path;
        card.branch_name = self.branch_name;
        card.source = match (self.source_path, self.source_digest) {
            (Some(path), Some(digest)) => Some(CardSource { path, digest }),
            _ => None,
        };
        card.claim = match (
            self.claim_agent,
            self.claim_run_id,
            self.claim_acquired_at,
            self.claim_expires_at,
        ) {
            (Some(agent), Some(run_id), Some(acquired_at), Some(expires_at)) => Some(Claim {
                agent,
                run_id: RunId::new(run_id)?,
                acquired_at,
                expires_at,
            }),
            _ => None,
        };
        card.updated_at = self.updated_at;
        Ok(card)
    }
}

struct RunRecord {
    id: String,
    card_id: String,
    state: String,
    agent: String,
    model: Option<String>,
    claim_expires_at: i64,
    turn_count: u32,
    token_count: u64,
    consecutive_failures: u32,
    last_error: Option<String>,
    result: Option<String>,
    proof: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl RunRecord {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            card_id: row.get(1)?,
            state: row.get(2)?,
            agent: row.get(3)?,
            model: row.get(4)?,
            claim_expires_at: row.get(5)?,
            turn_count: row.get(6)?,
            token_count: row.get(7)?,
            consecutive_failures: row.get(8)?,
            last_error: row.get(9)?,
            result: row.get(10)?,
            proof: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
        })
    }

    fn into_run(self) -> Result<Run> {
        Ok(Run {
            id: RunId::new(self.id)?,
            card_id: CardId::new(self.card_id)?,
            state: RunState::parse(&self.state).ok_or(StoreError::InvalidStoredValue {
                field: "runs.state",
                value: self.state,
            })?,
            agent: self.agent,
            model: self.model,
            claim_expires_at: self.claim_expires_at,
            turn_count: self.turn_count,
            token_count: self.token_count,
            consecutive_failures: self.consecutive_failures,
            last_error: self.last_error,
            result: self.result,
            proof: self.proof,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
