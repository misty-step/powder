#![forbid(unsafe_code)]

use std::{collections::HashMap, fs, path::Path};

use powder_core::{
    canonical_repo_label, Activity, ActivityId, ActivityType, Authority, Card, CardEvent,
    CardEventId, CardId, CardSource, CardStatus, Claim, ClaimReceipt, Comment, DomainError, Link,
    LinkId, Priority, ReadyQuery, Run, RunId, RunState,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;

mod answer_loop;
mod events;
mod identity;
mod repositories;
mod schema;
pub mod status_model_020;
#[cfg(test)]
mod tests;

pub use events::{
    CardEventEnvelope, DeadLetterDelivery, EventSubscription, EventSubscriptionCreated,
    EventTailItem, WebhookDelivery, CARD_EVENT_SCHEMA_VERSION, EVENT_TYPES,
};
pub use identity::{Actor, ActorKind, ApiKeyCreated, ApiKeyScope, ApiKeySummary, VerifiedApiKey};
pub use repositories::RepositorySummary;
use repositories::{summarize_repository_rows, RepositoryRow};

use schema::{
    CARD_COLUMNS, CARD_SELECT_ALL_SQL, CARD_SELECT_SQL, MIGRATE_1_TO_2, MIGRATE_2_TO_3,
    MIGRATE_3_TO_4, MIGRATE_4_TO_5, MIGRATE_5_TO_6, RUN_SELECT_SQL, SCHEMA, SCHEMA_VERSION,
};

pub type Result<T> = std::result::Result<T, StoreError>;

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

pub struct Store {
    connection: Connection,
}

/// Filter for [`Store::list_cards`]: `None` on either field means
/// unfiltered on that dimension.
#[derive(Debug, Clone, Default)]
pub struct CardFilter {
    pub status: Option<CardStatus>,
    pub repo: Option<String>,
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

    /// Applies migrations one version at a time until reaching
    /// `SCHEMA_VERSION`, so a database several versions behind steps through
    /// every intermediate migration rather than jumping straight to current
    /// while skipping schema changes those steps introduced.
    pub fn migrate(&mut self) -> Result<()> {
        loop {
            let current = self.schema_version()?;
            if current > SCHEMA_VERSION {
                return Err(StoreError::UnsupportedSchema(current));
            }
            if current == SCHEMA_VERSION {
                return Ok(());
            }
            let next = match current {
                0 => {
                    self.connection.execute_batch(SCHEMA)?;
                    SCHEMA_VERSION
                }
                1 => {
                    self.connection.execute_batch(MIGRATE_1_TO_2)?;
                    2
                }
                2 => {
                    self.connection.execute_batch(MIGRATE_2_TO_3)?;
                    3
                }
                3 => {
                    self.connection.execute_batch(MIGRATE_3_TO_4)?;
                    4
                }
                4 => {
                    self.connection.execute_batch(MIGRATE_4_TO_5)?;
                    5
                }
                5 => {
                    self.connection.execute_batch(MIGRATE_5_TO_6)?;
                    6
                }
                _ => return Err(StoreError::UnsupportedSchema(current)),
            };
            self.connection
                .execute_batch(&format!("PRAGMA user_version = {next}"))?;
        }
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

    /// Import backlog.d cards without clobbering live lifecycle state: a
    /// card that is claimed, running, awaiting input, or already at a
    /// terminal outcome keeps its stored status/claim, while its content
    /// (title, body, acceptance, labels, source digest, ...) still refreshes
    /// from the freshly parsed file. See [`Card::merge_reimport`].
    pub fn import_cards(&mut self, cards: Vec<Card>) -> Result<ImportOutcome> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcome = ImportOutcome::default();
        for incoming in cards {
            match load_card_optional(&transaction, &incoming.id)? {
                None => {
                    persist_card(&transaction, &incoming)?;
                    outcome.created += 1;
                }
                Some(current) => {
                    let class = classify_reimport(&current, &incoming);
                    persist_card(&transaction, &current.merge_reimport(incoming))?;
                    outcome.record(class);
                }
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    pub fn import_cards_with_events(
        &mut self,
        cards: Vec<Card>,
        actor: &str,
        now: i64,
    ) -> Result<ImportOutcome> {
        let actor = non_empty("actor", actor)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcome = ImportOutcome::default();
        for incoming in cards {
            match load_card_optional(&transaction, &incoming.id)? {
                None => {
                    persist_card(&transaction, &incoming)?;
                    append_card_event(
                        &transaction,
                        &incoming.id,
                        "create",
                        &actor,
                        "imported card",
                        now,
                    )?;
                    events::append_outbound_card_event(
                        &transaction,
                        &incoming,
                        "card-created",
                        &actor,
                        json!({"source": "import"}),
                        now,
                    )?;
                    outcome.created += 1;
                }
                Some(current) => {
                    let class = classify_reimport(&current, &incoming);
                    let merged = current.merge_reimport(incoming);
                    let previous = current.status;
                    persist_card(&transaction, &merged)?;
                    outcome.record(class);
                    if let Some(event_type) =
                        events::outbound_event_for_status_change(previous, merged.status)
                    {
                        append_card_event(
                            &transaction,
                            &merged.id,
                            "status",
                            &actor,
                            &format!("{} -> {}", previous.as_str(), merged.status.as_str()),
                            now,
                        )?;
                        events::append_outbound_card_event(
                            &transaction,
                            &merged,
                            event_type,
                            &actor,
                            json!({
                                "previous_status": previous.as_str(),
                                "status": merged.status.as_str(),
                                "source": "import"
                            }),
                            now,
                        )?;
                    }
                }
            }
        }
        transaction.commit()?;
        Ok(outcome)
    }

    /// Compute what [`Store::import_cards`] would do to `cards` without
    /// writing anything, so a caller can show a create/update/preserve/
    /// unchanged report before committing to the import.
    pub fn preview_import(&self, cards: &[Card]) -> Result<ImportOutcome> {
        let mut outcome = ImportOutcome::default();
        for incoming in cards {
            match load_card_optional(&self.connection, &incoming.id)? {
                None => outcome.created += 1,
                Some(current) => outcome.record(classify_reimport(&current, incoming)),
            }
        }
        Ok(outcome)
    }

    pub fn upsert_card(&mut self, card: Card) -> Result<Card> {
        let card_id = card.id.clone();
        persist_card(&self.connection, &card)?;
        load_card(&self.connection, &card_id)
    }

    pub fn upsert_card_with_events(&mut self, card: Card, actor: &str, now: i64) -> Result<Card> {
        let actor = non_empty("actor", actor)?;
        let card_id = card.id.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existed = load_card_optional(&transaction, &card_id)?.is_some();
        persist_card(&transaction, &card)?;
        let saved = load_card(&transaction, &card_id)?;
        append_card_event(
            &transaction,
            &saved.id,
            if existed { "update" } else { "create" },
            &actor,
            if existed {
                "updated card"
            } else {
                "created card"
            },
            now,
        )?;
        if !existed {
            events::append_outbound_card_event(
                &transaction,
                &saved,
                "card-created",
                &actor,
                json!({"source": "create-card"}),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(saved)
    }

    pub fn record_card_event(
        &mut self,
        card_id: &CardId,
        event_type: &str,
        actor: &str,
        payload: &str,
        now: i64,
    ) -> Result<CardEvent> {
        if self.get_card(card_id)?.is_none() {
            return Err(DomainError::not_found("card", card_id.to_string()).into());
        }
        append_card_event(&self.connection, card_id, event_type, actor, payload, now)
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
        let all_cards = records
            .into_iter()
            .map(CardRecord::into_card)
            .collect::<Result<Vec<_>>>()?;
        // reuses the same full scan already loaded above, rather than a
        // second query per blocker: a blocker missing from this map is
        // treated as still blocking (fail closed).
        let statuses: HashMap<_, _> = all_cards.iter().map(|c| (c.id.clone(), c.status)).collect();
        let mut cards = all_cards
            .into_iter()
            .filter(|card| {
                card.is_ready_at(query.now, |id| {
                    statuses.get(id).is_some_and(|status| status.is_terminal())
                })
            })
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

    /// List cards by optional `status`/`repo` filter, not just ready-eligible
    /// ones -- `list_ready` answers "what can an agent claim now"; this
    /// answers "what exists," including `blocked`, `review`, and `done`
    /// cards no other surface can enumerate without opening the database
    /// file directly. Same sort as `list_ready` (priority, age, id).
    pub fn list_cards(&self, filter: &CardFilter, limit: usize) -> Result<Vec<Card>> {
        let repo_filter_requested = filter.repo.is_some();
        let repo_filter = filter.repo.as_deref().and_then(canonical_repo_label);
        let mut statement = self.connection.prepare(CARD_SELECT_ALL_SQL)?;
        let records = statement
            .query_map([], CardRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cards = records
            .into_iter()
            .map(CardRecord::into_card)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|card| filter.status.map(|s| card.status == s).unwrap_or(true))
            .filter(|card| match repo_filter.as_deref() {
                Some(repo) => card.repo.as_deref() == Some(repo),
                None => !repo_filter_requested,
            })
            .collect::<Vec<_>>();

        cards.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        cards.truncate(limit.max(1));
        Ok(cards)
    }

    pub fn list_repositories(&self) -> Result<Vec<RepositorySummary>> {
        let mut statement = self.connection.prepare(CARD_SELECT_ALL_SQL)?;
        let records = statement
            .query_map([], CardRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        summarize_repository_rows(records.into_iter().map(|record| RepositoryRow {
            repo: record.repo,
            status: record.status,
        }))
    }

    pub fn claim_card(
        &mut self,
        card_id: &CardId,
        agent: &str,
        now: i64,
        ttl_seconds: u64,
        authority: &Authority,
    ) -> Result<ClaimReceipt> {
        let agent = non_empty("agent", agent)?;
        authority.require_identity(&agent)?;
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

        if let Some(claim) = card.active_claim_for_agent(&agent, now) {
            let receipt = claim_receipt(card_id, claim);
            transaction.commit()?;
            return Ok(receipt);
        }

        transaction.execute(
            "UPDATE runs
             SET state = 'stale', updated_at = ?2
             WHERE card_id = ?1
               AND state = 'active'
               AND claim_expires_at <= ?2",
            params![card_id.as_str(), now],
        )?;
        if let Some(expired) = card.claim.as_ref().filter(|claim| claim.is_expired(now)) {
            events::append_outbound_card_event(
                &transaction,
                &card,
                "claim-expired",
                &expired.agent,
                json!({
                    "run_id": expired.run_id.as_str(),
                    "agent": expired.agent.as_str(),
                    "expired_at": expired.expires_at
                }),
                now,
            )?;
        }

        let mut terminal_blockers = std::collections::HashSet::new();
        for id in &card.blocked_by {
            if let Some(blocker) = load_card_optional(&transaction, id)? {
                if blocker.status.is_terminal() {
                    terminal_blockers.insert(id.clone());
                }
            }
        }

        let run_id = RunId::new(format!("run-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?;
        let claim = card.apply_claim(agent.clone(), run_id.clone(), now, ttl_seconds, |id| {
            terminal_blockers.contains(id)
        })?;
        persist_card(&transaction, &card)?;

        let run = Run {
            id: run_id.clone(),
            card_id: card_id.clone(),
            state: RunState::Active,
            agent: agent.clone(),
            claim_expires_at: claim.expires_at,
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
            expires_at: claim.expires_at,
        })
    }

    pub fn update_status(
        &mut self,
        card_id: &CardId,
        status: CardStatus,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        let previous = card.status;
        let released_claim = card.apply_status(status, now)?;
        persist_card(&transaction, &card)?;
        if let Some(claim) = released_claim {
            close_run_for_status(&transaction, &claim.run_id, status, now, None)?;
            append_activity(
                &transaction,
                &claim.run_id,
                ActivityType::Action,
                &format!("status set {card_id} to {}", status.as_str()),
                now,
            )?;
        }
        append_card_event(
            &transaction,
            card_id,
            "status",
            &authority.actor_label(),
            &format!("{} -> {}", previous.as_str(), status.as_str()),
            now,
        )?;
        if let Some(event_type) = events::outbound_event_for_status_change(previous, status) {
            events::append_outbound_card_event(
                &transaction,
                &card,
                event_type,
                &authority.actor_label(),
                json!({
                    "previous_status": previous.as_str(),
                    "status": status.as_str()
                }),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(card)
    }

    pub fn update_relations(
        &mut self,
        card_id: &CardId,
        related: Vec<CardId>,
        blocks: Vec<CardId>,
        blocked_by: Vec<CardId>,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        card.apply_relations(related, blocks, blocked_by, now);
        persist_card(&transaction, &card)?;
        append_card_event(
            &transaction,
            card_id,
            "relations",
            &authority.actor_label(),
            &format!(
                "related={:?} blocks={:?} blocked_by={:?}",
                card.related, card.blocks, card.blocked_by
            ),
            now,
        )?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn release_claim(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        authority: &Authority,
    ) -> Result<ClaimReceipt> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        authority.require_holder(card.claim_holder())?;
        let claim = card.release_claim(run_id, now)?;
        persist_card(&transaction, &card)?;
        release_run(&transaction, run_id, now)?;
        append_activity(
            &transaction,
            run_id,
            ActivityType::Action,
            &format!("released {card_id}"),
            now,
        )?;
        events::append_outbound_card_event(
            &transaction,
            &card,
            "moved-to-ready",
            &authority.actor_label(),
            json!({"source": "release_claim", "run_id": run_id.as_str()}),
            now,
        )?;
        transaction.commit()?;
        Ok(claim_receipt(card_id, &claim))
    }

    pub fn renew_claim(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        ttl_seconds: u64,
        authority: &Authority,
    ) -> Result<ClaimReceipt> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        authority.require_holder(card.claim_holder())?;
        let claim = card.renew_claim(run_id, now, ttl_seconds)?;
        persist_card(&transaction, &card)?;
        let updated = transaction.execute(
            "UPDATE runs
             SET claim_expires_at = ?2, updated_at = ?3
             WHERE id = ?1",
            params![run_id.as_str(), claim.expires_at, now],
        )?;
        if updated == 0 {
            return Err(DomainError::not_found("run", run_id.to_string()).into());
        }
        append_activity(
            &transaction,
            run_id,
            ActivityType::Action,
            &format!("renewed {card_id} until {}", claim.expires_at),
            now,
        )?;
        transaction.commit()?;
        Ok(claim_receipt(card_id, &claim))
    }

    pub fn heartbeat_claim(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        authority: &Authority,
    ) -> Result<ClaimReceipt> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        authority.require_holder(card.claim_holder())?;
        let claim = card.heartbeat_claim(run_id, now)?;
        persist_card(&transaction, &card)?;
        let updated = transaction.execute(
            "UPDATE runs
             SET updated_at = ?2
             WHERE id = ?1",
            params![run_id.as_str(), now],
        )?;
        if updated == 0 {
            return Err(DomainError::not_found("run", run_id.to_string()).into());
        }
        append_activity(
            &transaction,
            run_id,
            ActivityType::Action,
            &format!("heartbeat {card_id}"),
            now,
        )?;
        transaction.commit()?;
        Ok(claim_receipt(card_id, &claim))
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

    /// Not claim-holder-gated, matching `add_link`: attaching a comment is
    /// an additive annotation any authenticated caller can make, not an
    /// exclusive mutation of the card's own state.
    pub fn add_comment(
        &mut self,
        card_id: &CardId,
        author: &str,
        body: &str,
        now: i64,
    ) -> Result<Comment> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = load_card(&transaction, card_id)?;
        let comment = Comment {
            card_id: card_id.clone(),
            author: non_empty("author", author)?,
            body: non_empty("body", body)?,
            created_at: now,
        };
        let id = format!("comment-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
        transaction.execute(
            "INSERT INTO comments (id, card_id, author, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                comment.card_id.as_str(),
                comment.author,
                comment.body,
                comment.created_at
            ],
        )?;
        events::append_outbound_card_event(
            &transaction,
            &card,
            "comment-added",
            &comment.author,
            json!({"author": comment.author.as_str(), "body": comment.body.as_str()}),
            now,
        )?;
        transaction.commit()?;
        Ok(comment)
    }

    pub fn request_input(
        &mut self,
        run_id: &RunId,
        question: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<Run> {
        let question = non_empty("question", question)?;
        let mut run = self
            .get_run(run_id)?
            .ok_or_else(|| DomainError::not_found("run", run_id.to_string()))?;
        let mut card = load_card(&self.connection, &run.card_id)?;
        authority.require_holder(card.claim_holder())?;

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
        append_card_event(
            &transaction,
            &card.id,
            "status",
            &authority.actor_label(),
            "awaiting input",
            now,
        )?;
        events::append_outbound_card_event(
            &transaction,
            &card,
            "awaiting-input",
            &authority.actor_label(),
            json!({"run_id": run_id.as_str(), "question": question}),
            now,
        )?;
        transaction.commit()?;
        Ok(run)
    }

    pub fn complete_card(
        &mut self,
        card_id: &CardId,
        proof: Option<&str>,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let proof = proof.map(|value| non_empty("proof", value)).transpose()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;

        let previous = card.status;
        let run_id = card.claim.as_ref().map(|claim| claim.run_id.clone());

        card.status = CardStatus::Done;
        card.claim = None;
        card.updated_at = now;
        persist_card(&transaction, &card)?;
        if let Some(run_id) = run_id {
            close_run_for_status(
                &transaction,
                &run_id,
                CardStatus::Done,
                now,
                proof.as_deref(),
            )?;
            append_activity(
                &transaction,
                &run_id,
                ActivityType::Response,
                proof
                    .as_deref()
                    .map(|proof| format!("completed: {proof}"))
                    .unwrap_or_else(|| "completed without proof".to_string())
                    .as_str(),
                now,
            )?;
        }
        append_card_event(
            &transaction,
            card_id,
            "status",
            &authority.actor_label(),
            &format!("{} -> done", previous.as_str()),
            now,
        )?;
        if !previous.is_terminal() {
            events::append_outbound_card_event(
                &transaction,
                &card,
                "completed",
                &authority.actor_label(),
                json!({
                    "previous_status": previous.as_str(),
                    "status": card.status.as_str(),
                    "proof": proof
                }),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(card)
    }
}

fn persist_card(connection: &Connection, card: &Card) -> Result<()> {
    let source_path = card.source.as_ref().map(|source| source.path.as_str());
    let source_digest = card.source.as_ref().map(|source| source.digest.as_str());
    let repo = card.repo.as_deref().and_then(canonical_repo_label);
    let claim_agent = card.claim.as_ref().map(|claim| claim.agent.as_str());
    let claim_run_id = card.claim.as_ref().map(|claim| claim.run_id.as_str());
    let claim_acquired_at = card.claim.as_ref().map(|claim| claim.acquired_at);
    let claim_expires_at = card.claim.as_ref().map(|claim| claim.expires_at);

    connection.execute(
        &format!(
            "INSERT INTO cards ({CARD_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               body = excluded.body,
               acceptance_json = excluded.acceptance_json,
               status = excluded.status,
               priority = excluded.priority,
               labels_json = excluded.labels_json,
               assignee = excluded.assignee,
               related_json = excluded.related_json,
               blocks_json = excluded.blocks_json,
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
            to_json(&card.related)?,
            to_json(&card.blocks)?,
            to_json(&card.blocked_by)?,
            repo,
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
            id, card_id, state, agent, claim_expires_at, proof,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
           card_id = excluded.card_id,
           state = excluded.state,
           agent = excluded.agent,
           claim_expires_at = excluded.claim_expires_at,
           proof = excluded.proof,
           created_at = excluded.created_at,
           updated_at = excluded.updated_at",
        params![
            run.id.as_str(),
            run.card_id.as_str(),
            run.state.as_str(),
            run.agent,
            run.claim_expires_at,
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

fn append_card_event(
    connection: &Connection,
    card_id: &CardId,
    event_type: &str,
    actor: &str,
    payload: &str,
    now: i64,
) -> Result<CardEvent> {
    let event = CardEvent {
        id: CardEventId::new(format!("event-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?,
        card_id: card_id.clone(),
        event_type: non_empty("event_type", event_type)?,
        actor: non_empty("actor", actor)?,
        payload: payload.to_owned(),
        created_at: now,
    };
    connection.execute(
        "INSERT INTO card_events (id, card_id, event_type, actor, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.id.as_str(),
            event.card_id.as_str(),
            event.event_type.as_str(),
            event.actor.as_str(),
            event.payload.as_str(),
            event.created_at
        ],
    )?;
    Ok(event)
}

fn release_run(connection: &Connection, run_id: &RunId, now: i64) -> Result<()> {
    let updated = connection.execute(
        "UPDATE runs
         SET state = 'released', claim_expires_at = ?2, updated_at = ?2
         WHERE id = ?1",
        params![run_id.as_str(), now],
    )?;
    if updated == 0 {
        return Err(DomainError::not_found("run", run_id.to_string()).into());
    }
    Ok(())
}

fn close_run_for_status(
    connection: &Connection,
    run_id: &RunId,
    status: CardStatus,
    now: i64,
    proof: Option<&str>,
) -> Result<()> {
    let state = if status.is_terminal() {
        RunState::Complete
    } else {
        RunState::Released
    };
    let updated = connection.execute(
        "UPDATE runs
         SET state = ?2,
             claim_expires_at = CASE WHEN ?2 = 'released' THEN ?3 ELSE claim_expires_at END,
             proof = COALESCE(?4, proof),
             updated_at = ?3
         WHERE id = ?1",
        params![run_id.as_str(), state.as_str(), now, proof],
    )?;
    if updated == 0 {
        return Err(DomainError::not_found("run", run_id.to_string()).into());
    }
    Ok(())
}

fn claim_receipt(card_id: &CardId, claim: &Claim) -> ClaimReceipt {
    ClaimReceipt {
        card_id: card_id.clone(),
        run_id: claim.run_id.clone(),
        agent: claim.agent.clone(),
        expires_at: claim.expires_at,
    }
}

fn load_card(connection: &Connection, card_id: &CardId) -> Result<Card> {
    connection
        .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
        .optional()?
        .ok_or_else(|| DomainError::not_found("card", card_id.to_string()).into())
        .and_then(CardRecord::into_card)
}

fn load_card_optional(connection: &Connection, card_id: &CardId) -> Result<Option<Card>> {
    connection
        .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
        .optional()?
        .map(CardRecord::into_card)
        .transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReimportClass {
    Preserved,
    Updated,
    Unchanged,
}

fn classify_reimport(current: &Card, incoming: &Card) -> ReimportClass {
    if current.protects_lifecycle_on_reimport() {
        return ReimportClass::Preserved;
    }
    let current_digest = current.source.as_ref().map(|source| source.digest.as_str());
    let incoming_digest = incoming
        .source
        .as_ref()
        .map(|source| source.digest.as_str());
    if current_digest == incoming_digest {
        ReimportClass::Unchanged
    } else {
        ReimportClass::Updated
    }
}

/// Counts of what a backlog.d import did (or, from
/// [`Store::preview_import`], would do) to each card: newly created, content
/// refreshed, lifecycle preserved against a stale reimport, or left
/// untouched because the source file hasn't changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportOutcome {
    pub created: usize,
    pub updated: usize,
    pub preserved: usize,
    pub unchanged: usize,
}

impl ImportOutcome {
    pub fn total(&self) -> usize {
        self.created + self.updated + self.preserved + self.unchanged
    }

    fn record(&mut self, class: ReimportClass) {
        match class {
            ReimportClass::Preserved => self.preserved += 1,
            ReimportClass::Updated => self.updated += 1,
            ReimportClass::Unchanged => self.unchanged += 1,
        }
    }
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
    related_json: String,
    blocks_json: String,
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
            related_json: row.get(8)?,
            blocks_json: row.get(9)?,
            blocked_by_json: row.get(10)?,
            repo: row.get(11)?,
            workspace_path: row.get(12)?,
            branch_name: row.get(13)?,
            source_path: row.get(14)?,
            source_digest: row.get(15)?,
            claim_agent: row.get(16)?,
            claim_run_id: row.get(17)?,
            claim_acquired_at: row.get(18)?,
            claim_expires_at: row.get(19)?,
            created_at: row.get(20)?,
            updated_at: row.get(21)?,
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
        card.related = from_json("cards.related_json", self.related_json)?;
        card.blocks = from_json("cards.blocks_json", self.blocks_json)?;
        card.blocked_by = from_json("cards.blocked_by_json", self.blocked_by_json)?;
        card.repo = self.repo.as_deref().and_then(canonical_repo_label);
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
    claim_expires_at: i64,
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
            claim_expires_at: row.get(4)?,
            proof: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
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
            claim_expires_at: self.claim_expires_at,
            proof: self.proof,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
