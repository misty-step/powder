#![forbid(unsafe_code)]

use std::{collections::HashMap, fs, path::Path};

use powder_core::{
    canonical_repo_label, canonical_repo_matches, repo_from_numeric_card_id_prefix,
    AcceptanceCriterion, Activity, ActivityId, ActivityType, AttachmentMeta, Authority, Card,
    CardEvent, CardEventId, CardId, CardSource, CardStatus, Claim, ClaimReceipt, Comment,
    CriterionProof, DomainError, EpicState, Estimate, Link, LinkId, Priority, ReadyQuery, Risk,
    Run, RunId, RunState, WorkLogEntry,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

mod answer_loop;
mod events;
mod identity;
mod relations;
mod repositories;
mod schema;
mod secrets;
#[cfg(test)]
mod tests;

pub use events::{
    CardEventEnvelope, DeadLetterDelivery, EventSubscription, EventSubscriptionCreated,
    EventTailItem, WebhookDelivery, CARD_EVENT_SCHEMA_VERSION, EVENT_TYPES,
};
pub use identity::{ApiKeyCreated, ApiKeyScope, ApiKeySummary, VerifiedApiKey};
use relations::{list_delta, mirror_delta, mirror_initial_relations};
pub use relations::{RelationField, RelationsDoctorIssue, RelationsDoctorReport};
use repositories::{resolve_registered_repository_for_write, resolve_repository_name};
pub use repositories::{
    RepositoryDoctorEntry, RepositoryDoctorReport, RepositoryMergeOutcome,
    RepositoryNormalizeChange, RepositoryNormalizeOutcome, RepositorySummary, RepositoryTier,
    RepositoryUpsert, RepositoryVisibility,
};
/// The current on-disk schema version `Store::migrate` converges to.
/// Public so a caller (`/readyz`'s schema-match gate is the motivating one)
/// can compare a database's actual `schema_version()` against what this
/// build of `powder-store` expects, rather than only being able to ask "did
/// migration succeed" after the fact.
pub use schema::SCHEMA_VERSION;

use schema::{
    CARD_COLUMNS, CARD_SELECT_ALL_SQL, CARD_SELECT_SQL, MIGRATE_10_TO_11, MIGRATE_11_TO_12,
    MIGRATE_12_TO_13, MIGRATE_13_TO_14, MIGRATE_14_TO_15, MIGRATE_15_TO_16, MIGRATE_2_TO_3,
    MIGRATE_5_TO_6, MIGRATE_6_TO_7, MIGRATE_7_TO_8, MIGRATE_9_TO_10, RUN_SELECT_SQL, SCHEMA,
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
    field_note_config: Option<FieldNoteConfig>,
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
///
/// `include_terminal` decides whether `Done`/`Shipped`/`Abandoned` cards are
/// eligible when no explicit `status` is requested (an explicit `status`
/// always wins -- asking for `status: done` returns `done` cards regardless
/// of this flag; see `list_cards_page`). It defaults to `true` (the
/// pre-powder-mcp-unfiltered-enumeration behavior: an unfiltered query sees
/// the whole board, terminal cards included) so every existing caller of
/// `CardFilter::default()` -- the HTTP `list_cards` route, the `powder
/// list-cards` CLI command, and the plain-store test suite -- keeps its
/// current behavior unchanged. Only `powder-mcp`'s `list_cards` tool opts
/// into `include_terminal: false` by default, because an agent enumerating
/// "what's on the board" with no filter is far more likely to be surprised
/// by a done/shipped/abandoned card silently filling its result window than
/// to be relying on seeing one.
#[derive(Debug, Clone)]
pub struct CardFilter {
    pub status: Option<CardStatus>,
    pub repo: Option<String>,
    pub estimate: Option<Estimate>,
    pub label: Option<String>,
    pub include_terminal: bool,
}

impl Default for CardFilter {
    fn default() -> Self {
        CardFilter {
            status: None,
            repo: None,
            estimate: None,
            label: None,
            include_terminal: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardListPage {
    pub cards: Vec<Card>,
    pub total_count: usize,
    /// How many of `total_count` were held back by
    /// [`CardFilter::include_terminal`]`: false` (always 0 when the filter
    /// includes terminal cards, when an explicit `status` was requested, or
    /// for `list_ready`). Kept separate so an envelope can distinguish "more
    /// matches beyond `limit` -- raise the limit" from "matches hidden by
    /// the terminal exclusion -- pass `include_terminal: true`": raising the
    /// limit does nothing for the latter, and a hint that conflates the two
    /// sends an agent in a loop.
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
    pub in_progress: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub awaiting_input: usize,
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
            CardStatus::InProgress => self.in_progress += cards,
            CardStatus::AwaitingInput => self.awaiting_input += cards,
            CardStatus::Done => self.done += cards,
            CardStatus::Shipped => self.shipped += cards,
            CardStatus::Abandoned => self.abandoned += cards,
        }
    }
}

fn is_zero(value: &usize) -> bool {
    *value == 0
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
/// are preserved from the stored row; lifecycle/source/workspace fields are
/// intentionally absent from this shape. `repo` is the one repository-
/// topology exception (powder-repo-hygiene): admin-gated at the HTTP layer
/// since it moves a card between board groupings rather than editing the
/// card's own content, but it lives here rather than behind a bulk-only
/// endpoint because single-card repo corrections (an orphaned card, a
/// mis-imported one) don't warrant `merge_repository_alias`'s all-matching-
/// cards blast radius. `Some(None)` clears the card to repo-less; `None`
/// (the field left off the patch entirely) leaves it untouched.
#[derive(Debug, Clone, Default)]
pub struct CardPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub acceptance: Option<Vec<String>>,
    pub proof_plan: Option<Vec<String>>,
    pub status: Option<CardStatus>,
    pub priority: Option<Priority>,
    pub estimate: Option<Estimate>,
    pub risk: Option<Risk>,
    pub labels: Option<Vec<String>>,
    pub repo: Option<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

struct MutationAudit<'a> {
    event_type: &'a str,
    actor: &'a str,
    payload: &'a str,
    subject_kind: &'a str,
    subject_id: &'a str,
    authority: &'a Authority,
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
                    self.backfill_repositories_from_cards()?;
                    7
                }
                7 => {
                    self.migrate_7_to_8()?;
                    self.apply_ratified_repository_tier_seed()?;
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
                _ => return Err(StoreError::UnsupportedSchema(current)),
            };
            self.connection
                .execute_batch(&format!("PRAGMA user_version = {next}"))?;
        }
    }

    /// powder-epic-truthful-ops: steps 1-10 originally ran their DDL
    /// unconditionally, unlike the `cards_has_column`-guarded steps below
    /// (11-16). A process crash (OOM kill, host reboot, `pkill -9`) between a
    /// step's DDL and its `PRAGMA user_version` bump left the DB schema
    /// ahead of its recorded version; the next boot would re-run the same
    /// `ALTER TABLE ... ADD COLUMN` and fail with "duplicate column name",
    /// wedging every subsequent boot until a human intervened by hand. These
    /// wrappers close that gap the same way 11-16 already do: check whether
    /// the DDL's effect is already present before re-issuing it. Table
    /// creation and index statements already use `IF NOT EXISTS` and are
    /// naturally idempotent, so only the bare `ALTER TABLE` steps need a
    /// guard.
    /// powder-epic-truthful-ops (review fix): `MIGRATE_1_TO_2` is DDL *plus*
    /// two backfill statements, and `execute_batch` autocommits per
    /// statement -- so guarding the whole batch on `actor_id`'s existence was
    /// wrong. A crash after the `ALTER TABLE ... ADD COLUMN actor_id` commits
    /// but before the two backfills run leaves the column present with every
    /// value NULL; on retry the single column-existence guard would see the
    /// column and skip the backfills *forever*. That is not cosmetic:
    /// `verify_api_key` INNER JOINs `api_keys` to `actors`, so a permanently
    /// unbackfilled `actor_id` silently stops every pre-existing key from
    /// authenticating. Decomposed into three independently-idempotent phases,
    /// mirroring `migrate_3_to_4`'s per-effect guards:
    ///
    /// 1. `actors` table + the `api_keys` index are `CREATE ... IF NOT
    ///    EXISTS`, safe to re-run unconditionally.
    /// 2. the `ADD COLUMN` is guarded on column existence (an `ALTER ... ADD
    ///    COLUMN` cannot be re-run).
    /// 3. the backfill is guarded on its own *effect* -- whether any row is
    ///    still `actor_id IS NULL` -- not on the column's existence, so an
    ///    interrupted backfill is finished on the next boot. (The backfill's
    ///    own `WHERE actor_id IS NULL` also makes re-running it harmless; the
    ///    completeness guard just avoids a pointless full-table UPDATE when
    ///    there is nothing left to do.)
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
                let (new_status, detail) = match old_status.as_str() {
                    "claimed" | "running" if has_valid_claim => ("in_progress", ""),
                    "claimed" | "running" if has_acceptance => {
                        ("ready", " (no valid claim; acceptance oracle present)")
                    }
                    "claimed" | "running" => ("backlog", " (no valid claim or acceptance oracle)"),
                    "blocked" if !has_acceptance => ("backlog", " (empty acceptance)"),
                    "blocked" if !has_blocked_by => (
                        "backlog",
                        " (no blocked_by relations; re-triage before claiming)",
                    ),
                    "blocked" => ("ready", ""),
                    other => (other, ""),
                };
                transaction.execute(
                    "UPDATE cards SET status = ?1 WHERE id = ?2",
                    params![new_status, card_id],
                )?;
                append_card_event(
                    &transaction,
                    &CardId::new(card_id)?,
                    "status",
                    "system:status-vocabulary-migration",
                    &format!("status-vocabulary migration: {old_status} -> {new_status}{detail}"),
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
                       AND e.payload IN (
                         'status-vocabulary migration: claimed -> in_progress',
                         'status-vocabulary migration: running -> in_progress'
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
                    "status",
                    "system:status-v17-repair",
                    &format!(
                        "status-v17 repair: in_progress -> {new_status} \
                         (claimless v17 migration)"
                    ),
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

    /// Proves the database file itself is writable, not just readable --
    /// `readiness_check`'s bare `SELECT 1` succeeds even against a
    /// read-only file, a full disk that still permits reads, or a
    /// replication target mid-restore. `BEGIN IMMEDIATE` acquires SQLite's
    /// write lock up front (unlike a deferred `BEGIN`, which only acquires
    /// it on the first write and would let a read-only file pass), so
    /// failure here means an actual write is currently impossible. The
    /// transaction never writes anything and always rolls back -- this is a
    /// probe, not a mutation.
    pub fn writable_probe(&self) -> Result<()> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE; ROLLBACK;")?;
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

    /// Upsert externally sourced cards without clobbering live lifecycle state: a
    /// card that is claimed, running, awaiting input, or already at a
    /// terminal outcome keeps its stored status/claim, while its content
    /// (title, body, acceptance, labels, source digest, ...) still refreshes
    /// from the incoming source. See [`Card::merge_reimport`].
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
    /// unchanged report before committing to the upsert. `content_repaired`
    /// surfaces cards whose source digest is unchanged but whose acceptance
    /// text differs from what is stored.
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
        card.title = secrets::scrub_secrets(&card.title);
        card.body = secrets::scrub_secrets(&card.body);
        // `acceptance` and `criteria` carry the same author-supplied text
        // (with_acceptance derives one from the other); scrub both with the
        // same deterministic function so they stay consistent.
        card.acceptance = scrub_string_list(std::mem::take(&mut card.acceptance));
        for criterion in &mut card.criteria {
            criterion.text = secrets::scrub_secrets(&criterion.text);
        }
        card.proof_plan = scrub_string_list(std::mem::take(&mut card.proof_plan));
        // Numeric-id repo inference is explicit-conflict-detection only: a
        // card id like `foo-123` never silently *attaches* repo "foo" (that
        // was the powder-repo-registry-tightness bug -- see the "why" in the
        // card). It still rejects an explicit `repo` that contradicts the
        // id's numeric-suffix prefix, so a card can't be mis-filed under a
        // conflicting repo by typo.
        if let Some(derived_repo) = repo_from_numeric_card_id_prefix(card_id.as_str()) {
            if let Some(repo) = card.repo.as_deref() {
                if !canonical_repo_matches(repo, &derived_repo) {
                    return Err(DomainError::validation(
                        "repo",
                        format!("repo {repo} does not match numeric card id prefix {derived_repo}"),
                    )
                    .into());
                }
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_card_optional(&transaction, &card_id)?.is_some() {
            return Err(DomainError::conflict(format!("card already exists: {card_id}")).into());
        }
        if let Some(parent_id) = card.parent.clone() {
            ensure_parent_linkable(&transaction, &card_id, &parent_id)?;
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
        if let Some(parent_id) = saved.parent.as_ref() {
            append_card_event(
                &transaction,
                parent_id,
                "decompose",
                &actor,
                &format!("child {card_id} created"),
                now,
            )?;
        }
        // A card born with related/blocks/blocked_by already set mirrors
        // those edges onto the named peers in the same transaction --
        // reciprocity is a birth-time guarantee, not something the caller
        // has to establish with follow-up update_relations calls
        // (powder-dogfood-2026-07-14-nonreciprocal-relations).
        mirror_initial_relations(&transaction, &saved, &actor, now)?;
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

    /// File a one-call papercut. The report maps deterministically to a
    /// backlog card labeled `papercut`; if `service` names a known repository
    /// entity the card is homed there, otherwise it carries a `service:<name>`
    /// label. The full report body and attribution are preserved, then run
    /// through the same secret-scrubbing path as comments and work logs.
    /// Emits a normal `create` audit event with the reporting agent as actor.
    pub fn file_papercut(
        &mut self,
        report: &powder_core::papercut::PapercutReport,
        actor: &str,
        now: i64,
    ) -> Result<Card> {
        let actor = non_empty("actor", actor)?;
        let resolved_repo = report
            .service
            .as_deref()
            .map(|service| self.get_repository(service))
            .transpose()?
            .flatten()
            .map(|summary| summary.repo);
        let id = CardId::new(format!(
            "papercut-{}",
            nanoid::nanoid!(12, &API_KEY_ALPHABET)
        ))?;
        let card = powder_core::papercut::file_papercut(
            report.clone(),
            resolved_repo.as_deref(),
            now,
            id,
        )?;
        self.create_card_with_events(card, &actor, now)
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
            card.title = non_empty_scrubbed("title", &title)?;
            patched_fields.push("title");
        }
        if let Some(body) = patch.body {
            card.body = secrets::scrub_secrets(&body);
            patched_fields.push("body");
        }
        if let Some(acceptance) = patch.acceptance {
            card = card.with_acceptance(scrub_string_list(acceptance));
            patched_fields.push("acceptance");
        }
        if let Some(proof_plan) = patch.proof_plan {
            card = card.with_proof_plan(scrub_string_list(proof_plan));
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
        if let Some(risk) = patch.risk {
            card.risk = Some(risk);
            patched_fields.push("risk");
        }
        if let Some(labels) = patch.labels {
            card.labels = clean_string_list(labels);
            patched_fields.push("labels");
        }
        if let Some(status) = patch.status {
            card.status = status;
            patched_fields.push("status");
        }
        if let Some(repo) = patch.repo {
            card.repo = repo;
            patched_fields.push("repo");
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
        // `persist_card` canonicalizes `repo` at write time via
        // `resolve_registered_repository_name` (an alias like
        // "misty-step/canary" becomes "canary" in the DB row) but only
        // borrows `card`, so the
        // in-memory value above is still whatever the caller passed. Reload
        // so the returned `Card` matches the row exactly -- same reason
        // `create_card_with_events` reloads after its own `persist_card`
        // call instead of returning its own `card` binding.
        let saved = load_card(&transaction, card_id)?;

        transaction.commit()?;
        Ok(saved)
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
        let subject_id = criterion.to_string();
        append_attributed_card_event(
            &transaction,
            card_id,
            MutationAudit {
                event_type: "criterion",
                actor: &actor,
                payload: &format!(
                    "criterion {} {}",
                    criterion,
                    if checked { "checked" } else { "unchecked" }
                ),
                subject_kind: "criterion",
                subject_id: &subject_id,
                authority,
            },
            now,
        )?;
        transaction.commit()?;
        Ok(card)
    }

    /// Repair a card's acceptance criteria by re-parsing the oracle source
    /// and applying the result while preserving checked/proof state for any
    /// criterion whose identity survives (same position and unchanged text,
    /// or stored text is a truncation-prefix of the new text). Status,
    /// claim, relations, comments, and source provenance are left untouched
    /// -- only the criteria text and the structured criteria columns change.
    pub fn repair_criteria(
        &mut self,
        card_id: &CardId,
        acceptance: Vec<String>,
        actor: &str,
        now: i64,
    ) -> Result<CriteriaRepair> {
        let actor = non_empty("actor", actor)?;
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
            append_card_event(
                &transaction,
                card_id,
                "repair",
                &actor,
                &format!("repaired {} acceptance criterion(s)", changes.len()),
                now,
            )?;
        }

        transaction.commit()?;
        Ok(CriteriaRepair {
            card_id: card_id.to_string(),
            criteria_changed: changes.len(),
            changes,
        })
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

    /// Continuation-aware variant of [`Store::list_ready_page`] --
    /// unchanged when `after` is `None` (delegated to by `list_ready_page`
    /// itself), used directly by the HTTP `/api/v1/cards/ready` route to
    /// resume past a prior page (powder-cards-api-paged-continuation).
    ///
    /// `after`, when set, must be the id of a card present in the *same*
    /// eligibility-filtered, topologically-ordered list this call
    /// recomputes from scratch (typically the `next_after`/last card id a
    /// prior call on this store returned); an id absent from that list --
    /// never existed, no longer ready-eligible, or filtered by different
    /// `query` parameters than the prior call used -- is rejected with a
    /// validation error rather than silently resuming from the start or
    /// skipping cards. This is an *interim* continuation over an
    /// already-materialized in-memory list this call still fully
    /// recomputes (full table scan, then eligibility filter, then
    /// topological order) every time -- it bounds response payload size,
    /// not per-request DB/CPU cost; see [`CardListPage::next_after`] and
    /// the separate `powder-store-sql-pushed-list-filtering` follow-up for
    /// what actually fixes that cost.
    pub fn list_ready_page_after(
        &self,
        query: ReadyQuery,
        after: Option<&CardId>,
    ) -> Result<CardListPage> {
        let all_cards = load_all_cards(&self.connection)?;
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
            cards.push(card);
        }
        let total_count = cards.len();

        let order = powder_core::order_ready_cards(cards);
        let cycle_card_ids = order.cycle_card_ids;
        let (cards, next_after) = paginate_ordered_cards(order.cards, query.limit, after)?;
        Ok(CardListPage {
            cards,
            total_count,
            excluded_terminal_count: 0,
            cycle_card_ids,
            next_after,
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
        // status/repo/estimate/label filters -- deliberately computed
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
        })
    }

    /// Raw count of every card in the store, ignoring every filter
    /// dimension -- used by `powder-mcp`'s `list_cards` envelope to tell a
    /// caller whose filtered query matched zero cards how large the board
    /// actually is, so a narrow filter never reads as an empty board.
    pub fn card_count(&self) -> Result<usize> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM cards", [], |row| row.get::<_, i64>(0))?
            as usize)
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
        let principal = authority.actor_label();
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
            events::append_outbound_card_event(
                &transaction,
                &card,
                "claim-expired",
                &expired.principal,
                json!({
                    "principal": expired.principal.as_str(),
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
        let mut card = load_card(&transaction, card_id)?;
        let previous = card.status;
        let released_claim = card.apply_status(status, now);
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
        if status.is_terminal() && !previous.is_terminal() {
            append_parent_rollup_event(
                &transaction,
                &card,
                &authority.actor_label(),
                &format!("child {card_id} reached {}", status.as_str()),
                now,
            )?;
        }
        transaction.commit()?;
        Ok(card)
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
        let mut card = load_card(&transaction, card_id)?;
        let actor = authority.actor_label();

        let related_delta = list_delta(&card.related, &related);
        let blocks_delta = list_delta(&card.blocks, &blocks);
        let blocked_by_delta = list_delta(&card.blocked_by, &blocked_by);

        card.apply_relations(related, blocks, blocked_by, now);
        persist_card(&transaction, &card)?;
        append_card_event(
            &transaction,
            card_id,
            "relations",
            &actor,
            &format!(
                "related={:?} blocks={:?} blocked_by={:?}",
                card.related, card.blocks, card.blocked_by
            ),
            now,
        )?;

        mirror_delta(
            &transaction,
            card_id,
            RelationField::Related,
            &related_delta,
            &actor,
            now,
        )?;
        mirror_delta(
            &transaction,
            card_id,
            RelationField::Blocks,
            &blocks_delta,
            &actor,
            now,
        )?;
        mirror_delta(
            &transaction,
            card_id,
            RelationField::BlockedBy,
            &blocked_by_delta,
            &actor,
            now,
        )?;

        transaction.commit()?;
        Ok(card)
    }

    /// Set or clear a card's explicit parent edge. Validates that the parent
    /// exists and that the link cannot create a cycle; audits the change on
    /// the child (`hierarchy`), the new parent (`decompose`), and the old
    /// parent (`hierarchy`). The parent's own status is never touched --
    /// decomposition is auditable coordination, not lifecycle.
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
        let mut card = load_card(&transaction, card_id)?;
        let previous = card.parent.clone();
        if previous == parent {
            transaction.commit()?;
            return Ok(card);
        }
        if let Some(new_parent) = parent.as_ref() {
            ensure_parent_linkable(&transaction, card_id, new_parent)?;
        }
        card.parent = parent.clone();
        card.updated_at = now;
        persist_card(&transaction, &card)?;
        let actor = authority.actor_label();
        let label = |value: &Option<CardId>| {
            value
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_string())
        };
        append_card_event(
            &transaction,
            card_id,
            "hierarchy",
            &actor,
            &format!("parent {} -> {}", label(&previous), label(&parent)),
            now,
        )?;
        if let Some(old_parent) = previous.as_ref() {
            if load_card_optional(&transaction, old_parent)?.is_some() {
                append_card_event(
                    &transaction,
                    old_parent,
                    "hierarchy",
                    &actor,
                    &format!("child {card_id} unlinked"),
                    now,
                )?;
            }
        }
        if let Some(new_parent) = parent.as_ref() {
            append_card_event(
                &transaction,
                new_parent,
                "decompose",
                &actor,
                &format!("child {card_id} linked"),
                now,
            )?;
        }
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
        authority.require_holder(card.claim_principal())?;
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
        authority.require_holder(card.claim_principal())?;
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
        authority.require_holder(card.claim_principal())?;
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
        authority.require_holder(card.claim_principal())?;
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

    pub fn attach_image(
        &mut self,
        card_id: &CardId,
        bytes: &[u8],
        mime: &str,
        filename: &str,
        principal: &str,
        now: i64,
    ) -> Result<AttachmentMeta> {
        let authority = Authority::actor(principal.to_owned(), false);
        self.attach_image_as(card_id, bytes, mime, filename, now, &authority)
    }

    pub fn attach_image_as(
        &mut self,
        card_id: &CardId,
        bytes: &[u8],
        mime: &str,
        filename: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<AttachmentMeta> {
        let mime = non_empty("mime", mime)?;
        if !is_supported_image_mime(&mime) {
            return Err(DomainError::validation(
                "mime",
                format!("unsupported image MIME type: {mime}"),
            )
            .into());
        }
        let filename = non_empty_scrubbed("filename", filename)?;
        let principal = authority.principal_name().unwrap_or("unchecked").to_owned();
        let id = format!("{:x}", Sha256::digest(bytes));
        let size = i64::try_from(bytes.len())
            .map_err(|_| DomainError::validation("bytes", "image is too large to store"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_card(&transaction, card_id)?;
        transaction.execute(
            "INSERT INTO attachments (id, mime, size, bytes, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO NOTHING",
            params![id, mime, size, bytes, now],
        )?;
        let (stored_mime, stored_size) = transaction.query_row(
            "SELECT mime, size FROM attachments WHERE id = ?1",
            [&id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        transaction.execute(
            "INSERT INTO card_attachments
             (card_id, attachment_id, filename, created_at, principal)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(card_id, attachment_id) DO UPDATE SET
               filename = excluded.filename,
               created_at = excluded.created_at,
               principal = excluded.principal",
            params![card_id.as_str(), id, filename, now, principal],
        )?;
        append_attributed_card_event(
            &transaction,
            card_id,
            MutationAudit {
                event_type: "attachment",
                actor: &principal,
                payload: "attached image",
                subject_kind: "attachment",
                subject_id: &id,
                authority,
            },
            now,
        )?;
        transaction.commit()?;
        Ok(AttachmentMeta {
            id,
            filename,
            mime: stored_mime,
            size: stored_size,
            created_at: now,
        })
    }

    pub fn attachment_blob(&self, id: &str) -> Result<Option<(String, Vec<u8>)>> {
        Ok(self
            .connection
            .query_row(
                "SELECT mime, bytes FROM attachments WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?)
    }

    pub fn detach(
        &mut self,
        card_id: &CardId,
        attachment_id: &str,
        principal: &str,
        now: i64,
    ) -> Result<()> {
        let authority = Authority::actor(principal.to_owned(), false);
        self.detach_as(card_id, attachment_id, now, &authority)
    }

    pub fn detach_as(
        &mut self,
        card_id: &CardId,
        attachment_id: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<()> {
        let attachment_id = non_empty("attachment_id", attachment_id)?;
        let principal = authority.principal_name().unwrap_or("unchecked").to_owned();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_card(&transaction, card_id)?;
        let removed = transaction.execute(
            "DELETE FROM card_attachments
             WHERE card_id = ?1 AND attachment_id = ?2",
            params![card_id.as_str(), attachment_id],
        )?;
        if removed == 0 {
            return Err(DomainError::not_found("attachment", attachment_id).into());
        }
        let referenced: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM card_attachments WHERE attachment_id = ?1",
            [&attachment_id],
            |row| row.get(0),
        )?;
        if referenced == 0 {
            transaction.execute("DELETE FROM attachments WHERE id = ?1", [&attachment_id])?;
        }
        append_attributed_card_event(
            &transaction,
            card_id,
            MutationAudit {
                event_type: "attachment",
                actor: &principal,
                payload: "detached image",
                subject_kind: "attachment",
                subject_id: &attachment_id,
                authority,
            },
            now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn attachments_for_card(&self, card_id: &CardId) -> Result<Vec<AttachmentMeta>> {
        load_card(&self.connection, card_id)?;
        let mut statement = self.connection.prepare(
            "SELECT card_attachments.attachment_id,
                    card_attachments.filename,
                    attachments.mime,
                    attachments.size,
                    card_attachments.created_at
             FROM card_attachments
             JOIN attachments ON attachments.id = card_attachments.attachment_id
             WHERE card_attachments.card_id = ?1
             ORDER BY card_attachments.created_at ASC, card_attachments.attachment_id ASC",
        )?;
        let attachments = statement
            .query_map([card_id.as_str()], |row| {
                Ok(AttachmentMeta {
                    id: row.get(0)?,
                    filename: row.get(1)?,
                    mime: row.get(2)?,
                    size: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(attachments)
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
        load_card(&transaction, card_id)?;
        let link = insert_link(&transaction, card_id, label, url, now)?;
        append_attributed_card_event(
            &transaction,
            card_id,
            MutationAudit {
                event_type: "link",
                actor: authority.principal_name().unwrap_or("unchecked"),
                payload: "added link",
                subject_kind: "link",
                subject_id: link.id.as_str(),
                authority,
            },
            now,
        )?;
        transaction.commit()?;
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
        let card = load_card(&transaction, card_id)?;
        let id = format!("comment-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
        let comment = Comment {
            id: id.clone(),
            card_id: card_id.clone(),
            author: non_empty_scrubbed("author", author)?,
            body: non_empty_scrubbed("body", body)?,
            created_at: now,
        };
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
            &transaction,
            card_id,
            MutationAudit {
                event_type: "comment",
                actor: &comment.author,
                payload: "added comment",
                subject_kind: "comment",
                subject_id: &comment.id,
                authority,
            },
            now,
        )?;
        events::append_outbound_card_event_for_audit(
            &transaction,
            &card,
            "comment-added",
            &comment.author,
            json!({"author": comment.author.as_str(), "body": comment.body.as_str()}),
            now,
            &audit_event,
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
        self.append_work_log_as(
            card_id,
            agent,
            attribution,
            body,
            now,
            &Authority::unchecked(),
        )
    }

    pub fn append_work_log_as(
        &mut self,
        card_id: &CardId,
        agent: &str,
        attribution: WorkLogAttribution<'_>,
        body: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<WorkLogEntry> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let card = load_card(&transaction, card_id)?;
        let run_id = attribution.run_id.map(RunId::new).transpose()?;
        let id = format!("work-log-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET));
        let entry = WorkLogEntry {
            id: id.clone(),
            card_id: card_id.clone(),
            agent: non_empty("agent", agent)?,
            // Attribution fields are caller-supplied free text too --
            // `reasoning` especially is documented chain-of-thought, the
            // highest-risk leak class this module's scrub exists for.
            model: attribution.model.map(secrets::scrub_secrets),
            reasoning: attribution.reasoning.map(secrets::scrub_secrets),
            harness: attribution.harness.map(secrets::scrub_secrets),
            run_id,
            body: non_empty_scrubbed("body", body)?,
            created_at: now,
        };
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
        let audit_event = append_attributed_card_event(
            &transaction,
            card_id,
            MutationAudit {
                event_type: "work-log",
                actor: &entry.agent,
                payload: "appended work log",
                subject_kind: "work_log",
                subject_id: &entry.id,
                authority,
            },
            now,
        )?;
        events::append_outbound_card_event_for_audit(
            &transaction,
            &card,
            "work-log-appended",
            &entry.agent,
            json!({
                "agent": entry.agent.as_str(),
                "model": entry.model,
                "harness": entry.harness,
            }),
            now,
            &audit_event,
        )?;
        transaction.commit()?;
        Ok(entry)
    }

    pub fn request_input(
        &mut self,
        run_id: &RunId,
        question: &str,
        now: i64,
        authority: &Authority,
    ) -> Result<Run> {
        let question = non_empty_scrubbed("question", question)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut run = answer_loop::load_run(&transaction, run_id)?;
        let mut card = load_card(&transaction, &run.card_id)?;
        if card.claim.as_ref().map(|claim| &claim.run_id) != Some(run_id) {
            return Err(DomainError::conflict(format!(
                "run {run_id} is not the current claim for card {}",
                card.id
            ))
            .into());
        }
        authority.require_holder(card.claim_principal())?;

        card.status = CardStatus::AwaitingInput;
        card.updated_at = now;
        run.state = RunState::AwaitingInput;
        run.updated_at = now;

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
        let proof = proof
            .map(|value| non_empty_scrubbed("proof", value))
            .transpose()?;
        let criterion_proofs = clean_criterion_proofs(criterion_proofs)?;
        let field_note_config = self.field_note_config.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut card = load_card(&transaction, card_id)?;

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
                    "proof": proof,
                    "criteria": card.criteria
                }),
                now,
            )?;
            append_parent_rollup_event(
                &transaction,
                &card,
                &authority.actor_label(),
                &proof
                    .as_deref()
                    .map(|proof| {
                        format!(
                            "child {card_id} completed with proof: {}",
                            EpicState::proof_snippet(proof)
                        )
                    })
                    .unwrap_or_else(|| format!("child {card_id} completed without proof")),
                now,
            )?;
            if let Some(config) = &field_note_config {
                maybe_spawn_field_note_draft(&transaction, &card, proof.as_deref(), config, now)?;
            }
        }
        transaction.commit()?;
        Ok(card)
    }
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

fn is_supported_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
    )
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

fn persist_card(connection: &Connection, card: &Card) -> Result<()> {
    let source_path = card.source.as_ref().map(|source| source.path.as_str());
    let source_digest = card.source.as_ref().map(|source| source.digest.as_str());
    let repo = card
        .repo
        .as_deref()
        .map(|repo| -> Result<String> {
            resolve_registered_repository_for_write(connection, repo, card.updated_at)?.ok_or_else(|| {
                DomainError::validation(
                    "repo",
                    format!(
                        "unregistered repo \"{repo}\": register it first via POST /api/v1/repositories (or the repository-upsert CLI/MCP command)"
                    ),
                )
                .into()
            })
        })
        .transpose()?;
    let claim_principal = card.claim.as_ref().map(|claim| claim.principal.as_str());
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
               priority = excluded.priority,
               estimate = excluded.estimate,
               labels_json = excluded.labels_json,
               assignee = excluded.assignee,
               related_json = excluded.related_json,
               blocks_json = excluded.blocks_json,
               blocked_by_json = excluded.blocked_by_json,
               repo = excluded.repo,
               source_path = excluded.source_path,
               source_digest = excluded.source_digest,
               claim_principal = excluded.claim_principal,
               claim_agent = excluded.claim_agent,
               claim_run_id = excluded.claim_run_id,
               claim_acquired_at = excluded.claim_acquired_at,
               claim_expires_at = excluded.claim_expires_at,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at,
               parent = excluded.parent,
               risk = excluded.risk"
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
            card.estimate.map(Estimate::as_str),
            to_json(&card.labels)?,
            card.assignee,
            to_json(&card.related)?,
            to_json(&card.blocks)?,
            to_json(&card.blocked_by)?,
            repo,
            source_path,
            source_digest,
            claim_principal,
            claim_agent,
            claim_run_id,
            claim_acquired_at,
            claim_expires_at,
            card.created_at,
            card.updated_at,
            card.parent.as_ref().map(CardId::as_str),
            card.risk.map(Risk::as_str)
        ],
    )?;
    Ok(())
}

fn persist_run(connection: &Connection, run: &Run) -> Result<()> {
    connection.execute(
        "INSERT INTO runs (
            id, card_id, state, principal, agent, claim_expires_at, proof,
            created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
           card_id = excluded.card_id,
           state = excluded.state,
           principal = excluded.principal,
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
        principal: None,
        subject_kind: None,
        subject_id: None,
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

fn append_attributed_card_event(
    connection: &Connection,
    card_id: &CardId,
    audit: MutationAudit<'_>,
    now: i64,
) -> Result<CardEvent> {
    let event = CardEvent {
        id: CardEventId::new(format!("event-{}", nanoid::nanoid!(12, &API_KEY_ALPHABET)))?,
        card_id: card_id.clone(),
        event_type: non_empty("event_type", audit.event_type)?,
        actor: non_empty("actor", audit.actor)?,
        payload: audit.payload.to_owned(),
        principal: audit.authority.principal_name().map(str::to_string),
        subject_kind: Some(non_empty("subject_kind", audit.subject_kind)?),
        subject_id: Some(non_empty("subject_id", audit.subject_id)?),
        created_at: now,
    };
    connection.execute(
        "INSERT INTO card_events (
           id, card_id, event_type, actor, payload, principal,
           subject_kind, subject_id, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event.id.as_str(),
            event.card_id.as_str(),
            event.event_type.as_str(),
            event.actor.as_str(),
            event.payload.as_str(),
            event.principal.as_deref(),
            event.subject_kind.as_deref(),
            event.subject_id.as_deref(),
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

/// Full unfiltered card scan, one query -- shared by [`Store::list_ready_page`]
/// and the transitive-blocker walk in `answer_loop::get_card_detail`, so
/// relation-graph traversals never need a second per-blocker query.
pub(crate) fn load_all_cards(connection: &Connection) -> Result<Vec<Card>> {
    let mut statement = connection.prepare(CARD_SELECT_ALL_SQL)?;
    let records = statement
        .query_map([], CardRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    records
        .into_iter()
        .map(|record| card_from_record(connection, record))
        .collect()
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

/// Child outcomes roll up as audit events on the parent: any child
/// transition into a terminal status appends a `rollup` event naming the
/// child and, for completions, a bounded proof snippet. Nothing here changes
/// the parent's own status -- parent acceptance stays authoritative.
fn append_parent_rollup_event(
    connection: &Connection,
    child: &Card,
    actor: &str,
    detail: &str,
    now: i64,
) -> Result<()> {
    let Some(parent_id) = child.parent.as_ref() else {
        return Ok(());
    };
    if load_card_optional(connection, parent_id)?.is_none() {
        return Ok(());
    }
    append_card_event(connection, parent_id, "rollup", actor, detail, now)?;
    Ok(())
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

/// Counts of what an external-source batch upsert did (or, from
/// [`Store::preview_import`], would do) to each card: newly created, content
/// refreshed, lifecycle preserved against a stale reimport, or left
/// untouched because the source has not changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportOutcome {
    pub created: usize,
    pub updated: usize,
    pub preserved: usize,
    pub unchanged: usize,
    /// Cards whose acceptance text actually changed on this reimport even
    /// though the source digest did not: an adapter fix repairing previously
    /// malformed criteria on already-sourced cards.
    /// Scoped to `ReimportClass::Unchanged` specifically -- an ordinary
    /// source edit changes the digest too (`ReimportClass::Updated`), and
    /// that acceptance-text delta is expected, not damage, so it must not
    /// inflate this counter. `preview_import` exposes the repair count before
    /// a batch is written.
    pub content_repaired: usize,
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
    estimate: Option<String>,
    labels_json: String,
    assignee: Option<String>,
    related_json: String,
    blocks_json: String,
    blocked_by_json: String,
    repo: Option<String>,
    source_path: Option<String>,
    source_digest: Option<String>,
    claim_principal: Option<String>,
    claim_agent: Option<String>,
    claim_run_id: Option<String>,
    claim_acquired_at: Option<i64>,
    claim_expires_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
    parent: Option<String>,
    risk: Option<String>,
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
            estimate: row.get(8)?,
            labels_json: row.get(9)?,
            assignee: row.get(10)?,
            related_json: row.get(11)?,
            blocks_json: row.get(12)?,
            blocked_by_json: row.get(13)?,
            repo: row.get(14)?,
            source_path: row.get(15)?,
            source_digest: row.get(16)?,
            claim_principal: row.get(17)?,
            claim_agent: row.get(18)?,
            claim_run_id: row.get(19)?,
            claim_acquired_at: row.get(20)?,
            claim_expires_at: row.get(21)?,
            created_at: row.get(22)?,
            updated_at: row.get(23)?,
            parent: row.get(24)?,
            risk: row.get(25)?,
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
            .with_risk(
                self.risk
                    .map(|raw| {
                        Risk::parse(&raw).ok_or(StoreError::InvalidStoredValue {
                            field: "cards.risk",
                            value: raw,
                        })
                    })
                    .transpose()?,
            )
            .with_created_at(self.created_at);
        if !oracle.criteria.is_empty() {
            card = card.with_criteria(oracle.criteria);
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
        card.parent = self.parent.map(CardId::new).transpose()?;
        card.repo = self.repo.as_deref().and_then(canonical_repo_label);
        card.source = match (self.source_path, self.source_digest) {
            (Some(path), Some(digest)) => Some(CardSource { path, digest }),
            _ => None,
        };
        card.claim = claim;
        card.updated_at = self.updated_at;
        Ok(card)
    }
}

struct RunRecord {
    id: String,
    card_id: String,
    state: String,
    principal: String,
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
            agent: row.get(4)?,
            claim_expires_at: row.get(5)?,
            proof: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
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
            agent: self.agent,
            claim_expires_at: self.claim_expires_at,
            proof: self.proof,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
