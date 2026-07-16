#![forbid(unsafe_code)]

use std::{collections::HashMap, fs, path::Path};

use powder_core::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
    AcceptanceCriterion, Activity, ActivityId, ActivityType, Authority, AutonomyClass, Card,
    CardEvent, CardEventId, CardId, CardSource, CardStatus, Claim, ClaimReceipt, Comment,
    CriterionProof, DomainError, Estimate, Link, LinkId, OperationField, OperationId,
    OperationKind, OperationRequest, OperationState, Priority, ReadyQuery, Run, RunId, RunState,
    WorkLogEntry,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

mod answer_loop;
mod events;
mod identity;
mod repositories;
mod schema;
mod secrets;
pub mod status_model_020;
#[cfg(test)]
mod tests;

pub use events::{
    CardEventEnvelope, DeadLetterDelivery, EventSubscription, EventSubscriptionCreated,
    EventTailItem, WebhookDelivery, CARD_EVENT_SCHEMA_VERSION, EVENT_TYPES,
};
pub use identity::{Actor, ActorKind, ApiKeyCreated, ApiKeyScope, ApiKeySummary, VerifiedApiKey};
use repositories::{ensure_repository_entity, repository_tier, resolve_repository_name};
pub use repositories::{
    RepositoryMergeOutcome, RepositorySummary, RepositoryTier, RepositoryUpsert,
    RepositoryVisibility,
};

use schema::{
    CARD_COLUMNS, CARD_SELECT_ALL_SQL, CARD_SELECT_SQL, MIGRATE_10_TO_11, MIGRATE_11_TO_12,
    MIGRATE_12_TO_13, MIGRATE_13_TO_14, MIGRATE_1_TO_2, MIGRATE_2_TO_3, MIGRATE_3_TO_4,
    MIGRATE_4_TO_5, MIGRATE_5_TO_6, MIGRATE_6_TO_7, MIGRATE_7_TO_8, MIGRATE_8_TO_9,
    MIGRATE_9_TO_10, RUN_SELECT_SQL, SCHEMA, SCHEMA_VERSION,
};

pub type Result<T> = std::result::Result<T, StoreError>;

pub const OPERATION_STATUS_SCHEMA_VERSION: &str = "powder.operation_status.v1";
pub const OPERATION_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const OPERATION_FAILURE_MESSAGE_MAX_BYTES: usize = 512;
pub const WORK_LOG_AGENT_MAX_BYTES: usize = 256;
pub const WORK_LOG_ATTRIBUTION_MAX_BYTES: usize = 256;
pub const WORK_LOG_BODY_MAX_BYTES: usize = 16 * 1024;
pub const COMPLETION_PROOF_MAX_BYTES: usize = 4 * 1024;
pub const CRITERION_PROOF_MAX_COUNT: usize = 128;
pub const CRITERION_PROOF_URL_MAX_BYTES: usize = 4 * 1024;

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
    field_note_config: Option<FieldNoteConfig>,
}

/// Config for the field-note seed generator (powder-921, content-harness
/// epic misty-step-912, generator #1): on a qualifying completion, spawn
/// exactly one draft card carrying the `proof` field verbatim as raw
/// drafting material. `None` (the default for every `Store` unless a
/// deployment opts in via [`Store::with_field_note_config`]) means the
/// generator never runs -- self-hosters of Powder who never configure this
/// see no behavior change from completing a card.
///
/// Both gates are deterministic per the content-harness design law
/// (misty-step-912): eligibility is never a model judgment call, only
/// `repo_allowlist` membership, a proof length floor, and a hard weekly cap.
#[derive(Debug, Clone, Default)]
pub struct FieldNoteConfig {
    /// Canonical repo names (as returned by `card.repo`) eligible to spawn
    /// drafts. A card with no repo, or a repo not in this list, never
    /// qualifies -- there is no "surprise" way to start narrating a repo.
    pub repo_allowlist: Vec<String>,
    /// Minimum trimmed character count of the `proof` field for it to count
    /// as substantive raw material rather than a bare link or "done".
    pub proof_min_chars: usize,
    /// Hard cap on drafts spawned by this generator in the trailing 7 days.
    /// Once reached, further qualifying completions produce nothing until
    /// the window rolls forward -- the discard-unseen half of the design
    /// law's weekly budget, enforced here rather than left to the review
    /// queue to triage after the fact.
    pub weekly_budget: usize,
}

/// One week in seconds, for the field-note weekly budget window.
const FIELD_NOTE_BUDGET_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;

/// The dedicated pseudo-repo every content-harness generator's drafts land
/// in, regardless of the source card's own repo -- "one review queue every
/// generator feeds" (misty-step-912) is implemented as one shared, filterable
/// repo tag rather than a bespoke queue table.
const FIELD_NOTE_REVIEW_REPO: &str = "content";

/// The label that marks a card as a content-harness draft. Combined with
/// always-empty `acceptance`, this is what keeps drafts out of `list_ready`:
/// [`Card::is_ready_at`] already refuses any card with no acceptance
/// criteria, so a draft can never be claimed or dispatched without a second
/// exclusion mechanism to keep in sync.
const FIELD_NOTE_DRAFT_LABEL: &str = "field-note-draft";

/// Filter for [`Store::list_cards`]: `None` on either field means
/// unfiltered on that dimension.
#[derive(Debug, Clone, Default)]
pub struct CardFilter {
    pub status: Option<CardStatus>,
    pub repo: Option<String>,
    pub autonomy: Option<AutonomyClass>,
    pub estimate: Option<Estimate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardListPage {
    pub cards: Vec<Card>,
    pub total_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct BoardStatsQuery {
    pub repo: Option<String>,
    pub include_hidden: bool,
    pub now: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BoardStats {
    pub totals: BoardStatsCounts,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<BoardStatsRepo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BoardStatsRepo {
    pub repo: Option<String>,
    #[serde(flatten)]
    pub counts: BoardStatsCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BoardStatsCounts {
    pub cards: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub backlog: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub ready: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub claimed: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub running: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub awaiting_input: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub blocked: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub done: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub shipped: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub abandoned: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub active_claims: usize,
}

impl BoardStatsCounts {
    fn add(&mut self, status: CardStatus, cards: usize, active_claims: usize) {
        self.cards += cards;
        self.active_claims += active_claims;
        match status {
            CardStatus::Backlog => self.backlog += cards,
            CardStatus::Ready => self.ready += cards,
            CardStatus::Claimed => self.claimed += cards,
            CardStatus::Running => self.running += cards,
            CardStatus::AwaitingInput => self.awaiting_input += cards,
            CardStatus::Blocked => self.blocked += cards,
            CardStatus::Done => self.done += cards,
            CardStatus::Shipped => self.shipped += cards,
            CardStatus::Abandoned => self.abandoned += cards,
        }
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Explicit partial update for mutable card fields. Fields left as `None`
/// are preserved from the stored row; lifecycle/source/workspace fields are
/// intentionally absent from this shape.
#[derive(Debug, Clone, Default)]
pub struct CardPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub acceptance: Option<Vec<String>>,
    pub proof_plan: Option<Vec<String>>,
    pub status: Option<CardStatus>,
    pub autonomy: Option<AutonomyClass>,
    pub priority: Option<Priority>,
    pub estimate: Option<Estimate>,
    pub labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionProofInput {
    pub criterion: usize,
    pub url: String,
}

/// The optional attribution fields `append_work_log` accepts alongside the
/// required `agent`: whatever the calling surface (Claude Code, Codex,
/// a harness) knows about itself. Bundled into one struct rather than four
/// positional `Option<&str>` parameters so the method stays under clippy's
/// argument-count lint without losing any field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkLogAttribution<'a> {
    pub model: Option<&'a str>,
    pub reasoning: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub run_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationFailure {
    pub code: String,
    pub message: String,
}

/// Bounded, versioned recovery view for one mutation operation.
///
/// `unknown` intentionally contains no inferred mutation outcome. A missing
/// row can mean the request never reached Powder, the transaction rolled
/// back, or the retention window elapsed. Callers must not treat it as
/// success or blindly retry after the retention deadline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationStatus {
    pub schema_version: String,
    pub operation_id: OperationId,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<OperationKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_card_id: Option<CardId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<OperationFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl OperationStatus {
    fn unknown(operation_id: OperationId) -> Self {
        Self {
            schema_version: OPERATION_STATUS_SCHEMA_VERSION.to_string(),
            operation_id,
            state: OperationState::Unknown,
            request_digest: None,
            kind: None,
            target_card_id: None,
            expected_run_id: None,
            result: None,
            failure: None,
            audit_event_id: None,
            created_at: None,
            updated_at: None,
            expires_at: None,
        }
    }
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
        let store = Self {
            connection,
            field_note_config: None,
        };
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
                    self.apply_ratified_repository_tier_seed()?;
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
                6 => {
                    self.connection.execute_batch(MIGRATE_6_TO_7)?;
                    self.backfill_repositories_from_cards()?;
                    7
                }
                7 => {
                    self.connection.execute_batch(MIGRATE_7_TO_8)?;
                    self.apply_ratified_repository_tier_seed()?;
                    8
                }
                8 => {
                    self.connection.execute_batch(MIGRATE_8_TO_9)?;
                    9
                }
                9 => {
                    self.connection.execute_batch(MIGRATE_9_TO_10)?;
                    10
                }
                10 => {
                    self.connection.execute_batch(MIGRATE_10_TO_11)?;
                    11
                }
                11 => {
                    self.migrate_11_to_12()?;
                    12
                }
                12 => {
                    self.migrate_12_to_13()?;
                    13
                }
                13 => {
                    self.connection.execute_batch(MIGRATE_13_TO_14)?;
                    14
                }
                _ => return Err(StoreError::UnsupportedSchema(current)),
            };
            self.connection
                .execute_batch(&format!("PRAGMA user_version = {next}"))?;
        }
    }

    fn migrate_11_to_12(&mut self) -> Result<()> {
        // This migration may have half-applied in the old ALTER-then-version
        // pattern; keep only this step idempotent instead of broadening the
        // migration contract retroactively.
        if !self.cards_has_column("autonomy")? {
            self.connection.execute_batch(MIGRATE_11_TO_12)?;
        }
        Ok(())
    }

    fn migrate_12_to_13(&mut self) -> Result<()> {
        if !self.cards_has_column("estimate")? {
            self.connection.execute_batch(MIGRATE_12_TO_13)?;
        }
        Ok(())
    }

    fn cards_has_column(&self, column: &str) -> Result<bool> {
        let mut statement = self.connection.prepare("PRAGMA table_info(cards)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(columns.iter().any(|name| name.eq_ignore_ascii_case(column)))
    }

    /// Opts this `Store` into the field-note seed generator (see
    /// [`FieldNoteConfig`]). A deployment calls this once at startup, from
    /// its own env-driven config; nothing else about `Store` changes for
    /// callers who never call it.
    pub fn with_field_note_config(mut self, config: FieldNoteConfig) -> Self {
        self.field_note_config = Some(config);
        self
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
                    let merged = current.merge_reimport(incoming);
                    outcome.record(class, &current, &merged);
                    persist_card(&transaction, &merged)?;
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
                    outcome.record(class, &current, &merged);
                    persist_card(&transaction, &merged)?;
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
    /// unchanged report before committing to the import. `content_repaired`
    /// (powder-963) surfaces cards whose source file digest is unchanged but
    /// whose re-parsed acceptance text now differs from what's stored -- the
    /// audit signal for a parser fix landing on already-imported cards
    /// (e.g. the hard-wrapped-continuation truncation bug) without a manual
    /// per-card diff against the backlog.d source.
    pub fn preview_import(&self, cards: &[Card]) -> Result<ImportOutcome> {
        let mut outcome = ImportOutcome::default();
        for incoming in cards {
            match load_card_optional(&self.connection, &incoming.id)? {
                None => outcome.created += 1,
                Some(current) => {
                    let class = classify_reimport(&current, incoming);
                    let merged = current.merge_reimport(incoming.clone());
                    outcome.record(class, &current, &merged);
                }
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

    pub fn create_card_with_events(
        &mut self,
        mut card: Card,
        actor: &str,
        now: i64,
    ) -> Result<Card> {
        let actor = non_empty("actor", actor)?;
        let card_id = card.id.clone();
        if let Some(derived_repo) = repo_from_numeric_card_id_prefix(card_id.as_str()) {
            match card.repo.as_deref() {
                Some(repo) if !canonical_repo_matches(repo, &derived_repo) => {
                    return Err(DomainError::validation(
                        "repo",
                        format!("repo {repo} does not match numeric card id prefix {derived_repo}"),
                    )
                    .into());
                }
                None => card.repo = Some(derived_repo),
                Some(_) => {}
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_card_optional(&transaction, &card_id)?.is_some() {
            return Err(DomainError::conflict(format!("card already exists: {card_id}")).into());
        }
        persist_card(&transaction, &card)?;
        let saved = load_card(&transaction, &card_id)?;
        append_card_event(
            &transaction,
            &saved.id,
            "create",
            &actor,
            "created card",
            now,
        )?;
        events::append_outbound_card_event(
            &transaction,
            &saved,
            "card-created",
            &actor,
            json!({"source": "create-card"}),
            now,
        )?;
        transaction.commit()?;
        Ok(saved)
    }

    pub fn patch_card(
        &mut self,
        card_id: &CardId,
        patch: CardPatch,
        actor: &str,
        now: i64,
    ) -> Result<Card> {
        let actor = non_empty("actor", actor)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        let mut patched_fields = Vec::new();

        if let Some(title) = patch.title {
            card.title = non_empty("title", &title)?;
            patched_fields.push("title");
        }
        if let Some(body) = patch.body {
            card.body = body;
            patched_fields.push("body");
        }
        if let Some(acceptance) = patch.acceptance {
            card = card.with_acceptance(acceptance);
            patched_fields.push("acceptance");
        }
        if let Some(proof_plan) = patch.proof_plan {
            card = card.with_proof_plan(proof_plan);
            patched_fields.push("proof_plan");
        }
        if let Some(priority) = patch.priority {
            card.priority = priority;
            patched_fields.push("priority");
        }
        if let Some(estimate) = patch.estimate {
            card.estimate = Some(estimate);
            patched_fields.push("estimate");
        }
        if let Some(labels) = patch.labels {
            card.labels = clean_string_list(labels);
            patched_fields.push("labels");
        }
        if let Some(status) = patch.status {
            if status == CardStatus::Ready {
                ensure_ready_repository_allowed(&transaction, &card)?;
            }
            card.status.validate_transition(status)?;
            card.status = status;
            patched_fields.push("status");
        }
        if let Some(autonomy) = patch.autonomy {
            card.autonomy = autonomy;
            patched_fields.push("autonomy");
        }

        if patched_fields.is_empty() {
            transaction.commit()?;
            return Ok(card);
        }

        card.updated_at = now;
        persist_card(&transaction, &card)?;
        append_card_event(
            &transaction,
            card_id,
            "patch",
            &actor,
            &format!("patched {}", patched_fields.join(", ")),
            now,
        )?;

        transaction.commit()?;
        Ok(card)
    }

    pub fn check_criterion(
        &mut self,
        card_id: &CardId,
        criterion: usize,
        actor: &str,
        checked: bool,
        now: i64,
    ) -> Result<Card> {
        let actor = non_empty("actor", actor)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        let criterion_state = criterion_mut(&mut card, criterion)?;
        if checked {
            criterion_state.checked_by = Some(actor.clone());
            criterion_state.checked_at = Some(now);
        } else {
            criterion_state.checked_by = None;
            criterion_state.checked_at = None;
        }
        card.updated_at = now;
        persist_card(&transaction, &card)?;
        append_card_event(
            &transaction,
            card_id,
            "criterion",
            &actor,
            &format!(
                "criterion {} {}",
                criterion,
                if checked { "checked" } else { "unchecked" }
            ),
            now,
        )?;
        transaction.commit()?;
        Ok(card)
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
        record
            .map(|record| card_from_record(&self.connection, record))
            .transpose()
    }

    pub fn get_run(&self, run_id: &RunId) -> Result<Option<Run>> {
        let record = self
            .connection
            .query_row(RUN_SELECT_SQL, [run_id.as_str()], RunRecord::from_row)
            .optional()?;
        record.map(RunRecord::into_run).transpose()
    }

    pub fn list_ready(&self, query: ReadyQuery) -> Result<Vec<Card>> {
        Ok(self.list_ready_page(query)?.cards)
    }

    pub fn list_ready_page(&self, query: ReadyQuery) -> Result<CardListPage> {
        let mut statement = self.connection.prepare(CARD_SELECT_ALL_SQL)?;
        let records = statement
            .query_map([], CardRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let all_cards = records
            .into_iter()
            .map(|record| card_from_record(&self.connection, record))
            .collect::<Result<Vec<_>>>()?;
        // reuses the same full scan already loaded above, rather than a
        // second query per blocker: a blocker missing from this map is
        // treated as still blocking (fail closed).
        let statuses: HashMap<_, _> = all_cards.iter().map(|c| (c.id.clone(), c.status)).collect();
        let mut cards = Vec::new();
        for card in all_cards {
            if !card.is_ready_at(query.now, |id| {
                statuses.get(id).is_some_and(|status| status.is_terminal())
            }) {
                continue;
            }
            if query
                .estimate
                .is_some_and(|estimate| card.estimate != Some(estimate))
            {
                continue;
            }
            if !card_repository_allows_ready(&self.connection, &card)? {
                continue;
            }
            cards.push(card);
        }
        let total_count = cards.len();

        cards.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        cards.truncate(query.limit);
        Ok(CardListPage { cards, total_count })
    }

    /// List cards by optional `status`/`autonomy`/`repo` filter, not just ready-eligible
    /// ones -- `list_ready` answers "what can an agent claim now"; this
    /// answers "what exists," including `blocked` and `done`
    /// cards no other surface can enumerate without opening the database
    /// file directly. Same sort as `list_ready` (priority, age, id).
    pub fn list_cards(&self, filter: &CardFilter, limit: usize) -> Result<Vec<Card>> {
        Ok(self.list_cards_page(filter, limit)?.cards)
    }

    pub fn list_cards_page(&self, filter: &CardFilter, limit: usize) -> Result<CardListPage> {
        let repo_filter_requested = filter.repo.is_some();
        let requested_repo_label = filter.repo.as_deref().and_then(canonical_repo_label);
        let repo_filter = filter
            .repo
            .as_deref()
            .map(|repo| resolve_repository_name(&self.connection, repo))
            .transpose()?
            .flatten()
            .or(requested_repo_label);
        let mut statement = self.connection.prepare(CARD_SELECT_ALL_SQL)?;
        let records = statement
            .query_map([], CardRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cards = records
            .into_iter()
            .map(|record| card_from_record(&self.connection, record))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|card| filter.status.map(|s| card.status == s).unwrap_or(true))
            .filter(|card| {
                filter
                    .autonomy
                    .map(|autonomy| card.autonomy == autonomy)
                    .unwrap_or(true)
            })
            .filter(|card| {
                filter
                    .estimate
                    .map(|estimate| card.estimate == Some(estimate))
                    .unwrap_or(true)
            })
            .filter(|card| match repo_filter.as_deref() {
                Some(repo) => {
                    card.repo.as_deref() == Some(repo)
                        || (card.repo.is_none()
                            && repo_from_numeric_card_id_prefix(card.id.as_str()).as_deref()
                                == Some(repo))
                }
                None => !repo_filter_requested,
            })
            .collect::<Vec<_>>();
        let total_count = cards.len();

        cards.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        cards.truncate(limit.max(1));
        Ok(CardListPage { cards, total_count })
    }

    pub fn board_stats(&self, query: BoardStatsQuery) -> Result<BoardStats> {
        let requested_repo_label = query.repo.as_deref().and_then(canonical_repo_label);
        let repo_filter = query
            .repo
            .as_deref()
            .map(|repo| resolve_repository_name(&self.connection, repo))
            .transpose()?
            .flatten()
            .or(requested_repo_label);

        let mut statement = self.connection.prepare(
            "SELECT c.repo,
                    c.status,
                    COUNT(*) AS card_count,
                    SUM(CASE
                          WHEN c.claim_agent IS NOT NULL
                           AND c.claim_expires_at > ?1
                          THEN 1 ELSE 0
                        END) AS active_claim_count
             FROM cards c
             LEFT JOIN repositories r ON r.name = c.repo
             WHERE (?2 OR COALESCE(r.visibility, 'visible') = 'visible')
               AND (?3 IS NULL OR c.repo = ?3)
             GROUP BY c.repo, c.status
             ORDER BY
               CASE COALESCE(r.tier, 'backburner')
                 WHEN 'active' THEN 0
                 WHEN 'backburner' THEN 1
                 ELSE 2
               END,
               c.repo ASC,
               c.status ASC",
        )?;
        let grouped = statement
            .query_map(
                params![query.now, query.include_hidden, repo_filter.as_deref()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut stats = BoardStats::default();
        for (repo, raw_status, card_count, active_claim_count) in grouped {
            let status =
                CardStatus::parse(&raw_status).ok_or_else(|| StoreError::InvalidStoredValue {
                    field: "cards.status",
                    value: raw_status,
                })?;
            let card_count = card_count.max(0) as usize;
            let active_claim_count = active_claim_count.max(0) as usize;
            stats.totals.add(status, card_count, active_claim_count);
            if stats.repos.last().is_none_or(|row| row.repo != repo) {
                stats.repos.push(BoardStatsRepo {
                    repo: repo.clone(),
                    counts: BoardStatsCounts::default(),
                });
            }
            stats
                .repos
                .last_mut()
                .expect("board stats row was inserted")
                .counts
                .add(status, card_count, active_claim_count);
        }
        Ok(stats)
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
        ensure_ready_repository_allowed(&transaction, &card)?;
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
        if status == CardStatus::Ready {
            ensure_ready_repository_allowed(&transaction, &card)?;
        }
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
        ensure_ready_repository_allowed(&transaction, &card)?;
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

    /// Hand an active claim to a different agent atomically (powder-936):
    /// no release-then-race window where a third party could grab the card
    /// between the release and the intended recipient's claim. Invocable by
    /// the current holder or an admin, same as renew/release/heartbeat.
    /// Same run id throughout -- this is a handoff on the existing lease,
    /// not a new claim -- so the activity trail records one transfer event
    /// naming both agents rather than a release paired with a claim.
    pub fn transfer_claim(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        to_agent: &str,
        now: i64,
        ttl_seconds: u64,
        authority: &Authority,
    ) -> Result<ClaimReceipt> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;
        authority.require_holder(card.claim_holder())?;
        let from_agent = card.claim_holder().unwrap_or_default().to_string();
        let claim = card.transfer_claim(run_id, to_agent, now, ttl_seconds)?;
        persist_card(&transaction, &card)?;
        let updated = transaction.execute(
            "UPDATE runs
             SET agent = ?2, claim_expires_at = ?3, updated_at = ?4
             WHERE id = ?1",
            params![run_id.as_str(), to_agent, claim.expires_at, now],
        )?;
        if updated == 0 {
            return Err(DomainError::not_found("run", run_id.to_string()).into());
        }
        append_activity(
            &transaction,
            run_id,
            ActivityType::Action,
            &format!("transferred {card_id} from {from_agent} to {to_agent}"),
            now,
        )?;
        transaction.commit()?;
        Ok(claim_receipt(card_id, &claim))
    }

    pub fn add_link(&mut self, card_id: &CardId, label: &str, url: &str, now: i64) -> Result<Link> {
        if self.get_card(card_id)?.is_none() {
            return Err(DomainError::not_found("card", card_id.to_string()).into());
        }
        insert_link(&self.connection, card_id, label, url, now)
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

    /// Not claim-holder-gated, matching `add_comment`/`add_link`: appending
    /// work_log context is additive, not an exclusive mutation of the
    /// card's own state -- any authenticated caller may narrate their own
    /// work. Only `agent` is required attribution; every field on
    /// `attribution` is whatever the calling surface can supply.
    /// `body` is scrubbed for known secret shapes before it is ever
    /// persisted (powder-943 governance ruling: this becomes fleet-retro
    /// synthesis input, so it gets the same scrub discipline as any other
    /// agent-output surface, at write time rather than read time).
    pub fn append_work_log(
        &mut self,
        card_id: &CardId,
        agent: &str,
        attribution: WorkLogAttribution<'_>,
        body: &str,
        now: i64,
    ) -> Result<WorkLogEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (entry, _) =
            append_work_log_in_transaction(&transaction, card_id, agent, attribution, body, now)?;
        transaction.commit()?;
        Ok(entry)
    }

    /// Retry-safe work-log append using one durable operation identity.
    ///
    /// This is the generic P2 substrate. It deliberately does not enforce
    /// that `run_id` is the card's current run. P3 owns that stricter rule.
    #[allow(clippy::too_many_arguments)]
    pub fn append_work_log_idempotent(
        &mut self,
        operation_id: OperationId,
        card_id: &CardId,
        agent: &str,
        attribution: WorkLogAttribution<'_>,
        body: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<OperationStatus> {
        validate_bounded_non_empty("agent", agent, WORK_LOG_AGENT_MAX_BYTES)?;
        validate_optional_bound("model", attribution.model, WORK_LOG_ATTRIBUTION_MAX_BYTES)?;
        validate_optional_bound(
            "reasoning",
            attribution.reasoning,
            WORK_LOG_ATTRIBUTION_MAX_BYTES,
        )?;
        validate_optional_bound(
            "harness",
            attribution.harness,
            WORK_LOG_ATTRIBUTION_MAX_BYTES,
        )?;
        validate_bounded_non_empty("body", body, WORK_LOG_BODY_MAX_BYTES)?;
        let expected_run = attribution.run_id.map(RunId::new).transpose()?;
        let request = OperationRequest::new(
            operation_id,
            OperationKind::WorkLogAppend,
            card_id.clone(),
            authority.operation_identity(),
            expected_run,
            &[
                OperationField {
                    name: "agent",
                    value: Some(agent),
                },
                OperationField {
                    name: "model",
                    value: attribution.model,
                },
                OperationField {
                    name: "reasoning",
                    value: attribution.reasoning,
                },
                OperationField {
                    name: "harness",
                    value: attribution.harness,
                },
                OperationField {
                    name: "body",
                    value: Some(body),
                },
            ],
        )?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_expired_operations(&transaction, now)?;
        if let Some(existing) = load_operation(&transaction, &request.id)? {
            return replay_operation(existing, &request);
        }
        reserve_operation(&transaction, &request, now)?;
        match append_work_log_in_transaction(&transaction, card_id, agent, attribution, body, now) {
            Ok((entry, event_id)) => {
                finish_operation(
                    &transaction,
                    &request.id,
                    OperationState::Succeeded,
                    Some(json!(entry)),
                    None,
                    Some(event_id),
                    now,
                )?;
            }
            Err(StoreError::Domain(error)) => {
                finish_operation(
                    &transaction,
                    &request.id,
                    OperationState::Rejected,
                    None,
                    Some(operation_failure(&error)),
                    None,
                    now,
                )?;
            }
            Err(error) => return Err(error),
        }
        let status = load_operation(&transaction, &request.id)?
            .ok_or_else(|| StoreError::InvalidStoredValue {
                field: "operation_id",
                value: request.id.to_string(),
            })?
            .into_status()?;
        transaction.commit()?;
        Ok(status)
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
        criterion_proofs: Vec<CriterionProofInput>,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (card, _) = complete_card_in_transaction(
            &transaction,
            card_id,
            proof,
            criterion_proofs,
            now,
            authority,
            self.field_note_config.as_ref(),
        )?;
        transaction.commit()?;
        Ok(card)
    }

    /// Retry-safe permissive completion using the shared P2 operation
    /// substrate. This preserves Powder's explicit operator correction path
    /// and does not add P1's expected-current-run precondition.
    pub fn complete_card_idempotent(
        &mut self,
        operation_id: OperationId,
        card_id: &CardId,
        proof: Option<&str>,
        criterion_proofs: Vec<CriterionProofInput>,
        now: i64,
        authority: &Authority,
    ) -> Result<OperationStatus> {
        validate_optional_bound("proof", proof, COMPLETION_PROOF_MAX_BYTES)?;
        if criterion_proofs.len() > CRITERION_PROOF_MAX_COUNT {
            return Err(DomainError::validation(
                "criterion_proofs",
                format!("must contain at most {CRITERION_PROOF_MAX_COUNT} items"),
            )
            .into());
        }
        for criterion_proof in &criterion_proofs {
            validate_bounded_non_empty(
                "criterion proof url",
                &criterion_proof.url,
                CRITERION_PROOF_URL_MAX_BYTES,
            )?;
        }
        let criterion_proofs = clean_criterion_proofs(criterion_proofs)?;
        let criterion_payload = to_json(&criterion_proofs)?;
        let request = OperationRequest::new(
            operation_id,
            OperationKind::Completion,
            card_id.clone(),
            authority.operation_identity(),
            None,
            &[
                OperationField {
                    name: "proof",
                    value: proof,
                },
                OperationField {
                    name: "criterion_proofs",
                    value: Some(&criterion_payload),
                },
            ],
        )?;
        let field_note_config = self.field_note_config.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_expired_operations(&transaction, now)?;
        if let Some(existing) = load_operation(&transaction, &request.id)? {
            return replay_operation(existing, &request);
        }
        reserve_operation(&transaction, &request, now)?;
        match complete_card_in_transaction(
            &transaction,
            card_id,
            proof,
            criterion_proofs,
            now,
            authority,
            field_note_config.as_ref(),
        ) {
            Ok((card, event_id)) => {
                finish_operation(
                    &transaction,
                    &request.id,
                    OperationState::Succeeded,
                    Some(json!({
                        "card_id": card.id,
                        "status": card.status,
                        "updated_at": card.updated_at,
                    })),
                    None,
                    Some(event_id),
                    now,
                )?;
            }
            Err(StoreError::Domain(error)) => {
                finish_operation(
                    &transaction,
                    &request.id,
                    OperationState::Rejected,
                    None,
                    Some(operation_failure(&error)),
                    None,
                    now,
                )?;
            }
            Err(error) => return Err(error),
        }
        let status = load_operation(&transaction, &request.id)?
            .ok_or_else(|| StoreError::InvalidStoredValue {
                field: "operation_id",
                value: request.id.to_string(),
            })?
            .into_status()?;
        transaction.commit()?;
        Ok(status)
    }

    /// Read one bounded operation outcome after pruning expired recovery
    /// records. Unknown is returned as data rather than a 404 so callers can
    /// distinguish an absent operation from transport failure.
    pub fn operation_status(
        &mut self,
        operation_id: &OperationId,
        now: i64,
        authority: &Authority,
    ) -> Result<OperationStatus> {
        prune_expired_operations(&self.connection, now)?;
        let Some(operation) = load_operation(&self.connection, operation_id)? else {
            return Ok(OperationStatus::unknown(operation_id.clone()));
        };
        authority.require_operation_authority(&operation.authority)?;
        operation.into_status()
    }

    pub fn prune_operations(&mut self, now: i64) -> Result<usize> {
        prune_expired_operations(&self.connection, now)
    }
}

fn append_work_log_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    card_id: &CardId,
    agent: &str,
    attribution: WorkLogAttribution<'_>,
    body: &str,
    now: i64,
) -> Result<(WorkLogEntry, String)> {
    let card = load_card(transaction, card_id)?;
    let run_id = attribution.run_id.map(RunId::new).transpose()?;
    let entry = WorkLogEntry {
        card_id: card_id.clone(),
        agent: non_empty("agent", agent)?,
        model: attribution.model.map(str::to_owned),
        reasoning: attribution.reasoning.map(str::to_owned),
        harness: attribution.harness.map(str::to_owned),
        run_id,
        body: secrets::scrub_secrets(&non_empty("body", body)?),
        created_at: now,
    };
    let id = format!("work-log-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
    transaction.execute(
        "INSERT INTO work_log_entries
         (id, card_id, agent, model, reasoning, harness, run_id, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            entry.card_id.as_str(),
            entry.agent,
            entry.model,
            entry.reasoning,
            entry.harness,
            entry.run_id.as_ref().map(RunId::as_str),
            entry.body,
            entry.created_at,
        ],
    )?;
    let event = events::append_outbound_card_event(
        transaction,
        &card,
        "work-log-appended",
        &entry.agent,
        json!({
            "agent": entry.agent.as_str(),
            "model": entry.model,
            "harness": entry.harness,
        }),
        now,
    )?;
    Ok((entry, event.event_id))
}

#[allow(clippy::too_many_arguments)]
fn complete_card_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    card_id: &CardId,
    proof: Option<&str>,
    criterion_proofs: Vec<CriterionProofInput>,
    now: i64,
    authority: &Authority,
    field_note_config: Option<&FieldNoteConfig>,
) -> Result<(Card, String)> {
    let proof = proof.map(|value| non_empty("proof", value)).transpose()?;
    let criterion_proofs = clean_criterion_proofs(criterion_proofs)?;
    let mut card = load_card(transaction, card_id)?;
    let previous = card.status;
    let run_id = card.claim.as_ref().map(|claim| claim.run_id.clone());

    card.status = CardStatus::Done;
    card.claim = None;
    for criterion_proof in criterion_proofs {
        let criterion = criterion_mut(&mut card, criterion_proof.criterion)?;
        criterion.proof_links.push(CriterionProof {
            url: criterion_proof.url,
            actor: authority.actor_label(),
            created_at: now,
        });
    }
    card.updated_at = now;
    persist_card(transaction, &card)?;
    if let Some(run_id) = run_id {
        close_run_for_status(
            transaction,
            &run_id,
            CardStatus::Done,
            now,
            proof.as_deref(),
        )?;
        append_activity(
            transaction,
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
    let audit_event = append_card_event(
        transaction,
        card_id,
        "status",
        &authority.actor_label(),
        &format!("{} -> done", previous.as_str()),
        now,
    )?;
    if !previous.is_terminal() {
        events::append_outbound_card_event(
            transaction,
            &card,
            "completed",
            &authority.actor_label(),
            json!({
                "previous_status": previous.as_str(),
                "status": card.status.as_str(),
                "proof": proof,
                "criteria": card.criteria
            }),
            now,
        )?;
        if let Some(config) = field_note_config {
            maybe_spawn_field_note_draft(transaction, &card, proof.as_deref(), config, now)?;
        }
    }
    Ok((card, audit_event.id.to_string()))
}

#[derive(Debug)]
struct StoredOperation {
    operation_id: OperationId,
    request_digest: String,
    kind: OperationKind,
    target_card_id: CardId,
    authority: String,
    expected_run_id: Option<RunId>,
    state: OperationState,
    result_json: Option<String>,
    failure_code: Option<String>,
    failure_message: Option<String>,
    audit_event_id: Option<String>,
    created_at: i64,
    updated_at: i64,
    expires_at: i64,
}

impl StoredOperation {
    fn into_status(self) -> Result<OperationStatus> {
        let result = self
            .result_json
            .as_deref()
            .map(|raw| from_json::<Value>("operation.result_json", raw.to_string()))
            .transpose()?;
        let failure = match (self.failure_code, self.failure_message) {
            (Some(code), Some(message)) => Some(OperationFailure { code, message }),
            (None, None) => None,
            (code, message) => {
                return Err(StoreError::InvalidStoredValue {
                    field: "operation_failure",
                    value: format!("code={code:?}, message={message:?}"),
                });
            }
        };
        Ok(OperationStatus {
            schema_version: OPERATION_STATUS_SCHEMA_VERSION.to_string(),
            operation_id: self.operation_id,
            state: self.state,
            request_digest: Some(self.request_digest),
            kind: Some(self.kind),
            target_card_id: Some(self.target_card_id),
            expected_run_id: self.expected_run_id,
            result,
            failure,
            audit_event_id: self.audit_event_id,
            created_at: Some(self.created_at),
            updated_at: Some(self.updated_at),
            expires_at: Some(self.expires_at),
        })
    }
}

fn reserve_operation(connection: &Connection, request: &OperationRequest, now: i64) -> Result<()> {
    connection.execute(
        "INSERT INTO mutation_operations (
           operation_id, request_digest, kind, target_card_id, authority,
           expected_run_id, state, result_json, failure_code, failure_message,
           audit_event_id, created_at, updated_at, expires_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', NULL, NULL, NULL, NULL, ?7, ?7, ?8)",
        params![
            request.id.as_str(),
            request.request_digest,
            request.kind.as_str(),
            request.target.as_str(),
            request.authority,
            request.expected_run.as_ref().map(RunId::as_str),
            now,
            now.saturating_add(OPERATION_RETENTION_SECONDS),
        ],
    )?;
    Ok(())
}

fn finish_operation(
    connection: &Connection,
    operation_id: &OperationId,
    state: OperationState,
    result: Option<Value>,
    failure: Option<OperationFailure>,
    audit_event_id: Option<String>,
    now: i64,
) -> Result<()> {
    debug_assert!(matches!(
        state,
        OperationState::Succeeded | OperationState::Rejected | OperationState::Failed
    ));
    let result_json = result.as_ref().map(to_json).transpose()?;
    connection.execute(
        "UPDATE mutation_operations
         SET state = ?2, result_json = ?3, failure_code = ?4,
             failure_message = ?5, audit_event_id = ?6, updated_at = ?7
         WHERE operation_id = ?1 AND state = 'pending'",
        params![
            operation_id.as_str(),
            state.as_str(),
            result_json,
            failure.as_ref().map(|failure| failure.code.as_str()),
            failure.as_ref().map(|failure| failure.message.as_str()),
            audit_event_id,
            now,
        ],
    )?;
    Ok(())
}

fn load_operation(
    connection: &Connection,
    operation_id: &OperationId,
) -> Result<Option<StoredOperation>> {
    connection
        .query_row(
            "SELECT operation_id, request_digest, kind, target_card_id, authority,
                    expected_run_id, state, result_json, failure_code, failure_message,
                    audit_event_id, created_at, updated_at, expires_at
             FROM mutation_operations WHERE operation_id = ?1",
            [operation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()?
        .map(
            |(
                operation_id,
                request_digest,
                kind,
                target_card_id,
                authority,
                expected_run_id,
                state,
                result_json,
                failure_code,
                failure_message,
                audit_event_id,
                created_at,
                updated_at,
                expires_at,
            )| {
                Ok(StoredOperation {
                    operation_id: OperationId::new(operation_id)?,
                    request_digest,
                    kind: OperationKind::parse(&kind).ok_or_else(|| {
                        StoreError::InvalidStoredValue {
                            field: "operation.kind",
                            value: kind,
                        }
                    })?,
                    target_card_id: CardId::new(target_card_id)?,
                    authority,
                    expected_run_id: expected_run_id.map(RunId::new).transpose()?,
                    state: OperationState::parse(&state).ok_or_else(|| {
                        StoreError::InvalidStoredValue {
                            field: "operation.state",
                            value: state,
                        }
                    })?,
                    result_json,
                    failure_code,
                    failure_message,
                    audit_event_id,
                    created_at,
                    updated_at,
                    expires_at,
                })
            },
        )
        .transpose()
}

fn replay_operation(
    existing: StoredOperation,
    request: &OperationRequest,
) -> Result<OperationStatus> {
    if existing.request_digest != request.request_digest {
        return Err(DomainError::conflict(format!(
            "operation id {} was already used for a different request",
            request.id
        ))
        .into());
    }
    existing.into_status()
}

fn prune_expired_operations(connection: &Connection, now: i64) -> Result<usize> {
    Ok(connection.execute(
        "DELETE FROM mutation_operations WHERE expires_at <= ?1",
        [now],
    )?)
}

fn operation_failure(error: &DomainError) -> OperationFailure {
    let code = match error {
        DomainError::Validation { .. } => "validation",
        DomainError::NotFound { .. } => "not_found",
        DomainError::Conflict(_) => "conflict",
        DomainError::Forbidden(_) => "forbidden",
        DomainError::ClaimExpired(_) => "claim_expired",
    };
    OperationFailure {
        code: code.to_string(),
        message: truncate_utf8(&error.to_string(), OPERATION_FAILURE_MESSAGE_MAX_BYTES),
    }
}

fn truncate_utf8(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn validate_bounded_non_empty(field: &'static str, value: &str, maximum: usize) -> Result<()> {
    non_empty(field, value)?;
    if value.len() > maximum {
        return Err(
            DomainError::validation(field, format!("must be at most {maximum} bytes")).into(),
        );
    }
    Ok(())
}

fn validate_optional_bound(field: &'static str, value: Option<&str>, maximum: usize) -> Result<()> {
    if let Some(value) = value {
        validate_bounded_non_empty(field, value, maximum)?;
    }
    Ok(())
}

/// The field-note seed generator's actual eligibility check and draft spawn
/// (powder-921). Runs inside `complete_card`'s own transaction, so a draft
/// either exists durably alongside the completion it came from or not at
/// all -- never a dangling side effect from a completion that itself rolled
/// back. Every gate is deterministic per the content-harness design law: no
/// model call decides eligibility here, only repo membership, a length
/// floor, and a hard weekly count.
fn maybe_spawn_field_note_draft(
    transaction: &rusqlite::Transaction,
    completed_card: &Card,
    proof: Option<&str>,
    config: &FieldNoteConfig,
    now: i64,
) -> Result<Option<Card>> {
    let Some(proof) = proof else {
        return Ok(None);
    };
    let proof = proof.trim();
    if proof.chars().count() < config.proof_min_chars {
        return Ok(None);
    }

    let Some(repo) = completed_card.repo.as_deref() else {
        return Ok(None);
    };
    // `card.repo` is already canonicalized to its short name (e.g. "powder",
    // not "misty-step/powder") by the time it's stored; canonicalize the
    // configured allowlist entries the same way `canonical_repo_matches` does
    // everywhere else in this crate, so an operator can list either spelling.
    if !config
        .repo_allowlist
        .iter()
        .any(|allowed| canonical_repo_matches(allowed, repo))
    {
        return Ok(None);
    }

    let cutoff = now - FIELD_NOTE_BUDGET_WINDOW_SECONDS;
    if count_field_note_drafts_since(transaction, cutoff)? >= config.weekly_budget {
        return Ok(None);
    }

    // Deterministic id from the source card: a card completes exactly once
    // (status only ever moves forward to a terminal state), so this can
    // never collide under normal operation. The existence check is a
    // defensive idempotency guard, not the primary uniqueness mechanism.
    let draft_id = CardId::new(format!("field-note-{}", completed_card.id))?;
    if load_card_optional(transaction, &draft_id)?.is_some() {
        return Ok(None);
    }

    let source_links = answer_loop::load_links_for_card(transaction, &completed_card.id)?;

    let mut body = format!(
        "Seed proof captured verbatim from {} ({repo}) for drafting in the operator voice. \
         Machine-drafted; not for autopost.\n\n---\n\n{proof}",
        completed_card.id
    );
    if !source_links.is_empty() {
        body.push_str("\n\n---\nEvidence links:\n");
        for link in &source_links {
            body.push_str(&format!("- {}: {}\n", link.label, link.url));
        }
    }

    let mut draft = Card::new(
        draft_id.clone(),
        format!("Field note seed: {}", completed_card.title),
        body,
    )?
    .with_status(CardStatus::Backlog)
    .with_created_at(now);
    draft.labels = vec![FIELD_NOTE_DRAFT_LABEL.to_string()];
    draft.related = vec![completed_card.id.clone()];
    draft.repo = Some(FIELD_NOTE_REVIEW_REPO.to_string());
    draft.updated_at = now;

    persist_card(transaction, &draft)?;
    append_card_event(
        transaction,
        &draft_id,
        "create",
        "field-note-generator",
        &format!("spawned field-note draft from {}", completed_card.id),
        now,
    )?;
    for link in &source_links {
        insert_link(transaction, &draft_id, &link.label, &link.url, now)?;
    }

    Ok(Some(draft))
}

/// How many field-note drafts (identified by [`FIELD_NOTE_REVIEW_REPO`] +
/// [`FIELD_NOTE_DRAFT_LABEL`]) were created at or after `cutoff`. A full
/// table scan mirrors the existing `list_ready`/`list_cards` pattern --
/// Powder's card counts don't warrant a dedicated indexed query for this.
fn count_field_note_drafts_since(connection: &Connection, cutoff: i64) -> Result<usize> {
    let mut statement = connection.prepare(CARD_SELECT_ALL_SQL)?;
    let records = statement
        .query_map([], CardRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut count = 0;
    for record in records {
        let card = card_from_record(connection, record)?;
        if card.created_at >= cutoff
            && card.repo.as_deref() == Some(FIELD_NOTE_REVIEW_REPO)
            && card
                .labels
                .iter()
                .any(|label| label == FIELD_NOTE_DRAFT_LABEL)
        {
            count += 1;
        }
    }
    Ok(count)
}

fn insert_link(
    connection: &Connection,
    card_id: &CardId,
    label: &str,
    url: &str,
    now: i64,
) -> Result<Link> {
    let link = Link {
        id: LinkId::new(format!("link-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?,
        card_id: card_id.clone(),
        label: non_empty("label", label)?,
        url: non_empty("url", url)?,
        created_at: now,
    };
    connection.execute(
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

fn persist_card(connection: &Connection, card: &Card) -> Result<()> {
    let source_path = card.source.as_ref().map(|source| source.path.as_str());
    let source_digest = card.source.as_ref().map(|source| source.digest.as_str());
    let repo = card
        .repo
        .as_deref()
        .map(|repo| ensure_repository_entity(connection, repo, card.updated_at, Some("card repo")))
        .transpose()?
        .flatten();
    let claim_agent = card.claim.as_ref().map(|claim| claim.agent.as_str());
    let claim_run_id = card.claim.as_ref().map(|claim| claim.run_id.as_str());
    let claim_acquired_at = card.claim.as_ref().map(|claim| claim.acquired_at);
    let claim_expires_at = card.claim.as_ref().map(|claim| claim.expires_at);

    connection.execute(
        &format!(
            "INSERT INTO cards ({CARD_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               body = excluded.body,
               acceptance_json = excluded.acceptance_json,
               criteria_json = excluded.criteria_json,
               proof_plan_json = excluded.proof_plan_json,
               status = excluded.status,
               autonomy = excluded.autonomy,
               priority = excluded.priority,
               estimate = excluded.estimate,
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
            to_json(&card.criteria)?,
            to_json(&card.proof_plan)?,
            card.status.as_str(),
            card.autonomy.as_str(),
            card.priority.as_str(),
            card.estimate.map(Estimate::as_str),
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
        .and_then(|record| card_from_record(connection, record))
}

fn load_card_optional(connection: &Connection, card_id: &CardId) -> Result<Option<Card>> {
    connection
        .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
        .optional()?
        .map(|record| card_from_record(connection, record))
        .transpose()
}

fn card_from_record(connection: &Connection, record: CardRecord) -> Result<Card> {
    let mut card = record.into_card()?;
    if let Some(repo) = card.repo.as_deref() {
        card.repo = resolve_repository_name(connection, repo)?;
    }
    Ok(card)
}

fn card_repository_allows_ready(connection: &Connection, card: &Card) -> Result<bool> {
    let Some(repo) = card.repo.as_deref() else {
        return Ok(true);
    };
    Ok(repository_tier(connection, repo)?.allows_ready())
}

fn ensure_ready_repository_allowed(connection: &Connection, card: &Card) -> Result<()> {
    let Some(repo) = card.repo.as_deref() else {
        return Ok(());
    };
    let tier = repository_tier(connection, repo)?;
    if tier.allows_ready() {
        Ok(())
    } else {
        Err(DomainError::conflict(format!(
            "repository {repo} is {}; only active repositories may move cards to ready",
            tier.as_str()
        ))
        .into())
    }
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
    /// Cards whose acceptance text actually changed on this reimport even
    /// though the source file's digest did NOT (powder-963): a parser fix
    /// repairing previously-truncated criteria on already-imported cards.
    /// Scoped to `ReimportClass::Unchanged` specifically -- an ordinary
    /// source edit changes the digest too (`ReimportClass::Updated`), and
    /// that acceptance-text delta is expected, not damage, so it must not
    /// inflate this counter. Audit an already-imported repo for backlog.d
    /// parser damage by running `preview_import` (or `import-repo
    /// --dry-run` from the CLI) after a parser fix ships and reading this
    /// count instead of hand-diffing every card against its source file.
    pub content_repaired: usize,
}

impl ImportOutcome {
    pub fn total(&self) -> usize {
        self.created + self.updated + self.preserved + self.unchanged
    }

    fn record(&mut self, class: ReimportClass, current: &Card, merged: &Card) {
        match class {
            ReimportClass::Preserved => self.preserved += 1,
            ReimportClass::Updated => self.updated += 1,
            ReimportClass::Unchanged => self.unchanged += 1,
        }
        if class == ReimportClass::Unchanged && current.acceptance != merged.acceptance {
            self.content_repaired += 1;
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

fn clean_string_list(items: impl IntoIterator<Item = String>) -> Vec<String> {
    items
        .into_iter()
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn criterion_mut(card: &mut Card, criterion: usize) -> Result<&mut AcceptanceCriterion> {
    if card.criteria.is_empty() && !card.acceptance.is_empty() {
        let refreshed = card
            .acceptance
            .iter()
            .filter_map(|item| AcceptanceCriterion::new(item.clone()).ok())
            .collect::<Vec<_>>();
        card.criteria = refreshed;
    }
    card.criteria.get_mut(criterion).ok_or_else(|| {
        DomainError::validation(
            "criterion",
            format!("criterion index {criterion} not found"),
        )
        .into()
    })
}

fn clean_criterion_proofs(inputs: Vec<CriterionProofInput>) -> Result<Vec<CriterionProofInput>> {
    inputs
        .into_iter()
        .map(|input| {
            Ok(CriterionProofInput {
                criterion: input.criterion,
                url: non_empty("criterion_proof.url", &input.url)?,
            })
        })
        .collect()
}

struct CardRecord {
    id: String,
    title: String,
    body: String,
    acceptance_json: String,
    criteria_json: String,
    proof_plan_json: String,
    status: String,
    autonomy: String,
    priority: String,
    estimate: Option<String>,
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
            criteria_json: row.get(4)?,
            proof_plan_json: row.get(5)?,
            status: row.get(6)?,
            autonomy: row.get(7)?,
            priority: row.get(8)?,
            estimate: row.get(9)?,
            labels_json: row.get(10)?,
            assignee: row.get(11)?,
            related_json: row.get(12)?,
            blocks_json: row.get(13)?,
            blocked_by_json: row.get(14)?,
            repo: row.get(15)?,
            workspace_path: row.get(16)?,
            branch_name: row.get(17)?,
            source_path: row.get(18)?,
            source_digest: row.get(19)?,
            claim_agent: row.get(20)?,
            claim_run_id: row.get(21)?,
            claim_acquired_at: row.get(22)?,
            claim_expires_at: row.get(23)?,
            created_at: row.get(24)?,
            updated_at: row.get(25)?,
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
            .with_autonomy(AutonomyClass::parse(&self.autonomy).ok_or(
                StoreError::InvalidStoredValue {
                    field: "cards.autonomy",
                    value: self.autonomy,
                },
            )?)
            .with_priority(Priority::parse(&self.priority).ok_or(
                StoreError::InvalidStoredValue {
                    field: "cards.priority",
                    value: self.priority,
                },
            )?)
            .with_estimate(
                self.estimate
                    .map(|raw| {
                        Estimate::parse(&raw).ok_or(StoreError::InvalidStoredValue {
                            field: "cards.estimate",
                            value: raw,
                        })
                    })
                    .transpose()?,
            )
            .with_created_at(self.created_at);
        let criteria =
            from_json::<Vec<AcceptanceCriterion>>("cards.criteria_json", self.criteria_json)?;
        if !criteria.is_empty() {
            card = card.with_criteria(criteria);
        }
        card = card.with_proof_plan(from_json::<Vec<String>>(
            "cards.proof_plan_json",
            self.proof_plan_json,
        )?);
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
