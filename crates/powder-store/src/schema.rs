pub const SCHEMA_VERSION: u32 = 23;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS seed_runs (
  seed_name TEXT PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS api_keys (
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
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON api_keys(key_prefix, revoked_at);

CREATE TABLE IF NOT EXISTS cards (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  body TEXT NOT NULL,
  acceptance_json TEXT NOT NULL,
  criteria_json TEXT NOT NULL DEFAULT '[]',
  proof_plan_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL,
  priority TEXT NOT NULL,
  estimate TEXT,
  labels_json TEXT NOT NULL,
  assignee TEXT,
  related_json TEXT NOT NULL,
  blocks_json TEXT NOT NULL,
  blocked_by_json TEXT NOT NULL,
  repo TEXT,
  source_path TEXT,
  source_digest TEXT,
  claim_principal TEXT,
  claim_agent TEXT,
  claim_run_id TEXT,
  claim_acquired_at INTEGER,
  claim_expires_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  parent TEXT,
  risk TEXT
);
CREATE INDEX IF NOT EXISTS idx_cards_status_priority ON cards(status, priority, created_at, id);

CREATE TABLE IF NOT EXISTS attachments (
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
  ON card_attachments(card_id, created_at, attachment_id);
CREATE INDEX IF NOT EXISTS idx_cards_parent ON cards(parent);

CREATE TABLE IF NOT EXISTS repositories (
  name TEXT PRIMARY KEY,
  visibility TEXT NOT NULL DEFAULT 'visible',
  tier TEXT NOT NULL DEFAULT 'backburner',
  import_provenance TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repositories_visibility ON repositories(visibility, name);
CREATE INDEX IF NOT EXISTS idx_repositories_tier ON repositories(tier, name);

CREATE TABLE IF NOT EXISTS repository_aliases (
  alias TEXT PRIMARY KEY,
  repository_name TEXT NOT NULL REFERENCES repositories(name) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repository_aliases_repository ON repository_aliases(repository_name, alias);

CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  state TEXT NOT NULL,
  principal TEXT NOT NULL,
  agent TEXT NOT NULL,
  claim_expires_at INTEGER NOT NULL,
  proof TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_runs_card_created ON runs(card_id, created_at DESC);

CREATE TABLE IF NOT EXISTS activities (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  activity_type TEXT NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_activities_run_created ON activities(run_id, created_at);

CREATE TABLE IF NOT EXISTS card_events (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  event_type TEXT NOT NULL,
  actor TEXT NOT NULL,
  payload TEXT NOT NULL,
  principal TEXT,
  subject_kind TEXT,
  subject_id TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_card_events_card_created ON card_events(card_id, created_at);
CREATE INDEX IF NOT EXISTS idx_card_events_subject ON card_events(card_id, subject_kind, subject_id);

CREATE TABLE IF NOT EXISTS links (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  url TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS comments (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  author TEXT NOT NULL,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS work_log_entries (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  model TEXT,
  reasoning TEXT,
  harness TEXT,
  run_id TEXT,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_work_log_entries_card_created ON work_log_entries(card_id, created_at);

CREATE TABLE IF NOT EXISTS event_subscriptions (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  event_filter_json TEXT NOT NULL,
  signing_secret_hash TEXT NOT NULL,
  signing_secret TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  disabled_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_event_subscriptions_active ON event_subscriptions(disabled_at, created_at, id);

CREATE TABLE IF NOT EXISTS outbound_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  audit_event_id TEXT REFERENCES card_events(id) ON DELETE SET NULL,
  payload_json TEXT NOT NULL,
  occurred_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbound_events_card_created ON outbound_events(card_id, sequence);
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbound_events_audit
  ON outbound_events(audit_event_id) WHERE audit_event_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS webhook_deliveries (
  id TEXT PRIMARY KEY,
  subscription_id TEXT NOT NULL REFERENCES event_subscriptions(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL REFERENCES outbound_events(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  last_attempt_at INTEGER,
  last_status INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(subscription_id, event_id)
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_due ON webhook_deliveries(status, next_attempt_at, id);

CREATE TABLE IF NOT EXISTS webhook_delivery_attempts (
  id TEXT PRIMARY KEY,
  delivery_id TEXT NOT NULL REFERENCES webhook_deliveries(id) ON DELETE CASCADE,
  attempt_number INTEGER NOT NULL,
  status_code INTEGER,
  error TEXT,
  attempted_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_delivery_attempts_delivery ON webhook_delivery_attempts(delivery_id, attempt_number);
"#;

// The `MIGRATE_1_TO_2` step (create `actors`, add `api_keys.actor_id`,
// backfill an actor per key + point each key at it, index) moved inline into
// `Store::migrate_1_to_2`: it is DDL *plus* backfill, and `execute_batch`
// autocommits per statement, so a single all-or-nothing constant guarded on
// column existence would skip the backfill forever after a crash between the
// ALTER and the backfill. See that function's doc comment for the
// three-phase, per-effect idempotency it needs instead.

/// External-content FTS5 schema. The ordinary tables remain the source of truth;
/// triggers keep this derived search spine synchronized in the same transaction
/// as every source write, including direct SQL writers and migration fixtures.
pub const SEARCH_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS search_documents (
  doc_id INTEGER PRIMARY KEY,
  source_table TEXT NOT NULL,
  source_field TEXT NOT NULL,
  source_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  content TEXT NOT NULL,
  UNIQUE(source_table, source_field, source_id)
);
CREATE INDEX IF NOT EXISTS idx_search_documents_card ON search_documents(card_id, source_table, source_field, source_id);

CREATE VIRTUAL TABLE IF NOT EXISTS card_search_fts USING fts5(
  source_table UNINDEXED,
  source_field UNINDEXED,
  source_id UNINDEXED,
  created_at UNINDEXED,
  card_id,
  content,
  content='search_documents',
  content_rowid='doc_id',
  tokenize = 'unicode61 tokenchars ''-_'''
);

CREATE TRIGGER IF NOT EXISTS search_documents_ai AFTER INSERT ON search_documents BEGIN
  INSERT INTO card_search_fts(rowid, source_table, source_field, source_id, created_at, card_id, content)
  VALUES (new.doc_id, new.source_table, new.source_field, new.source_id, new.created_at, new.card_id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS search_documents_ad AFTER DELETE ON search_documents BEGIN
  INSERT INTO card_search_fts(card_search_fts, rowid, source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('delete', old.doc_id, old.source_table, old.source_field, old.source_id, old.created_at, old.card_id, old.content);
END;
CREATE TRIGGER IF NOT EXISTS search_documents_au AFTER UPDATE ON search_documents BEGIN
  INSERT INTO card_search_fts(card_search_fts, rowid, source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('delete', old.doc_id, old.source_table, old.source_field, old.source_id, old.created_at, old.card_id, old.content);
  INSERT INTO card_search_fts(rowid, source_table, source_field, source_id, created_at, card_id, content)
  VALUES (new.doc_id, new.source_table, new.source_field, new.source_id, new.created_at, new.card_id, new.content);
END;

CREATE TRIGGER IF NOT EXISTS cards_search_ai AFTER INSERT ON cards BEGIN
  DELETE FROM search_documents WHERE source_table = 'cards' AND source_id = new.id;
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('cards', 'title', new.id, new.created_at, new.id, new.title);
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('cards', 'body', new.id, new.created_at, new.id, new.body);
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('cards', 'criteria', new.id, new.created_at, new.id,
    COALESCE(
      NULLIF((SELECT group_concat(json_extract(value, '$.text'), ' ')
        FROM json_each(new.criteria_json)
        WHERE json_type(value, '$.text') = 'text'), ''),
      (SELECT group_concat(value, ' ') FROM json_each(new.acceptance_json)),
      ''));
END;
CREATE TRIGGER IF NOT EXISTS cards_search_au AFTER UPDATE OF id, title, body, criteria_json, acceptance_json, created_at ON cards BEGIN
  DELETE FROM search_documents WHERE source_table = 'cards' AND source_id = old.id;
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('cards', 'title', new.id, new.created_at, new.id, new.title);
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('cards', 'body', new.id, new.created_at, new.id, new.body);
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('cards', 'criteria', new.id, new.created_at, new.id,
    COALESCE(
      NULLIF((SELECT group_concat(json_extract(value, '$.text'), ' ')
        FROM json_each(new.criteria_json)
        WHERE json_type(value, '$.text') = 'text'), ''),
      (SELECT group_concat(value, ' ') FROM json_each(new.acceptance_json)),
      ''));
END;
CREATE TRIGGER IF NOT EXISTS cards_search_ad AFTER DELETE ON cards BEGIN
  DELETE FROM search_documents WHERE source_table = 'cards' AND source_id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS comments_search_ai AFTER INSERT ON comments BEGIN
  DELETE FROM search_documents WHERE source_table = 'comments' AND source_id = new.id;
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('comments', 'body', new.id, new.created_at, new.card_id, new.body);
END;
CREATE TRIGGER IF NOT EXISTS comments_search_au AFTER UPDATE OF id, card_id, body, created_at ON comments BEGIN
  DELETE FROM search_documents WHERE source_table = 'comments' AND source_id = old.id;
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('comments', 'body', new.id, new.created_at, new.card_id, new.body);
END;
CREATE TRIGGER IF NOT EXISTS comments_search_ad AFTER DELETE ON comments BEGIN
  DELETE FROM search_documents WHERE source_table = 'comments' AND source_id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS work_log_search_ai AFTER INSERT ON work_log_entries BEGIN
  DELETE FROM search_documents WHERE source_table = 'work_log_entries' AND source_id = new.id;
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('work_log_entries', 'body', new.id, new.created_at, new.card_id, new.body);
END;
CREATE TRIGGER IF NOT EXISTS work_log_search_au AFTER UPDATE OF id, card_id, body, created_at ON work_log_entries BEGIN
  DELETE FROM search_documents WHERE source_table = 'work_log_entries' AND source_id = old.id;
  INSERT INTO search_documents(source_table, source_field, source_id, created_at, card_id, content)
  VALUES ('work_log_entries', 'body', new.id, new.created_at, new.card_id, new.body);
END;
CREATE TRIGGER IF NOT EXISTS work_log_search_ad AFTER DELETE ON work_log_entries BEGIN
  DELETE FROM search_documents WHERE source_table = 'work_log_entries' AND source_id = old.id;
END;
"#;

/// Existing keys were bcrypt-hashed; tag them explicitly so `verify_api_key`
/// keeps using bcrypt for them (they never break) while every newly created
/// key hashes with SHA-256 instead (the correct tool for a high-entropy
/// random secret, and far cheaper than bcrypt's deliberately slow KDF).
pub const MIGRATE_2_TO_3: &str = r#"
ALTER TABLE api_keys ADD COLUMN hash_algorithm TEXT NOT NULL DEFAULT 'bcrypt';
"#;

/// `model`/`turn_count`/`token_count`/`consecutive_failures`/`last_error`/
/// `result` were never set to a real value by any surface -- only ever
/// re-persisted as whatever was already there (0/None from claim time) via
/// the store's own `ON CONFLICT ... = excluded.*` upsert. Dead columns since
/// the day this schema was written; `proof` is untouched, since
/// `complete_card` genuinely writes it.
///
/// powder-epic-truthful-ops: this step's SQL used to live here as
/// `MIGRATE_3_TO_4`, run unconditionally. It now lives inline in
/// `Store::migrate_3_to_4`, one `DROP COLUMN` per dead column, each guarded
/// by `table_has_column` -- a crash partway through the six drops needs to
/// finish only the columns still present on retry, which a single
/// all-or-nothing batch constant can't express. See that function's doc
/// comment.
///
/// The `MIGRATE_4_TO_5` step (the two `cards` `ADD COLUMN`s plus the
/// `card_events` table) moved the same way, into `Store::migrate_4_to_5`.
pub const MIGRATE_5_TO_6: &str = r#"
CREATE TABLE IF NOT EXISTS event_subscriptions (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  event_filter_json TEXT NOT NULL,
  signing_secret_hash TEXT NOT NULL,
  signing_secret TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  disabled_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_event_subscriptions_active ON event_subscriptions(disabled_at, created_at, id);

CREATE TABLE IF NOT EXISTS outbound_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  payload_json TEXT NOT NULL,
  occurred_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_outbound_events_card_created ON outbound_events(card_id, sequence);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
  id TEXT PRIMARY KEY,
  subscription_id TEXT NOT NULL REFERENCES event_subscriptions(id) ON DELETE CASCADE,
  event_id TEXT NOT NULL REFERENCES outbound_events(id) ON DELETE CASCADE,
  status TEXT NOT NULL,
  attempt_count INTEGER NOT NULL DEFAULT 0,
  next_attempt_at INTEGER NOT NULL,
  last_attempt_at INTEGER,
  last_status INTEGER,
  last_error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(subscription_id, event_id)
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_due ON webhook_deliveries(status, next_attempt_at, id);

CREATE TABLE IF NOT EXISTS webhook_delivery_attempts (
  id TEXT PRIMARY KEY,
  delivery_id TEXT NOT NULL REFERENCES webhook_deliveries(id) ON DELETE CASCADE,
  attempt_number INTEGER NOT NULL,
  status_code INTEGER,
  error TEXT,
  attempted_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_webhook_delivery_attempts_delivery ON webhook_delivery_attempts(delivery_id, attempt_number);
"#;

pub const MIGRATE_6_TO_7: &str = r#"
CREATE TABLE IF NOT EXISTS repositories (
  name TEXT PRIMARY KEY,
  visibility TEXT NOT NULL DEFAULT 'visible',
  import_provenance TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repositories_visibility ON repositories(visibility, name);

CREATE TABLE IF NOT EXISTS repository_aliases (
  alias TEXT PRIMARY KEY,
  repository_name TEXT NOT NULL REFERENCES repositories(name) ON DELETE CASCADE,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_repository_aliases_repository ON repository_aliases(repository_name, alias);
"#;

pub const MIGRATE_7_TO_8: &str = r#"
ALTER TABLE repositories ADD COLUMN tier TEXT NOT NULL DEFAULT 'backburner';
CREATE INDEX IF NOT EXISTS idx_repositories_tier ON repositories(tier, name);
"#;

// The `MIGRATE_8_TO_9` step (the two `cards` `ADD COLUMN`s for
// `criteria_json`/`proof_plan_json`) moved into `Store::migrate_8_to_9` for
// the same per-column-guard reason as `MIGRATE_3_TO_4`/`MIGRATE_4_TO_5`
// above.

/// powder-931: key hygiene is currently a manual, error-prone audit against
/// a list with no signal for "is anything still using this". Recording the
/// last successful `verify_api_key` per key makes an orphaned-key inventory
/// mechanical instead of archaeological.
pub const MIGRATE_9_TO_10: &str = r#"
ALTER TABLE api_keys ADD COLUMN last_used_at INTEGER;
"#;

/// powder-943: work_log is a first-class, high-frequency, fully-attributed
/// context field agents append while actively working a card -- distinct
/// from `comments`, which stays low-frequency and human-facing.
pub const MIGRATE_10_TO_11: &str = r#"
CREATE TABLE IF NOT EXISTS work_log_entries (
  id TEXT PRIMARY KEY,
  card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  model TEXT,
  reasoning TEXT,
  harness TEXT,
  run_id TEXT,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_work_log_entries_card_created ON work_log_entries(card_id, created_at);
"#;

/// powder-945: autonomy is a card class (`auto`/`review`), not a lifecycle
/// state. Existing instances default to conservative operator review.
pub const MIGRATE_11_TO_12: &str = r#"
ALTER TABLE cards ADD COLUMN autonomy TEXT NOT NULL DEFAULT 'review';
"#;

/// powder-964: source file's `Estimate: S/M/L/XL` header has no Powder
/// equivalent, so an autonomous chewer has to read a full card body to
/// gauge complexity. Nullable/optional: existing cards are not required to
/// backfill it.
pub const MIGRATE_12_TO_13: &str = r#"
ALTER TABLE cards ADD COLUMN estimate TEXT;
"#;

/// powder-epic-hierarchy-rollup: explicit parent/child hierarchy edge. A
/// child card names its parent; children are derived by query. Nullable --
/// hierarchy is opt-in and `related`/`blocks`/`blocked_by` keep their
/// existing semantics untouched.
pub const MIGRATE_13_TO_14: &str = r#"
ALTER TABLE cards ADD COLUMN parent TEXT;
CREATE INDEX IF NOT EXISTS idx_cards_parent ON cards(parent);
"#;

/// powder-epic-one-card-model: `workspace_path`/`branch_name` were written
/// by a repo-checkout workflow this instance never ran end to end -- every
/// production card carries null in both. `assignee` is untouched; its fate
/// belongs to a different epic. Follows the `MIGRATE_3_TO_4` precedent of
/// dropping dead columns outright rather than carrying them forever.
pub const MIGRATE_14_TO_15: &str = r#"
ALTER TABLE cards DROP COLUMN workspace_path;
ALTER TABLE cards DROP COLUMN branch_name;
"#;

/// powder-autonomy-removal: `autonomy` (`auto`/`review`) gated nothing --
/// `claim_readiness` never consulted it, and the approval queue keys on
/// `run.state` plus approval-labeled links, not autonomy. Pure decorative
/// metadata; dropped outright with no compatibility shim, following the
/// `MIGRATE_3_TO_4`/`MIGRATE_14_TO_15` precedent for dead columns. Legacy
/// values are discarded, not migrated to any replacement field.
pub const MIGRATE_15_TO_16: &str = r#"
ALTER TABLE cards DROP COLUMN autonomy;
"#;

pub const CARD_COLUMNS: &str = "id, title, body, acceptance_json, criteria_json, proof_plan_json, status, priority, estimate, labels_json,
assignee, related_json, blocks_json, blocked_by_json, repo, source_path,
source_digest, claim_principal, claim_agent, claim_run_id, claim_acquired_at, claim_expires_at,
created_at, updated_at, parent, risk";

pub const CARD_SELECT_SQL: &str = "SELECT id, title, body, acceptance_json, criteria_json, proof_plan_json, status, priority, estimate,
labels_json, assignee, related_json, blocks_json, blocked_by_json, repo,
source_path, source_digest, claim_principal, claim_agent, claim_run_id, claim_acquired_at,
claim_expires_at, created_at, updated_at, parent, risk FROM cards WHERE id = ?1";

pub const CARD_SELECT_ALL_SQL: &str = "SELECT id, title, body, acceptance_json, criteria_json, proof_plan_json, status, priority, estimate,
labels_json, assignee, related_json, blocks_json, blocked_by_json, repo,
source_path, source_digest, claim_principal, claim_agent, claim_run_id, claim_acquired_at,
claim_expires_at, created_at, updated_at, parent, risk FROM cards";

pub const RUN_SELECT_SQL: &str =
    "SELECT id, card_id, state, principal, agent, claim_expires_at, proof,
created_at, updated_at FROM runs WHERE id = ?1";
