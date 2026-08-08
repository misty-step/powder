#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
};

use powder_core::{
    AcceptanceCriterion, Activity, ActivityId, ActivityType, Authority, Card, CardEvent,
    CardEventChange, CardEventId, CardEventType, CardId, CardStatus, CardSummary, Claim,
    ClaimReceipt, Comment, CriterionProof, DenialClass, DomainError, Link, LinkId, Operation,
    Priority, ReadyCursor, ReadyQuery, Run, RunId, RunState, WorkLogEntry,
};
use rusqlite::{
    functions::{Context, FunctionFlags},
    params,
    types::ValueRef,
    Connection, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

mod answer_loop;
mod events;
mod identity;
mod relations;
mod schema;
mod secrets;
#[cfg(test)]
mod tests;

pub use events::{CardEventEnvelope, EventTailItem, CARD_EVENT_SCHEMA_VERSION, EVENT_TYPES};
pub use identity::{ApiKeyCreated, ApiKeyScope, ApiKeySummary, VerifiedApiKey};
use relations::{list_delta, mirror_delta_with_authority, mirror_initial_relations_with_authority};
pub use relations::{
    ParentDoctorIssue, ParentGraphReport, ParentIssueKind, RelationField, RelationIssueKind,
    RelationsDoctorIssue, RelationsDoctorReport,
};
pub use schema::SCHEMA_VERSION;

use schema::{
    CARD_COLUMNS, CARD_SELECT_ALL_SQL, CARD_SELECT_SQL, MIGRATE_10_TO_11, MIGRATE_11_TO_12,
    MIGRATE_12_TO_13, MIGRATE_13_TO_14, MIGRATE_14_TO_15, MIGRATE_15_TO_16, MIGRATE_2_TO_3,
    MIGRATE_5_TO_6, MIGRATE_6_TO_7, MIGRATE_7_TO_8, MIGRATE_9_TO_10, RUN_SELECT_SQL, SCHEMA,
    SEARCH_SCHEMA,
};

pub type Result<T> = std::result::Result<T, StoreError>;

const READY_SNAPSHOT_TTL_SECONDS: i64 = 60 * 60;

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
    #[error("invalid search cursor: {0}")]
    InvalidSearchCursor(String),
}

pub struct Store {
    connection: Connection,
}

/// Durable request identity for a keyed mutation. The digest is computed from
/// the complete semantic payload before the mutation enters a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRequest {
    pub operation: Operation,
    pub resource: String,
    pub principal: String,
    pub key: String,
    pub payload_digest: String,
    pub now: i64,
    pub expires_at: i64,
}

impl IdempotencyRequest {
    pub fn from_payload<P: Serialize>(
        operation: Operation,
        resource: impl Into<String>,
        authority: &Authority,
        key: impl Into<String>,
        payload: &P,
        now: i64,
        ttl_seconds: i64,
    ) -> Result<Self> {
        let payload = serde_json::to_vec(payload)?;
        let resource = non_empty("resource", &resource.into())?;
        let principal = authority
            .principal_name()
            .ok_or_else(|| {
                DomainError::authority_denied(
                    DenialClass::Unauthenticated,
                    "keyed mutations require an authenticated principal",
                )
            })?
            .to_string();
        let key = non_empty("idempotency_key", &key.into())?;
        if ttl_seconds <= 0 {
            return Err(DomainError::validation("ttl_seconds", "must be positive").into());
        }
        Ok(Self {
            operation,
            resource,
            principal,
            key,
            payload_digest: format!("sha256:{:x}", Sha256::digest(payload)),
            now,
            expires_at: now.saturating_add(ttl_seconds),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyOutcome<T> {
    pub value: T,
    pub replayed: bool,
}

/// Shared request metadata for a keyed mutation.
#[derive(Debug, Clone, Copy)]
pub struct KeyedOperationContext<'a> {
    now: i64,
    idempotency_key: &'a str,
    authority: &'a Authority,
}

impl<'a> KeyedOperationContext<'a> {
    pub fn new(now: i64, idempotency_key: &'a str, authority: &'a Authority) -> Self {
        Self {
            now,
            idempotency_key,
            authority,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub q: String,
    pub status: Option<CardStatus>,
    pub repo: Option<String>,
    pub label: Option<String>,
    pub priority: Option<Priority>,
    pub created_after: Option<i64>,
    pub created_before: Option<i64>,
    pub updated_after: Option<i64>,
    pub updated_before: Option<i64>,
    pub limit: usize,
    pub after: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    pub card: CardSummary,
    pub source_kind: String,
    pub source_field: String,
    pub source_created_at: i64,
    pub snippet: String,
    pub blocked_by: Vec<CardId>,
    pub rank: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPage {
    pub matches: Vec<SearchResult>,
    pub total_count: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

/// Validates every schema-v17 key-to-actor mapping before the migration can
/// create, drop, or rewrite any table. Revoked keys are intentionally included:
/// silently deleting a revoked credential would still make the migration
/// lossy and would hide corrupt identity state from the operator.
fn preflight_schema_17_key_actors(transaction: &Transaction<'_>) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT api_keys.id, api_keys.actor_id, actors.id,
                actors.kind, actors.display_name
         FROM api_keys
         LEFT JOIN actors ON actors.id = api_keys.actor_id
         ORDER BY api_keys.id",
    )?;
    let mappings = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    let mut defects = Vec::new();
    for (key_id, actor_id, joined_actor_id, actor_kind, display_name) in mappings {
        let mut classes = Vec::new();
        if actor_id.is_none() {
            classes.push("null_actor_id");
        } else if joined_actor_id.is_none() {
            classes.push("dangling_actor_id");
        } else {
            if display_name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
            {
                classes.push("blank_display_name");
            }
            if actor_kind.as_deref().is_none_or(|kind| {
                !matches!(kind.trim().to_ascii_lowercase().as_str(), "agent" | "user")
            }) {
                classes.push("invalid_actor_kind");
            }
        }
        if !classes.is_empty() {
            defects.push(format!("{key_id} [{}]", classes.join(", ")));
        }
    }

    if defects.is_empty() {
        Ok(())
    } else {
        Err(StoreError::InvalidStoredValue {
            field: "schema v17 api key actor mapping",
            value: defects.join("; "),
        })
    }
}

/// Filter for [`Store::list_cards`].
#[derive(Debug, Clone)]
pub struct CardFilter {
    pub status: Option<CardStatus>,
    pub repo: Option<String>,
    pub label: Option<String>,
    pub include_terminal: bool,
}

impl Default for CardFilter {
    fn default() -> Self {
        CardFilter {
            status: None,
            repo: None,
            label: None,
            include_terminal: true,
        }
    }
}
#[derive(Debug)]
pub struct CardListPage {
    pub cards: Vec<Card>,
    pub total_count: usize,
    /// How many of `total_count` were held back by
    /// [`CardFilter::include_terminal`] being false. This stays separate so
    /// an envelope can distinguish matches beyond `limit` from cards hidden by
    /// the terminal filter.
    pub excluded_terminal_count: usize,
    /// powder-epic-ready-plan: ids from `cards`' *full eligible set* (before
    /// `limit` truncation, mirroring how `total_count` already describes
    /// the untruncated set) that sit **on** a `blocks`/`blocked_by` cycle
    /// among that eligible set -- the members of a strongly connected
    /// component, the only cards whose relative order cannot be
    /// topological (they order among themselves by the stable
    /// priority/age/id sort instead). Cards merely *downstream* of a cycle
    /// are never listed here: they keep a genuine topological position
    /// after the cycle that blocks them. Always empty for
    /// [`Store::list_cards_page`] (it never computes a topological order);
    /// populated only by [`Store::list_ready_page`]. See
    /// [`powder_core::order_ready_cards`] for why a cycle is reported here
    /// rather than causing a hang or a panic.
    pub cycle_card_ids: Vec<CardId>,
    /// powder-cards-api-paged-continuation: the id of the last card in
    /// `cards`, present only when the *same* already-computed,
    /// already-ordered list this call built (full scan, then filter, then
    /// sort/topological-order -- see [`Store::list_cards_page_after`]/
    /// [`Store::list_ready_page_after`]) has more cards beyond this page.
    /// Pass it back as `after` on the next call to resume immediately past
    /// it. This is an *interim* continuation over an in-memory list a call
    /// fully recomputes every time, not SQL-pushed keyset pagination -- it
    /// bounds response payload size, not per-request DB/CPU cost (that is
    /// the separate, deliberately-deferred
    /// `powder-store-sql-pushed-list-filtering` follow-up). `None` on the
    /// last page, or whenever the eligible set already fits within
    /// `limit`.
    pub next_after: Option<CardId>,
    /// Encoded durable Ready continuation when the page needs another page.
    /// The v3 token is opaque and store-backed; it contains no card IDs.
    pub ready_cursor: Option<String>,
}

/// Wall-clock seconds, for migration-generated timestamps only. Every
/// domain-facing write threads `now` in from its caller (so tests stay
/// deterministic); `migrate()` has no caller-supplied clock to thread
/// through, and a one-time schema migration's own audit-event timestamp is
/// infra bookkeeping, not a domain decision.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Explicit partial update for mutable card fields. Fields left as `None`
/// are preserved from the stored row. `Some(None)` clears the card's repo;
/// `None` leaves it unchanged.
#[derive(Debug, Clone, Default)]
pub struct CardPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub acceptance: Option<Vec<String>>,
    pub proof_plan: Option<Vec<String>>,
    pub status: Option<CardStatus>,
    pub priority: Option<Priority>,
    pub labels: Option<Vec<String>>,
    pub repo: Option<Option<String>>,
}
pub struct CriterionProofInput {
    pub criterion: usize,
    pub url: String,
}

struct MutationAudit<'a> {
    operation: Operation,
    event_type: CardEventType,
    actor: &'a str,
    change: CardEventChange,
    resource: &'a str,
    semantic_identity: Option<&'a str>,
    run_id: Option<&'a RunId>,
    reason: Option<&'a str>,
    subject_kind: &'a str,
    subject_id: &'a str,
    authority: &'a Authority,
}

/// Every authority-aware card mutation enters this evaluator after loading
/// the current claim snapshot. A worker can only act for its own principal,
/// current unexpired claim, semantic worker label, and (when supplied) run.
/// Admin/trusted-local authority is deliberately the correction escape hatch.
fn authorize_card_operation(
    authority: &Authority,
    operation: Operation,
    card: &Card,
    run_id: Option<&RunId>,
    worker: Option<&str>,
    now: i64,
) -> Result<()> {
    authority.authorize_operation_with_worker(
        operation,
        card.claim.as_ref(),
        run_id,
        worker,
        now,
    )?;
    if matches!(authority.role(), powder_core::PrincipalRole::Agent) {
        if let Some(target_run) = run_id {
            let Some(claim) = card.claim.as_ref() else {
                return Err(DomainError::authority_denied(
                    DenialClass::ClaimRequired,
                    format!("operation {} requires the current run", operation.as_str()),
                )
                .into());
            };
            if claim.run_id != *target_run {
                return Err(DomainError::authority_denied(
                    DenialClass::CrossResource,
                    format!("operation {} targets another run", operation.as_str()),
                )
                .into());
            }
        }
    }
    Ok(())
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
        store.connection.create_scalar_function(
            "powder_card_id_is_canonical",
            1,
            FunctionFlags::SQLITE_DETERMINISTIC,
            |context: &Context<'_>| {
                let raw = match context.get_raw(0) {
                    ValueRef::Text(raw) => match std::str::from_utf8(raw) {
                        Ok(raw) => raw,
                        Err(_) => return Ok(false),
                    },
                    _ => return Ok(false),
                };
                Ok(CardId::new(raw)
                    .map(|card_id| card_id.as_str() == raw)
                    .unwrap_or(false))
            },
        )?;
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
                self.ensure_principal_role_columns()?;
                return Ok(());
            }
            let next = match current {
                0 => {
                    self.connection.execute_batch(SCHEMA)?;
                    self.connection.execute_batch(SEARCH_SCHEMA)?;
                    SCHEMA_VERSION
                }
                1 => {
                    self.migrate_1_to_2()?;
                    2
                }
                2 => {
                    self.migrate_2_to_3()?;
                    3
                }
                3 => {
                    self.migrate_3_to_4()?;
                    4
                }
                4 => {
                    self.migrate_4_to_5()?;
                    5
                }
                5 => {
                    self.connection.execute_batch(MIGRATE_5_TO_6)?;
                    6
                }
                6 => {
                    self.connection.execute_batch(MIGRATE_6_TO_7)?;
                    7
                }
                7 => {
                    self.migrate_7_to_8()?;
                    8
                }
                8 => {
                    self.migrate_8_to_9()?;
                    9
                }
                9 => {
                    self.migrate_9_to_10()?;
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
                    self.migrate_13_to_14()?;
                    14
                }
                14 => {
                    self.migrate_14_to_15()?;
                    15
                }
                15 => {
                    self.migrate_15_to_16()?;
                    16
                }
                16 => {
                    self.migrate_16_to_17()?;
                    17
                }
                17 => {
                    self.migrate_17_to_18()?;
                    18
                }
                18 => {
                    self.migrate_18_to_19()?;
                    19
                }
                19 => {
                    self.migrate_19_to_20()?;
                    20
                }
                20 => {
                    self.migrate_20_to_21()?;
                    21
                }
                21 => {
                    self.migrate_21_to_22()?;
                    22
                }
                22 => {
                    self.migrate_22_to_23()?;
                    23
                }
                23 => {
                    self.migrate_23_to_24()?;
                    24
                }
                24 => {
                    self.migrate_24_to_25()?;
                    25
                }
                25 => {
                    self.migrate_25_to_26()?;
                    26
                }
                26 => {
                    self.migrate_26_to_27()?;
                    27
                }
                27 => {
                    self.migrate_27_to_28()?;
                    28
                }
                28 => {
                    self.migrate_28_to_29()?;
                    29
                }
                _ => return Err(StoreError::UnsupportedSchema(current)),
            };
            self.connection
                .execute_batch(&format!("PRAGMA user_version = {next}"))?;
        }
    }

    /// Canonical Store boundary for every matrix operation whose rule is
    /// keyed. Faces supply only semantic payload/resource data; principal and
    /// role come from the authenticated authority, and the closure runs inside
    /// the same transaction as the durable receipt.
    pub fn with_keyed_operation<T, P, F>(
        &mut self,
        operation: Operation,
        resource: impl Into<String>,
        payload: &P,
        context: KeyedOperationContext<'_>,
        mutation: F,
    ) -> Result<IdempotencyOutcome<T>>
    where
        T: Serialize + DeserializeOwned,
        P: Serialize,
        F: FnOnce(&Transaction<'_>) -> Result<T>,
    {
        let KeyedOperationContext {
            now,
            idempotency_key: key,
            authority,
        } = context;
        if !matches!(
            operation.rule().idempotency,
            powder_core::IdempotencyMode::Keyed
        ) {
            return Err(DomainError::validation(
                "operation",
                format!("{} is not keyed", operation.as_str()),
            )
            .into());
        }
        let request = IdempotencyRequest::from_payload(
            operation,
            resource,
            authority,
            key,
            payload,
            now,
            24 * 60 * 60,
        )?;
        self.with_idempotency(&request, mutation)
    }

    /// Execute one keyed mutation and persist its receipt in the same
    /// transaction as the mutation. Immediate locking serializes duplicate
    /// deliveries, so a replay returns the original value without running the
    /// mutation closure again; a different payload under the same key returns a
    /// stable structured idempotency_conflict denial.
    pub fn with_idempotency<T, F>(
        &mut self,
        request: &IdempotencyRequest,
        mutation: F,
    ) -> Result<IdempotencyOutcome<T>>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&Transaction<'_>) -> Result<T>,
    {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM operation_idempotency
             WHERE operation = ?1 AND resource = ?2 AND principal = ?3
               AND idempotency_key = ?4 AND expires_at <= ?5",
            params![
                request.operation.as_str(),
                request.resource,
                request.principal,
                request.key,
                request.now,
            ],
        )?;
        let existing = transaction
            .query_row(
                "SELECT payload_digest, receipt_json
                 FROM operation_idempotency
                 WHERE operation = ?1 AND resource = ?2 AND principal = ?3
                   AND idempotency_key = ?4",
                params![
                    request.operation.as_str(),
                    request.resource,
                    request.principal,
                    request.key,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((digest, receipt_json)) = existing {
            if digest != request.payload_digest {
                return Err(DomainError::authority_denied(
                    DenialClass::IdempotencyConflict,
                    format!(
                        "idempotency key already records a different payload for {} {}",
                        request.operation.as_str(),
                        request.resource
                    ),
                )
                .into());
            }
            let value = serde_json::from_str(&receipt_json)?;
            transaction.commit()?;
            return Ok(IdempotencyOutcome {
                value,
                replayed: true,
            });
        }
        let value = mutation(&transaction)?;
        let receipt_json = serde_json::to_string(&value)?;
        transaction.execute(
            "INSERT INTO operation_idempotency
             (operation, resource, principal, idempotency_key, payload_digest,
              receipt_json, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                request.operation.as_str(),
                request.resource,
                request.principal,
                request.key,
                request.payload_digest,
                receipt_json,
                request.now,
                request.expires_at,
            ],
        )?;
        transaction.commit()?;
        Ok(IdempotencyOutcome {
            value,
            replayed: false,
        })
    }

    /// Remove expired request receipts. This is bounded so an operator can run
    /// it safely from a maintenance loop without monopolizing the store.
    pub fn gc_idempotency(&mut self, now: i64, limit: usize) -> Result<usize> {
        if limit == 0 {
            return Ok(0);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM operation_idempotency
             WHERE rowid IN (
                 SELECT rowid FROM operation_idempotency
                 WHERE expires_at <= ?1 ORDER BY expires_at ASC, rowid ASC LIMIT ?2
             )",
            params![now, limit as i64],
        )?;
        transaction.commit()?;
        Ok(removed)
    }

    /// Searches through the FTS spine with SQL-side ranking, filtering, and
    /// pagination. Only the requested page is hydrated into `Card` values;
    /// the window count keeps `total_count` exact without materializing every
    /// matching source row in Rust.
    pub fn search_page(&self, query: &SearchQuery) -> Result<SearchPage> {
        let query_text = query.q.trim();
        if query_text.is_empty() || query.limit == 0 || !self.table_exists("card_search_fts")? {
            return Ok(SearchPage {
                matches: Vec::new(),
                total_count: 0,
                has_more: false,
                next_after: None,
            });
        }
        let Some(match_query) = rewrite_search_query(query_text) else {
            return Ok(SearchPage {
                matches: Vec::new(),
                total_count: 0,
                has_more: false,
                next_after: None,
            });
        };
        let fingerprint = search_query_fingerprint(query);
        let offset = query
            .after
            .as_deref()
            .map(|cursor| decode_search_cursor(cursor, &fingerprint))
            .transpose()?
            .unwrap_or(0);
        let label = query
            .label
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let id_prefix = format!("{}%", escape_like_prefix(query_text));
        let cte = r#"
            WITH matched(source_table, source_field, source_id, card_id, created_at, snippet, match_rank) AS (
                SELECT source_table, source_field, source_id, card_id, created_at,
                       snippet(card_search_fts, 5, '', '', '…', 32),
                       bm25(card_search_fts)
                FROM card_search_fts
                WHERE card_search_fts MATCH ?1
                UNION ALL
                SELECT 'cards', 'id', documents.card_id, documents.card_id, cards.created_at,
                       documents.card_id, -1000.0
                FROM search_documents documents
                JOIN cards ON cards.id = documents.card_id
                WHERE documents.source_table = 'cards'
                  AND (documents.card_id = ?2 OR documents.card_id LIKE ?3 ESCAPE '\')
                GROUP BY documents.card_id
            )
        "#;
        let filters = r#"
            WHERE (?4 IS NULL OR c.status = ?4)
              AND (?5 IS NULL OR c.priority = ?5)
              AND (?6 IS NULL OR c.created_at >= ?6)
              AND (?7 IS NULL OR c.created_at <= ?7)
              AND (?8 IS NULL OR c.updated_at >= ?8)
              AND (?9 IS NULL OR c.updated_at <= ?9)
              AND (?10 IS NULL OR EXISTS (
                    SELECT 1 FROM json_each(c.labels_json)
                    WHERE lower(json_each.value) = lower(?10)
                  ))
              AND (?11 IS NULL OR c.repo = ?11)
        "#;
        let page_sql = format!(
            "{cte} SELECT m.source_table, m.source_field, m.source_id, m.card_id,
                    m.created_at, m.snippet, m.match_rank, COUNT(*) OVER()
             FROM matched m
             JOIN cards c ON c.id = m.card_id
             {filters}
             ORDER BY m.match_rank, m.source_table, m.source_field, m.card_id, m.source_id, m.created_at
             LIMIT ?12 OFFSET ?13"
        );
        let mut statement = self.connection.prepare(&page_sql)?;
        let mut rows = statement.query(params![
            match_query,
            query_text,
            id_prefix,
            query.status.map(|status| status.as_str()),
            query.priority.map(|priority| priority.as_str()),
            query.created_after,
            query.created_before,
            query.updated_after,
            query.updated_before,
            label,
            query.repo.as_deref(),
            query.limit as i64,
            offset as i64,
        ])?;
        let mut raw_rows = Vec::new();
        let mut total_count = None;
        while let Some(row) = rows.next()? {
            total_count = Some(row.get::<_, i64>(7)? as usize);
            raw_rows.push(RawSearchMatch {
                source_table: row.get(0)?,
                source_field: row.get(1)?,
                card_id: CardId::new(row.get::<_, String>(3)?)?,
                created_at: row.get(4)?,
                snippet: row.get(5)?,
                rank: row.get(6)?,
            });
        }
        drop(rows);
        drop(statement);

        let total_count = match total_count {
            Some(total_count) => total_count,
            None => {
                let count_sql = format!(
                    "{cte} SELECT COUNT(*)
                     FROM matched m
                     JOIN cards c ON c.id = m.card_id
                     {filters}"
                );
                self.connection.query_row(
                    &count_sql,
                    params![
                        match_query,
                        query_text,
                        id_prefix,
                        query.status.map(|status| status.as_str()),
                        query.priority.map(|priority| priority.as_str()),
                        query.created_after,
                        query.created_before,
                        query.updated_after,
                        query.updated_before,
                        label,
                        query.repo.as_deref(),
                    ],
                    |row| row.get::<_, i64>(0),
                )? as usize
            }
        };

        let mut summaries = HashMap::<String, CardSummary>::new();
        let mut blockers = HashMap::<String, Vec<CardId>>::new();
        let mut results = Vec::with_capacity(raw_rows.len());
        for row in raw_rows {
            let key = row.card_id.as_str().to_string();
            let (summary, blocked_by) = if let Some(summary) = summaries.get(&key).cloned() {
                (summary, blockers.get(&key).cloned().unwrap_or_default())
            } else {
                let card = match self.get_card(&row.card_id)? {
                    Some(card) => card,
                    None => continue,
                };
                let summary = card.summary();
                let blocked_by = card.blocked_by.clone();
                summaries.insert(key.clone(), summary.clone());
                blockers.insert(key, blocked_by.clone());
                (summary, blocked_by)
            };
            results.push(SearchResult {
                card: summary,
                blocked_by,
                source_kind: row.source_table,
                source_field: row.source_field,
                source_created_at: row.created_at,
                snippet: row.snippet,
                rank: row.rank,
            });
        }
        let end = offset.saturating_add(query.limit).min(total_count);
        let has_more = end < total_count;
        Ok(SearchPage {
            matches: results,
            total_count,
            has_more,
            next_after: has_more.then(|| encode_search_cursor(&fingerprint, end)),
        })
    }

    fn migrate_1_to_2(&mut self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS actors (
              id TEXT PRIMARY KEY,
              kind TEXT NOT NULL,
              display_name TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );",
        )?;
        if !self.table_has_column("api_keys", "actor_id")? {
            self.connection
                .execute_batch("ALTER TABLE api_keys ADD COLUMN actor_id TEXT;")?;
        }
        let backfill_incomplete = self
            .connection
            .query_row(
                "SELECT 1 FROM api_keys WHERE actor_id IS NULL LIMIT 1",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if backfill_incomplete {
            self.connection.execute_batch(
                "INSERT OR IGNORE INTO actors (id, kind, display_name, created_at)
                 SELECT
                   'actor-' || id,
                   CASE scope WHEN 'agent' THEN 'agent' ELSE 'user' END,
                   name,
                   created_at
                 FROM api_keys
                 WHERE actor_id IS NULL;

                 UPDATE api_keys
                 SET actor_id = 'actor-' || id
                 WHERE actor_id IS NULL;",
            )?;
        }
        self.connection.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);",
        )?;
        Ok(())
    }

    fn migrate_2_to_3(&mut self) -> Result<()> {
        if !self.table_has_column("api_keys", "hash_algorithm")? {
            self.connection.execute_batch(MIGRATE_2_TO_3)?;
        }
        Ok(())
    }

    /// Unlike the ADD-COLUMN steps above, this step drops six columns from
    /// `runs` in one batch -- a crash could leave some already dropped and
    /// others not. Each column is checked and dropped independently
    /// (mirroring `migrate_14_to_15`'s partial-drop recovery) instead of
    /// guarding the whole batch behind a single column, which would either
    /// re-run a `DROP COLUMN` against an already-missing column (error) or
    /// skip columns that still need dropping.
    fn migrate_3_to_4(&mut self) -> Result<()> {
        for column in [
            "model",
            "turn_count",
            "token_count",
            "consecutive_failures",
            "last_error",
            "result",
        ] {
            if self.table_has_column("runs", column)? {
                self.connection
                    .execute_batch(&format!("ALTER TABLE runs DROP COLUMN {column};"))?;
            }
        }
        Ok(())
    }

    fn migrate_4_to_5(&mut self) -> Result<()> {
        if !self.cards_has_column("related_json")? {
            self.connection.execute_batch(
                "ALTER TABLE cards ADD COLUMN related_json TEXT NOT NULL DEFAULT '[]';",
            )?;
        }
        if !self.cards_has_column("blocks_json")? {
            self.connection.execute_batch(
                "ALTER TABLE cards ADD COLUMN blocks_json TEXT NOT NULL DEFAULT '[]';",
            )?;
        }
        // The table/index half of MIGRATE_4_TO_5 already uses `IF NOT
        // EXISTS` and is safe to run unconditionally regardless of which
        // ALTER above just ran.
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS card_events (
              id TEXT PRIMARY KEY,
              card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
              event_type TEXT NOT NULL,
              actor TEXT NOT NULL,
              payload TEXT NOT NULL,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_card_events_card_created ON card_events(card_id, created_at);",
        )?;
        Ok(())
    }

    fn migrate_7_to_8(&mut self) -> Result<()> {
        if !self.table_has_column("repositories", "tier")? {
            self.connection.execute_batch(MIGRATE_7_TO_8)?;
        } else {
            // MIGRATE_7_TO_8 also creates idx_repositories_tier; if a prior
            // run added the column but crashed before the index, the guard
            // above would skip both. `IF NOT EXISTS` makes re-issuing the
            // index safe on its own.
            self.connection.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_repositories_tier ON repositories(tier, name);",
            )?;
        }
        Ok(())
    }

    fn migrate_8_to_9(&mut self) -> Result<()> {
        if !self.cards_has_column("criteria_json")? {
            self.connection.execute_batch(
                "ALTER TABLE cards ADD COLUMN criteria_json TEXT NOT NULL DEFAULT '[]';",
            )?;
        }
        if !self.cards_has_column("proof_plan_json")? {
            self.connection.execute_batch(
                "ALTER TABLE cards ADD COLUMN proof_plan_json TEXT NOT NULL DEFAULT '[]';",
            )?;
        }
        Ok(())
    }

    fn migrate_9_to_10(&mut self) -> Result<()> {
        if !self.table_has_column("api_keys", "last_used_at")? {
            self.connection.execute_batch(MIGRATE_9_TO_10)?;
        }
        Ok(())
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

    fn migrate_13_to_14(&mut self) -> Result<()> {
        if !self.cards_has_column("parent")? {
            self.connection.execute_batch(MIGRATE_13_TO_14)?;
        }
        Ok(())
    }

    fn migrate_14_to_15(&mut self) -> Result<()> {
        if self.cards_has_column("workspace_path")? {
            self.connection.execute_batch(MIGRATE_14_TO_15)?;
        } else if self.cards_has_column("branch_name")? {
            // MIGRATE_14_TO_15 drops both columns in one batch; if a prior
            // run crashed between the two ALTERs, workspace_path is already
            // gone but branch_name is still there. Re-running the full
            // batch would fail on `DROP COLUMN workspace_path` against a
            // column that no longer exists, so finish the other half alone.
            self.connection
                .execute_batch("ALTER TABLE cards DROP COLUMN branch_name;")?;
        }
        Ok(())
    }

    fn migrate_15_to_16(&mut self) -> Result<()> {
        if self.cards_has_column("autonomy")? {
            self.connection.execute_batch(MIGRATE_15_TO_16)?;
        }
        Ok(())
    }

    /// powder-status-vocabulary: collapses the nine-status vocabulary to
    /// seven. A `claimed`/`running` card with a complete claim becomes
    /// `in_progress` -- the claim struct already carries who/lease/liveness,
    /// so a status bit distinguishing "claimed but not yet running" from
    /// "running" was a second, driftable copy of claim presence. A claimless
    /// legacy card instead returns to `ready` when it carries an acceptance
    /// oracle, or `backlog` when it does not; otherwise it would be stranded
    /// in `in_progress`, where neither `list_ready` nor a fresh claim can
    /// recover it. Malformed partial or complete-but-blank claim columns count
    /// as claimless through the same decoder used by [`CardRecord::into_card`],
    /// and their stored bytes remain untouched. Structured criteria are
    /// authoritative over the legacy acceptance list through that same shared
    /// card decoder. `blocked` is dropped entirely: blocking
    /// eligibility is already derived from `blocked_by` relations at claim
    /// time ([`powder_core::Card::claim_readiness`]) regardless of status, so
    /// an explicit `blocked` status was a second, driftable copy of that
    /// derived fact.
    ///
    /// Where a former-`blocked` card lands depends on what it actually
    /// carries:
    /// - real `blocked_by` relations -> `ready`: `list_ready`/claiming keep
    ///   excluding it until every blocker resolves, so nothing becomes
    ///   claimable that was not already;
    /// - non-empty acceptance but NO `blocked_by` relations -> `backlog`:
    ///   on the live board most blocked cards record their blocker only as
    ///   prose (operator timers, missing secrets, vendor bugs, pending
    ///   decisions) with zero relations wired, and mapping those to `ready`
    ///   would make them immediately claimable by the fleet with no
    ///   compensating control. Backlog forces a human re-triage: wire the
    ///   relations or promote deliberately (adversarial review of PR #134,
    ///   ratified 2026-07-14);
    /// - empty acceptance -> `backlog`, mirroring
    ///   [`CardStatus::default_for_acceptance`], the same rule a freshly
    ///   created card is defaulted by ("ready is a query, not vibes",
    ///   VISION.md).
    ///
    /// Every other status (`backlog`, `ready`, `awaiting_input`, `done`,
    /// `shipped`, `abandoned`) is untouched -- `awaiting_input` stays
    /// first-class and queryable, and the three terminal outcomes stay
    /// distinguishable (operator ruling, 2026-07-14). Claims/runs/
    /// relations/events are never touched by this migration; only the
    /// `status` column on affected cards changes, plus one audit
    /// `card_events` row per changed card. Idempotent: guarded by the
    /// surrounding `migrate()` loop, which only ever runs the 16->17 step
    /// once (a database already at or past schema 17 never re-enters this
    /// function), and the whole step commits atomically so a crash
    /// mid-migration leaves the prior schema version to retry cleanly
    /// rather than a half-applied status column.
    fn migrate_16_to_17(&mut self) -> Result<()> {
        // Every real database has carried `status NOT NULL` since schema
        // creation (v0); this guard exists only so a synthetic test double
        // that fabricates a bare `cards(id)` table to exercise one unrelated
        // intermediate migration step (see e.g.
        // `migration_11_to_12_tolerates_half_applied_autonomy_column`) can
        // still walk `migrate()` all the way to current without growing a
        // phantom `status` column it has no reason to carry.
        if !self.cards_has_column("status")? {
            return Ok(());
        }
        let now = unix_now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = transaction.prepare(
                "SELECT id, status, acceptance_json, criteria_json, blocked_by_json,
                        claim_agent, claim_run_id, claim_acquired_at, claim_expires_at
                 FROM cards
                 WHERE status IN ('claimed', 'running', 'blocked')
                 ORDER BY id",
            )?;
            let affected = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, Option<i64>>(8)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            for (
                card_id,
                old_status,
                acceptance_json,
                criteria_json,
                blocked_by_json,
                claim_agent,
                claim_run_id,
                claim_acquired_at,
                claim_expires_at,
            ) in affected
            {
                let oracle = decode_stored_oracle(acceptance_json, criteria_json)?;
                let has_acceptance = !oracle.acceptance.is_empty();
                let blocked_by =
                    from_json::<Vec<String>>("cards.blocked_by_json", blocked_by_json)?;
                let has_blocked_by = blocked_by.iter().any(|id| !id.trim().is_empty());
                let has_valid_claim = decode_stored_claim(
                    claim_agent.clone(),
                    claim_agent,
                    claim_run_id,
                    claim_acquired_at,
                    claim_expires_at,
                )?
                .is_some();
                let new_status = match old_status.as_str() {
                    "claimed" | "running" if has_valid_claim => "in_progress",
                    "claimed" | "running" if has_acceptance => "ready",
                    "claimed" | "running" => "backlog",
                    "blocked" if !has_acceptance || !has_blocked_by => "backlog",
                    "blocked" => "ready",
                    other => other,
                };
                transaction.execute(
                    "UPDATE cards SET status = ?1 WHERE id = ?2",
                    params![new_status, card_id],
                )?;
                let current = CardStatus::parse(new_status).ok_or_else(|| {
                    DomainError::validation("event.status", "invalid migrated status")
                })?;
                let previous = match old_status.as_str() {
                    "claimed" | "running" => CardStatus::InProgress,
                    "blocked" => current,
                    other => CardStatus::parse(other).ok_or_else(|| {
                        DomainError::validation("event.status", "invalid stored status")
                    })?,
                };
                append_card_event(
                    &transaction,
                    &CardId::new(card_id)?,
                    CardEventType::Status,
                    "system:status-vocabulary-migration",
                    CardEventChange::Status { previous, current },
                    now,
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Separates credential principal from semantic worker/run identity.
    /// Existing keys retain their hashes, prefixes, scopes, revocation and
    /// last-used metadata; the former actor display name becomes the neutral
    /// principal. Existing live leases use their worker label as the best
    /// lossless legacy principal because older schemas recorded no other
    /// authenticated identity on the claim or run.
    fn migrate_17_to_18(&mut self) -> Result<()> {
        let has_legacy_keys = self.table_has_column("api_keys", "actor_id")?;
        let needs_card_principal =
            self.cards_has_column("claim_agent")? && !self.cards_has_column("claim_principal")?;
        let needs_run_principal = self.table_has_column("runs", "agent")?
            && !self.table_has_column("runs", "principal")?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if has_legacy_keys {
            preflight_schema_17_key_actors(&transaction)?;
            transaction.execute_batch(
                "CREATE TABLE api_keys_v18 (
                   id TEXT PRIMARY KEY,
                   principal TEXT NOT NULL,
                   name TEXT NOT NULL,
                   key_prefix TEXT NOT NULL,
                   key_hash TEXT NOT NULL,
                   hash_algorithm TEXT NOT NULL DEFAULT 'sha256',
                   scope TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   revoked_at INTEGER,
                   last_used_at INTEGER
                 );
                 INSERT INTO api_keys_v18
                   (id, principal, name, key_prefix, key_hash, hash_algorithm,
                    scope, created_at, revoked_at, last_used_at)
                 SELECT api_keys.id, actors.display_name, api_keys.name,
                        api_keys.key_prefix, api_keys.key_hash,
                        api_keys.hash_algorithm, api_keys.scope,
                        api_keys.created_at, api_keys.revoked_at,
                        api_keys.last_used_at
                 FROM api_keys
                 JOIN actors ON actors.id = api_keys.actor_id;
                 DROP TABLE api_keys;
                 ALTER TABLE api_keys_v18 RENAME TO api_keys;
                 CREATE INDEX idx_api_keys_prefix
                   ON api_keys(key_prefix, revoked_at);
                 DROP TABLE actors;",
            )?;
        }
        if needs_card_principal {
            transaction.execute_batch(
                "ALTER TABLE cards ADD COLUMN claim_principal TEXT;
                 UPDATE cards
                 SET claim_principal = claim_agent
                 WHERE claim_agent IS NOT NULL;",
            )?;
        }
        if needs_run_principal {
            transaction.execute_batch(
                "ALTER TABLE runs
                   ADD COLUMN principal TEXT NOT NULL DEFAULT 'legacy';
                 UPDATE runs SET principal = agent;",
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Repairs the seven claimless production cards that schema v17 moved
    /// from `claimed`/`running` to `in_progress` before claim decoding was
    /// unified. Selection is provenance-based rather than id-based: a card
    /// must still be `in_progress`, carry one of the exact v17 migration
    /// events that created that status, and decode as claimless under the
    /// same principal/worker/run decoder used by normal card reads. The
    /// effective acceptance oracle likewise uses the shared card decoder.
    ///
    /// Apart from the corrected status and one explicit repair event, every
    /// persisted byte is left untouched. The status predicate also makes the
    /// step safe to retry if the transaction commits before `user_version`
    /// is advanced: repaired rows no longer match on the second pass.
    fn migrate_18_to_19(&mut self) -> Result<()> {
        // A database claiming schema v18 must carry every field the repair
        // reads. Fail closed on schema drift so the outer migration loop
        // cannot advance `user_version` while silently skipping the repair.
        for column in [
            "status",
            "acceptance_json",
            "criteria_json",
            "claim_principal",
            "claim_agent",
            "claim_run_id",
            "claim_acquired_at",
            "claim_expires_at",
        ] {
            if !self.cards_has_column(column)? {
                return Err(StoreError::InvalidStoredValue {
                    field: "schema v18",
                    value: format!("missing cards.{column}"),
                });
            }
        }
        for column in [
            "id",
            "card_id",
            "event_type",
            "actor",
            "payload",
            "created_at",
        ] {
            if !self.table_has_column("card_events", column)? {
                return Err(StoreError::InvalidStoredValue {
                    field: "schema v18",
                    value: format!("missing card_events.{column}"),
                });
            }
        }
        for column in [
            "id",
            "card_id",
            "state",
            "principal",
            "agent",
            "claim_expires_at",
            "proof",
            "created_at",
            "updated_at",
        ] {
            if !self.table_has_column("runs", column)? {
                return Err(StoreError::InvalidStoredValue {
                    field: "schema v18",
                    value: format!("missing runs.{column}"),
                });
            }
        }
        for column in [
            "id",
            "principal",
            "name",
            "key_prefix",
            "key_hash",
            "hash_algorithm",
            "scope",
            "created_at",
            "revoked_at",
            "last_used_at",
        ] {
            if !self.table_has_column("api_keys", column)? {
                return Err(StoreError::InvalidStoredValue {
                    field: "schema v18",
                    value: format!("missing api_keys.{column}"),
                });
            }
        }
        if self.table_exists("actors")? {
            return Err(StoreError::InvalidStoredValue {
                field: "schema v18",
                value: "legacy actors table still present".to_string(),
            });
        }

        let now = unix_now();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = transaction.prepare(
                "SELECT c.id, c.acceptance_json, c.criteria_json,
                        c.claim_principal, c.claim_agent, c.claim_run_id,
                        c.claim_acquired_at, c.claim_expires_at
                 FROM cards c
                 WHERE c.status = 'in_progress'
                   AND EXISTS (
                     SELECT 1
                     FROM card_events e
                     WHERE e.card_id = c.id
                       AND e.event_type = 'status'
                       AND e.actor = 'system:status-vocabulary-migration'
                       AND (
                         e.payload IN (
                           'status-vocabulary migration: claimed -> in_progress',
                           'status-vocabulary migration: running -> in_progress'
                         )
                         OR e.payload LIKE '%\"kind\":\"status\"%'

                       )
                       AND e.created_at = (
                         SELECT MAX(latest.created_at)
                         FROM card_events latest
                         WHERE latest.card_id = c.id
                           AND latest.event_type = 'status'
                       )
                       AND NOT EXISTS (
                         SELECT 1
                         FROM card_events ambiguous
                         WHERE ambiguous.card_id = c.id
                           AND ambiguous.event_type = 'status'
                           AND ambiguous.created_at >= e.created_at
                           AND ambiguous.id <> e.id
                           AND (
                             ambiguous.actor <> e.actor
                             OR ambiguous.payload <> e.payload
                           )
                       )
                   )
                 ORDER BY c.id",
            )?;
            let candidates = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);

            for (
                card_id,
                acceptance_json,
                criteria_json,
                claim_principal,
                claim_agent,
                claim_run_id,
                claim_acquired_at,
                claim_expires_at,
            ) in candidates
            {
                if decode_stored_claim(
                    claim_principal,
                    claim_agent,
                    claim_run_id,
                    claim_acquired_at,
                    claim_expires_at,
                )?
                .is_some()
                {
                    continue;
                }

                let oracle = decode_stored_oracle(acceptance_json, criteria_json)?;
                let new_status = if oracle.acceptance.is_empty() {
                    "backlog"
                } else {
                    "ready"
                };
                transaction.execute(
                    "UPDATE cards
                     SET status = ?1
                     WHERE id = ?2 AND status = 'in_progress'",
                    params![new_status, card_id],
                )?;
                append_card_event(
                    &transaction,
                    &CardId::new(card_id)?,
                    CardEventType::Status,
                    "system:status-v17-repair",
                    CardEventChange::Status {
                        previous: CardStatus::InProgress,
                        current: CardStatus::parse(new_status).ok_or_else(|| {
                            DomainError::validation("event.status", "invalid migrated status")
                        })?,
                    },
                    now,
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Adds authenticated provenance to the shared mutation-audit envelope.
    /// Every legacy value remains untouched: the new columns are nullable,
    /// so old card events and outbound payloads retain their exact bytes and
    /// explicitly carry unknown provenance rather than a fabricated identity.
    fn migrate_20_to_21(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS attachments (
               id TEXT PRIMARY KEY,
               mime TEXT NOT NULL,
               size INTEGER NOT NULL,
               bytes BLOB NOT NULL,
               created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS card_attachments (
               card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
               attachment_id TEXT NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
               filename TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               principal TEXT NOT NULL,
               PRIMARY KEY(card_id, attachment_id)
             );
             CREATE INDEX IF NOT EXISTS idx_card_attachments_card_created
               ON card_attachments(card_id, created_at, attachment_id);",
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// powder-risk-signal-field: the orthogonal blast-radius x
    /// reversibility x uncertainty axis alongside `estimate` (v12->v13).
    /// Nullable, guarded the same way `migrate_12_to_13` guards
    /// `estimate` -- a crash between this `ALTER TABLE` and the
    /// `PRAGMA user_version` bump must not re-issue the same `ADD COLUMN`
    /// on retry and fail with "duplicate column name".
    fn migrate_21_to_22(&mut self) -> Result<()> {
        if !self.cards_has_column("risk")? {
            self.connection
                .execute_batch("ALTER TABLE cards ADD COLUMN risk TEXT;")?;
        }
        Ok(())
    }

    /// Creates and backfills the external-content FTS5 index. Every source
    /// row is copied through the same trigger-maintained search spine used by
    /// live writes, so retrying after a crash can only replace the same
    /// source-keyed documents and rebuild the derived index from source truth.
    fn migrate_22_to_23(&mut self) -> Result<()> {
        // A few unit fixtures intentionally carry only the table needed by an
        // earlier migration. They are not searchable databases, so leave them
        // untouched rather than creating triggers against absent source tables.
        for table in ["cards", "comments", "work_log_entries"] {
            if !self.table_exists(table)? {
                return Ok(());
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SEARCH_SCHEMA)?;
        transaction.execute_batch(
            "INSERT OR REPLACE INTO search_documents
               (source_table, source_field, source_id, created_at, card_id, content)
             SELECT 'cards', 'title', id, created_at, id, title FROM cards
             UNION ALL
             SELECT 'cards', 'body', id, created_at, id, body FROM cards
             UNION ALL
             SELECT 'cards', 'criteria', id, created_at, id,
                    COALESCE(
                      NULLIF((SELECT group_concat(json_extract(value, '$.text'), ' ')
                        FROM json_each(cards.criteria_json)
                        WHERE json_type(value, '$.text') = 'text'), ''),
                      (SELECT group_concat(value, ' ') FROM json_each(cards.acceptance_json)),
                      '')
             FROM cards
             UNION ALL
             SELECT 'comments', 'body', id, created_at, card_id, body FROM comments
             UNION ALL
             SELECT 'work_log_entries', 'body', id, created_at, card_id, body
             FROM work_log_entries;",
        )?;
        transaction.execute(
            "INSERT INTO card_search_fts(card_search_fts) VALUES ('rebuild')",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Rebuilds the external-content FTS index with card IDs kept as exact
    /// metadata rather than indexed text. Card IDs are searched through the
    /// explicit source-document path in `search_page`, so an identifier hit
    /// has the exact `cards/id` provenance instead of one false hit per
    /// title/body/criteria document.
    fn migrate_23_to_24(&mut self) -> Result<()> {
        if !self.table_exists("card_search_fts")? {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "DROP TABLE card_search_fts;
             CREATE VIRTUAL TABLE card_search_fts USING fts5(
               source_table UNINDEXED,
               source_field UNINDEXED,
               source_id UNINDEXED,
               created_at UNINDEXED,
               card_id UNINDEXED,
               content,
               content='search_documents',
               content_rowid='doc_id',
               tokenize = 'unicode61 tokenchars ''-_'''
             );
             INSERT INTO card_search_fts(card_search_fts) VALUES ('rebuild');",
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_24_to_25(&self) -> Result<()> {
        self.ensure_principal_role_columns()
    }

    fn migrate_26_to_27(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (column, sql) in [
            (
                "operation",
                "ALTER TABLE card_events ADD COLUMN operation TEXT;",
            ),
            (
                "resource",
                "ALTER TABLE card_events ADD COLUMN resource TEXT;",
            ),
            (
                "semantic_identity",
                "ALTER TABLE card_events ADD COLUMN semantic_identity TEXT;",
            ),
            ("run_id", "ALTER TABLE card_events ADD COLUMN run_id TEXT;"),
            ("reason", "ALTER TABLE card_events ADD COLUMN reason TEXT;"),
        ] {
            let present: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('card_events') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )?;
            if present == 0 {
                transaction.execute_batch(sql)?;
            }
        }
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS operation_idempotency (
               operation TEXT NOT NULL,
               resource TEXT NOT NULL,
               principal TEXT NOT NULL,
               idempotency_key TEXT NOT NULL,
               payload_digest TEXT NOT NULL,
               receipt_json TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               expires_at INTEGER NOT NULL,
               PRIMARY KEY(operation, resource, principal, idempotency_key)
             );
             CREATE INDEX IF NOT EXISTS idx_operation_idempotency_expires
               ON operation_idempotency(expires_at);",
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_27_to_28(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (column, sql) in [
            (
                "telemetry_attempt_count",
                "ALTER TABLE runs ADD COLUMN telemetry_attempt_count INTEGER;",
            ),
            (
                "telemetry_input_tokens",
                "ALTER TABLE runs ADD COLUMN telemetry_input_tokens INTEGER;",
            ),
            (
                "telemetry_output_tokens",
                "ALTER TABLE runs ADD COLUMN telemetry_output_tokens INTEGER;",
            ),
            (
                "telemetry_reasoning_tokens",
                "ALTER TABLE runs ADD COLUMN telemetry_reasoning_tokens INTEGER;",
            ),
            (
                "telemetry_estimated_cost_usd_micros",
                "ALTER TABLE runs ADD COLUMN telemetry_estimated_cost_usd_micros INTEGER;",
            ),
            (
                "telemetry_duration_ms",
                "ALTER TABLE runs ADD COLUMN telemetry_duration_ms INTEGER;",
            ),
            (
                "telemetry_pricing_version",
                "ALTER TABLE runs ADD COLUMN telemetry_pricing_version TEXT;",
            ),
            (
                "telemetry_outcome",
                "ALTER TABLE runs ADD COLUMN telemetry_outcome TEXT;",
            ),
            (
                "telemetry_unattributed_attempt_count",
                "ALTER TABLE runs ADD COLUMN telemetry_unattributed_attempt_count INTEGER;",
            ),
        ] {
            let present: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('runs') WHERE name = ?1",
                [column],
                |row| row.get(0),
            )?;
            if present == 0 {
                transaction.execute_batch(sql)?;
            }
        }

        transaction.execute_batch("CREATE TABLE IF NOT EXISTS run_telemetry_attempts (id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE, provider TEXT, model TEXT, harness TEXT, reasoning TEXT, input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER, estimated_cost_usd_micros INTEGER, duration_ms INTEGER, outcome TEXT, pricing_version TEXT, input_rate_usd_per_million_micros INTEGER, output_rate_usd_per_million_micros INTEGER, reasoning_rate_usd_per_million_micros INTEGER, principal TEXT, created_at INTEGER NOT NULL); CREATE INDEX IF NOT EXISTS idx_run_telemetry_attempts_run ON run_telemetry_attempts(run_id, created_at, id); CREATE INDEX IF NOT EXISTS idx_run_telemetry_attempts_model ON run_telemetry_attempts(model, provider, created_at);")?;
        transaction.commit()?;
        Ok(())
    }
    /// Normalizes retired typed run and activity spellings in one transaction.
    /// The optional activities-table guard keeps older partial schemas retryable.
    fn migrate_28_to_29(&mut self) -> Result<()> {
        let has_activities = self.table_exists("activities")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE runs SET state = 'active' WHERE state = 'running'",
            [],
        )?;
        if has_activities {
            transaction.execute(
                "UPDATE activities
                 SET activity_type = 'elicitation'
                 WHERE activity_type = 'question'",
                [],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn migrate_25_to_26(&self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS ready_snapshots (
               id TEXT PRIMARY KEY,
               query_fingerprint TEXT NOT NULL,
               ordered_digest TEXT NOT NULL DEFAULT '',
               created_at INTEGER NOT NULL,
               expires_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_ready_snapshots_expires ON ready_snapshots(expires_at);
             CREATE INDEX IF NOT EXISTS idx_ready_snapshots_query_digest
               ON ready_snapshots(query_fingerprint, ordered_digest, expires_at);
             CREATE TABLE IF NOT EXISTS ready_snapshot_items (
               snapshot_id TEXT NOT NULL REFERENCES ready_snapshots(id) ON DELETE CASCADE,
               position INTEGER NOT NULL,
               card_id TEXT NOT NULL,
               PRIMARY KEY(snapshot_id, position),
               UNIQUE(snapshot_id, card_id)
             );
             CREATE INDEX IF NOT EXISTS idx_ready_snapshot_items_card ON ready_snapshot_items(snapshot_id, card_id);",
        )?;
        Ok(())
    }

    fn migrate_19_to_20(&mut self) -> Result<()> {
        let needs_principal = !self.table_has_column("card_events", "principal")?;
        let needs_subject_kind = !self.table_has_column("card_events", "subject_kind")?;
        let needs_subject_id = !self.table_has_column("card_events", "subject_id")?;
        let needs_audit_event_id = !self.table_has_column("outbound_events", "audit_event_id")?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if needs_principal {
            transaction.execute_batch("ALTER TABLE card_events ADD COLUMN principal TEXT;")?;
        }
        if needs_subject_kind {
            transaction.execute_batch("ALTER TABLE card_events ADD COLUMN subject_kind TEXT;")?;
        }
        if needs_subject_id {
            transaction.execute_batch("ALTER TABLE card_events ADD COLUMN subject_id TEXT;")?;
        }
        if needs_audit_event_id {
            transaction.execute_batch(
                "ALTER TABLE outbound_events
                   ADD COLUMN audit_event_id TEXT
                   REFERENCES card_events(id) ON DELETE SET NULL;",
            )?;
        }
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_card_events_subject
               ON card_events(card_id, subject_kind, subject_id);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_outbound_events_audit
               ON outbound_events(audit_event_id)
               WHERE audit_event_id IS NOT NULL;",
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn cards_has_column(&self, column: &str) -> Result<bool> {
        self.table_has_column("cards", column)
    }

    fn table_exists(&self, table: &str) -> Result<bool> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_master
               WHERE type = 'table' AND name = ?1
             )",
            [table],
            |row| row.get(0),
        )?)
    }

    /// `table` is always an internal, hardcoded literal from a call site in
    /// this module -- never caller/user-controlled -- so interpolating it
    /// into the `PRAGMA table_info(...)` statement (which cannot bind table
    /// names as parameters) carries no injection risk.
    fn table_has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(columns.iter().any(|name| name.eq_ignore_ascii_case(column)))
    }

    /// Proves the database file itself is writable, not just readable. A
    /// read-only file, a full disk that still permits reads, or a replication
    /// target mid-restore can still answer read queries. `BEGIN IMMEDIATE`
    /// acquires SQLite's write lock up front (unlike a deferred `BEGIN`, which
    /// only acquires it on the first write and would let a read-only file pass),
    /// so failure here means an actual write is currently impossible. The
    /// transaction never writes anything and always rolls back -- this is a
    /// probe, not a mutation.
    pub fn writable_probe(&self) -> Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")?;
        Ok(())
    }

    fn ensure_principal_role_columns(&self) -> Result<()> {
        if self.table_exists("runs")? && !self.table_has_column("runs", "role")? {
            self.connection
                .execute_batch("ALTER TABLE runs ADD COLUMN role TEXT NOT NULL DEFAULT 'agent';")?;
        }
        if self.table_exists("activities")? {
            if !self.table_has_column("activities", "principal")? {
                self.connection
                    .execute_batch("ALTER TABLE activities ADD COLUMN principal TEXT;")?;
            }
            if !self.table_has_column("activities", "role")? {
                self.connection
                    .execute_batch("ALTER TABLE activities ADD COLUMN role TEXT;")?;
            }
        }
        if self.table_exists("card_events")? && !self.table_has_column("card_events", "role")? {
            self.connection
                .execute_batch("ALTER TABLE card_events ADD COLUMN role TEXT;")?;
        }
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

    pub fn upsert_card(&mut self, card: Card) -> Result<Card> {
        let card_id = card.id.clone();
        persist_card(&self.connection, &card)?;
        load_card(&self.connection, &card_id)
    }

    pub fn create_card_with_events(&mut self, card: Card, actor: &str, now: i64) -> Result<Card> {
        self.create_card_with_events_as(card, &Authority::actor(actor.to_owned(), false), now)
    }

    pub fn create_card_with_events_as(
        &mut self,
        card: Card,
        authority: &Authority,
        now: i64,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let saved = create_card_in_transaction(&transaction, card, authority, now)?;
        transaction.commit()?;
        Ok(saved)
    }

    /// Keyed card creation commits the card, audit event, outbound event, and
    /// receipt atomically. A duplicate delivery returns the original card.
    pub fn create_card_with_events_as_keyed(
        &mut self,
        card: Card,
        idempotency_key: &str,
        authority: &Authority,
        now: i64,
    ) -> Result<IdempotencyOutcome<Card>> {
        let resource = format!("card:{}", card.id.as_str());
        let payload = serde_json::json!({"card": card, "reason": "create"});
        let request = IdempotencyRequest::from_payload(
            Operation::CreateCard,
            resource,
            authority,
            idempotency_key,
            &payload,
            now,
            24 * 60 * 60,
        )?;
        self.with_idempotency(&request, |transaction| {
            create_card_in_transaction(transaction, card, authority, now)
        })
    }

    pub fn patch_card(
        &mut self,
        card_id: &CardId,
        patch: CardPatch,
        actor: &str,
        now: i64,
    ) -> Result<Card> {
        self.patch_card_as(
            card_id,
            patch,
            &Authority::actor(actor.to_owned(), false),
            now,
        )
    }

    pub fn patch_card_as(
        &mut self,
        card_id: &CardId,
        patch: CardPatch,
        authority: &Authority,
        now: i64,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let saved = patch_card_in_transaction(&transaction, card_id, patch, authority, now)?;
        transaction.commit()?;
        Ok(saved)
    }

    pub fn patch_card_as_keyed(
        &mut self,
        card_id: &CardId,
        patch: CardPatch,
        idempotency_key: &str,
        authority: &Authority,
        now: i64,
    ) -> Result<IdempotencyOutcome<Card>> {
        let payload = json!({"patch": format!("{patch:?}")});
        self.with_keyed_operation(
            Operation::PatchCard,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| patch_card_in_transaction(transaction, card_id, patch, authority, now),
        )
    }

    pub fn check_criterion(
        &mut self,
        card_id: &CardId,
        criterion: usize,
        actor: &str,
        checked: bool,
        now: i64,
    ) -> Result<Card> {
        self.check_criterion_as(
            card_id,
            criterion,
            actor,
            checked,
            now,
            &Authority::unchecked(),
        )
    }

    pub fn check_criterion_as(
        &mut self,
        card_id: &CardId,
        criterion: usize,
        actor: &str,
        checked: bool,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = check_criterion_in_transaction(
            &transaction,
            card_id,
            criterion,
            actor,
            checked,
            now,
            authority,
        )?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn check_criterion_as_keyed(
        &mut self,
        card_id: &CardId,
        criterion: usize,
        actor: &str,
        checked: bool,
        context: KeyedOperationContext<'_>,
    ) -> Result<IdempotencyOutcome<Card>> {
        let KeyedOperationContext {
            now,
            idempotency_key,
            authority,
        } = context;
        let payload = json!({"criterion": criterion, "actor": actor, "checked": checked});
        self.with_keyed_operation(
            Operation::CheckCriterion,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                check_criterion_in_transaction(
                    transaction,
                    card_id,
                    criterion,
                    actor,
                    checked,
                    now,
                    authority,
                )
            },
        )
    }

    pub fn repair_criteria(
        &mut self,
        card_id: &CardId,
        acceptance: Vec<String>,
        actor: &str,
        now: i64,
    ) -> Result<CriteriaRepair> {
        self.repair_criteria_as(
            card_id,
            acceptance,
            &Authority::actor(actor.to_owned(), false),
            now,
        )
    }

    pub fn repair_criteria_as(
        &mut self,
        card_id: &CardId,
        acceptance: Vec<String>,
        authority: &Authority,
        now: i64,
    ) -> Result<CriteriaRepair> {
        let actor = non_empty("actor", &authority.actor_label())?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = load_card(&transaction, card_id)?;
        let previous: Vec<String> = card.acceptance.clone();
        let previous_criteria = card.criteria.clone();
        let repaired = card.repair_acceptance(acceptance).with_updated_at(now);

        let changes: Vec<CriteriaChange> = repaired
            .acceptance
            .iter()
            .enumerate()
            .filter_map(|(index, current)| {
                previous
                    .get(index)
                    .filter(|prev| *prev != current)
                    .map(|prev| {
                        let state_preserved = previous_criteria
                            .get(index)
                            .zip(repaired.criteria.get(index))
                            .map(|(before, after)| {
                                before.checked_at == after.checked_at
                                    && before.checked_by == after.checked_by
                                    && before.proof_links == after.proof_links
                            })
                            .unwrap_or(false);
                        CriteriaChange {
                            index,
                            previous: prev.clone(),
                            current: current.clone(),
                            state_preserved,
                        }
                    })
            })
            .collect();

        if !changes.is_empty() {
            persist_card(&transaction, &repaired)?;
            append_card_event_with_authority(
                &transaction,
                card_id,
                CardEventType::Patch,
                &actor,
                CardEventChange::Patch {
                    fields: vec!["acceptance".to_string()],
                },
                now,
                authority,
            )?;
        }

        transaction.commit()?;
        Ok(CriteriaRepair {
            card_id: card_id.to_string(),
            criteria_changed: changes.len(),
            changes,
        })
    }

    pub fn get_card(&self, card_id: &CardId) -> Result<Option<Card>> {
        let record = self
            .connection
            .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
            .optional()?;
        record.map(card_from_record).transpose()
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

    /// `cards` is ordered topologically over `blocks`/`blocked_by` edges
    /// confined to the eligible set (see
    /// [`powder_core::order_ready_cards`]'s doc comment for the full
    /// eligibility-vs-ordering-vs-explanation design); an eligible set with
    /// no such edges among its members orders exactly as it always has --
    /// priority, then age, then id. `cycle_card_ids` names exactly the
    /// eligible cards **on** a `blocks`/`blocked_by` cycle; those cards
    /// still appear in `cards` (grouped, in the stable order, at the
    /// cycle's own topological position) and every other card -- including
    /// cards downstream of a cycle -- keeps a genuine topological position,
    /// so nothing is dropped and no orderable edge is ignored.
    pub fn list_ready_page(&self, query: ReadyQuery) -> Result<CardListPage> {
        self.list_ready_page_after(query, None)
    }

    /// Continuation-aware Ready listing. A first page materializes a durable
    /// SQLite snapshot when more cards remain; identical first-page polls reuse
    /// an unexpired snapshot by ordered-card digest. Follow-up pages use the
    /// opaque v3 cursor position, skip departed cards, and append new arrivals
    /// after captured positions. Every continuation is v3 and query-bound.
    pub fn list_ready_page_after(
        &self,
        query: ReadyQuery,
        after: Option<&ReadyCursor>,
    ) -> Result<CardListPage> {
        if let Some(cursor) = after {
            if !cursor.matches_query(&query) {
                return Err(DomainError::validation(
                    "after",
                    "stale continuation cursor: query filters do not match",
                )
                .into());
            }
        }
        let all_cards = load_all_cards(&self.connection)?;
        let statuses: HashMap<_, _> = all_cards.iter().map(|c| (c.id.clone(), c.status)).collect();
        let mut eligible = Vec::new();
        for card in all_cards {
            if !card.is_ready_at(query.now, |id| {
                statuses.get(id).is_some_and(|status| status.is_terminal())
            }) {
                continue;
            }
            if query.repo.as_ref().is_some_and(|repositories| {
                !repositories
                    .iter()
                    .any(|repository| card.repo.as_deref() == Some(repository))
            }) {
                continue;
            }
            if query
                .priority
                .is_some_and(|priority| card.priority != priority)
            {
                continue;
            }
            eligible.push(card);
        }
        let total_count = eligible.len();
        let order = powder_core::order_ready_cards(eligible);
        let cycle_card_ids = order.cycle_card_ids;
        let ordered_cards = order.cards;

        if let Some(cursor) = after.filter(|cursor| cursor.is_durable()) {
            return self.list_ready_snapshot_page(
                &query,
                cursor,
                ordered_cards,
                total_count,
                cycle_card_ids,
            );
        }

        if after.is_none() && ordered_cards.len() > query.limit {
            let ordered_digest = ready_order_digest(&ordered_cards);
            let transaction =
                Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)?;
            transaction.execute(
                "DELETE FROM ready_snapshots WHERE expires_at <= ?1",
                [query.now],
            )?;
            let existing = transaction
                .query_row(
                    "SELECT id FROM ready_snapshots
                     WHERE query_fingerprint = ?1 AND ordered_digest = ?2 AND expires_at > ?3
                     ORDER BY created_at DESC, id DESC LIMIT 1",
                    rusqlite::params![query.fingerprint(), ordered_digest, query.now],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let snapshot_id = if let Some(snapshot_id) = existing {
                transaction.commit()?;
                snapshot_id
            } else {
                let snapshot_id =
                    format!("ready-snapshot-{}", nanoid::nanoid!(20, &API_KEY_ALPHABET));
                transaction.execute(
                    "INSERT INTO ready_snapshots(id, query_fingerprint, ordered_digest, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        snapshot_id,
                        query.fingerprint(),
                        ordered_digest,
                        query.now,
                        query.now.saturating_add(READY_SNAPSHOT_TTL_SECONDS),
                    ],
                )?;
                for (position, card) in ordered_cards.iter().enumerate() {
                    transaction.execute(
                        "INSERT INTO ready_snapshot_items(snapshot_id, position, card_id) VALUES (?1, ?2, ?3)",
                        rusqlite::params![
                            snapshot_id,
                            i64::try_from(position).unwrap_or(i64::MAX),
                            card.id.as_str()
                        ],
                    )?;
                }
                transaction.commit()?;
                snapshot_id
            };
            let end = query.limit.max(1).min(ordered_cards.len());
            let cards = ordered_cards.into_iter().take(end).collect::<Vec<_>>();
            let next_after = cards.last().map(|card| card.id.clone());
            let ready_cursor = ReadyCursor::for_snapshot(&query, snapshot_id, end).encode();
            return Ok(CardListPage {
                cards,
                total_count,
                excluded_terminal_count: 0,
                cycle_card_ids,
                next_after,
                ready_cursor: Some(ready_cursor),
            });
        }

        let cards = ordered_cards
            .into_iter()
            .take(query.limit.max(1))
            .collect::<Vec<_>>();
        Ok(CardListPage {
            cards,
            total_count,
            excluded_terminal_count: 0,
            cycle_card_ids,
            next_after: None,
            ready_cursor: None,
        })
    }

    fn list_ready_snapshot_page(
        &self,
        query: &ReadyQuery,
        cursor: &ReadyCursor,
        ordered_cards: Vec<Card>,
        total_count: usize,
        cycle_card_ids: Vec<CardId>,
    ) -> Result<CardListPage> {
        let snapshot_id = cursor
            .snapshot_id()
            .ok_or_else(|| DomainError::validation("after", "invalid continuation cursor"))?;
        let current_by_id = ordered_cards
            .iter()
            .cloned()
            .map(|card| (card.id.as_str().to_owned(), card))
            .collect::<HashMap<_, _>>();
        let current_order_ids = ordered_cards
            .iter()
            .map(|card| card.id.clone())
            .collect::<Vec<_>>();
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM ready_snapshots WHERE expires_at <= ?1",
            [query.now],
        )?;
        let metadata = transaction
            .query_row(
                "SELECT query_fingerprint, expires_at FROM ready_snapshots WHERE id = ?1",
                [snapshot_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((fingerprint, _expires_at)) = metadata else {
            transaction.commit()?;
            return Err(
                DomainError::validation("after", "unknown or expired continuation cursor").into(),
            );
        };
        if fingerprint != query.fingerprint() {
            transaction.commit()?;
            return Err(DomainError::validation(
                "after",
                "stale continuation cursor: query filters do not match",
            )
            .into());
        }
        let snapshot_ids = {
            let mut statement = transaction
                .prepare("SELECT card_id FROM ready_snapshot_items WHERE snapshot_id = ?1")?;
            let rows = statement
                .query_map([snapshot_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            rows
        };
        let mut next_position = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM ready_snapshot_items WHERE snapshot_id = ?1",
            [snapshot_id],
            |row| row.get::<_, i64>(0),
        )?;
        // Newly eligible arrivals are appended in the current topological order;
        // captured positions never move, even when graph changes or cards depart.
        for card_id in &current_order_ids {
            if snapshot_ids.contains(card_id.as_str()) {
                continue;
            }
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO ready_snapshot_items(snapshot_id, position, card_id) VALUES (?1, ?2, ?3)",
                rusqlite::params![snapshot_id, next_position, card_id.as_str()],
            )?;
            if inserted > 0 {
                next_position += 1;
            }
        }
        let snapshot_len = usize::try_from(next_position).map_err(|_| {
            DomainError::validation("after", "invalid continuation cursor position")
        })?;
        if cursor.position() >= snapshot_len {
            transaction.commit()?;
            return Err(
                DomainError::validation("after", "invalid continuation cursor position").into(),
            );
        }
        let start_position = i64::try_from(cursor.position()).map_err(|_| {
            DomainError::validation("after", "invalid continuation cursor position")
        })?;
        let limit = query.limit.max(1);
        let mut page = Vec::with_capacity(limit);
        let mut extra_position = None;
        {
            let mut statement = transaction.prepare(
                "SELECT position, card_id FROM ready_snapshot_items
                 WHERE snapshot_id = ?1 AND position >= ?2 ORDER BY position",
            )?;
            let mut rows = statement.query(rusqlite::params![snapshot_id, start_position])?;
            while let Some(row) = rows.next()? {
                let position: i64 = row.get(0)?;
                let card_id: String = row.get(1)?;
                if let Some(card) = current_by_id.get(&card_id) {
                    if page.len() < limit {
                        page.push(card.clone());
                    } else {
                        extra_position = Some(position);
                        break;
                    }
                }
            }
        }
        let (next_after, ready_cursor) = if let Some(position) = extra_position {
            let last = page.last().map(|card| card.id.clone());
            let cursor = ReadyCursor::for_snapshot(
                query,
                snapshot_id.to_owned(),
                usize::try_from(position).map_err(|_| {
                    DomainError::validation("after", "invalid continuation cursor position")
                })?,
            )
            .encode();
            (last, Some(cursor))
        } else {
            (None, None)
        };
        transaction.commit()?;
        Ok(CardListPage {
            cards: page,
            total_count,
            excluded_terminal_count: 0,
            cycle_card_ids,
            next_after,
            ready_cursor,
        })
    }

    /// List cards by optional `status`/`repo` filter, not just ready-eligible
    /// ones -- `list_ready` answers "what can an agent claim now"; this
    /// answers "what exists," including `blocked` and `done`
    /// cards no other surface can enumerate without opening the database
    /// file directly. Same sort as `list_ready` (priority, age, id).
    pub fn list_cards(&self, filter: &CardFilter, limit: usize) -> Result<Vec<Card>> {
        Ok(self.list_cards_page(filter, limit)?.cards)
    }

    pub fn list_cards_page(&self, filter: &CardFilter, limit: usize) -> Result<CardListPage> {
        self.list_cards_page_after(filter, limit, None)
    }

    /// Continuation-aware variant of [`Store::list_cards_page`] --
    /// unchanged when `after` is `None` (delegated to by `list_cards_page`
    /// itself), used directly by the HTTP `/api/v1/cards` route to resume
    /// past a prior page (powder-cards-api-paged-continuation). See
    /// [`Store::list_ready_page_after`]'s doc comment for what `after` does
    /// and does not buy: it lets a caller reach cards beyond `limit` from
    /// this same already-computed, already-sorted list; it does not push
    /// filtering or sorting into SQL, so it does not bound per-request
    /// DB/CPU cost (`powder-store-sql-pushed-list-filtering` is the
    /// separate follow-up for that).
    pub fn list_cards_page_after(
        &self,
        filter: &CardFilter,
        limit: usize,
        after: Option<&CardId>,
    ) -> Result<CardListPage> {
        let repo_filter = filter.repo.as_deref();
        let mut statement = self.connection.prepare(CARD_SELECT_ALL_SQL)?;
        let records = statement
            .query_map([], CardRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut cards = records
            .into_iter()
            .map(card_from_record)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|card| filter.status.map(|s| card.status == s).unwrap_or(true))
            .filter(|card| match repo_filter {
                Some(repo) => card.repo.as_deref() == Some(repo),
                None => true,
            })
            .filter(|card| {
                filter.label.as_ref().is_none_or(|wanted| {
                    let wanted = wanted.trim().to_ascii_lowercase();
                    card.labels
                        .iter()
                        .any(|label| label.trim().eq_ignore_ascii_case(&wanted))
                })
            })
            .collect::<Vec<_>>();
        // `total_count` reports how many cards match the caller's *explicit*
        // status/repo/label filters -- deliberately computed
        // before the `include_terminal` exclusion below, so a caller that
        // asks for the whole board (no explicit status) and gets terminal
        // cards silently held back still sees the true match count rather
        // than an undercount that reads as "the board is smaller than it
        // is." An explicit `status` filter is authoritative and is never
        // second-guessed by `include_terminal`. The number held back is
        // reported separately as `excluded_terminal_count` so envelope
        // builders can say exactly which remedy (raise `limit` vs. pass
        // `include_terminal: true`) recovers which cards.
        let total_count = cards.len();
        if filter.status.is_none() && !filter.include_terminal {
            cards.retain(|card| !card.status.is_terminal());
        }
        let excluded_terminal_count = total_count - cards.len();

        cards.sort_by(powder_core::ready_sort_cmp);
        let (cards, next_after) = paginate_ordered_cards(cards, limit.max(1), after)?;
        Ok(CardListPage {
            cards,
            total_count,
            excluded_terminal_count,
            cycle_card_ids: Vec::new(),
            next_after,
            ready_cursor: None,
        })
    }

    /// Raw count of every card in the store, ignoring every filter dimension.
    /// Callers use it when they need the board size independent of a query.
    pub fn card_count(&self) -> Result<usize> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM cards", [], |row| row.get::<_, i64>(0))?
            as usize)
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
        let principal = authority.actor_label();
        authority.authorize_operation_with_worker(
            Operation::ClaimCard,
            None,
            None,
            Some(agent.as_str()),
            now,
        )?;
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

        if let Some(claim) = card.claim.as_ref().filter(|claim| {
            claim.principal == principal && claim.agent == agent && !claim.is_expired(now)
        }) {
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
            events::append_outbound_card_event_with_authority(
                &transaction,
                &card,
                CardEventType::ClaimExpired,
                authority,
                CardEventChange::Claim {
                    action: powder_core::ClaimEventAction::Expired,
                    principal: Some(expired.principal.clone()),
                    run_id: Some(expired.run_id.clone()),
                    agent: Some(expired.agent.clone()),
                    expires_at: Some(expired.expires_at),
                },
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
        let claim = card.apply_claim(
            principal.clone(),
            agent.clone(),
            run_id.clone(),
            now,
            ttl_seconds,
            |id| terminal_blockers.contains(id),
        )?;
        persist_card(&transaction, &card)?;

        let run = Run {
            id: run_id.clone(),
            card_id: card_id.clone(),
            state: RunState::Active,
            principal: principal.clone(),
            role: authority.role_label().to_string(),
            agent: agent.clone(),
            claim_expires_at: claim.expires_at,
            proof: None,
            created_at: now,
            updated_at: now,
        };
        persist_run(&transaction, &run)?;
        append_activity_attributed(
            &transaction,
            &run_id,
            ActivityType::Action,
            &format!("claimed {card_id}"),
            authority.principal_name(),
            Some(authority.role_label()),
            now,
        )?;
        transaction.commit()?;

        Ok(ClaimReceipt {
            card_id: card_id.clone(),
            run_id,
            principal,
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
        let card = update_status_in_transaction(&transaction, card_id, status, now, authority)?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn update_status_keyed(
        &mut self,
        card_id: &CardId,
        status: CardStatus,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<Card>> {
        let payload = json!({"status": status.as_str()});
        self.with_keyed_operation(
            Operation::UpdateStatus,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                update_status_in_transaction(transaction, card_id, status, now, authority)
            },
        )
    }

    /// Replace a card's `related`/`blocks`/`blocked_by` lists and mirror
    /// exactly the delta onto every touched peer, atomically, in the same
    /// transaction as the primary write
    /// (powder-dogfood-2026-07-14-nonreciprocal-relations): an id newly
    /// added to `blocked_by` gets this card added to its own `blocks`; an
    /// id removed gets this card removed from its `blocks`; `related` is
    /// symmetric both ways. Only the changed ids are touched on a peer --
    /// its other, unrelated relations are left alone. A dangling id (no
    /// card with that id exists) is tolerated, same as before this change;
    /// mirroring is simply skipped for it. See the `relations` module doc
    /// comment for the full design rationale.
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
        let card = update_relations_in_transaction(
            &transaction,
            card_id,
            related,
            blocks,
            blocked_by,
            now,
            authority,
        )?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn update_relations_keyed(
        &mut self,
        card_id: &CardId,
        related: Vec<CardId>,
        blocks: Vec<CardId>,
        blocked_by: Vec<CardId>,
        context: KeyedOperationContext<'_>,
    ) -> Result<IdempotencyOutcome<Card>> {
        let KeyedOperationContext {
            now,
            idempotency_key,
            authority,
        } = context;
        let payload = json!({
            "related": related,
            "blocks": blocks,
            "blocked_by": blocked_by,
        });
        self.with_keyed_operation(
            Operation::UpdateRelations,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                update_relations_in_transaction(
                    transaction,
                    card_id,
                    related,
                    blocks,
                    blocked_by,
                    now,
                    authority,
                )
            },
        )
    }

    /// Set or clear a card's explicit parent edge. Validates that the parent
    /// exists and that the link cannot create a cycle. Audits the child and
    /// affected parent cards without changing lifecycle status.
    pub fn set_parent(
        &mut self,
        card_id: &CardId,
        parent: Option<CardId>,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = set_parent_in_transaction(&transaction, card_id, parent, now, authority)?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn set_parent_keyed(
        &mut self,
        card_id: &CardId,
        parent: Option<CardId>,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<Card>> {
        let payload = json!({"parent": parent});
        self.with_keyed_operation(
            Operation::SetParent,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| set_parent_in_transaction(transaction, card_id, parent, now, authority),
        )
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
        let receipt = release_claim_in_transaction(&transaction, card_id, run_id, now, authority)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn release_claim_keyed(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<ClaimReceipt>> {
        let payload = json!({"run_id": run_id, "action": "release"});
        self.with_keyed_operation(
            Operation::ReleaseClaim,
            format!("claim:{}:{}", card_id.as_str(), run_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                release_claim_in_transaction(transaction, card_id, run_id, now, authority)
            },
        )
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
        let receipt =
            renew_claim_in_transaction(&transaction, card_id, run_id, now, ttl_seconds, authority)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn renew_claim_keyed(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        ttl_seconds: u64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<ClaimReceipt>> {
        let payload = json!({"run_id": run_id, "ttl_seconds": ttl_seconds, "action": "renew"});
        self.with_keyed_operation(
            Operation::RenewClaim,
            format!("claim:{}:{}", card_id.as_str(), run_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                renew_claim_in_transaction(
                    transaction,
                    card_id,
                    run_id,
                    now,
                    ttl_seconds,
                    authority,
                )
            },
        )
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
        let receipt =
            heartbeat_claim_in_transaction(&transaction, card_id, run_id, now, authority)?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn heartbeat_claim_keyed(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<ClaimReceipt>> {
        let payload = json!({"run_id": run_id, "action": "heartbeat"});
        self.with_keyed_operation(
            Operation::HeartbeatClaim,
            format!("claim:{}:{}", card_id.as_str(), run_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                heartbeat_claim_in_transaction(transaction, card_id, run_id, now, authority)
            },
        )
    }

    /// Hand an active claim to a different agent atomically (powder-936).
    /// The keyed variant records the request receipt with the mutation, so a
    /// retried delivery returns the original transfer without adding a second
    /// audit event or changing the lease again.
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
        let receipt = transfer_claim_in_transaction(
            &transaction,
            card_id,
            run_id,
            to_agent,
            now,
            ttl_seconds,
            authority,
        )?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn transfer_claim_keyed(
        &mut self,
        card_id: &CardId,
        run_id: &RunId,
        to_agent: &str,
        ttl_seconds: u64,
        context: KeyedOperationContext<'_>,
    ) -> Result<IdempotencyOutcome<ClaimReceipt>> {
        let KeyedOperationContext {
            now,
            idempotency_key,
            authority,
        } = context;
        let payload = json!({"run_id": run_id, "to_agent": to_agent, "ttl_seconds": ttl_seconds, "action": "transfer"});
        self.with_keyed_operation(
            Operation::TransferClaim,
            format!("claim:{}:{}", card_id.as_str(), run_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                transfer_claim_in_transaction(
                    transaction,
                    card_id,
                    run_id,
                    to_agent,
                    now,
                    ttl_seconds,
                    authority,
                )
            },
        )
    }

    pub fn add_link(&mut self, card_id: &CardId, label: &str, url: &str, now: i64) -> Result<Link> {
        self.add_link_as(card_id, label, url, now, &Authority::unchecked())
    }

    pub fn add_link_as(
        &mut self,
        card_id: &CardId,
        label: &str,
        url: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<Link> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let link = add_link_in_transaction(&transaction, card_id, label, url, now, authority)?;
        transaction.commit()?;
        Ok(link)
    }

    pub fn add_link_as_keyed(
        &mut self,
        card_id: &CardId,
        label: &str,
        url: &str,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<Link>> {
        let payload = json!({"label": label, "url": url});
        self.with_keyed_operation(
            Operation::AddLink,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| add_link_in_transaction(transaction, card_id, label, url, now, authority),
        )
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
        self.add_comment_as(card_id, author, body, now, &Authority::unchecked())
    }

    pub fn add_comment_as(
        &mut self,
        card_id: &CardId,
        author: &str,
        body: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<Comment> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let comment =
            add_comment_in_transaction(&transaction, card_id, author, body, now, authority)?;
        transaction.commit()?;
        Ok(comment)
    }

    /// Keyed comment writes use the durable operation receipt ledger. The
    /// receipt is committed with the comment and outbound event, so retries
    /// cannot append a second comment or event.
    pub fn add_comment_as_keyed(
        &mut self,
        card_id: &CardId,
        author: &str,
        body: &str,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<Comment>> {
        let payload = json!({"author": author, "body": body});
        let request = IdempotencyRequest::from_payload(
            Operation::AddComment,
            format!("card:{}", card_id.as_str()),
            authority,
            idempotency_key,
            &payload,
            now,
            24 * 60 * 60,
        )?;
        self.with_idempotency(&request, |transaction| {
            add_comment_in_transaction(transaction, card_id, author, body, now, authority)
        })
    }

    /// Append a typed work-log body with agent and optional run attribution.
    pub fn append_work_log(
        &mut self,
        card_id: &CardId,
        agent: &str,
        run_id: Option<&str>,
        body: &str,
        now: i64,
    ) -> Result<WorkLogEntry> {
        self.append_work_log_as(card_id, agent, run_id, body, now, &Authority::unchecked())
    }

    pub fn append_work_log_as(
        &mut self,
        card_id: &CardId,
        agent: &str,
        run_id: Option<&str>,
        body: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<WorkLogEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry = append_work_log_in_transaction(
            &transaction,
            card_id,
            agent,
            run_id,
            body,
            now,
            authority,
        )?;
        transaction.commit()?;
        Ok(entry)
    }

    pub fn append_work_log_as_keyed(
        &mut self,
        card_id: &CardId,
        agent: &str,
        run_id: Option<&str>,
        body: &str,
        context: KeyedOperationContext<'_>,
    ) -> Result<IdempotencyOutcome<WorkLogEntry>> {
        let KeyedOperationContext {
            now,
            idempotency_key,
            authority,
        } = context;
        let payload = json!({"agent": agent, "run_id": run_id, "body": body});
        let request = IdempotencyRequest::from_payload(
            Operation::WorkLog,
            format!("card:{}", card_id.as_str()),
            authority,
            idempotency_key,
            &payload,
            now,
            24 * 60 * 60,
        )?;
        self.with_idempotency(&request, |transaction| {
            append_work_log_in_transaction(
                transaction,
                card_id,
                agent,
                run_id,
                body,
                now,
                authority,
            )
        })
    }

    pub fn request_input(
        &mut self,
        run_id: &RunId,
        question: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<Run> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = request_input_in_transaction(&transaction, run_id, question, now, authority)?;
        transaction.commit()?;
        Ok(run)
    }

    pub fn request_input_keyed(
        &mut self,
        run_id: &RunId,
        question: &str,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<Run>> {
        let payload = json!({"question": question});
        self.with_keyed_operation(
            Operation::RequestInput,
            format!("run:{}", run_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                request_input_in_transaction(transaction, run_id, question, now, authority)
            },
        )
    }

    pub fn complete_card(
        &mut self,
        card_id: &CardId,
        proof: Option<&str>,
        criterion_proofs: Vec<CriterionProofInput>,
        now: i64,
        authority: &Authority,
    ) -> Result<Card> {
        let proof = proof
            .map(|value| non_empty_scrubbed("proof", value))
            .transpose()?;
        let criterion_proofs = clean_criterion_proofs(criterion_proofs)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = complete_card_in_transaction(
            &transaction,
            card_id,
            proof,
            criterion_proofs,
            now,
            authority,
        )?;
        transaction.commit()?;
        Ok(card)
    }

    pub fn complete_card_keyed(
        &mut self,
        card_id: &CardId,
        proof: Option<&str>,
        criterion_proofs: Vec<CriterionProofInput>,
        now: i64,
        idempotency_key: &str,
        authority: &Authority,
    ) -> Result<IdempotencyOutcome<Card>> {
        let proof = proof
            .map(|value| non_empty_scrubbed("proof", value))
            .transpose()?;
        let criterion_proofs = clean_criterion_proofs(criterion_proofs)?;
        let payload = json!({
            "proof": proof,
            "criterion_proofs": criterion_proofs
                .iter()
                .map(|item| json!({"criterion": item.criterion, "url": item.url}))
                .collect::<Vec<_>>(),
        });
        self.with_keyed_operation(
            Operation::CompleteCard,
            format!("card:{}", card_id.as_str()),
            &payload,
            KeyedOperationContext::new(now, idempotency_key, authority),
            |transaction| {
                complete_card_in_transaction(
                    transaction,
                    card_id,
                    proof,
                    criterion_proofs,
                    now,
                    authority,
                )
            },
        )
    }
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
        label: non_empty_scrubbed("label", label)?,
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

fn add_link_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    label: &str,
    url: &str,
    now: i64,
    authority: &Authority,
) -> Result<Link> {
    let card = load_card(transaction, card_id)?;
    authorize_card_operation(
        authority,
        Operation::AddLink,
        &card,
        None,
        card.claim.as_ref().map(|claim| claim.agent.as_str()),
        now,
    )?;
    let link = insert_link(transaction, card_id, label, url, now)?;
    let actor = authority.actor_label();
    append_attributed_card_event(
        transaction,
        card_id,
        MutationAudit {
            operation: Operation::AddLink,
            resource: card_id.as_str(),
            semantic_identity: card.claim.as_ref().map(|claim| claim.agent.as_str()),
            run_id: card.claim.as_ref().map(|claim| &claim.run_id),
            reason: None,
            event_type: CardEventType::Link,
            actor: &actor,
            change: CardEventChange::Link {
                id: Some(link.id.clone()),
                label: Some(link.label.clone()),
                url: Some(link.url.clone()),
            },
            subject_kind: "link",
            subject_id: link.id.as_str(),
            authority,
        },
        now,
    )?;
    Ok(link)
}

fn request_input_in_transaction(
    transaction: &Transaction<'_>,
    run_id: &RunId,
    question: &str,
    now: i64,
    authority: &Authority,
) -> Result<Run> {
    let question = non_empty_scrubbed("question", question)?;
    let mut run = answer_loop::load_run(transaction, run_id)?;
    let mut card = load_card(transaction, &run.card_id)?;
    if card.claim.as_ref().map(|claim| &claim.run_id) != Some(run_id) {
        return Err(DomainError::conflict(format!(
            "run {run_id} is not the current claim for card {}",
            card.id
        ))
        .into());
    }
    authorize_card_operation(
        authority,
        Operation::RequestInput,
        &card,
        Some(run_id),
        None,
        now,
    )?;
    card.status = CardStatus::AwaitingInput;
    card.updated_at = now;
    run.state = RunState::AwaitingInput;
    run.updated_at = now;
    persist_card(transaction, &card)?;
    persist_run(transaction, &run)?;
    append_activity_attributed(
        transaction,
        run_id,
        ActivityType::Elicitation,
        &question,
        authority.principal_name(),
        Some(authority.role_label()),
        now,
    )?;
    let actor = authority.actor_label();
    append_attributed_card_event(
        transaction,
        &card.id,
        MutationAudit {
            operation: Operation::RequestInput,
            resource: card.id.as_str(),
            semantic_identity: card.claim.as_ref().map(|claim| claim.agent.as_str()),
            run_id: Some(run_id),
            reason: None,
            event_type: CardEventType::RequestInput,
            actor: &actor,
            change: CardEventChange::Input {
                action: powder_core::InputEventAction::Requested,
                run_id: Some(run_id.clone()),
                text: Some(question.clone()),
            },
            subject_kind: "run",
            subject_id: run_id.as_str(),
            authority,
        },
        now,
    )?;
    events::append_outbound_card_event_with_authority(
        transaction,
        &card,
        CardEventType::AwaitingInput,
        authority,
        CardEventChange::Input {
            action: powder_core::InputEventAction::Requested,
            run_id: Some(run_id.clone()),
            text: Some(question),
        },
        now,
    )?;
    Ok(run)
}

fn persist_card(connection: &Connection, card: &Card) -> Result<()> {
    let repo = card.repo.as_deref();
    let claim_principal = card.claim.as_ref().map(|claim| claim.principal.as_str());
    let claim_agent = card.claim.as_ref().map(|claim| claim.agent.as_str());
    let claim_run_id = card.claim.as_ref().map(|claim| claim.run_id.as_str());
    let claim_acquired_at = card.claim.as_ref().map(|claim| claim.acquired_at);
    let claim_expires_at = card.claim.as_ref().map(|claim| claim.expires_at);

    connection.execute(
        &format!(
            "INSERT INTO cards ({CARD_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               body = excluded.body,
               acceptance_json = excluded.acceptance_json,
               criteria_json = excluded.criteria_json,
               proof_plan_json = excluded.proof_plan_json,
               status = excluded.status,
               priority = excluded.priority,
               labels_json = excluded.labels_json,
               related_json = excluded.related_json,
               blocks_json = excluded.blocks_json,
               blocked_by_json = excluded.blocked_by_json,
               repo = excluded.repo,
               claim_principal = excluded.claim_principal,
               claim_agent = excluded.claim_agent,
               claim_run_id = excluded.claim_run_id,
               claim_acquired_at = excluded.claim_acquired_at,
               claim_expires_at = excluded.claim_expires_at,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at,
               parent = excluded.parent"
        ),
        params![
            card.id.as_str(),
            card.title,
            card.body,
            to_json(&card.acceptance)?,
            to_json(&card.criteria)?,
            to_json(&card.proof_plan)?,
            card.status.as_str(),
            card.priority.as_str(),
            to_json(&card.labels)?,
            to_json(&card.related)?,
            to_json(&card.blocks)?,
            to_json(&card.blocked_by)?,
            repo,
            claim_principal,
            claim_agent,
            claim_run_id,
            claim_acquired_at,
            claim_expires_at,
            card.created_at,
            card.updated_at,
            card.parent.as_ref().map(CardId::as_str),
        ],
    )?;
    Ok(())
}

fn persist_run(connection: &Connection, run: &Run) -> Result<()> {
    connection.execute(
        "INSERT INTO runs (
            id, card_id, state, principal, role, agent, claim_expires_at, proof,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET
           card_id = excluded.card_id,
           state = excluded.state,
           principal = excluded.principal,
           role = excluded.role,
           agent = excluded.agent,
           claim_expires_at = excluded.claim_expires_at,
           proof = excluded.proof,
           created_at = excluded.created_at,
           updated_at = excluded.updated_at",
        params![
            run.id.as_str(),
            run.card_id.as_str(),
            run.state.as_str(),
            run.principal,
            run.role,
            run.agent,
            run.claim_expires_at,
            run.proof,
            run.created_at,
            run.updated_at
        ],
    )?;
    Ok(())
}
struct RunRecord {
    id: String,
    card_id: String,
    state: String,
    principal: String,
    role: String,
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
            principal: row.get(3)?,
            role: row.get(4)?,
            agent: row.get(5)?,
            claim_expires_at: row.get(6)?,
            proof: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
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
            principal: self.principal,
            role: self.role,
            agent: self.agent,
            claim_expires_at: self.claim_expires_at,
            proof: self.proof,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn append_activity_attributed(
    connection: &Connection,
    run_id: &RunId,
    activity_type: ActivityType,
    payload: &str,
    principal: Option<&str>,
    role: Option<&str>,
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
        principal: principal.map(str::to_string),
        role: role.map(str::to_string),
        created_at: now,
    };
    connection.execute(
        "INSERT INTO activities (id, run_id, activity_type, payload, principal, role, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            activity.id.as_str(),
            activity.run_id.as_str(),
            activity.activity_type.as_str(),
            activity.payload,
            activity.principal,
            activity.role,
            activity.created_at
        ],
    )?;
    Ok(activity)
}

fn append_card_event(
    connection: &Connection,
    card_id: &CardId,
    event_type: CardEventType,
    actor: &str,
    change: CardEventChange,
    now: i64,
) -> Result<CardEvent> {
    append_card_event_with_authority(
        connection,
        card_id,
        event_type,
        actor,
        change,
        now,
        &Authority::unchecked(),
    )
}

fn complete_card_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    proof: Option<String>,
    criterion_proofs: Vec<CriterionProofInput>,
    now: i64,
    authority: &Authority,
) -> Result<Card> {
    let mut card = load_card(transaction, card_id)?;
    authorize_card_operation(authority, Operation::CompleteCard, &card, None, None, now)?;
    let previous = card.status;
    let criteria: Vec<usize> = criterion_proofs.iter().map(|item| item.criterion).collect();
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
        append_activity_attributed(
            transaction,
            &run_id,
            ActivityType::Response,
            proof.as_deref().unwrap_or("completed without proof"),
            authority.principal_name(),
            Some(authority.role_label()),
            now,
        )?;
    }
    append_card_event_with_authority(
        transaction,
        card_id,
        CardEventType::Status,
        &authority.actor_label(),
        CardEventChange::Completion {
            previous,
            current: CardStatus::Done,
            proof: proof.clone(),
            criteria: criteria.clone(),
        },
        now,
        authority,
    )?;
    if !previous.is_terminal() {
        events::append_outbound_card_event_with_authority(
            transaction,
            &card,
            CardEventType::Completed,
            authority,
            CardEventChange::Completion {
                previous,
                current: card.status,
                proof,
                criteria,
            },
            now,
        )?;
    }
    Ok(card)
}

fn set_parent_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    parent: Option<CardId>,
    now: i64,
    authority: &Authority,
) -> Result<Card> {
    let mut card = load_card(transaction, card_id)?;
    authorize_card_operation(authority, Operation::SetParent, &card, None, None, now)?;
    let previous = card.parent.clone();
    if previous == parent {
        return Ok(card);
    }
    if let Some(new_parent) = parent.as_ref() {
        ensure_parent_linkable(transaction, card_id, new_parent)?;
    }
    card.parent = parent.clone();
    card.updated_at = now;
    persist_card(transaction, &card)?;
    let actor = authority.actor_label();
    append_card_event_with_authority(
        transaction,
        card_id,
        CardEventType::Hierarchy,
        &actor,
        CardEventChange::Parent {
            previous: previous.clone(),
            current: parent.clone(),
        },
        now,
        authority,
    )?;
    if let Some(old_parent) = previous.as_ref() {
        if load_card_optional(transaction, old_parent)?.is_some() {
            append_card_event_with_authority(
                transaction,
                old_parent,
                CardEventType::Hierarchy,
                &actor,
                CardEventChange::Parent {
                    previous: Some(card_id.clone()),
                    current: None,
                },
                now,
                authority,
            )?;
        }
    }
    if let Some(new_parent) = parent.as_ref() {
        append_card_event_with_authority(
            transaction,
            new_parent,
            CardEventType::Hierarchy,
            &actor,
            CardEventChange::Parent {
                previous: None,
                current: Some(card_id.clone()),
            },
            now,
            authority,
        )?;
    }
    Ok(card)
}

fn update_relations_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    related: Vec<CardId>,
    blocks: Vec<CardId>,
    blocked_by: Vec<CardId>,
    now: i64,
    authority: &Authority,
) -> Result<Card> {
    let mut card = load_card(transaction, card_id)?;
    authorize_card_operation(
        authority,
        Operation::UpdateRelations,
        &card,
        None,
        None,
        now,
    )?;
    let actor = authority.actor_label();
    let related_delta = list_delta(&card.related, &related);
    let blocks_delta = list_delta(&card.blocks, &blocks);
    let blocked_by_delta = list_delta(&card.blocked_by, &blocked_by);
    card.apply_relations(related, blocks, blocked_by, now);
    persist_card(transaction, &card)?;
    append_card_event_with_authority(
        transaction,
        card_id,
        CardEventType::Relations,
        &actor,
        CardEventChange::Relations {
            related: card.related.clone(),
            blocks: card.blocks.clone(),
            blocked_by: card.blocked_by.clone(),
        },
        now,
        authority,
    )?;
    mirror_delta_with_authority(
        transaction,
        card_id,
        RelationField::Related,
        &related_delta,
        authority,
        now,
    )?;
    mirror_delta_with_authority(
        transaction,
        card_id,
        RelationField::Blocks,
        &blocks_delta,
        authority,
        now,
    )?;
    mirror_delta_with_authority(
        transaction,
        card_id,
        RelationField::BlockedBy,
        &blocked_by_delta,
        authority,
        now,
    )?;
    Ok(card)
}

fn update_status_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    status: CardStatus,
    now: i64,
    authority: &Authority,
) -> Result<Card> {
    let mut card = load_card(transaction, card_id)?;
    authorize_card_operation(authority, Operation::UpdateStatus, &card, None, None, now)?;
    let previous = card.status;
    let released_claim = card.apply_status(status, now);
    persist_card(transaction, &card)?;
    if let Some(claim) = released_claim {
        close_run_for_status(transaction, &claim.run_id, status, now, None)?;
        append_activity_attributed(
            transaction,
            &claim.run_id,
            ActivityType::Action,
            &format!("status set {card_id} to {}", status.as_str()),
            authority.principal_name(),
            Some(authority.role_label()),
            now,
        )?;
    }
    append_card_event_with_authority(
        transaction,
        card_id,
        CardEventType::Status,
        &authority.actor_label(),
        CardEventChange::Status {
            previous,
            current: status,
        },
        now,
        authority,
    )?;
    if let Some(event_type) = events::outbound_event_for_status_change(previous, status) {
        events::append_outbound_card_event_with_authority(
            transaction,
            &card,
            event_type,
            authority,
            CardEventChange::Status {
                previous,
                current: status,
            },
            now,
        )?;
    }
    Ok(card)
}

fn check_criterion_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    criterion: usize,
    actor: &str,
    checked: bool,
    now: i64,
    authority: &Authority,
) -> Result<Card> {
    let actor = non_empty("actor", actor)?;
    authority.require_identity(&actor).map_err(|error| {
        DomainError::authority_denied(DenialClass::IdentityMismatch, error.to_string())
    })?;
    let mut card = load_card(transaction, card_id)?;
    authorize_card_operation(authority, Operation::CheckCriterion, &card, None, None, now)?;
    let criterion_state = criterion_mut(&mut card, criterion)?;
    if checked {
        criterion_state.checked_by = Some(actor.clone());
        criterion_state.checked_at = Some(now);
    } else {
        criterion_state.checked_by = None;
        criterion_state.checked_at = None;
    }
    card.updated_at = now;
    persist_card(transaction, &card)?;
    let subject_id = criterion.to_string();
    append_attributed_card_event(
        transaction,
        card_id,
        MutationAudit {
            operation: Operation::CheckCriterion,
            resource: card_id.as_str(),
            semantic_identity: Some(actor.as_str()),
            run_id: None,
            reason: Some("criterion correction"),
            event_type: CardEventType::Criterion,
            actor: &actor,
            change: CardEventChange::Criterion {
                index: criterion,
                checked,
            },
            subject_kind: "criterion",
            subject_id: &subject_id,
            authority,
        },
        now,
    )?;
    Ok(card)
}

fn patch_card_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    patch: CardPatch,
    authority: &Authority,
    now: i64,
) -> Result<Card> {
    let actor = non_empty("actor", &authority.actor_label())?;
    let mut card = load_card(transaction, card_id)?;
    authorize_card_operation(authority, Operation::PatchCard, &card, None, None, now)?;
    let mut patched_fields = Vec::new();
    if let Some(title) = patch.title {
        card.title = non_empty_scrubbed("title", &title)?;
        patched_fields.push("title".to_string());
    }
    if let Some(body) = patch.body {
        card.body = secrets::scrub_secrets(&body);
        patched_fields.push("body".to_string());
    }
    if let Some(acceptance) = patch.acceptance {
        card = card.with_acceptance(scrub_string_list(acceptance));
        patched_fields.push("acceptance".to_string());
    }
    if let Some(proof_plan) = patch.proof_plan {
        card = card.with_proof_plan(scrub_string_list(proof_plan));
        patched_fields.push("proof_plan".to_string());
    }
    if let Some(priority) = patch.priority {
        card.priority = priority;
        patched_fields.push("priority".to_string());
    }
    if let Some(labels) = patch.labels {
        card.labels = clean_string_list(labels);
        patched_fields.push("labels".to_string());
    }
    let mut status_change = None;
    if let Some(status) = patch.status {
        if status != card.status {
            status_change = Some((card.status, status));
        }
        card.status = status;
        patched_fields.push("status".to_string());
    }
    if let Some(repo) = patch.repo {
        card.repo = repo;
        patched_fields.push("repo".to_string());
    }
    if patched_fields.is_empty() {
        return Ok(card);
    }
    card.updated_at = now;
    persist_card(transaction, &card)?;
    append_card_event_with_authority(
        transaction,
        card_id,
        CardEventType::Patch,
        &actor,
        CardEventChange::Patch {
            fields: patched_fields,
        },
        now,
        authority,
    )?;
    if let Some((previous, status)) = status_change {
        if let Some(event_type) = events::outbound_event_for_status_change(previous, status) {
            events::append_outbound_card_event_with_authority(
                transaction,
                &card,
                event_type,
                authority,
                CardEventChange::Status {
                    previous,
                    current: status,
                },
                now,
            )?;
        }
    }
    load_card(transaction, card_id)
}

fn create_card_in_transaction(
    transaction: &Transaction<'_>,
    mut card: Card,
    authority: &Authority,
    now: i64,
) -> Result<Card> {
    let actor = non_empty("actor", &authority.actor_label())?;
    let card_id = card.id.clone();
    card.title = secrets::scrub_secrets(&card.title);
    card.body = secrets::scrub_secrets(&card.body);
    card.acceptance = scrub_string_list(std::mem::take(&mut card.acceptance));
    for criterion in &mut card.criteria {
        criterion.text = secrets::scrub_secrets(&criterion.text);
    }
    card.proof_plan = scrub_string_list(std::mem::take(&mut card.proof_plan));
    if load_card_optional(transaction, &card_id)?.is_some() {
        return Err(DomainError::conflict(format!("card already exists: {card_id}")).into());
    }
    if let Some(parent_id) = card.parent.clone() {
        ensure_parent_linkable(transaction, &card_id, &parent_id)?;
    }
    persist_card(transaction, &card)?;
    let saved = load_card(transaction, &card_id)?;
    append_card_event_with_authority(
        transaction,
        &saved.id,
        CardEventType::Create,
        &actor,
        CardEventChange::Create {
            source: "create-card".to_string(),
        },
        now,
        authority,
    )?;
    if let Some(parent_id) = saved.parent.as_ref() {
        append_card_event_with_authority(
            transaction,
            parent_id,
            CardEventType::Hierarchy,
            &actor,
            CardEventChange::Parent {
                previous: None,
                current: Some(card_id.clone()),
            },
            now,
            authority,
        )?;
    }
    mirror_initial_relations_with_authority(transaction, &saved, authority, now)?;
    events::append_outbound_card_event_with_authority(
        transaction,
        &saved,
        CardEventType::CardCreated,
        authority,
        CardEventChange::Create {
            source: "create-card".to_string(),
        },
        now,
    )?;
    Ok(saved)
}

fn append_work_log_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    agent: &str,
    run_id: Option<&str>,
    body: &str,
    now: i64,
    authority: &Authority,
) -> Result<WorkLogEntry> {
    let card = load_card(transaction, card_id)?;
    let run_id = match run_id {
        Some(raw) => Some(RunId::new(raw)?),
        None => card.claim.as_ref().map(|claim| claim.run_id.clone()),
    };
    let id = format!("work-log-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
    let entry = WorkLogEntry {
        id: id.clone(),
        card_id: card_id.clone(),
        agent: non_empty("agent", agent)?,
        run_id,
        body: non_empty_scrubbed("body", body)?,
        created_at: now,
    };
    authorize_card_operation(
        authority,
        Operation::WorkLog,
        &card,
        entry.run_id.as_ref(),
        Some(entry.agent.as_str()),
        now,
    )?;
    transaction.execute(
        "INSERT INTO work_log_entries
         (id, card_id, agent, run_id, body, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            entry.card_id.as_str(),
            entry.agent,
            entry.run_id.as_ref().map(RunId::as_str),
            entry.body,
            entry.created_at,
        ],
    )?;
    let audit_event = append_attributed_card_event(
        transaction,
        card_id,
        MutationAudit {
            operation: Operation::WorkLog,
            resource: card_id.as_str(),
            semantic_identity: Some(entry.agent.as_str()),
            run_id: entry.run_id.as_ref(),
            reason: None,
            event_type: CardEventType::WorkLog,
            actor: &entry.agent,
            change: CardEventChange::WorkLog {
                agent: entry.agent.clone(),
                run_id: entry.run_id.clone(),
                body: entry.body.clone(),
            },
            subject_kind: "work_log",
            subject_id: &entry.id,
            authority,
        },
        now,
    )?;
    events::append_outbound_card_event_for_audit(
        transaction,
        &card,
        CardEventType::WorkLogAppended,
        &entry.agent,
        CardEventChange::WorkLog {
            agent: entry.agent.clone(),
            run_id: entry.run_id.clone(),
            body: entry.body.clone(),
        },
        now,
        &audit_event,
    )?;
    Ok(entry)
}

fn add_comment_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    author: &str,
    body: &str,
    now: i64,
    authority: &Authority,
) -> Result<Comment> {
    let card = load_card(transaction, card_id)?;
    authorize_card_operation(authority, Operation::AddComment, &card, None, None, now)?;
    let id = format!("comment-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
    let comment = Comment {
        id: id.clone(),
        card_id: card_id.clone(),
        author: non_empty_scrubbed("author", author)?,
        body: non_empty_scrubbed("body", body)?,
        created_at: now,
    };
    authority
        .require_identity(&comment.author)
        .map_err(|error| {
            DomainError::authority_denied(DenialClass::IdentityMismatch, error.to_string())
        })?;
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
    let audit_event = append_attributed_card_event(
        transaction,
        card_id,
        MutationAudit {
            operation: Operation::AddComment,
            resource: card_id.as_str(),
            semantic_identity: Some(comment.author.as_str()),
            run_id: None,
            reason: None,
            event_type: CardEventType::Comment,
            actor: &comment.author,
            change: CardEventChange::Comment {
                author: comment.author.clone(),
                body: comment.body.clone(),
            },
            subject_kind: "comment",
            subject_id: &comment.id,
            authority,
        },
        now,
    )?;
    events::append_outbound_card_event_for_audit(
        transaction,
        &card,
        CardEventType::CommentAdded,
        &comment.author,
        CardEventChange::Comment {
            author: comment.author.clone(),
            body: comment.body.clone(),
        },
        now,
        &audit_event,
    )?;
    Ok(comment)
}

fn operation_for_event(event_type: CardEventType) -> Option<Operation> {
    match event_type {
        CardEventType::Create => Some(Operation::CreateCard),
        CardEventType::Patch => Some(Operation::PatchCard),
        CardEventType::Status => Some(Operation::UpdateStatus),
        CardEventType::Relations => Some(Operation::UpdateRelations),
        CardEventType::Hierarchy => Some(Operation::SetParent),
        CardEventType::Claim => Some(Operation::ClaimCard),
        CardEventType::Release => Some(Operation::ReleaseClaim),
        CardEventType::Renew => Some(Operation::RenewClaim),
        CardEventType::Heartbeat => Some(Operation::HeartbeatClaim),
        CardEventType::Transfer => Some(Operation::TransferClaim),
        CardEventType::Link => Some(Operation::AddLink),
        CardEventType::Comment => Some(Operation::AddComment),
        CardEventType::WorkLog => Some(Operation::WorkLog),
        CardEventType::RequestInput => Some(Operation::RequestInput),
        CardEventType::AnswerInput => Some(Operation::AnswerInput),
        CardEventType::Complete => Some(Operation::CompleteCard),
        _ => None,
    }
}

fn append_card_event_with_authority(
    connection: &Connection,
    card_id: &CardId,
    event_type: CardEventType,
    actor: &str,
    change: CardEventChange,
    now: i64,
    authority: &Authority,
) -> Result<CardEvent> {
    if change.is_retired() {
        return Err(
            DomainError::validation("change", "retired event variants are read-only").into(),
        );
    }
    let payload = to_json(&change)?;
    let operation = operation_for_event(event_type);
    let reason = operation
        .filter(|operation| operation.rule().audit.reason)
        .map(|_| payload.clone());
    let event = CardEvent {
        id: CardEventId::new(format!("event-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?,
        card_id: card_id.clone(),
        event_type: non_empty("event_type", event_type.as_str())?,
        actor: non_empty("actor", actor)?,
        change,
        principal: authority.principal_name().map(str::to_string),
        role: Some(authority.role_label().to_string()),
        subject_kind: None,
        subject_id: None,
        operation: operation.map(|operation| operation.as_str().to_string()),
        resource: Some(format!("card:{}", card_id.as_str())),
        semantic_identity: Some(actor.to_string()),
        run_id: None,
        reason,
        created_at: now,
    };
    connection.execute(
        "INSERT INTO card_events (
           id, card_id, event_type, actor, payload, principal, role,
           operation, resource, semantic_identity, run_id, reason, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            event.id.as_str(),
            event.card_id.as_str(),
            event.event_type.as_str(),
            event.actor.as_str(),
            payload,
            event.principal.as_deref(),
            event.role.as_deref(),
            event.operation.as_deref(),
            event.resource.as_deref(),
            event.semantic_identity.as_deref(),
            event.run_id.as_deref(),
            event.reason.as_deref(),
            event.created_at
        ],
    )?;
    Ok(event)
}

fn append_attributed_card_event(
    connection: &Connection,
    card_id: &CardId,
    audit: MutationAudit<'_>,
    now: i64,
) -> Result<CardEvent> {
    if audit.change.is_retired() {
        return Err(
            DomainError::validation("change", "retired event variants are read-only").into(),
        );
    }
    let event = CardEvent {
        id: CardEventId::new(format!("event-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?,
        card_id: card_id.clone(),
        event_type: non_empty("event_type", audit.event_type.as_str())?,
        actor: non_empty("actor", audit.actor)?,
        change: audit.change,
        principal: audit.authority.principal_name().map(str::to_string),
        role: Some(audit.authority.role_label().to_string()),
        subject_kind: Some(non_empty("subject_kind", audit.subject_kind)?),
        subject_id: Some(non_empty("subject_id", audit.subject_id)?),
        operation: Some(audit.operation.as_str().to_string()),
        resource: Some(non_empty("resource", audit.resource)?),
        semantic_identity: audit.semantic_identity.map(str::to_string),
        run_id: audit.run_id.map(ToString::to_string),
        reason: audit.reason.map(str::to_string),
        created_at: now,
    };
    let payload = to_json(&event.change)?;
    connection.execute(
        "INSERT INTO card_events (
           id, card_id, event_type, actor, payload, principal, role,
           subject_kind, subject_id, operation, resource, semantic_identity, run_id, reason, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            event.id.as_str(),
            event.card_id.as_str(),
            event.event_type.as_str(),
            event.actor.as_str(),
            payload,
            event.principal.as_deref(),
            event.role.as_deref(),
            event.subject_kind.as_deref(),
            event.subject_id.as_deref(),
            event.operation.as_deref(),
            event.resource.as_deref(),
            event.semantic_identity.as_deref(),
            event.run_id.as_deref(),
            event.reason.as_deref(),
            event.created_at,
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

fn release_claim_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    run_id: &RunId,
    now: i64,
    authority: &Authority,
) -> Result<ClaimReceipt> {
    let mut card = load_card(transaction, card_id)?;
    let worker = card.claim.as_ref().map(|claim| claim.agent.as_str());
    authorize_card_operation(
        authority,
        Operation::ReleaseClaim,
        &card,
        Some(run_id),
        worker,
        now,
    )?;
    let claim = card.release_claim(run_id, now)?;
    persist_card(transaction, &card)?;
    release_run(transaction, run_id, now)?;
    append_activity_attributed(
        transaction,
        run_id,
        ActivityType::Action,
        &format!("released {card_id}"),
        authority.principal_name(),
        Some(authority.role_label()),
        now,
    )?;
    events::append_outbound_card_event_with_authority(
        transaction,
        &card,
        CardEventType::MovedToReady,
        authority,
        CardEventChange::Status {
            previous: CardStatus::InProgress,
            current: CardStatus::Ready,
        },
        now,
    )?;
    Ok(claim_receipt(card_id, &claim))
}

fn renew_claim_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    run_id: &RunId,
    now: i64,
    ttl_seconds: u64,
    authority: &Authority,
) -> Result<ClaimReceipt> {
    let mut card = load_card(transaction, card_id)?;
    let worker = card.claim.as_ref().map(|claim| claim.agent.as_str());
    authorize_card_operation(
        authority,
        Operation::RenewClaim,
        &card,
        Some(run_id),
        worker,
        now,
    )?;
    let claim = card.renew_claim(run_id, now, ttl_seconds)?;
    persist_card(transaction, &card)?;
    let updated = transaction.execute(
        "UPDATE runs
         SET claim_expires_at = ?2, updated_at = ?3
         WHERE id = ?1",
        params![run_id.as_str(), claim.expires_at, now],
    )?;
    if updated == 0 {
        return Err(DomainError::not_found("run", run_id.to_string()).into());
    }
    append_activity_attributed(
        transaction,
        run_id,
        ActivityType::Action,
        &format!("renewed {card_id} until {}", claim.expires_at),
        authority.principal_name(),
        Some(authority.role_label()),
        now,
    )?;
    Ok(claim_receipt(card_id, &claim))
}

fn heartbeat_claim_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    run_id: &RunId,
    now: i64,
    authority: &Authority,
) -> Result<ClaimReceipt> {
    let mut card = load_card(transaction, card_id)?;
    let worker = card.claim.as_ref().map(|claim| claim.agent.as_str());
    authorize_card_operation(
        authority,
        Operation::HeartbeatClaim,
        &card,
        Some(run_id),
        worker,
        now,
    )?;
    let claim = card.heartbeat_claim(run_id, now)?;
    persist_card(transaction, &card)?;
    let updated = transaction.execute(
        "UPDATE runs
         SET updated_at = ?2
         WHERE id = ?1",
        params![run_id.as_str(), now],
    )?;
    if updated == 0 {
        return Err(DomainError::not_found("run", run_id.to_string()).into());
    }
    append_activity_attributed(
        transaction,
        run_id,
        ActivityType::Action,
        &format!("heartbeat {card_id}"),
        authority.principal_name(),
        Some(authority.role_label()),
        now,
    )?;
    Ok(claim_receipt(card_id, &claim))
}

fn transfer_claim_in_transaction(
    transaction: &Transaction<'_>,
    card_id: &CardId,
    run_id: &RunId,
    to_agent: &str,
    now: i64,
    ttl_seconds: u64,
    authority: &Authority,
) -> Result<ClaimReceipt> {
    let mut card = load_card(transaction, card_id)?;
    let worker = card.claim.as_ref().map(|claim| claim.agent.as_str());
    authorize_card_operation(
        authority,
        Operation::TransferClaim,
        &card,
        Some(run_id),
        worker,
        now,
    )?;
    let from_agent = card.claim_holder().unwrap_or_default().to_string();
    let claim = card.transfer_claim(run_id, to_agent, now, ttl_seconds)?;
    persist_card(transaction, &card)?;
    let updated = transaction.execute(
        "UPDATE runs
         SET agent = ?2, claim_expires_at = ?3, updated_at = ?4
         WHERE id = ?1",
        params![run_id.as_str(), to_agent, claim.expires_at, now],
    )?;
    if updated == 0 {
        return Err(DomainError::not_found("run", run_id.to_string()).into());
    }
    append_activity_attributed(
        transaction,
        run_id,
        ActivityType::Action,
        &format!("transferred {card_id} from {from_agent} to {to_agent}"),
        authority.principal_name(),
        Some(authority.role_label()),
        now,
    )?;
    Ok(claim_receipt(card_id, &claim))
}

fn claim_receipt(card_id: &CardId, claim: &Claim) -> ClaimReceipt {
    ClaimReceipt {
        card_id: card_id.clone(),
        run_id: claim.run_id.clone(),
        principal: claim.principal.clone(),
        agent: claim.agent.clone(),
        expires_at: claim.expires_at,
    }
}

fn load_card(connection: &Connection, card_id: &CardId) -> Result<Card> {
    connection
        .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
        .optional()?
        .ok_or_else(|| DomainError::not_found("card", card_id.to_string()).into())
        .and_then(card_from_record)
}

fn load_card_optional(connection: &Connection, card_id: &CardId) -> Result<Option<Card>> {
    connection
        .query_row(CARD_SELECT_SQL, [card_id.as_str()], CardRecord::from_row)
        .optional()?
        .map(card_from_record)
        .transpose()
}

#[derive(Debug)]
struct RawSearchMatch {
    source_table: String,
    source_field: String,
    card_id: CardId,
    created_at: i64,
    snippet: String,
    rank: f64,
}

fn quote_fts_prefix(term: &str) -> String {
    format!("\"{}\"*", term.replace('"', "\"\""))
}

fn quote_fts_exact(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

fn rewrite_search_query(query: &str) -> Option<String> {
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    match terms.as_slice() {
        [] => None,
        [term] => Some(format!(
            "({} OR {})",
            quote_fts_exact(term),
            quote_fts_prefix(term)
        )),
        terms => Some(format!(
            "NEAR({}, 10)",
            terms
                .iter()
                .map(|term| quote_fts_prefix(term))
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

fn escape_like_prefix(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn search_query_fingerprint(query: &SearchQuery) -> String {
    let values = [
        query.q.trim().to_string(),
        query
            .status
            .map(|value| value.as_str().to_string())
            .unwrap_or_default(),
        query.repo.clone().unwrap_or_default(),
        query.label.clone().unwrap_or_default(),
        query
            .priority
            .map(|value| value.as_str().to_string())
            .unwrap_or_default(),
        query
            .created_after
            .map(|value| value.to_string())
            .unwrap_or_default(),
        query
            .created_before
            .map(|value| value.to_string())
            .unwrap_or_default(),
        query
            .updated_after
            .map(|value| value.to_string())
            .unwrap_or_default(),
        query
            .updated_before
            .map(|value| value.to_string())
            .unwrap_or_default(),
    ];
    format!("{:x}", Sha256::digest(values.join("\u{1f}").as_bytes()))
}

fn encode_search_cursor(fingerprint: &str, offset: usize) -> String {
    format!("v1:{fingerprint}:{offset}")
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_search_cursor(raw: &str, expected_fingerprint: &str) -> Result<usize> {
    if raw.is_empty() || !raw.is_ascii() || !raw.len().is_multiple_of(2) {
        return Err(StoreError::InvalidSearchCursor(raw.to_string()));
    }
    let bytes = (0..raw.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&raw[index..index + 2], 16)
                .map_err(|_| StoreError::InvalidSearchCursor(raw.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;
    let payload =
        String::from_utf8(bytes).map_err(|_| StoreError::InvalidSearchCursor(raw.to_string()))?;
    let mut fields = payload.split(':');
    if fields.next() != Some("v1") || fields.next() != Some(expected_fingerprint) {
        return Err(StoreError::InvalidSearchCursor(
            "cursor does not match query or filters".to_string(),
        ));
    }
    fields
        .next()
        .filter(|value| !value.is_empty() && fields.next().is_none())
        .ok_or_else(|| StoreError::InvalidSearchCursor(raw.to_string()))?
        .parse::<usize>()
        .map_err(|_| StoreError::InvalidSearchCursor(raw.to_string()))
}

fn card_from_record(record: CardRecord) -> Result<Card> {
    record.into_card()
}

/// Full unfiltered card scan, one query -- shared by [`Store::list_ready_page`]
/// and the transitive-blocker walk in `answer_loop::get_card_detail`, so
/// relation-graph traversals never need a second per-blocker query.
pub(crate) fn load_all_cards(connection: &Connection) -> Result<Vec<Card>> {
    let mut statement = connection.prepare(CARD_SELECT_ALL_SQL)?;
    let records = statement
        .query_map([], CardRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records.into_iter().map(card_from_record).collect()
}

fn ready_order_digest(cards: &[Card]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ready-order-v1\0");
    for card in cards {
        let bytes = card.id.as_str().as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

/// Shared continuation-slicing step for [`Store::list_cards_page_after`]
/// and [`Store::list_ready_page_after`] (powder-cards-api-paged-continuation):
/// `cards` is the caller's already fully-computed, already-ordered eligible
/// list (post filter, post sort/topological-order, pre-truncate) -- this
/// helper never touches the database or recomputes anything, it only walks
/// that in-memory `Vec` to find where a prior page left off.
///
/// `after`, when set, must name a card present in `cards`; an id that
/// doesn't appear there (never existed in this order, filtered out by
/// different query parameters than the prior call used, or gone ineligible
/// since) is rejected outright rather than silently resuming from the
/// start or skipping over cards -- a wrong resume point would look like no
/// bug at all while quietly dropping or duplicating cards for the caller.
///
/// Returns the `limit`-sized (or shorter, on the last page) slice starting
/// just after `after`'s position, plus `next_after`: the id to pass on the
/// following call, present only when this slice didn't reach the end of
/// `cards`.
fn paginate_ordered_cards(
    mut cards: Vec<Card>,
    limit: usize,
    after: Option<&CardId>,
) -> Result<(Vec<Card>, Option<CardId>)> {
    let limit = limit.max(1);
    let start = match after {
        None => 0,
        Some(after_id) => {
            let position = cards
                .iter()
                .position(|card| card.id == *after_id)
                .ok_or_else(|| {
                    DomainError::validation(
                        "after",
                        format!(
                            "card {after_id} is not in the current result set (stale or \
                             filtered-out continuation token)"
                        ),
                    )
                })?;
            position + 1
        }
    };
    let end = (start + limit).min(cards.len());
    let next_after = (end < cards.len()).then(|| cards[end - 1].id.clone());
    let page = cards.drain(start..end).collect();
    Ok((page, next_after))
}

/// A parent edge must point at an existing card and must not close a cycle:
/// walking up from the proposed parent may never reach the child. A dangling
/// ancestor edge (parent card deleted out from under a child) terminates the
/// walk as a root rather than erroring -- reads already tolerate it.
fn ensure_parent_linkable(
    connection: &Connection,
    child_id: &CardId,
    parent_id: &CardId,
) -> Result<()> {
    if parent_id == child_id {
        return Err(DomainError::validation("parent", "card cannot be its own parent").into());
    }
    let Some(mut ancestor) = load_card_optional(connection, parent_id)? else {
        return Err(DomainError::not_found("card", parent_id.to_string()).into());
    };
    let mut hops = 0;
    loop {
        if ancestor.id == *child_id {
            return Err(DomainError::conflict(format!(
                "linking {child_id} under {parent_id} would create a hierarchy cycle"
            ))
            .into());
        }
        let Some(next_id) = ancestor.parent.clone() else {
            return Ok(());
        };
        hops += 1;
        if hops > 64 {
            return Err(
                DomainError::conflict("hierarchy depth limit (64) exceeded".to_string()).into(),
            );
        }
        match load_card_optional(connection, &next_id)? {
            Some(next) => ancestor = next,
            None => return Ok(()),
        }
    }
}

/// Report from `Store::repair_criteria`: which criteria changed and whether
/// checked/proof state was preserved at each position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriteriaRepair {
    pub card_id: String,
    pub criteria_changed: usize,
    pub changes: Vec<CriteriaChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CriteriaChange {
    pub index: usize,
    pub previous: String,
    pub current: String,
    pub state_preserved: bool,
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

/// The effective acceptance oracle encoded by the two legacy card columns.
/// Structured criteria are authoritative when at least one non-blank item is
/// present; otherwise the cleaned string list remains the source of truth.
/// Both migration classification and ordinary card materialization use this
/// decoder so a card cannot be migrated according to an oracle `get_card`
/// would then replace with different data.
struct StoredOracle {
    acceptance: Vec<String>,
    criteria: Vec<AcceptanceCriterion>,
}

fn decode_stored_oracle(acceptance_json: String, criteria_json: String) -> Result<StoredOracle> {
    let fallback_acceptance = clean_string_list(from_json::<Vec<String>>(
        "cards.acceptance_json",
        acceptance_json,
    )?);
    let criteria = from_json::<Vec<AcceptanceCriterion>>("cards.criteria_json", criteria_json)?
        .into_iter()
        .filter(|criterion| !criterion.text.trim().is_empty())
        .collect::<Vec<_>>();
    let acceptance = if criteria.is_empty() {
        fallback_acceptance
    } else {
        criteria
            .iter()
            .map(|criterion| criterion.text.clone())
            .collect()
    };
    Ok(StoredOracle {
        acceptance,
        criteria,
    })
}

/// Decodes the persisted principal/worker/run claim tuple. Partial tuples and
/// complete tuples with a blank identity are claimless; this leaves their raw
/// database bytes available for diagnosis while ensuring every reader agrees
/// with migrations about whether active work exists.
fn decode_stored_claim(
    principal: Option<String>,
    agent: Option<String>,
    run_id: Option<String>,
    acquired_at: Option<i64>,
    expires_at: Option<i64>,
) -> Result<Option<Claim>> {
    let (Some(principal), Some(agent), Some(run_id), Some(acquired_at), Some(expires_at)) =
        (principal, agent, run_id, acquired_at, expires_at)
    else {
        return Ok(None);
    };
    if principal.trim().is_empty() || agent.trim().is_empty() || run_id.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(Claim {
        principal,
        agent,
        run_id: RunId::new(run_id)?,
        acquired_at,
        expires_at,
    }))
}

fn non_empty(field: &'static str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(DomainError::validation(field, "value cannot be empty").into())
    } else {
        Ok(trimmed.to_owned())
    }
}

/// `non_empty` plus [`secrets::scrub_secrets`] in one call: the write-boundary
/// helper for every agent/human free-text field (powder-scrub-write-boundary).
/// Scrubbing happens here, inside the store's own write functions, rather
/// than in any adapter, so there is exactly one seam credential-shaped text
/// must cross on its way into persistence -- outbound event payloads built
/// from the already-scrubbed value are clean for free.
fn non_empty_scrubbed(field: &'static str, value: &str) -> Result<String> {
    Ok(secrets::scrub_secrets(&non_empty(field, value)?))
}

/// [`secrets::scrub_secrets`] over a list of free-text items (acceptance
/// criteria, proof-plan steps) at the same write boundary as
/// [`non_empty_scrubbed`]. Lives here rather than in `powder-core`'s
/// `with_acceptance`/`with_proof_plan` because core imports no adapter or
/// scrubbing machinery -- persistence-side sanitization is the store's job.
fn scrub_string_list(items: impl IntoIterator<Item = String>) -> Vec<String> {
    items
        .into_iter()
        .map(|item| secrets::scrub_secrets(&item))
        .collect()
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
    priority: String,
    labels_json: String,
    related_json: String,
    blocks_json: String,
    blocked_by_json: String,
    repo: Option<String>,
    claim_principal: Option<String>,
    claim_agent: Option<String>,
    claim_run_id: Option<String>,
    claim_acquired_at: Option<i64>,
    claim_expires_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
    parent: Option<String>,
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
            priority: row.get(7)?,
            labels_json: row.get(8)?,
            related_json: row.get(9)?,
            blocks_json: row.get(10)?,
            blocked_by_json: row.get(11)?,
            repo: row.get(12)?,
            claim_principal: row.get(13)?,
            claim_agent: row.get(14)?,
            claim_run_id: row.get(15)?,
            claim_acquired_at: row.get(16)?,
            claim_expires_at: row.get(17)?,
            created_at: row.get(18)?,
            updated_at: row.get(19)?,
            parent: row.get(20)?,
        })
    }

    fn into_card(self) -> Result<Card> {
        let oracle = decode_stored_oracle(self.acceptance_json, self.criteria_json)?;
        let claim = decode_stored_claim(
            self.claim_principal,
            self.claim_agent,
            self.claim_run_id,
            self.claim_acquired_at,
            self.claim_expires_at,
        )?;
        let mut card = Card::new(CardId::new(self.id)?, self.title, self.body)?
            .with_acceptance(oracle.acceptance)
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
        if !oracle.criteria.is_empty() {
            card = card.with_criteria(oracle.criteria);
        }
        card = card.with_proof_plan(from_json::<Vec<String>>(
            "cards.proof_plan_json",
            self.proof_plan_json,
        )?);
        card.labels = from_json("cards.labels_json", self.labels_json)?;
        card.related = from_json("cards.related_json", self.related_json)?;
        card.blocks = from_json("cards.blocks_json", self.blocks_json)?;
        card.blocked_by = from_json("cards.blocked_by_json", self.blocked_by_json)?;
        card.parent = self.parent.map(CardId::new).transpose()?;
        card.repo = self.repo;
        card.claim = claim;
        card.updated_at = self.updated_at;
        Ok(card)
    }
}
